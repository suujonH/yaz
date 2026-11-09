//! 页面。每个页面一个目录 `pages/<name>/`：`mod.rs` 是组件，`page.css` 是这个页面专用的样式。

pub mod chat;
pub mod composer;
pub mod contacts;
pub mod data_move;
pub mod dialogs;
pub mod favorites;
pub mod files;
pub mod friend_manager;
pub mod history;
pub mod image_viewer;
pub mod login;
pub mod main_window;
pub mod messages;
pub mod notification;
pub mod reminders;
pub mod search;
pub mod settings;
pub mod system_notices;
pub mod tray_messages;

/// 所有页面的样式，进入每个窗口的 `<style>`。
pub const PAGES_CSS: &str = concat!(
    include_str!("page.css"),
    include_str!("chat/page.css"),
    include_str!("contacts/page.css"),
    include_str!("data_move/page.css"),
    include_str!("dialogs/page.css"),
    include_str!("favorites/page.css"),
    include_str!("files/page.css"),
    include_str!("friend_manager/page.css"),
    include_str!("history/page.css"),
    include_str!("image_viewer/page.css"),
    include_str!("login/page.css"),
    include_str!("main_window/page.css"),
    include_str!("messages/page.css"),
    include_str!("notification/page.css"),
    include_str!("reminders/page.css"),
    include_str!("search/page.css"),
    include_str!("settings/page.css"),
    include_str!("system_notices/page.css"),
    include_str!("tray_messages/page.css"),
);

/// 开发者模式页的样式。Release 不编译这个页面。
#[cfg(debug_assertions)]
pub const DEVELOPER_CSS: &str = include_str!("settings/developer.css");
#[cfg(not(debug_assertions))]
pub const DEVELOPER_CSS: &str = "";
