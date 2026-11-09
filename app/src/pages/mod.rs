//! Pages. Each page has a directory `pages/<name>/`: `mod.rs` is the component and `page.css` is
//! the style dedicated to that page.

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

/// Styles of all pages, which go into the `<style>` of every window.
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

/// Styles of the developer mode page. Release does not compile this page.
#[cfg(debug_assertions)]
pub const DEVELOPER_CSS: &str = include_str!("settings/developer.css");
#[cfg(not(debug_assertions))]
pub const DEVELOPER_CSS: &str = "";
