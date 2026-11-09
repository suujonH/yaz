use std::time::Duration;

use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// 用户多久没有操作键盘鼠标（整个系统）；查询失败时返回零。
#[allow(unsafe_code)]
pub fn user_idle_time() -> Duration {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: info 已按要求设置 cbSize。
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return Duration::ZERO;
        }
        Duration::from_millis(u64::from(GetTickCount().wrapping_sub(info.dwTime)))
    }
}
