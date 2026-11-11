//! `modem.ini` 的读取。
//!
//! 格式约定（保持极简，方便手改）：
//! * `[section]` 分段，`key = value` 赋值；
//! * `;` 或 `#` 开头的整行是注释，**不支持行尾注释**：URL 里可能出现 `;`，
//!   按分号截断会破坏 URL；
//! * 只按**第一个** `=` 切分，后面的 `=` 原样留在 value 里；
//! * 查找时不区分大小写，保存时保留原始大小写。`[reboot.fields]` 里的
//!   `IF_ACTION` 是要原样发给光猫的表单字段名，不能变小写。
//!
//! 优先级：环境变量 > ini > 内置默认值。

use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "modem.ini";

// ---------------------------------------------------------------------------
// 结构体
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OntCfg {
    pub host: String,
    pub username: String,
    pub password: String,
    pub timeout: u64,
    pub retries: u32,
    pub retry_backoff: f32,
}

#[derive(Debug, Clone)]
pub struct RouterCfg {
    pub host: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct RebootSpec {
    pub url: String,
    pub method: String,
    /// 人工确认标志：只有显式写 `yes` 才允许真正提交
    pub confirmed: bool,
    /// `[reboot.fields]` 里的固定表单字段，执行时会覆盖页面隐藏域里的同名字段
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// 实际读取的配置文件路径（日志里会打印，托盘菜单也按它打开文件）
    pub path: PathBuf,
    pub ont: OntCfg,
    pub router: RouterCfg,
    pub reboot: RebootSpec,
}

impl Config {
    pub fn ont_base(&self) -> String {
        format!("http://{}", self.ont.host)
    }

    pub fn router_base(&self) -> String {
        format!("http://{}", self.router.host)
    }
}

// ---------------------------------------------------------------------------
// 默认值
// ---------------------------------------------------------------------------

fn default_ont() -> OntCfg {
    OntCfg {
        host: "192.168.1.1".into(),
        username: "useradmin".into(),
        password: String::new(),
        timeout: 8,
        retries: 3,
        retry_backoff: 1.5,
    }
}

fn default_router() -> RouterCfg {
    RouterCfg {
        host: "192.168.31.1".into(),
        password: String::new(),
    }
}

fn default_reboot() -> RebootSpec {
    RebootSpec {
        url: String::new(),
        method: "POST".into(),
        confirmed: false,
        fields: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// ini 解析
// ---------------------------------------------------------------------------

/// 一条 ini 记录。`section` 和 `key` 保留原始大小写，`value` 去掉外层引号后原样保存。
#[derive(Debug, Clone)]
pub struct Entry {
    pub section: String,
    pub key: String,
    pub value: String,
}

pub fn parse_ini(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut section = String::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        if let Some(rest) = line.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                section = rest[..end].trim().to_string();
            }
            continue;
        }

        let Some(eq) = line.find('=') else {
            continue; // 没有等号的行直接忽略，不报错
        };
        let key = line[..eq].trim();
        if key.is_empty() {
            continue;
        }
        out.push(Entry {
            section: section.clone(),
            key: key.to_string(),
            value: unquote(line[eq + 1..].trim()).to_string(),
        });
    }

    out
}

fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn get<'a>(entries: &'a [Entry], section: &str, key: &str) -> Option<&'a str> {
    entries
        .iter()
        .rev() // 后出现的同名项覆盖先出现的
        .find(|e| e.section.eq_ignore_ascii_case(section) && e.key.eq_ignore_ascii_case(key))
        .map(|e| e.value.as_str())
        .filter(|v| !v.is_empty())
}

/// 取配置项：先看 `env` 参数指定的环境变量，再查 ini。
fn pick(entries: &[Entry], section: &str, key: &str, env: &str) -> Option<String> {
    if let Some(v) = std::env::var(env).ok().filter(|v| !v.is_empty()) {
        return Some(v);
    }
    get(entries, section, key).map(str::to_string)
}

fn parse_num<T: std::str::FromStr>(s: &str, fallback: T) -> T {
    s.trim().parse::<T>().unwrap_or(fallback)
}

// ---------------------------------------------------------------------------
// 载入
// ---------------------------------------------------------------------------

/// 默认配置路径：exe 同目录的 `modem.ini`。
pub fn default_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(CONFIG_FILE_NAME)))
        .unwrap_or_else(|| PathBuf::from(CONFIG_FILE_NAME))
}

pub fn load(path: Option<&Path>) -> Result<Config, String> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(default_path);

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读取配置失败 {}: {e}", path.display())),
    };
    let ini = parse_ini(&text);

    let mut ont = default_ont();
    if let Some(v) = pick(&ini, "ont", "host", "ONT_HOST") {
        ont.host = v;
    }
    if let Some(v) = pick(&ini, "ont", "username", "ONT_USER") {
        ont.username = v;
    }
    if let Some(v) = pick(&ini, "ont", "password", "ONT_PASS") {
        ont.password = v;
    }
    if let Some(v) = pick(&ini, "ont", "timeout", "ONT_TIMEOUT") {
        ont.timeout = parse_num(&v, ont.timeout);
    }
    if let Some(v) = pick(&ini, "ont", "retries", "ONT_RETRIES") {
        ont.retries = parse_num(&v, ont.retries).max(1);
    }

    let mut router = default_router();
    if let Some(v) = pick(&ini, "router", "host", "ROUTER_HOST") {
        router.host = v;
    }
    if let Some(v) = pick(&ini, "router", "password", "ROUTER_PASS") {
        router.password = v;
    }

    let mut reboot = default_reboot();
    if let Some(v) = pick(&ini, "reboot", "url", "ONT_REBOOT_URL") {
        reboot.url = v;
    }
    if let Some(v) = pick(&ini, "reboot", "method", "ONT_REBOOT_METHOD") {
        reboot.method = v.to_ascii_uppercase();
    }
    if let Some(v) = pick(&ini, "reboot", "confirmed", "ONT_REBOOT_CONFIRMED") {
        reboot.confirmed = is_true(&v);
    }
    for e in &ini {
        if e.section.eq_ignore_ascii_case("reboot.fields") && !e.key.is_empty() {
            // 字段名保留原始大小写
            reboot.fields.push((e.key.clone(), e.value.clone()));
        }
    }

    Ok(Config {
        path,
        ont,
        router,
        reboot,
    })
}

fn is_true(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "yes" | "true" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_equals_sign_inside_value() {
        let e = parse_ini("[reboot]\nurl = /getpage.gch?pid=1002&nextpage=a.gch\n");
        assert_eq!(e[0].value, "/getpage.gch?pid=1002&nextpage=a.gch");
    }

    #[test]
    fn keeps_field_name_case() {
        let e = parse_ini("[reboot.fields]\nIF_ACTION = devrestart\nflag = 1\n");
        assert_eq!(e[0].key, "IF_ACTION");
        assert_eq!(e[1].key, "flag");
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let e = parse_ini("[ONT]\nHost = 10.0.0.1\n");
        assert_eq!(get(&e, "ont", "host"), Some("10.0.0.1"));
    }

    #[test]
    fn ignores_comment_lines() {
        let e = parse_ini("; comment\n# comment\n[ont]\nhost=1.1.1.1\n");
        assert_eq!(e.len(), 1);
    }
}
