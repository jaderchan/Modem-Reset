//! 中兴 F612 光猫：登录、解析重启表单、提交重启。
//!
//! 逻辑与 1.x 的 Python 实现一致（该实现不随仓库发布）。

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex_lite::Regex;

use crate::config::Config;
use crate::httpc::HttpClient;

/// pid=1002 是 F612 的主框架页；登录成功后由这个页面加载整个菜单
const MAIN_FRAME_PID: &str = "1002";

// 登录页的 Frm_Logintoken 由内联 JS 在提交前写入（本固件为 "1"）
static LOGIN_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)Frm_Logintoken["']\)\s*\.\s*value\s*=\s*["']([^"']*)["']"#).unwrap()
});

// 每个表单都会由 JS 动态补上 _SESSION_TOKEN，值来自：var session_token = "5867547247862882";
static SESSION_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)session_token\s*=\s*["']([^"']+)["']"#).unwrap());

// 内联脚本调用 Transfer_meaning('IF_ERRORSTR','SUCC')，把值写进隐藏域
static TRANSFER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)Transfer_meaning\(\s*["']([^"']+)["']\s*,\s*["']([^"']*)["']\s*\)"#).unwrap()
});

static INPUT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)<input\b[^>]*>"#).unwrap());

static X_ESCAPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\\x([0-9a-f]{2})"#).unwrap());

pub struct ZteClient {
    pub http: HttpClient,
    host: String,
    username: String,
    password: String,
    verbose: bool,
    pub logged_in: bool,
}

impl ZteClient {
    pub fn new(cfg: &Config, verbose: bool) -> Self {
        Self {
            http: HttpClient::new(
                &cfg.ont_base(),
                cfg.ont.timeout,
                cfg.ont.retries,
                cfg.ont.retry_backoff,
                verbose,
            ),
            host: cfg.ont.host.clone(),
            username: cfg.ont.username.clone(),
            password: cfg.ont.password.clone(),
            verbose,
            logged_in: false,
        }
    }

    fn log(&self, msg: &str) {
        if self.verbose {
            crate::log::say(msg);
        } else {
            crate::log::write_only(msg);
        }
    }

    fn fetch_login_token(&mut self) -> String {
        let Ok(page) = self.http.get("/") else {
            return "0".to_string();
        };
        LOGIN_TOKEN_RE
            .captures(&page.body)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| "0".to_string())
    }

    /// 提交登录表单。返回 true 表示会话可用。
    pub fn login(&mut self) -> bool {
        if self.password.is_empty() {
            self.log("! 未配置密码，无法登录。请填写 modem.ini 的 [ont] password 或设置 ONT_PASS");
            return false;
        }

        let payload = vec![
            ("_lang".to_string(), String::new()),
            ("frashnum".to_string(), String::new()),
            ("action".to_string(), "login".to_string()),
            ("Frm_Logintoken".to_string(), self.fetch_login_token()),
            ("Username".to_string(), self.username.clone()),
            ("Password".to_string(), self.password.clone()),
        ];

        self.log(&format!("-> 登录 {} @ {}", self.username, self.host));

        let resp = match self.http.post_form("/", &payload) {
            Ok(r) => r,
            Err(e) => {
                self.log(&format!("! 登录请求失败: {e}"));
                return false;
            }
        };
        let body = &resp.body;

        // 登录失败次数过多时，光猫会返回锁定倒计时页面，此时必须停下，继续尝试可能锁死账号
        if body.contains("setDisable")
            && body.contains("window.name")
            && body.contains("Frm_Logintoken")
        {
            let head = body.split("<form").next().unwrap_or("");
            if body.contains("errmsg") && !head.contains("action") {
                self.log("! 疑似登录锁定倒计时，已中止，勿重试");
                return false;
            }
        }

        // 跳转到主框架页 = 成功
        if body.contains("getpage.gch") || body.contains("pid=") {
            self.logged_in = true;
            self.log("<- 登录成功");
            return true;
        }

        // 还是登录页 = 失败
        if body.contains("fLogin") || body.contains("Frm_Username") {
            self.log("<- 登录失败（仍在登录页）");
            return false;
        }

        // 有的固件登录后返回空白 302，用主框架页二次确认
        let probe = self
            .http
            .get(&format!("/getpage.gch?pid={MAIN_FRAME_PID}"))
            .map(|r| r.body)
            .unwrap_or_default();
        self.logged_in = !probe.contains("Frm_Username");
        self.log(&format!(
            "<- {}",
            if self.logged_in {
                "登录成功"
            } else {
                "登录失败"
            }
        ));
        self.logged_in
    }

    pub fn logout(&mut self) {
        let _ = self.http.get("/getpage.gch?pid=1001&logout=1");
        self.logged_in = false;
    }

    /// GET 重启页（只读），解析出浏览器提交时会带上的隐藏域和本次会话的 token。
    pub fn fetch_reboot_form(
        &mut self,
        path: &str,
    ) -> Result<(BTreeMap<String, String>, Option<String>), String> {
        let resp = self.http.get(path)?;
        // 登录后的页面里也有一个用于退出的 fLogin 表单，不能据此判断；
        // 真正的登录页一定带用户名输入框 Frm_Username
        if resp.body.contains("Frm_Username") {
            return Err("重启页返回登录页，会话已失效（登录未成功）".to_string());
        }
        Ok(parse_form(&resp.body))
    }
}

/// 从重启页解析表单字段。隐藏域初始 value 常为空，真正的值由紧随其后的
/// `Transfer_meaning` 调用写入，所以隐藏域的 `value` 和 `Transfer_meaning` 调用都要读。
pub fn parse_form(html: &str) -> (BTreeMap<String, String>, Option<String>) {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();

    for tag in INPUT_RE.find_iter(form_block(html, "fSubmit")) {
        let t = tag.as_str();
        if !attr(t, "type")
            .map(|v| v.eq_ignore_ascii_case("hidden"))
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(name) = attr(t, "name").filter(|n| !n.is_empty()) {
            fields.insert(name, attr(t, "value").unwrap_or_default());
        }
    }

    for cap in TRANSFER_RE.captures_iter(html) {
        fields.insert(cap[1].to_string(), js_unescape(&cap[2]));
    }

    let token = SESSION_TOKEN_RE.captures(html).map(|c| c[1].to_string());
    if let Some(t) = &token {
        fields.insert("_SESSION_TOKEN".to_string(), t.clone());
    }

    (fields, token)
}

/// 正则元字符转义。`regex-lite` 没有 `regex::escape`，这里自行实现。
/// `form_id` 是配置或常量传进来的，理论上可能出现元字符，转义一次更稳。
fn escape(s: &str) -> String {
    const SPECIAL: &[char] = &[
        '\\', '.', '+', '*', '?', '(', ')', '|', '[', ']', '{', '}', '^', '$', '#', '&', '-', '~',
    ];
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 截取 `fSubmit` 表单内部，找不到就退回整页。
fn form_block<'a>(html: &'a str, form_id: &str) -> &'a str {
    let pattern = format!(
        r#"(?is)<form\b[^>]*\bid\s*=\s*["']?{}["']?[^>]*>(.*?)</form>"#,
        escape(form_id)
    );
    match Regex::new(&pattern).ok().and_then(|re| re.captures(html)) {
        Some(cap) => cap.get(1).map(|m| m.as_str()).unwrap_or(html),
        None => html,
    }
}

/// 取一个 HTML 标签上的属性值，单引号、双引号和无引号都支持。
fn attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!(r#"(?i)\b{name}\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#);
    let re = Regex::new(&pattern).ok()?;
    let caps = re.captures(tag)?;
    caps.iter()
        .skip(1)
        .flatten()
        .next()
        .map(|m| m.as_str().to_string())
}

/// 还原 Transfer_meaning 里的 `\xNN` 转义（如 `\x2d` -> `-`）。
fn js_unescape(text: &str) -> String {
    X_ESCAPE_RE
        .replace_all(text, |caps: &regex_lite::Captures| {
            let byte = u8::from_str_radix(&caps[1], 16).unwrap_or(b'?');
            (byte as char).to_string()
        })
        .to_string()
}
