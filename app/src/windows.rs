//! Entry points for opening every child window. Pages call only the functions here; window size,
//! title, and singleton rules are all registered here. The content of each window is in
//! `pages/<name>/`.

use crate::pages;
use crate::pages::chat::ChatArgs;
use crate::pages::history::HistoryArgs;
use crate::pages::login::AddAccountArgs;
use crate::pages::search::{RecordsArgs, SearchMoreArgs};
use crate::pages::settings::SettingsTarget;
use crate::services::services;
use crate::shell::{WindowSpec, close_window, open_with_args, prepare_with_args};
use crate::state::{AppCommand, FocusRequest, send_command};
use dioxus::desktop::tao::dpi::PhysicalPosition;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::cell::Cell;
use std::time::Duration;
use zulip_core::ConversationKey;
use zulip_session::NotificationRequest;

/// Size of the login and "add account" windows.
const LOGIN_SIZE: (f64, f64) = (320.0, 448.0);
const LOGIN_KEY: &str = "login";

/// How long the pointer has to rest on an entry before its window is rendered in advance.
const HOVER_DELAY: Duration = Duration::from_millis(100);

/// A window request: opened by the `open_*` functions, or rendered in advance in the hidden spare
/// window while the pointer rests on the entry that opens it ([`hover`]). Both go through the same
/// builder so the prepared page matches what a click opens.
struct Request<T> {
    spec: WindowSpec,
    root: fn() -> Element,
    args: T,
    account: Option<i64>,
}

impl<T: Clone + PartialEq + 'static> Request<T> {
    fn open(self) {
        open_with_args(self.spec, self.root, self.args, self.account);
    }

    fn prepare(self) {
        prepare_with_args(self.spec, self.root, self.args, self.account);
    }
}

/// Windows that list entries render in advance on hover.
#[derive(Clone, Debug, PartialEq)]
pub enum WindowTarget {
    /// What [`open_chat`] opens; only rendered in advance when it opens a window.
    Chat {
        account: i64,
        key: ConversationKey,
    },
    Settings(SettingsTarget),
    Favorites(i64),
    Files(i64),
    Reminders(i64),
    FriendManager(i64),
    SystemNotices(i64),
    AddAccount,
}

thread_local! {
    /// Increased on every hover change; a delayed prepare only runs if it is still the latest.
    static HOVER: Cell<u64> = const { Cell::new(0) };
}

/// The pointer rests on an entry that opens `target` (`None`: it left). After [`HOVER_DELAY`] the
/// window is rendered in the hidden spare window, so clicking only has to show it.
pub fn hover(target: Option<WindowTarget>) {
    let generation = HOVER.get() + 1;
    HOVER.set(generation);
    let Some(target) = target else { return };
    spawn_forever(async move {
        tokio::time::sleep(HOVER_DELAY).await;
        if HOVER.get() == generation {
            prepare(target);
        }
    });
}

fn prepare(target: WindowTarget) {
    match target {
        WindowTarget::Chat { account, key } => {
            if is_notification_bot(account, &key) {
                prepare(WindowTarget::SystemNotices(account));
            } else if uses_chat_windows() {
                chat_window(account, key, None).prepare();
            }
        }
        WindowTarget::Settings(target) => settings(target).prepare(),
        WindowTarget::Favorites(account) => favorites(account).prepare(),
        WindowTarget::Files(account) => files(account).prepare(),
        WindowTarget::Reminders(account) => {
            if reminders_in_window() {
                reminders(account).prepare();
            }
        }
        WindowTarget::FriendManager(account) => friend_manager(account).prepare(),
        WindowTarget::SystemNotices(account) => {
            if uses_chat_windows() {
                system_notices(account).prepare();
            }
        }
        WindowTarget::AddAccount => add_account(AddAccountArgs::default()).prepare(),
    }
}

/// Whether to open the conversation in a window or on the right side of the main window.
pub fn uses_chat_windows() -> bool {
    let settings = &services().settings;
    settings.classic_mode() || settings.open_chat_in_new_window()
}

fn is_notification_bot(account: i64, key: &ConversationKey) -> bool {
    let Some(session) = services().hub.session(account) else {
        return false;
    };
    match key {
        ConversationKey::Dm(dm) => {
            let others = dm.other_user_ids();
            others.len() == 1 && Some(others[0]) == session.notification_bot_id()
        }
        _ => false,
    }
}

/// The only entry point for opening a conversation: depending on settings, put it in a standalone
/// window or on the right of the main window; a DM with the notification bot opens a system
/// notification. `focus` is the message to locate.
pub fn open_chat(account: i64, key: ConversationKey, focus: Option<i64>) {
    let focus = focus.map(FocusRequest::new);
    if is_notification_bot(account, &key) {
        open_system_notices(account);
    } else if uses_chat_windows() {
        open_chat_window_at(account, key, focus);
    } else {
        send_command(AppCommand::ShowChat {
            account,
            key,
            focus,
        });
        send_command(AppCommand::ShowMain);
    }
}

/// Show a conversation in the main window whatever the window setting is, with `focus` the message
/// to locate. Classic mode has no right side: it opens the conversation's window instead.
pub fn show_chat_in_main(account: i64, key: ConversationKey, focus: Option<i64>) {
    let focus = focus.map(FocusRequest::new);
    if is_notification_bot(account, &key) {
        open_system_notices(account);
    } else if services().settings.classic_mode() {
        open_chat_window_at(account, key, focus);
    } else {
        send_command(AppCommand::ShowChat {
            account,
            key,
            focus,
        });
        send_command(AppCommand::ShowMain);
    }
}

/// Open a conversation in a standalone window. Only one window per conversation; the size is
/// shared.
pub fn open_chat_window(account: i64, key: ConversationKey, focus: Option<i64>) {
    open_chat_window_at(account, key, focus.map(FocusRequest::new));
}

fn open_chat_window_at(account: i64, key: ConversationKey, focus: Option<FocusRequest>) {
    chat_window(account, key, focus).open();
}

fn chat_window(
    account: i64,
    key: ConversationKey,
    focus: Option<FocusRequest>,
) -> Request<ChatArgs> {
    let title = services()
        .hub
        .session(account)
        .map(|s| s.chat_title(&key))
        .unwrap_or_default();
    Request {
        spec: WindowSpec::new(format!("chat:{account}:{key}"), title, (760.0, 640.0))
            .min_size((520.0, 480.0))
            .bounds_key("chat")
            .keep_position(),
        root: pages::chat::ChatWindow,
        args: ChatArgs { key, focus },
        account: Some(account),
    }
}

pub fn open_settings(target: SettingsTarget) {
    settings(target).open();
}

fn settings(target: SettingsTarget) -> Request<SettingsTarget> {
    Request {
        spec: WindowSpec::new("settings", "设置", (1060.0, 600.0)).min_size((900.0, 420.0)),
        root: pages::settings::SettingsWindow,
        args: target,
        account: None,
    }
}

/// Image viewer. `images` are image URLs (URLs that need login are fine), starting from the
/// `index`-th.
pub fn open_image_viewer(account: i64, images: Vec<String>, index: usize) {
    open_with_args(
        WindowSpec::new("image-viewer", "图片查看", (900.0, 680.0))
            .min_size((480.0, 360.0))
            .fixed_background(pages::image_viewer::BACKGROUND),
        pages::image_viewer::ImageViewerWindow,
        pages::image_viewer::ImageViewerArgs { images, index },
        Some(account),
    );
}

/// Favorites. The account can be switched inside the window (`AccountSwitcher`); the parameter is
/// the account ID.
pub fn open_favorites(account: i64) {
    favorites(account).open();
}

fn favorites(account: i64) -> Request<i64> {
    Request {
        spec: WindowSpec::new("favorites", "收藏", (960.0, 640.0)).min_size((720.0, 420.0)),
        root: pages::favorites::FavoritesWindow,
        args: account,
        account: None,
    }
}

/// Files. The account can be switched inside the window (`AccountSwitcher`); the parameter is the
/// account ID.
pub fn open_files(account: i64) {
    files(account).open();
}

fn files(account: i64) -> Request<i64> {
    Request {
        spec: WindowSpec::new("files", "文件", (960.0, 640.0)).min_size((760.0, 420.0)),
        root: pages::files::FilesWindow,
        args: account,
        account: None,
    }
}

/// Chat history of a conversation; `query` is the search term at open time.
pub fn open_history(account: i64, key: ConversationKey, query: Option<String>) {
    let title = services()
        .hub
        .session(account)
        .map(|s| format!("{} 的聊天记录", s.title(&key)))
        .unwrap_or_default();
    open_with_args(
        WindowSpec::new(format!("history:{account}:{key}"), title, (660.0, 720.0))
            .min_size((480.0, 420.0))
            .bounds_key("history"),
        pages::history::HistoryWindow,
        HistoryArgs { key, query },
        Some(account),
    );
}

/// Message reminders go on the right of the main window; only classic mode, which has no right
/// side, opens a window.
pub fn open_reminders(account: i64) {
    if reminders_in_window() {
        reminders(account).open();
    } else {
        send_command(AppCommand::ShowReminders { account });
        send_command(AppCommand::ShowMain);
    }
}

fn reminders_in_window() -> bool {
    services().settings.classic_mode()
}

fn reminders(account: i64) -> Request<()> {
    Request {
        spec: WindowSpec::new(format!("reminders:{account}"), "消息提醒", (760.0, 560.0))
            .min_size((640.0, 420.0))
            .bounds_key("reminders"),
        root: pages::reminders::RemindersWindow,
        args: (),
        account: Some(account),
    }
}

pub fn open_friend_manager(account: i64) {
    friend_manager(account).open();
}

fn friend_manager(account: i64) -> Request<()> {
    Request {
        spec: WindowSpec::new(
            format!("friend-manager:{account}"),
            "好友管理器",
            (840.0, 600.0),
        )
        .min_size((680.0, 460.0))
        .bounds_key("friend-manager"),
        root: pages::friend_manager::FriendManagerWindow,
        args: (),
        account: Some(account),
    }
}

/// System notification: in the default mode it goes on the right of the main window, otherwise a
/// window opens.
pub fn open_system_notices(account: i64) {
    if uses_chat_windows() {
        system_notices(account).open();
    } else {
        send_command(AppCommand::ShowNotices { account });
        send_command(AppCommand::ShowMain);
    }
}

fn system_notices(account: i64) -> Request<()> {
    Request {
        spec: WindowSpec::new(
            format!("system-notices:{account}"),
            "系统通知",
            (720.0, 600.0),
        )
        .min_size((480.0, 400.0))
        .bounds_key("system-notices"),
        root: pages::system_notices::SystemNoticesWindow,
        args: (),
        account: Some(account),
    }
}

/// Chat records found by a search: the conversations on the left, their messages on the right.
pub fn open_message_records(account: i64, args: RecordsArgs) {
    open_with_args(
        WindowSpec::new(
            format!("message-records:{account}"),
            "聊天记录",
            (680.0, 720.0),
        )
        .min_size((560.0, 420.0))
        .bounds_key("message-records"),
        pages::search::MessageRecordsWindow,
        args,
        Some(account),
    );
}

/// "More" of search results.
pub fn open_search_more(account: i64, args: SearchMoreArgs) {
    let title = args.window_title();
    open_with_args(
        WindowSpec::new(format!("search-more:{account}"), title, (380.0, 560.0))
            .min_size((300.0, 300.0))
            .bounds_key("search-more"),
        pages::search::SearchMoreWindow,
        args,
        Some(account),
    );
}

/// Startup login. The root window (the main UI) stays hidden meanwhile; it closes this window with
/// [`close_login`] once it has entered the main UI and shown itself.
pub fn open_login() {
    open_with_args(
        WindowSpec::new(LOGIN_KEY, "YaZ", LOGIN_SIZE).fixed(),
        pages::login::LoginWindow,
        (),
        None,
    );
}

pub fn close_login() {
    close_window(LOGIN_KEY);
}

/// Add an account (when a main window already exists). When `account` is non-empty it is prefilled;
/// `error` is shown in the form.
pub fn open_add_account(args: AddAccountArgs) {
    add_account(args).open();
}

fn add_account(args: AddAccountArgs) -> Request<AddAccountArgs> {
    Request {
        spec: WindowSpec::new("add-account", "添加账号", LOGIN_SIZE).fixed(),
        root: pages::login::AddAccountWindow,
        args,
        account: None,
    }
}

/// Desktop notification popup. A new notification replaces the old one.
pub fn show_notification(
    account: i64,
    request: NotificationRequest,
    click: pages::notification::NotificationClick,
) {
    open_with_args(
        WindowSpec::new("notification", "YaZ", (320.0, 88.0))
            .fixed()
            .popup(),
        pages::notification::NotificationWindow,
        pages::notification::NotificationArgs {
            account,
            request,
            click,
        },
        None,
    );
}

/// Tray message list (hovering the tray icon while it blinks); `anchor` is the icon's screen
/// position.
pub fn open_tray_messages(anchor: PhysicalPosition<f64>) {
    open_with_args(
        WindowSpec::new("tray-messages", "YaZ", (300.0, 200.0))
            .fixed()
            .popup(),
        pages::tray_messages::TrayMessagesWindow,
        (anchor.x, anchor.y),
        None,
    );
}
