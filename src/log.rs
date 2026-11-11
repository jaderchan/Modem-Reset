//! 极简日志：追加写 exe 同目录的 `modem-reset.log`，超过 1 MB 自动轮转一份 `.1`。
//!
//! 托盘模式下没有控制台，`println!` 会静默失败，所以输出一律写入日志文件，
//! 能打印时再额外输出到 stdout。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub fn dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn path() -> PathBuf {
    dir().join("modem-reset.log")
}

/// 返回形如 `2026-11-10 14:32:05` 的本地时间字符串。
pub fn stamp() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        // SAFETY: SYSTEMTIME 是纯 POD，全零是合法的初值
        let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
        // SAFETY: st 是本地栈上变量，GetLocalTime 只写它，不持有指针。
        unsafe { GetLocalTime(&mut st) };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        )
    }
    #[cfg(not(windows))]
    {
        String::from("----")
    }
}

fn rotate_if_needed(p: &PathBuf) {
    let too_big = fs::metadata(p)
        .map(|meta| meta.len() > MAX_LOG_BYTES)
        .unwrap_or(false);
    if too_big {
        let _ = fs::rename(p, p.with_extension("log.1"));
    }
}

/// 写日志文件，不打印。
pub fn write_only(msg: &str) {
    let p = path();
    rotate_if_needed(&p);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{} {}", stamp(), msg);
    }
}

/// 写日志文件并打印。stdout 可用时才打印；托盘模式没有控制台，只写文件。
pub fn say(msg: &str) {
    if crate::sys::stdout_ok() {
        println!("{msg}");
    }
    write_only(msg);
}

/// 原样写一行，不加时间戳。用于把多行块整体落盘。
#[allow(dead_code)] // 预留：目前没有调用方
pub fn block(msg: &str) {
    let p = path();
    rotate_if_needed(&p);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{msg}");
    }
}
