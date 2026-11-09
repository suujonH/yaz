//! Taskbar flashing, foreground detection, idle time, notification sound.

use super::PlatformError;
use dioxus::desktop::DesktopService;
use dioxus::desktop::tao::platform::windows::WindowExtWindows;
use std::mem::size_of;
use std::path::Path;
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetForegroundWindow,
    GetWindowThreadProcessId, SW_SHOWNORMAL,
};
use windows::core::{HSTRING, w};

pub(super) fn hwnd_of(window: &DesktopService) -> HWND {
    HWND(window.window.hwnd() as *mut _)
}

/// Opens a file or folder with the system default program.
pub fn open_path(path: &Path) -> Result<(), PlatformError> {
    let target = HSTRING::from(path.as_os_str());
    // A return value not greater than 32 means failure.
    let result = unsafe { ShellExecuteW(None, w!("open"), &target, None, None, SW_SHOWNORMAL) };
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(PlatformError::Open(code))
    }
}

/// Flashes the taskbar button until the window returns to the foreground.
pub fn flash_taskbar(window: &DesktopService) {
    let info = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd: hwnd_of(window),
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    // SAFETY: info is fully initialized and hwnd belongs to a live window.
    let _ = unsafe { FlashWindowEx(&info) };
}

/// Whether the window is really in the foreground (has keyboard focus).
pub fn is_foreground(window: &DesktopService) -> bool {
    // SAFETY: there are no arguments.
    unsafe { GetForegroundWindow() == hwnd_of(window) }
}

/// Whether the foreground window belongs to this program (any of its windows).
pub fn app_is_foreground() -> bool {
    let mut pid = 0u32;
    // SAFETY: pid is a valid output location; when there is no foreground window it returns 0 and
    // pid stays 0.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    pid == std::process::id()
}

/// Time since the last keyboard or mouse input.
pub fn idle_time() -> Duration {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: info is a valid output location and cbSize is set.
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return Duration::ZERO;
    }
    // SAFETY: there are no arguments.
    let now = unsafe { GetTickCount() };
    Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime)))
}

/// Plays the system "new message" notification sound (asynchronously).
pub fn play_notification_sound() {
    // SAFETY: the alias is a static wide string.
    let _ = unsafe {
        PlaySoundW(
            w!("Notification.IM"),
            None,
            SND_ALIAS | SND_ASYNC | SND_NODEFAULT,
        )
    };
}

/// Whether the Windows "app mode" is dark.
pub fn system_prefers_dark() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: value and size are valid output locations; the length matches REG_DWORD.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_path_reports_missing_file() {
        let result = open_path(Path::new(r"Z:\不存在的目录\不存在的文件.txt"));
        assert!(matches!(result, Err(PlatformError::Open(_))));
    }
}
