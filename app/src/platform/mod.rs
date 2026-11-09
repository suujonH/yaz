//! Platform integration: tray, taskbar flashing, run at startup, clipboard, file picker, idle time,
//! edge-hide, notification sound.

mod autostart;
mod clipboard;
mod dialogs;
mod edge_hide;
mod system;
mod tray;

/// Resource ID of the icon embedded in the exe (the ID written by `set_icon` in build.rs).
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
