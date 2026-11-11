//! Windows 平台杂项：消息框、控制台附着、打开文件。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Console::{
    GetConsoleWindow, GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleOutputCP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNO, MessageBoxW,
};

const MB_YES: i32 = 6;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 开启 DPI 感知。
///
/// 不做这一步，在 125%、150% 和 200% 缩放下 Windows 会把整个窗口当位图拉伸：
/// 文字发虚、`GetSystemMetrics` 拿到的是 DPI 虚拟化后的坐标、窗口居中也会偏。
///
/// 顺序：每显示器 DPI 感知 v2（Windows 10 1703 及以上）→ 每显示器 DPI 感知
/// （Windows 8.1 及以上）→ 系统级 DPI 感知（Vista 及以上）。
pub fn enable_dpi_awareness() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, PROCESS_PER_MONITOR_DPI_AWARE,
        SetProcessDpiAwareness, SetProcessDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware;
    // SAFETY: 三个都是进程级的一次性设置，任意一个成功即可，失败不影响后续调用。
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 {
            return;
        }
        if SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE) == 0 {
            return; // S_OK
        }
        SetProcessDPIAware();
    }
}

/// 坐标为 (x, y) 的点所在显示器的 DPI（96 表示 100%）。用于建窗口前估算尺寸。
pub fn dpi_at(x: i32, y: i32) -> u32 {
    use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    let point = windows_sys::Win32::Foundation::POINT { x, y };
    // SAFETY: 只做查询，返回的 HMONITOR 不由我们释放。
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (0u32, 0u32);
        if !monitor.is_null()
            && GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) == 0
            && dx >= 48
        {
            return dx;
        }
        let system = windows_sys::Win32::UI::HiDpi::GetDpiForSystem();
        if system >= 48 { system } else { 96 }
    }
}

/// 坐标为 (x, y) 的点所在显示器的工作区（已排除任务栏）。
pub fn work_area_at(x: i32, y: i32) -> windows_sys::Win32::Foundation::RECT {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };

    // SAFETY: MONITORINFO 是 POD，cbSize 按文档填好；只读取系统写回的数据。
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if !monitor.is_null() && GetMonitorInfoW(monitor, &mut info) != 0 {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1024,
                bottom: 768,
            }
        }
    }
}

/// 当前进程是否连着控制台。双击启动和托盘模式下为 false。
pub fn has_console() -> bool {
    // SAFETY: 无参查询，返回 NULL 表示没有控制台。
    unsafe { !GetConsoleWindow().is_null() }
}

/// stdout 是否可用：控制台、重定向到文件、管道都算。
/// 双击启动的 GUI 进程这里拿不到句柄，`println!` 的输出会被丢弃。
pub fn stdout_ok() -> bool {
    const INVALID_HANDLE: *mut core::ffi::c_void = -1isize as *mut core::ffi::c_void;
    // SAFETY: 无参查询。
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        !h.is_null() && h != INVALID_HANDLE
    }
}

/// 显示消息框，返回用户点击的按钮 ID。
pub fn message_box(text: &str, title: &str, flags: u32) -> i32 {
    let t = wide(text);
    let c = wide(title);
    // SAFETY: 两个指针都是本次调用期间有效的、以 NUL 结尾的 UTF-16 缓冲区。
    unsafe {
        MessageBoxW(
            std::ptr::null_mut::<core::ffi::c_void>() as HWND,
            t.as_ptr(),
            c.as_ptr(),
            flags,
        )
    }
}

pub fn info(text: &str) {
    message_box(
        text,
        "Modem-Reset",
        MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
    );
}

pub fn error(text: &str) {
    message_box(
        text,
        "Modem-Reset",
        MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
    );
}

/// 危险操作前的二次确认。
pub fn confirm(text: &str) -> bool {
    message_box(
        text,
        "Modem-Reset",
        MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
    ) == MB_YES
}

/// 用 `cmd /c start` 交给系统默认程序打开；这样不必直接调用 `ShellExecuteW`。
pub fn open_path(path: &std::path::Path) {
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", ""])
        .arg(path)
        .spawn();
}

/// 本程序是 GUI 子系统（双击不闪黑框），从 cmd 或 PowerShell 启动时手动附加到
/// 父进程的控制台，CLI 模式才有输出。
pub fn attach_console() {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    const INVALID_HANDLE: *mut core::ffi::c_void = -1isize as *mut core::ffi::c_void;
    // SAFETY: 全是无状态的查询/设置调用，句柄由系统提供。
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let has_handle = !h.is_null() && h != INVALID_HANDLE;

        if has_handle {
            let kind = GetFileType(h);
            // 已经重定向到文件或管道：保持原样，否则输出会写到别处
            if kind == FILE_TYPE_PIPE || kind == FILE_TYPE_DISK {
                return;
            }
        } else if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return; // 双击启动：没有父控制台，正常情况
        }

        // 走到这里说明输出目标是控制台。Rust 的 println! 写 UTF-8 字节，
        // 而中文系统控制台默认 GBK（代码页 936），不切代码页就是乱码（等价于 chcp 65001）。
        let _ = FILE_TYPE_CHAR;
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
    }
}
