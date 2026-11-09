use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "YaZ";

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("读写注册表失败：{0:?}")]
    Registry(WIN32_ERROR),
    #[error("找不到程序路径：{0}")]
    ExePath(#[from] std::io::Error),
    #[error("系统无法打开（错误码 {0}）")]
    Open(isize),
}

/// 开机自动运行：当前用户的启动项（注册表 Run 键）。状态以注册表为准，不另外保存。
/// 启动项指向其它位置（程序被移动过）时视为未开启，重新开启后指向当前程序。
pub struct LaunchAtStartup {
    key_path: String,
    value_name: String,
    exe_path: String,
}

impl LaunchAtStartup {
    pub fn current() -> Result<Self, PlatformError> {
        Ok(Self::new(
            RUN_KEY,
            VALUE_NAME,
            &std::env::current_exe()?.to_string_lossy(),
        ))
    }

    fn new(key_path: &str, value_name: &str, exe_path: &str) -> Self {
        Self {
            key_path: key_path.into(),
            value_name: value_name.into(),
            exe_path: exe_path.into(),
        }
    }

    // 路径可能含空格
    fn command(&self) -> String {
        format!("\"{}\"", self.exe_path)
    }

    pub fn is_enabled(&self) -> bool {
        read_value(&self.key_path, &self.value_name)
            .is_some_and(|v| v.eq_ignore_ascii_case(&self.command()))
    }

    pub fn set(&self, enabled: bool) -> Result<(), PlatformError> {
        if enabled {
            write_value(&self.key_path, &self.value_name, &self.command())
        } else {
            delete_value(&self.key_path, &self.value_name)
        }
    }
}

fn read_value(key_path: &str, name: &str) -> Option<String> {
    let (key, name) = (HSTRING::from(key_path), HSTRING::from(name));
    let mut size = 0u32;
    // SAFETY: 参数都是有效的以 NUL 结尾的宽字符串，这次调用只取长度。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if status.is_err() || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: buffer 至少有 size 字节，RegGetValueW 不会写出 size 范围。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

fn write_value(key_path: &str, name: &str, value: &str) -> Result<(), PlatformError> {
    let mut key = HKEY::default();
    // SAFETY: key 是有效的输出位置；成功后在下面关闭。
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key_path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_READ,
            None,
            &mut key,
            None,
        )
    }
    .ok()
    .map_err(|e| PlatformError::Registry(WIN32_ERROR::from_error(&e).unwrap_or_default()))?;
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: Vec<u8> = wide.iter().flat_map(|c| c.to_le_bytes()).collect();
    // SAFETY: key 刚打开且有效；bytes 是完整的 REG_SZ 数据（含结尾 NUL）。
    let result = unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(&bytes)) };
    // SAFETY: key 此后没有使用。
    let _ = unsafe { RegCloseKey(key) };
    result
        .ok()
        .map_err(|e| PlatformError::Registry(WIN32_ERROR::from_error(&e).unwrap_or_default()))
}

fn delete_value(key_path: &str, name: &str) -> Result<(), PlatformError> {
    // SAFETY: 参数都是有效的以 NUL 结尾的宽字符串。
    let status = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key_path),
            &HSTRING::from(name),
        )
    };
    if status.is_ok() || status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(PlatformError::Registry(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::RegDeleteTreeW;

    const EXE: &str = r"C:\Program Files\YaZ\yaz.exe";

    /// 只用测试自己的键，不碰真实的启动项；结束时删除。
    struct TestKey(String);

    impl TestKey {
        fn new(name: &str) -> Self {
            Self(format!(r"Software\YaZ.Tests\{name}-{}", std::process::id()))
        }

        fn startup(&self, exe: &str) -> LaunchAtStartup {
            LaunchAtStartup::new(&self.0, "YaZ", exe)
        }

        fn raw(&self) -> Option<String> {
            read_value(&self.0, "YaZ")
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            // SAFETY: 参数是有效的宽字符串。
            let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(self.0.as_str())) };
        }
    }

    #[test]
    fn disabled_when_key_does_not_exist() {
        assert!(!TestKey::new("missing").startup(EXE).is_enabled());
    }

    #[test]
    fn enable_writes_quoted_path() {
        let key = TestKey::new("enable");
        key.startup(EXE).set(true).unwrap();
        assert!(key.startup(EXE).is_enabled());
        assert_eq!(key.raw(), Some(format!("\"{EXE}\"")));
    }

    #[test]
    fn disable_removes_value() {
        let key = TestKey::new("disable");
        key.startup(EXE).set(true).unwrap();
        key.startup(EXE).set(false).unwrap();
        assert!(!key.startup(EXE).is_enabled());
        assert_eq!(key.raw(), None);
    }

    #[test]
    fn disable_when_already_disabled_does_nothing() {
        let key = TestKey::new("noop");
        key.startup(EXE).set(true).unwrap();
        key.startup(EXE).set(false).unwrap();
        key.startup(EXE).set(false).unwrap();
        assert!(!key.startup(EXE).is_enabled());
    }

    #[test]
    fn disabled_when_value_points_to_another_location() {
        let key = TestKey::new("moved");
        key.startup(r"D:\Old\yaz.exe").set(true).unwrap();
        assert!(!key.startup(EXE).is_enabled());
    }

    #[test]
    fn enable_repoints_to_current_location() {
        let key = TestKey::new("repoint");
        key.startup(r"D:\Old\yaz.exe").set(true).unwrap();
        key.startup(EXE).set(true).unwrap();
        assert!(key.startup(EXE).is_enabled());
    }
}
