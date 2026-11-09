//! 任务栏闪烁、前台判断、空闲时间、提示音。

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

/// 用系统默认程序打开文件或文件夹。
pub fn open_path(path: &Path) -> Result<(), PlatformError> {
    let target = HSTRING::from(path.as_os_str());
    // 返回值不大于 32 表示失败。
    let result = unsafe { ShellExecuteW(None, w!("open"), &target, None, None, SW_SHOWNORMAL) };
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(PlatformError::Open(code))
    }
}

/// 让任务栏按钮闪烁，直到窗口回到前台。
pub fn flash_taskbar(window: &DesktopService) {
    let info = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd: hwnd_of(window),
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    // SAFETY: info 完整初始化，hwnd 属于存活的窗口。
    let _ = unsafe { FlashWindowEx(&info) };
}

/// 窗口是否真正在前台（有键盘焦点）。
pub fn is_foreground(window: &DesktopService) -> bool {
    // SAFETY: 没有参数。
    unsafe { GetForegroundWindow() == hwnd_of(window) }
}

/// 前台窗口是否属于本程序（任何一个窗口）。
pub fn app_is_foreground() -> bool {
    let mut pid = 0u32;
    // SAFETY: pid 是有效的输出位置；前台窗口为空时返回 0，pid 保持 0。
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    pid == std::process::id()
}

/// 距上次键盘鼠标输入的时间。
pub fn idle_time() -> Duration {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: info 是有效的输出位置，cbSize 已设置。
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return Duration::ZERO;
    }
    // SAFETY: 没有参数。
    let now = unsafe { GetTickCount() };
    Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime)))
}

/// 播放系统的“新消息”提示音（异步）。
pub fn play_notification_sound() {
    // SAFETY: 别名是静态宽字符串。
    let _ = unsafe {
        PlaySoundW(
            w!("Notification.IM"),
            None,
            SND_ALIAS | SND_ASYNC | SND_NODEFAULT,
        )
    };
}

/// Windows 的“应用模式”是否为深色。
pub fn system_prefers_dark() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: value 和 size 是有效的输出位置，长度与 REG_DWORD 一致。
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
