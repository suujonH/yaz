//! Platform integration: tray, taskbar flashing, run at startup, clipboard, file picker, idle time,
//! edge-hide, notification sound, text recognition.

mod autostart;
mod clipboard;
mod dialogs;
mod edge_hide;
mod ocr;
mod system;
mod tray;
mod tray_avatar;

/// Resource ID of the icon embedded in the exe (the ID written by `set_icon` in build.rs).
pub const ICON_RESOURCE_ID: u16 = 1;

pub use autostart::{LaunchAtStartup, PlatformError};
pub use clipboard::{
    ClipboardImage, copied_image, copied_markdown, copy_image, copy_rich, copy_text, paste_image,
    paste_text, remember_image, remember_markdown,
};
pub use dialogs::{FileFilter, IMAGE_FILTER, pick_file, pick_files, pick_folder, save_file};
pub use edge_hide::{is_hidden_at_edge, use_edge_hide, work_area, work_area_at};
#[cfg(test)]
pub use ocr::OcrWord;
pub use ocr::{OcrLine, recognize_text};
pub use system::{
    app_is_foreground, flash_taskbar, idle_time, is_foreground, open_path, play_notification_sound,
    system_prefers_dark,
};
pub use tray::{IconBox, Tray, TrayEvent, TrayItem, use_tray};
pub use tray_avatar::{image_icon, text_icon};
