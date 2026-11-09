use std::time::Duration;

use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// How long since the user last used the keyboard or mouse (system-wide); returns zero if the query
/// fails.
#[allow(unsafe_code)]
pub fn user_idle_time() -> Duration {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: info has cbSize set as required.
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return Duration::ZERO;
        }
        Duration::from_millis(u64::from(GetTickCount().wrapping_sub(info.dwTime)))
    }
}
