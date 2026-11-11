//! 开机自启：写 HKCU\Software\Microsoft\Windows\CurrentVersion\Run。
//!
//! 比往“启动”文件夹放快捷方式省事（不用 COM 创建 .lnk），也不需要管理员权限。

use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "ModemReset";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_key(key_name: &str, write: bool) -> Option<HKEY> {
    let key = wide(key_name);
    let access = if write {
        KEY_QUERY_VALUE | KEY_SET_VALUE
    } else {
        KEY_QUERY_VALUE
    };
    let mut hkey: HKEY = std::ptr::null_mut();
    // SAFETY: key 是本函数内有效的 NUL 结尾 UTF-16；hkey 由系统写入，成功时再关闭。
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, access, &mut hkey) };
    if rc == 0 { Some(hkey) } else { None }
}

/// 值是否存在。查不到时 `RegQueryValueExW` 返回 `ERROR_FILE_NOT_FOUND`，那就是“没有”，
/// 绝不能算已启用（否则菜单勾选状态和点击后的效果都会相反）。
fn value_exists(hkey: HKEY, value_name: &str) -> bool {
    let name = wide(value_name);
    let mut kind = 0u32;
    let mut len = 0u32;
    // SAFETY: 缓冲区传空时只查询值是否存在，文档允许这种用法。
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut len,
        )
    };
    rc == 0
}

fn value_exists_in(key_name: &str, value_name: &str) -> bool {
    let Some(hkey) = open_key(key_name, false) else {
        return false;
    };
    let present = value_exists(hkey, value_name);
    // SAFETY: hkey 来自 open_key()，只关一次。
    unsafe { RegCloseKey(hkey) };
    present
}

/// 当前是否已设置开机自启。
pub fn enabled() -> bool {
    value_exists_in(RUN_KEY, VALUE_NAME)
}

/// 设置开机自启；`self_path` 是当前 exe 的完整路径，`Err` 是 Win32 错误码。
pub fn set(on: bool, self_path: &str) -> Result<(), u32> {
    set_at(RUN_KEY, VALUE_NAME, on, self_path)
}

fn set_at(key_name: &str, value_name: &str, on: bool, self_path: &str) -> Result<(), u32> {
    let Some(hkey) = open_key(key_name, true) else {
        return Err(1);
    };
    let name = wide(value_name);

    let rc = if on {
        // 给路径加上引号，含空格的路径不会被命令行拆开
        let value = wide(&format!("\"{self_path}\""));
        // SAFETY: value 的生命周期覆盖本次调用，长度按字节计（含结尾 NUL）。
        unsafe {
            RegSetValueExW(
                hkey,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                (value.len() * 2) as u32,
            )
        }
    } else {
        // SAFETY: 名称合法，删除不存在的值会返回 ERROR_FILE_NOT_FOUND，视为成功。
        unsafe { RegDeleteValueW(hkey, name.as_ptr()) }
    };

    // SAFETY: hkey 来自 open_key()，只关一次。
    unsafe { RegCloseKey(hkey) };

    if rc == 0 || rc == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(rc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::{RegCreateKeyExW, RegDeleteKeyW};

    // 只用来写入测试值，测试结束就删；和真正的 Run 键完全隔离
    const TEST_KEY: &str = r"Software\ModemResetTest";
    const TEST_VALUE: &str = "RoundTrip";

    /// RegOpenKeyExW 不会创建键，所以测试自己造一个。
    fn create_test_key() -> bool {
        let key = wide(TEST_KEY);
        let mut hkey: HKEY = std::ptr::null_mut();
        // SAFETY: key 是有效的 NUL 结尾 UTF-16；hkey 和 disposition 由系统写入，
        // 成功时立刻关闭，disposition 允许传空。
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        };
        if rc == 0 {
            // SAFETY: 句柄来自 RegCreateKeyExW，只关一次。
            unsafe { RegCloseKey(hkey) };
            true
        } else {
            false
        }
    }

    fn delete_test_key() {
        let key = wide(TEST_KEY);
        // SAFETY: 只删本测试自己建的键；预定义句柄权限足够。
        unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key.as_ptr()) };
    }

    #[test]
    fn registry_roundtrip() {
        // 受限账户或组策略下可能建不了键，这时跳过而不是误报失败
        if !create_test_key() {
            return;
        }

        let check = || {
            assert!(!value_exists_in(TEST_KEY, TEST_VALUE), "新键里不该有值");

            set_at(TEST_KEY, TEST_VALUE, true, r"C:\fake\ModemReset.exe").expect("写入失败");
            assert!(value_exists_in(TEST_KEY, TEST_VALUE), "写入后应能查到");

            set_at(TEST_KEY, TEST_VALUE, false, "").expect("删除失败");
            assert!(!value_exists_in(TEST_KEY, TEST_VALUE), "删除后不该再查到");
        };

        let result = std::panic::catch_unwind(check);
        delete_test_key();
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }
}
