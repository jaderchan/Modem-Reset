//! Modem-Reset 2.0：中兴 F612 光猫 / 小米路由器重启工具。
//!
//! 一个 exe，三种用法：
//!   ModemReset.exe              启动托盘（双击）
//!   ModemReset.exe ont          重启光猫（默认预演）
//!   ModemReset.exe router       重启路由器（默认预演）
//!
//! 安全约定（三道闸门，缺一不可）：
//!   1. 必须显式 `--execute`，否则只打印将要发送的请求
//!   2. modem.ini 里 `[reboot] confirmed = yes`
//!   3. 默认还要交互确认（`--yes` 可跳过，仅供托盘和看门狗调用）

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod config;
mod httpc;
mod icon;
mod log;
mod raster;
mod sys;
mod tray;
mod xiaomi;
mod zte;

use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use config::Config;

// 退出码沿用 1.x 的约定，托盘和计划任务靠它判断结果
pub const EXIT_OK: i32 = 0;
pub const EXIT_UNREACHABLE: i32 = 2;
pub const EXIT_LOGIN: i32 = 3;
pub const EXIT_UNCONFIGURED: i32 = 4;
pub const EXIT_GATE: i32 = 5;
pub const EXIT_NOT_BACK: i32 = 6;

fn main() {
    // 必须在创建任何窗口之前调用，否则后面的 DPI 上下文就改不了了
    #[cfg(windows)]
    sys::enable_dpi_awareness();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = parse_args(&args);

    // 只有命令行模式才附加控制台：托盘模式附加后，用户关掉那个 cmd 窗口
    // 会一并结束托盘进程。判断依据是解析后的命令，而不是原始 args；
    // 否则 `ModemReset.exe -c x.ini`（托盘模式）会因为 x.ini 被错误地附加控制台。
    #[cfg(windows)]
    if !is_tray_mode(&parsed) {
        sys::attach_console();
    }

    let code = match parsed {
        Parsed::Help => {
            print_help();
            EXIT_OK
        }
        Parsed::Version => {
            println!("ModemReset {}", env!("CARGO_PKG_VERSION"));
            EXIT_OK
        }
        Parsed::Error { msg, show_help } => {
            eprintln!("{msg}\n");
            if show_help {
                print_help();
            }
            1
        }
        Parsed::Run(opts) => run(opts),
    };
    std::process::exit(code);
}

/// 命令行解析结果。
struct Options {
    execute: bool,
    assume_yes: bool,
    cfg_path: Option<PathBuf>,
    command: String,
}

enum Parsed {
    Run(Options),
    Help,
    Version,
    /// 参数错误；`show_help` 决定要不要附上用法说明
    Error {
        msg: String,
        show_help: bool,
    },
}

fn parse_args(args: &[String]) -> Parsed {
    let mut opts = Options {
        execute: false,
        assume_yes: false,
        cfg_path: None,
        command: String::new(),
    };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--execute" | "-e" => opts.execute = true,
            "--yes" | "-y" => opts.assume_yes = true,
            "--config" | "-c" => {
                i += 1;
                match args.get(i) {
                    Some(p) => opts.cfg_path = Some(PathBuf::from(p)),
                    None => {
                        return Parsed::Error {
                            msg: "--config 后面要跟配置文件路径".into(),
                            show_help: false,
                        };
                    }
                }
            }
            "--help" | "-h" => return Parsed::Help,
            "--version" | "-V" => return Parsed::Version,
            other if other.starts_with('-') => {
                return Parsed::Error {
                    msg: format!("未知参数：{other}"),
                    show_help: true,
                };
            }
            other => opts.command = other.to_string(),
        }
        i += 1;
    }

    Parsed::Run(opts)
}

/// 无命令或 `tray` 才是托盘模式。
#[cfg(windows)]
fn is_tray_mode(parsed: &Parsed) -> bool {
    match parsed {
        Parsed::Run(opts) => opts.command.is_empty() || opts.command == "tray",
        _ => false,
    }
}

fn run(opts: Options) -> i32 {
    let Options {
        execute,
        assume_yes,
        cfg_path,
        command,
    } = opts;

    match command.as_str() {
        "ont" | "reboot" => match load(cfg_path.as_deref()) {
            Ok(cfg) => cmd_ont(&cfg, execute, assume_yes),
            Err(e) => fail(&e),
        },
        "router" | "router-reboot" => match load(cfg_path.as_deref()) {
            Ok(cfg) => cmd_router(&cfg, execute, assume_yes),
            Err(e) => fail(&e),
        },
        "" | "tray" => {
            if !tray::ensure_single() {
                return EXIT_OK; // 已有实例，已经通知它弹面板
            }
            tray::run(cfg_path)
        }
        other => {
            eprintln!("未知命令：{other}\n");
            print_help();
            1
        }
    }
}

fn load(path: Option<&std::path::Path>) -> Result<Config, String> {
    let cfg = config::load(path)?;
    Ok(cfg)
}

fn fail(msg: &str) -> i32 {
    log::say(&format!("× {msg}"));
    #[cfg(windows)]
    if !sys::has_console() {
        sys::error(msg);
    }
    EXIT_UNCONFIGURED
}

// ---------------------------------------------------------------------------
// 光猫
// ---------------------------------------------------------------------------

pub(crate) fn cmd_ont(cfg: &Config, execute: bool, assume_yes: bool) -> i32 {
    let spec = &cfg.reboot;

    if spec.url.is_empty() {
        log::say("× 尚未配置重启接口，请填写 modem.ini 的 [reboot] url");
        return EXIT_UNCONFIGURED;
    }

    log::say("====================================================================");
    log::say("重启请求规格");
    log::say("====================================================================");
    log::say(&format!(
        "  目标 : {} {}{}",
        spec.method,
        cfg.ont_base(),
        spec.url
    ));
    log::say(&format!(
        "  数据 : {}",
        if spec.fields.is_empty() {
            "(空)".to_string()
        } else {
            spec.fields
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&")
        }
    ));
    log::say("  附加 : 执行时先 GET 该页，自动补 _SESSION_TOKEN 等隐藏域");
    log::say(&format!(
        "  确认 : {}",
        if spec.confirmed {
            "已人工确认"
        } else {
            "未确认"
        }
    ));

    if !execute {
        log::say("\n[预演] 未加 --execute，未发送任何请求。");
        log::say(&format!("确认无误后执行：{} ont --execute", exe_name()));
        return EXIT_OK;
    }

    if !spec.confirmed {
        log::say("\n× modem.ini 中 [reboot] confirmed 仍为 no。");
        log::say("  请核实接口无误后再改为 yes，以防误重启。");
        return EXIT_GATE;
    }

    if !assume_yes && !ask("确认重启光猫？网络将中断约 2～5 分钟。[y/N] ") {
        log::say("已取消。");
        return EXIT_OK;
    }

    let mut client = zte::ZteClient::new(cfg, true);
    if !client.login() {
        log::say("× 登录失败，未发送重启请求。");
        return EXIT_LOGIN;
    }

    // 先 GET 一次重启页，读取隐藏域和本次会话的 _SESSION_TOKEN。
    // 浏览器依靠 JS 在提交前给表单补上这个 token。
    let (page_fields, token) = match client.fetch_reboot_form(&spec.url) {
        Ok(v) => v,
        Err(e) => {
            log::say(&format!("× 读取重启页失败: {e}"));
            client.logout();
            return EXIT_UNREACHABLE;
        }
    };

    let mut data = page_fields;
    // ini 里的值优先级最高（`IF_ACTION`、`flag` 等），但 token 必须用本次会话的新值
    for (k, v) in &spec.fields {
        data.insert(k.clone(), v.clone());
    }
    if let Some(t) = &token {
        data.insert("_SESSION_TOKEN".to_string(), t.clone());
    }

    log::say(&format!(
        "<- 已读取表单 {} 个字段（token={}）",
        data.len(),
        if token.is_some() { "有" } else { "无" }
    ));

    let form: Vec<(String, String)> = data.into_iter().collect();
    log::say("-> 发送重启请求");
    match client.http.request(&spec.method, &spec.url, Some(&form)) {
        Ok(resp) => log::say(&format!(
            "<- 已接受  HTTP {}  {} bytes",
            resp.status,
            resp.body.len()
        )),
        Err(e) => {
            // 光猫重启时会直接断开，连接被重置通常意味着命令已生效
            log::say(&format!("! 连接中断: {e}"));
            log::say("  光猫在重启时主动断开属正常现象，命令很可能已生效。");
        }
    }

    if wait_online(&cfg.ont.host, 12, 10) {
        EXIT_OK
    } else {
        EXIT_NOT_BACK
    }
}

// ---------------------------------------------------------------------------
// 路由器
// ---------------------------------------------------------------------------

pub(crate) fn cmd_router(cfg: &Config, execute: bool, assume_yes: bool) -> i32 {
    log::say("====================================================================");
    log::say("重启目标：小米路由器");
    log::say("====================================================================");
    log::say(&format!("  地址 : {}", cfg.router.host));
    log::say("  接口 : POST /cgi-bin/luci/;stok=<token>/api/xqsystem/reboot");
    log::say(&format!(
        "  凭据 : {}",
        if cfg.router.password.is_empty() {
            "未配置（需 modem.ini 的 [router] password 或 ROUTER_PASS）"
        } else {
            "已配置"
        }
    ));

    if !execute {
        log::say("\n[预演] 未加 --execute，未发送任何请求。");
        log::say(&format!("确认后执行：{} router --execute", exe_name()));
        return EXIT_OK;
    }

    if cfg.router.password.is_empty() {
        log::say("\n× 未配置路由器管理密码。");
        return EXIT_GATE;
    }

    if !assume_yes && !ask("确认重启路由器？网络将中断约 1～2 分钟。[y/N] ") {
        log::say("已取消。");
        return EXIT_OK;
    }

    let mut client = xiaomi::XiaomiClient::new(cfg, true);
    if !client.reboot() {
        return EXIT_LOGIN;
    }

    if wait_online(&cfg.router.host, 12, 10) {
        EXIT_OK
    } else {
        EXIT_NOT_BACK
    }
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/// 轮询设备的 80 端口，直到重新上线或超时。
fn wait_online(host: &str, attempts: u32, interval_secs: u64) -> bool {
    log::say("\n等待设备重新上线……");
    for i in 1..=attempts {
        std::thread::sleep(Duration::from_secs(interval_secs));
        let elapsed = i as u64 * interval_secs;
        if tcp_alive(host, 80, 3) {
            log::say(&format!("  {elapsed:>3}s  已恢复"));
            return true;
        }
        log::say(&format!("  {elapsed:>3}s  仍不可达……"));
    }
    log::say(&format!(
        "× {} 分钟内未恢复，请检查电源或手工断电重启。",
        attempts as u64 * interval_secs / 60
    ));
    false
}

fn tcp_alive(host: &str, port: u16, timeout_secs: u64) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(timeout_secs)).is_ok())
}

/// 交互确认。没有控制台（托盘或双击）时，stdin 会直接 EOF 或读取出错，按取消处理。
fn ask(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    line.trim().eq_ignore_ascii_case("y")
}

fn exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "ModemReset.exe".to_string())
}

fn print_help() {
    println!(
        r#"ModemReset {}：中兴 F612 光猫 / 小米路由器重启工具

用法：
  ModemReset.exe                 启动托盘（双击运行）
  ModemReset.exe ont             重启光猫（默认预演）
  ModemReset.exe router          重启路由器（默认预演）

选项：
  -e, --execute                  真正发送重启请求
  -y, --yes                      跳过交互确认（托盘和计划任务用）
  -c, --config <路径>            指定配置文件（默认读取 exe 同目录的 modem.ini）
  -h, --help                     显示本帮助
  -V, --version                  显示版本

退出码：0 成功 / 2 设备不可达 / 3 登录失败 / 4 未配置
        5 缺少密码或未确认 / 6 命令已发出但未恢复
"#,
        env!("CARGO_PKG_VERSION")
    );
}
