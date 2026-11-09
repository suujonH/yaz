//! 平台集成：托盘、任务栏闪烁、开机自动运行、剪贴板、文件选择、空闲时间、贴边隐藏、提示音。

mod autostart;
mod clipboard;
mod dialogs;
mod edge_hide;
mod system;
mod tray;

/// exe 内嵌图标的资源 ID（build.rs 中 `set_icon` 写入的 ID）。
pub const ICON_RESOURCE_ID: u16 = 1;

pub use autostart::{LaunchAtStartup, PlatformError};
pub use clipboard::{
    ClipboardImage, copied_markdown, copy_image, copy_text, paste_image, paste_text,
    remember_markdown,
};
pub use dialogs::{FileFilter, IMAGE_FILTER, pick_file, pick_files, pick_folder, save_file};
pub use edge_hide::{is_hidden_at_edge, use_edge_hide, work_area};
pub use system::{
    app_is_foreground, flash_taskbar, idle_time, is_foreground, open_path, play_notification_sound,
    system_prefers_dark,
};
pub use tray::{Tray, TrayEvent, TrayItem, use_tray};
