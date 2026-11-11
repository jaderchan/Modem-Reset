//! 小米路由器（MiWiFi，LuCI 接口）：先登录取得 stok，再调用重启接口。
//!
//! 接口是公开的 MiWR API，没有未公开的路径：
//!   POST /cgi-bin/luci/api/xqsystem/login      -> {"code":0,"token":"<stok>"}
//!   POST /cgi-bin/luci/;stok=<stok>/api/xqsystem/reboot
//!
//! 响应是 JSON，但我们只要 `code` 和 `token` 两个字段，用正则直接从响应文本里取，
//! 不必为此引入 JSON 库（配置文件那边已经彻底不用 JSON 了）。

use std::sync::LazyLock;

use regex_lite::Regex;

use crate::config::Config;
use crate::httpc::HttpClient;
use crate::log;

const LOGIN_PATH: &str = "/cgi-bin/luci/api/xqsystem/login";
const REBOOT_PATH: &str = "/cgi-bin/luci/;stok={stok}/api/xqsystem/reboot";
#[allow(dead_code)] // 预留：目前没有调用
const SHUTDOWN_PATH: &str = "/cgi-bin/luci/;stok={stok}/api/xqsystem/shutdown";

static CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""code"\s*:\s*(-?\d+)"#).unwrap());
static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""token"\s*:\s*"([^"]*)""#).unwrap());

pub struct XiaomiClient {
    http: HttpClient,
    host: String,
    password: String,
    verbose: bool,
    stok: Option<String>,
}

impl XiaomiClient {
    pub fn new(cfg: &Config, verbose: bool) -> Self {
        Self {
            http: HttpClient::new(
                &cfg.router_base(),
                cfg.ont.timeout,
                cfg.ont.retries,
                cfg.ont.retry_backoff,
                verbose,
            ),
            host: cfg.router.host.clone(),
            password: cfg.router.password.clone(),
            verbose,
            stok: None,
        }
    }

    fn log(&self, msg: &str) {
        if self.verbose {
            log::say(msg);
        } else {
            log::write_only(msg);
        }
    }

    pub fn login(&mut self) -> bool {
        if self.password.is_empty() {
            self.log(
                "! 未配置路由器管理密码。请填写 modem.ini 的 [router] password 或设置 ROUTER_PASS",
            );
            return false;
        }

        // 先访问登录页，让服务端建立会话状态（部分固件依赖）
        let _ = self.http.get("/cgi-bin/luci/web");

        let nonce = nonce();
        self.log(&format!("-> 登录路由器 {}", self.host));

        let form = vec![
            ("router_password".to_string(), self.password.clone()),
            ("nonce".to_string(), nonce),
        ];
        let out = match self.http.post_form(LOGIN_PATH, &form) {
            Ok(r) => r.body,
            Err(e) => {
                self.log(&format!("! 登录请求失败: {e}"));
                return false;
            }
        };

        let code = code_of(&out);
        if code == Some(0) {
            self.stok = TOKEN_RE.captures(&out).map(|c| c[1].to_string());
            let shown: String = self.stok.as_deref().unwrap_or("").chars().take(8).collect();
            self.log(&format!("<- 登录成功，token={shown}……"));
            return true;
        }

        self.log(&format!("<- 登录失败 code={code:?} body={}", snippet(&out)));
        false
    }

    pub fn reboot(&mut self) -> bool {
        if self.stok.is_none() && !self.login() {
            return false;
        }
        let stok = self.stok.clone().unwrap_or_default();
        let path = REBOOT_PATH.replace("{stok}", &stok);

        self.log("-> 发送重启请求");
        match self.http.post_form(&path, &[]) {
            Ok(r) => {
                if code_of(&r.body) == Some(0) {
                    self.log("<- 已接受");
                    true
                } else {
                    self.log(&format!("<- 失败 body={}", snippet(&r.body)));
                    false
                }
            }
            Err(e) => {
                // 路由器重启瞬间断连属正常，视作命令已生效
                self.log(&format!("! 连接中断: {e}（命令很可能已生效）"));
                true
            }
        }
    }
}

fn code_of(body: &str) -> Option<i64> {
    CODE_RE
        .captures(body)
        .and_then(|c| c[1].parse::<i64>().ok())
}

fn snippet(body: &str) -> String {
    let s: String = body.chars().take(120).collect();
    s.replace(['\r', '\n'], " ")
}

/// 32 个十六进制字符组成的随机串。官方前端用 `Math.random()` 生成，这里用时间戳和进程号，
/// 只要每次不同即可（nonce 只用于防重放）。
fn nonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = nanos ^ ((std::process::id() as u128) << 96);
    format!("{mixed:032x}")
}
