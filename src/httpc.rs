//! 带 Cookie、重试，并把响应字节按 Latin-1 映射的 HTTP 客户端。
//!
//! 两个关键设计：
//!
//! 1. **Cookie 自行管理**（不引入 cookie_store）：登录只返回一个会话 Cookie，
//!    记住 `name=value`，下次请求带上即可，省一个依赖。
//! 2. **响应按 Latin-1 映射成字符串**：光猫页面是 gb2312，但我们要提取的
//!    `_SESSION_TOKEN`、`<input name=...>`、`Transfer_meaning(...)` 全是 ASCII，
//!    按字节 1:1 映射后正则照常命中，完全不需要 GB 编码表。
//!
//! 用的是 ureq 3（`default-features = false`，只启用 HTTP，不含 TLS 和 gzip）：它不再自带
//! `AgentBuilder`，请求统一走 `http::Request` + `Agent::run`。

use std::io::Read;
use std::thread::sleep;
use std::time::Duration;

use crate::log;

pub struct Resp {
    pub status: u16,
    pub body: String,
}

pub struct HttpClient {
    agent: ureq::Agent,
    base: String,
    cookies: Vec<(String, String)>,
    timeout: Duration,
    retries: u32,
    backoff: f32,
    verbose: bool,
}

impl HttpClient {
    pub fn new(base: &str, timeout_secs: u64, retries: u32, backoff: f32, verbose: bool) -> Self {
        let timeout = Duration::from_secs(timeout_secs.max(1));
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .max_redirects(5)
            // 4xx/5xx 也是有效响应，交给调用方判断（光猫重启时常见 502/空响应）
            .http_status_as_error(false)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base: base.trim_end_matches('/').to_string(),
            cookies: Vec::new(),
            timeout,
            retries: retries.max(1),
            backoff,
            verbose,
        }
    }

    pub fn get(&mut self, path: &str) -> Result<Resp, String> {
        self.request("GET", path, None)
    }

    pub fn post_form(&mut self, path: &str, form: &[(String, String)]) -> Result<Resp, String> {
        self.request("POST", path, Some(form))
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn store_cookie(&mut self, raw: &str) {
        // 形如 "SESSIONID=abc; path=/; HttpOnly"，只取第一段
        let pair = raw.split(';').next().unwrap_or("").trim();
        let Some(eq) = pair.find('=') else { return };
        let (name, value) = (pair[..eq].trim(), pair[eq + 1..].trim());
        if name.is_empty() {
            return;
        }
        if let Some(slot) = self.cookies.iter_mut().find(|(k, _)| k == name) {
            slot.1 = value.to_string();
        } else {
            self.cookies.push((name.to_string(), value.to_string()));
        }
    }

    fn finish(&mut self, resp: ureq::http::Response<ureq::Body>) -> Resp {
        let status = resp.status().as_u16();

        let cookies: Vec<String> = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_string))
            .collect();
        for raw in cookies {
            self.store_cookie(&raw);
        }

        let mut buf = Vec::new();
        // 上限 8 MB，防止对端返回过多数据把内存占满
        let _ = resp
            .into_body()
            .into_reader()
            .take(8 * 1024 * 1024)
            .read_to_end(&mut buf);

        // 字节到字符 1:1 映射：ASCII 保持不变，中文会变成 Latin-1 范围内的字符，
        // 但不影响正则匹配
        let body = buf.iter().map(|&b| b as char).collect();
        Resp { status, body }
    }

    /// 带指数退避的重试。嵌入式 web server 经常“连得上但不回包”，重试是主要容错点。
    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        form: Option<&[(String, String)]>,
    ) -> Result<Resp, String> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base, path)
        };
        let cookie = self.cookie_header();
        let mut last = String::new();

        for attempt in 1..=self.retries {
            let mut builder = ureq::http::Request::builder()
                .method(method)
                .uri(url.as_str())
                .header("User-Agent", "Mozilla/5.0 (compatible; ModemReset/2.0)")
                // 光猫的 mini web server 对 Keep-Alive 支持不稳，显式关掉
                .header("Connection", "close")
                .header("Accept", "text/html,application/xhtml+xml,*/*")
                .header("Accept-Encoding", "identity");

            if !cookie.is_empty() {
                builder = builder.header("Cookie", cookie.as_str());
            }

            let outcome = match form {
                Some(pairs) => {
                    let body = form_encode(pairs);
                    let req = builder
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .body(body);
                    match req {
                        Ok(req) => self.agent.run(req),
                        Err(e) => return Err(format!("构造请求失败: {e}")),
                    }
                }
                None => match builder.body(()) {
                    Ok(req) => self.agent.run(req),
                    Err(e) => return Err(format!("构造请求失败: {e}")),
                },
            };

            match outcome {
                // 4xx/5xx 已在 ureq 的 Agent 配置里关掉“状态码当错误”，这里拿到的都是响应
                Ok(resp) => return Ok(self.finish(resp)),
                Err(e) => {
                    last = e.to_string();
                    if attempt < self.retries {
                        let delay = self.backoff * attempt as f32;
                        // 刻意不用 `{delay:.1}`：浮点格式化会把 core 里整套
                        // flt2dec 表带进 exe（约 16 KB），这里只要一位小数。
                        let tenths = (delay * 10.0).round() as u32;
                        let msg = format!(
                            "  ! 第 {attempt} 次请求失败（{last}），{}.{}s 后重试",
                            tenths / 10,
                            tenths % 10
                        );
                        if self.verbose {
                            println!("{msg}");
                        }
                        log::write_only(&msg);
                        sleep(Duration::from_secs_f32(delay));
                    }
                }
            }
        }

        Err(format!("请求 {url} 连续 {} 次失败: {last}", self.retries))
    }

    #[allow(dead_code)]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// `application/x-www-form-urlencoded` 编码。
///
/// 只保留 RFC 3986 的 unreserved 字符，其余按 UTF-8 逐字节 `%XX`，空格写成 `+`。
/// 比 ureq 自带的编码更严格，但解码结果完全一致。
fn form_encode(pairs: &[(String, String)]) -> String {
    let mut out = String::new();
    for (k, v) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        encode_part(&mut out, k);
        out.push('=');
        encode_part(&mut out, v);
    }
    out
}

fn encode_part(out: &mut String, s: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0F) as usize] as char);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn form_encoding_matches_urlencoded() {
        let pairs = vec![
            ("IF_ACTION".to_string(), "devrestart".to_string()),
            ("_SESSION_TOKEN".to_string(), "a+b/c=d e".to_string()),
        ];
        assert_eq!(
            form_encode(&pairs),
            "IF_ACTION=devrestart&_SESSION_TOKEN=a%2Bb%2Fc%3Dd+e"
        );
    }

    #[test]
    fn cookie_roundtrip() {
        let mut client = HttpClient::new("http://127.0.0.1", 1, 1, 1.0, false);
        client.store_cookie("SESSIONID=abc; path=/; HttpOnly");
        client.store_cookie("foo=bar");
        client.store_cookie("SESSIONID=xyz; path=/");
        assert_eq!(client.cookie_header(), "SESSIONID=xyz; foo=bar");
    }

    /// 起一个只服务若干请求的本地 HTTP 服务器，返回端口和收到的原始请求。
    fn serve(responses: Vec<Vec<u8>>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口失败");
        let port = listener.local_addr().expect("取本地地址失败").port();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept 失败");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("设置读超时失败");

                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let n = stream.read(&mut chunk).expect("读请求失败");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(pos) = text.find("\r\n\r\n") {
                        let len = text[..pos]
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                if name.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if buf.len() >= pos + 4 + len {
                            break;
                        }
                    }
                }

                requests.push(String::from_utf8_lossy(&buf).to_string());
                stream.write_all(&response).expect("写响应失败");
                stream.flush().expect("flush 失败");
            }
            requests
        });
        (port, handle)
    }

    #[test]
    fn get_reads_body_and_stores_cookie() {
        let (port, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nSet-Cookie: SESSIONID=abc; path=/\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_vec(),
        ]);
        let mut client = HttpClient::new(&format!("http://127.0.0.1:{port}"), 5, 1, 1.0, false);

        let resp = client.get("/").expect("GET 失败");
        assert_eq!((resp.status, resp.body.as_str()), (200, "hello"));
        assert_eq!(client.cookie_header(), "SESSIONID=abc");

        let req = server.join().expect("服务器线程 panic").remove(0);
        assert!(req.starts_with("GET / HTTP/1.1\r\n"), "{req}");
    }

    #[test]
    fn post_form_keeps_5xx_as_response() {
        let (port, server) = serve(vec![
            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad"
                .to_vec(),
        ]);
        let mut client = HttpClient::new(&format!("http://127.0.0.1:{port}"), 5, 1, 1.0, false);

        let form = vec![("a".to_string(), "b c".to_string())];
        let resp = client.post_form("/submit", &form).expect("POST 失败");
        // 502 不重试、不当错误，原样交给调用方
        assert_eq!((resp.status, resp.body.as_str()), (502, "bad"));

        let req = server.join().expect("服务器线程 panic").remove(0);
        assert!(req.starts_with("POST /submit HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("a=b+c"), "{req}");
        assert!(
            req.to_ascii_lowercase()
                .contains("content-type: application/x-www-form-urlencoded"),
            "{req}"
        );
    }

    #[test]
    fn follows_redirect_and_maps_latin1() {
        let (port, server) = serve(vec![
            b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\xe9Z".to_vec(),
        ]);
        let mut client = HttpClient::new(&format!("http://127.0.0.1:{port}"), 5, 1, 1.0, false);

        let resp = client.get("/start").expect("GET 失败");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "éZ");

        let requests = server.join().expect("服务器线程 panic");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1].starts_with("GET /next HTTP/1.1\r\n"),
            "{}",
            requests[1]
        );
    }
}
