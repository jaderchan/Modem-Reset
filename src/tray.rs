//! 托盘外壳：Win32 `Shell_NotifyIcon` + 右键菜单 + 双击弹出的小面板。
//!
//! 刻意不引入 `tray-icon` 和 `winit`：只用一个隐藏窗口收消息，菜单每次现建现删，
//! 重启流程丢到工作线程，UI 线程永不阻塞。二进制因此能稳定在 1～2 MB。
//!
//! 交互：
//! * 左键双击或再次启动 exe → 三按钮面板（重启路由器、重启光猫、关闭）
//! * 右键 → 菜单（含状态行、打开配置文件和日志、开机自启、退出）

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DEFAULT_GUI_FONT,
    DEFAULT_PITCH, DeleteObject, FW_NORMAL, GetStockObject, HFONT, OUT_DEFAULT_PRECIS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, AppendMenuW, BS_PUSHBUTTON, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, FindWindowW, GetClientRect,
    GetCursorPos, GetMessageW, GetSystemMetrics, IDC_ARROW, IsWindow, LoadCursorW, MF_CHECKED,
    MF_DISABLED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, MoveWindow, PostMessageW,
    PostQuitMessage, RegisterClassW, SM_CXSMICON, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOZORDER,
    SendMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_DPICHANGED, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WM_SETFONT, WNDCLASSW, WS_CAPTION,
    WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

use crate::{config, icon, log, sys};

// 自定义消息
const WM_TRAY: u32 = WM_APP + 1; // 托盘图标的鼠标事件
const WM_RESULT: u32 = WM_APP + 2; // 工作线程回报结果
const WM_SHOWPANEL: u32 = WM_APP + 3; // 第二个实例请求弹出面板

// 菜单和按钮命令 ID
const MENU_ROUTER: usize = 101;
const MENU_ONT: usize = 102;
const MENU_CONFIG: usize = 103;
const MENU_LOG: usize = 104;
const MENU_AUTOSTART: usize = 105;
const MENU_QUIT: usize = 106;

const BTN_ROUTER: usize = 201;
const BTN_ONT: usize = 202;
const BTN_CLOSE: usize = 203;

const TRAY_CLASS: &str = "ModemResetTrayWnd";
const PANEL_CLASS: &str = "ModemResetPanelWnd";
const TRAY_UID: u32 = 1;
const MUTEX_NAME: &str = "Local\\ModemResetTraySingleton";

static BUSY: AtomicBool = AtomicBool::new(false);
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static PANEL_HWND: AtomicIsize = AtomicIsize::new(0);
static PANEL_FONT: AtomicIsize = AtomicIsize::new(0);
static TRAY_ICON: AtomicIsize = AtomicIsize::new(0);
static LAST_STATUS: Mutex<String> = Mutex::new(String::new());
static PANEL_CHILDREN: Mutex<Vec<isize>> = Mutex::new(Vec::new());
/// 托盘进程实际使用的配置文件路径（`-c` 指定的，或 exe 同目录的 modem.ini）
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone, Copy)]
enum Target {
    Ont = 1,
    Router = 2,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Target::Ont => "光猫",
            Target::Router => "路由器",
        }
    }

    fn confirm_text(self) -> &'static str {
        match self {
            Target::Ont => "确认重启光猫（ZTE F612）？\n\n网络将中断约 2～5 分钟。",
            Target::Router => "确认重启小米路由器？\n\n网络将中断约 1～2 分钟。",
        }
    }
}

// ---------------------------------------------------------------------------
// 单实例
// ---------------------------------------------------------------------------

/// 尽量保证只有一个托盘进程。已有实例时通知它弹面板并返回 false；
/// 互斥量创建失败时返回 true，避免把用户挡在门外。
pub fn ensure_single() -> bool {
    let name = wide(MUTEX_NAME);
    // SAFETY: 命名的互斥量在进程生命周期内保持有效（故意不释放句柄）
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    if handle.is_null() {
        return true; // 创建失败就当没有限制，避免挡住用户
    }
    if !already {
        return true;
    }

    // 最多等 1 秒，等第一个实例把窗口建好；仍找不到就静默退出
    let class = wide(TRAY_CLASS);
    for _ in 0..20 {
        // SAFETY: class 是有效的 NUL 结尾 UTF-16 字符串
        let hwnd = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
        if !hwnd.is_null() {
            // SAFETY: hwnd 来自系统，消息号自定义且在 wndproc 里有处理
            unsafe { PostMessageW(hwnd, WM_SHOWPANEL, 0, 0) };
            log::write_only("检测到已有实例，已请求它弹出面板");
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

pub fn run(config_file: Option<PathBuf>) -> i32 {
    // 只存一次：后续都走 config_path()，避免各处重复推导默认路径
    let _ = CONFIG_PATH.set(config_file.unwrap_or_else(config::default_path));
    log::write_only(&format!("配置文件：{}", config_path().display()));

    // SAFETY: 整个函数都是 Win32 调用；所有指针都指向本函数栈上、调用期间有效的缓冲区。
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        if hinst.is_null() {
            return 1;
        }

        register_class(hinst, TRAY_CLASS, Some(tray_proc));
        register_class(hinst, PANEL_CLASS, Some(panel_proc));

        let (class, title) = (wide(TRAY_CLASS), wide("Modem-Reset"));
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            0, // 隐藏窗口，只用来收消息
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            sys::error("创建托盘窗口失败，程序无法启动。");
            return 1;
        }
        TRAY_HWND.store(hwnd as isize, Ordering::SeqCst);
        log::write_only(&format!("托盘窗口 hwnd={:x}", hwnd as isize));

        let hicon = icon::power_icon(tray_icon_size());
        if hicon.is_null() {
            log::write_only("! 托盘图标创建失败（CreateIconIndirect 返回空）");
        }
        TRAY_ICON.store(hicon as isize, Ordering::SeqCst);
        if shell_icon(NIM_ADD, hwnd, hicon) {
            log::write_only("托盘图标已添加");
        } else {
            // 不弹模态框，否则资源管理器尚未启动时会卡住整个启动流程
            log::write_only("! 托盘图标添加失败（资源管理器可能未响应）");
        }
        set_tip(hwnd, "Modem-Reset：右键重启路由器和光猫");

        log::write_only("托盘启动");

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        shell_icon(NIM_DELETE, hwnd, std::ptr::null_mut());
        log::write_only("托盘退出");
        0
    }
}

unsafe fn register_class(
    hinst: windows_sys::Win32::Foundation::HINSTANCE,
    name: &str,
    proc_: windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC,
) {
    unsafe {
        let class = wide(name);
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: proc_,
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
    }
}

// ---------------------------------------------------------------------------
// 窗口过程
// ---------------------------------------------------------------------------

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe {
        match msg {
            WM_TRAY => {
                match (lp as u32) & 0xFFFF {
                    WM_RBUTTONUP => show_menu(hwnd),
                    WM_LBUTTONDBLCLK => show_panel(),
                    _ => {}
                }
                0
            }
            WM_SHOWPANEL => {
                show_panel();
                0
            }
            WM_COMMAND => {
                on_command(hwnd, wp & 0xFFFF);
                0
            }
            WM_RESULT => {
                report(hwnd, wp as i32, lp as i32);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = wp & 0xFFFF;
                if PANEL_HWND.load(Ordering::SeqCst) == hwnd as isize {
                    PANEL_HWND.store(0, Ordering::SeqCst);
                }
                DestroyWindow(hwnd);
                match id {
                    BTN_ROUTER => start(Target::Router),
                    BTN_ONT => start(Target::Ont),
                    _ => {}
                }
                0
            }
            WM_CLOSE => {
                PANEL_HWND.store(0, Ordering::SeqCst);
                DestroyWindow(hwnd);
                0
            }
            WM_DPICHANGED => {
                // 面板被拖到另一块缩放不同的屏幕上：按系统建议的矩形重建尺寸，
                // 再按新的 DPI 重新摆放按钮、更换字体。
                let dpi = (wp & 0xFFFF) as u32;
                // SAFETY: WM_DPICHANGED 的 lParam 按文档指向一个系统提供的 RECT，
                // 在本次消息处理期间有效。
                let suggested = &*(lp as *const RECT);
                log::write_only(&format!(
                    "面板 WM_DPICHANGED dpi={dpi} 建议={}x{}",
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top
                ));
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                layout_children(hwnd, dpi);
                0
            }
            WM_DESTROY => {
                if PANEL_HWND.load(Ordering::SeqCst) == hwnd as isize {
                    PANEL_HWND.store(0, Ordering::SeqCst);
                }
                if let Ok(mut children) = PANEL_CHILDREN.lock() {
                    children.clear();
                }
                let font = PANEL_FONT.swap(0, Ordering::SeqCst);
                if font != 0 {
                    DeleteObject(font as _);
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

// ---------------------------------------------------------------------------
// 菜单
// ---------------------------------------------------------------------------

unsafe fn show_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }

        let status = LAST_STATUS
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| String::new());
        if !status.is_empty() {
            add_item(menu, MF_STRING | MF_DISABLED | MF_GRAYED, 0, &status);
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        }

        add_item(menu, MF_STRING, MENU_ROUTER, "重启路由器（小米）");
        add_item(menu, MF_STRING, MENU_ONT, "重启光猫（ZTE F612）");
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        add_item(menu, MF_STRING, MENU_CONFIG, "打开配置文件");
        add_item(menu, MF_STRING, MENU_LOG, "打开日志");

        let auto = crate::autostart::enabled();
        add_item(
            menu,
            MF_STRING | if auto { MF_CHECKED } else { 0 },
            MENU_AUTOSTART,
            "开机自启",
        );

        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        add_item(menu, MF_STRING, MENU_QUIT, "退出");

        let mut pt: POINT = std::mem::zeroed();
        GetCursorPos(&mut pt);

        // 不先调用 SetForegroundWindow，点击别处时菜单可能不会消失（Win32 的已知行为）
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        PostMessageW(hwnd, WM_NULL, 0, 0);

        if cmd > 0 {
            on_command(hwnd, cmd as usize);
        }
    }
}

unsafe fn add_item(menu: *mut core::ffi::c_void, flags: u32, id: usize, text: &str) {
    unsafe {
        let w = wide(text);
        AppendMenuW(menu, flags, id, w.as_ptr());
    }
}

unsafe fn on_command(hwnd: HWND, id: usize) {
    unsafe {
        match id {
            MENU_ROUTER => start(Target::Router),
            MENU_ONT => start(Target::Ont),
            MENU_CONFIG => {
                let path = config_path();
                if path.exists() {
                    sys::open_path(&path);
                } else {
                    sys::info(&format!(
                        "还没有配置文件。\n\n请把 modem.ini.example 改名为\n{}",
                        path.display()
                    ));
                }
            }
            MENU_LOG => {
                let path = log::path();
                if path.exists() {
                    sys::open_path(&path);
                } else {
                    sys::info("还没有日志。");
                }
            }
            MENU_AUTOSTART => match toggle_autostart() {
                Ok(on) => set_tip(
                    hwnd,
                    &format!(
                        "Modem-Reset：开机自启已{}",
                        if on { "开启" } else { "关闭" }
                    ),
                ),
                Err(rc) => sys::error(&format!("修改开机自启失败（错误码 {rc}）")),
            },
            MENU_QUIT => {
                DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

fn toggle_autostart() -> Result<bool, u32> {
    let on = !crate::autostart::enabled();
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|_| 1u32)?;
    crate::autostart::set(on, &exe)?;
    Ok(on)
}

// ---------------------------------------------------------------------------
// 三按钮面板
// ---------------------------------------------------------------------------

unsafe fn show_panel() {
    unsafe {
        let existing = PANEL_HWND.load(Ordering::SeqCst);
        if existing != 0 && IsWindow(existing as HWND) != 0 {
            SetForegroundWindow(existing as HWND);
            return;
        }

        let hinst = GetModuleHandleW(std::ptr::null());

        // 面板出现在鼠标所在的那块屏幕上，尺寸按那块屏的 DPI 换算
        let mut cursor: POINT = std::mem::zeroed();
        GetCursorPos(&mut cursor);
        let dpi = sys::dpi_at(cursor.x, cursor.y);
        let m = metrics(dpi);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE;
        let ex_style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
        // 用 AdjustWindowRectEx 把“客户区尺寸”换算成“窗口尺寸”，
        // 否则标题栏和边框会挤占按钮的空间。
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: m.client_w,
            bottom: m.client_h,
        };
        AdjustWindowRectEx(&mut frame, style, 0, ex_style);
        let (win_w, win_h) = (frame.right - frame.left, frame.bottom - frame.top);
        log::write_only(&format!(
            "面板 dpi={dpi} 客户区={}x{} 窗口={win_w}x{win_h}",
            m.client_w, m.client_h
        ));

        let work = sys::work_area_at(cursor.x, cursor.y);
        let x = work.left + ((work.right - work.left) - win_w) / 2;
        let y = work.top + ((work.bottom - work.top) - win_h) / 2;

        let (class, title) = (wide(PANEL_CLASS), wide("Modem-Reset"));
        let hwnd = CreateWindowExW(
            ex_style,
            class.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            win_w,
            win_h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        PANEL_HWND.store(hwnd as isize, Ordering::SeqCst);

        let button = wide("BUTTON");
        let items: [(usize, &str); 3] = [
            (BTN_ROUTER, "重启路由器（小米）"),
            (BTN_ONT, "重启光猫（ZTE F612）"),
            (BTN_CLOSE, "关闭"),
        ];
        let mut children = Vec::with_capacity(items.len());
        for (id, label) in items {
            let text = wide(label);
            let child = CreateWindowExW(
                0,
                button.as_ptr(),
                text.as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
                0,
                0,
                10,
                10, // 先占位，layout_children 会按 DPI 摆放
                hwnd,
                id as *mut core::ffi::c_void,
                hinst,
                std::ptr::null(),
            );
            if !child.is_null() {
                children.push(child as isize);
            }
        }
        if let Ok(mut stored) = PANEL_CHILDREN.lock() {
            *stored = children;
        }
        layout_children(hwnd, dpi);

        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
    }
}

/// 三个按钮在 96 DPI 下的逻辑尺寸，其余 DPI 按比例放大。
struct Metrics {
    client_w: i32,
    client_h: i32,
    margin: i32,
    top: i32,
    btn_h: i32,
    gap: i32,
}

fn scale96(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64 + 48) / 96) as i32
}

fn metrics(dpi: u32) -> Metrics {
    let margin = scale96(18, dpi);
    let top = scale96(16, dpi);
    let btn_h = scale96(34, dpi);
    let gap = scale96(12, dpi);
    Metrics {
        client_w: scale96(360, dpi),
        client_h: top * 2 + btn_h * 3 + gap * 2,
        margin,
        top,
        btn_h,
        gap,
    }
}

/// 按当前 DPI 重新摆放面板按钮并设置同 DPI 的字体。
unsafe fn layout_children(hwnd: HWND, dpi: u32) {
    unsafe {
        let m = metrics(dpi);
        let font = panel_font(dpi);

        // 客户区宽度未必等于 m.client_w：标题栏和边框不随 DPI 等比缩放，
        // 所以按窗口的实际客户区摆，避免按钮偏窄或溢出。
        let mut rc: RECT = std::mem::zeroed();
        // SAFETY: hwnd 是本进程的面板窗口，rc 是栈上输出参数。
        GetClientRect(hwnd, &mut rc);
        let avail = rc.right - rc.left;
        let width = if avail > m.margin * 2 {
            avail - m.margin * 2
        } else {
            m.client_w - m.margin * 2
        };

        let children = PANEL_CHILDREN.lock().map(|v| v.clone()).unwrap_or_default();

        for (i, child) in children.iter().enumerate() {
            let y = m.top + (m.btn_h + m.gap) * i as i32;
            MoveWindow(*child as HWND, m.margin, y, width, m.btn_h, 1);
            if !font.is_null() {
                SendMessageW(*child as HWND, WM_SETFONT, font as usize, 1);
            }
        }
    }
}

/// 创建一个与 DPI 匹配的 UI 字体；失败时返回系统默认字体，调用方不得删除该句柄。
unsafe fn panel_font(dpi: u32) -> HFONT {
    unsafe {
        let previous = PANEL_FONT.swap(0, Ordering::SeqCst);
        if previous != 0 {
            DeleteObject(previous as _);
        }

        let face: Vec<u16> = "Microsoft YaHei UI\0".encode_utf16().collect();
        let font = CreateFontW(
            -scale96(12, dpi), // 约 9 pt
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            face.as_ptr(),
        );
        if font.is_null() {
            return GetStockObject(DEFAULT_GUI_FONT);
        }
        PANEL_FONT.store(font as isize, Ordering::SeqCst);
        font
    }
}

/// 托盘图标按系统小图标尺寸渲染（100% 是 16 px，200% 是 32 px）。
fn tray_icon_size() -> u32 {
    // SAFETY: 无参查询
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if size >= 16 { size as u32 } else { 16 }
}

// ---------------------------------------------------------------------------
// 重启流程（工作线程）
// ---------------------------------------------------------------------------

/// 托盘进程使用的配置文件路径。`-c` 没给时就是 exe 同目录的 modem.ini。
fn config_path() -> PathBuf {
    CONFIG_PATH
        .get()
        .cloned()
        .unwrap_or_else(config::default_path)
}

fn start(target: Target) {
    if BUSY.swap(true, Ordering::SeqCst) {
        sys::info("已有任务正在执行，请稍候。");
        return;
    }
    if !sys::confirm(target.confirm_text()) {
        BUSY.store(false, Ordering::SeqCst);
        return;
    }

    let tray = TRAY_HWND.load(Ordering::SeqCst);
    let label = target.label();
    let path = config_path();
    thread::spawn(move || {
        log::write_only(&format!("开始重启{label}"));
        let rc = match config::load(Some(&path)) {
            Ok(cfg) => {
                log::write_only(&format!("使用配置 {}", cfg.path.display()));
                match target {
                    Target::Ont => crate::cmd_ont(&cfg, true, true),
                    Target::Router => crate::cmd_router(&cfg, true, true),
                }
            }
            Err(e) => {
                log::write_only(&format!("× {e}"));
                crate::EXIT_UNCONFIGURED
            }
        };
        BUSY.store(false, Ordering::SeqCst);

        // SAFETY: tray 是本进程的窗口句柄；窗口可能在退出时已销毁，PostMessage 会失败，
        // 此时进程本来也要结束了，忽略返回值。
        unsafe { PostMessageW(tray as HWND, WM_RESULT, rc as usize, target as isize) };
    });
}

/// 工作线程的结果回到 UI 线程后统一提示。
unsafe fn report(hwnd: HWND, rc: i32, target_raw: i32) {
    let target = if target_raw == Target::Ont as i32 {
        "光猫"
    } else {
        "路由器"
    };
    let hint = match rc {
        0 => "已重新上线",
        2 => "设备不可达",
        3 => "登录失败，请检查密码",
        4 => "尚未配置重启接口",
        5 => "缺少密码，或未确认",
        6 => "命令已发出，但等待时间内未恢复",
        _ => "未知错误",
    };

    let status = format!("上次：{target}{hint} {}", short_time());
    if let Ok(mut s) = LAST_STATUS.lock() {
        *s = status.clone();
    }
    log::write_only(&format!("[{target}] rc={rc} {hint}"));

    set_tip(hwnd, &format!("Modem-Reset：{hint}"));
    balloon(
        hwnd,
        if rc == 0 { "完成" } else { "失败" },
        &status,
        rc != 0,
    );
}

fn short_time() -> String {
    let s = log::stamp();
    s.rsplit(' ').next().unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// Shell_NotifyIcon 封装
// ---------------------------------------------------------------------------

fn base_nid(
    hwnd: HWND,
    hicon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW 是 POD，全零后再逐字段填写是标准初始化方式。
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = hicon;
    nid
}

unsafe fn shell_icon(
    op: u32,
    hwnd: HWND,
    hicon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
) -> bool {
    unsafe {
        let mut nid = base_nid(hwnd, hicon);
        copy_text(&mut nid.szTip, "Modem-Reset");
        Shell_NotifyIconW(op, &nid) != 0
    }
}

fn set_tip(hwnd: HWND, text: &str) {
    let hicon =
        TRAY_ICON.load(Ordering::SeqCst) as windows_sys::Win32::UI::WindowsAndMessaging::HICON;
    // SAFETY: hwnd 有效；文本长度由 copy_text 截断到缓冲区大小。
    unsafe {
        let mut nid = base_nid(hwnd, hicon);
        copy_text(&mut nid.szTip, text);
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn balloon(hwnd: HWND, title: &str, body: &str, is_error: bool) {
    let hicon =
        TRAY_ICON.load(Ordering::SeqCst) as windows_sys::Win32::UI::WindowsAndMessaging::HICON;
    // SAFETY: 同上；szInfo/szInfoTitle 在调用期间有效。
    unsafe {
        let mut nid = base_nid(hwnd, hicon);
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = if is_error { NIIF_ERROR } else { NIIF_INFO };
        copy_text(&mut nid.szInfoTitle, title);
        copy_text(&mut nid.szInfo, body);
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// 把 `&str` 写进定长 UTF-16 缓冲区，保证有结尾 NUL，超长截断。
fn copy_text(dst: &mut [u16], src: &str) {
    if dst.is_empty() {
        return;
    }
    let mut i = 0;
    for ch in src.encode_utf16() {
        if i + 1 >= dst.len() {
            break;
        }
        dst[i] = ch;
        i += 1;
    }
    dst[i] = 0;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_metrics_scale_with_dpi() {
        // 96 DPI = 100%
        let m = metrics(96);
        assert_eq!((m.client_w, m.client_h), (360, 158));
        assert_eq!((m.margin, m.top, m.btn_h, m.gap), (18, 16, 34, 12));

        // 144 DPI = 150%，所有尺寸等比放大，按钮不会互相压住
        let m = metrics(144);
        assert_eq!((m.client_w, m.client_h), (540, 237));
        assert!(m.client_h > m.top * 2 + m.btn_h * 3);
    }
}
