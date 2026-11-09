//! 所有子窗口的打开入口。页面只调这里的函数，窗口的大小、标题、单例规则都在这里登记。
//! 每个窗口的内容在 `pages/<name>/`。

use crate::pages;
use crate::pages::chat::ChatArgs;
use crate::pages::history::HistoryArgs;
use crate::pages::login::AddAccountArgs;
use crate::pages::search::SearchMoreArgs;
use crate::pages::settings::SettingsTarget;
use crate::services::services;
use crate::shell::{WindowSpec, open_with_args};
use crate::state::{AccountScope, AppCommand, FocusRequest, send_command};
use dioxus::desktop::tao::dpi::PhysicalPosition;
use zulip_core::ConversationKey;
use zulip_session::NotificationRequest;

/// 打开会话窗口还是放到主窗口右侧。
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

/// 打开会话的唯一入口：按设置放进独立窗口或主窗口右侧；与通知机器人的私聊打开系统通知。
/// `focus` 是要定位的消息。
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

/// 在独立窗口中打开会话。同一会话只开一个窗口，大小共用。
pub fn open_chat_window(account: i64, key: ConversationKey, focus: Option<i64>) {
    open_chat_window_at(account, key, focus.map(FocusRequest::new));
}

fn open_chat_window_at(account: i64, key: ConversationKey, focus: Option<FocusRequest>) {
    let title = services()
        .hub
        .session(account)
        .map(|s| s.chat_title(&key))
        .unwrap_or_default();
    open_with_args(
        WindowSpec::new(format!("chat:{account}:{key}"), title, (760.0, 640.0))
            .min_size((520.0, 480.0))
            .bounds_key("chat"),
        pages::chat::ChatWindow,
        ChatArgs { key, focus },
        move |dom| dom.with_root_context(AccountScope(account)),
    );
}

pub fn open_settings(target: SettingsTarget) {
    open_with_args(
        WindowSpec::new("settings", "设置", (1060.0, 600.0)).min_size((900.0, 420.0)),
        pages::settings::SettingsWindow,
        target,
        |dom| dom,
    );
}

/// 图片查看器。`images` 是图片地址（登录后才能访问的地址也可以），从第 `index` 张开始。
pub fn open_image_viewer(account: i64, images: Vec<String>, index: usize) {
    open_with_args(
        WindowSpec::new("image-viewer", "图片查看", (900.0, 680.0))
            .min_size((480.0, 360.0))
            .fixed_background(pages::image_viewer::BACKGROUND)
            .centered(),
        pages::image_viewer::ImageViewerWindow,
        pages::image_viewer::ImageViewerArgs { images, index },
        move |dom| dom.with_root_context(AccountScope(account)),
    );
}

/// 收藏。窗口内可以切换账号（`AccountSwitcher`），参数是账号 ID。
pub fn open_favorites(account: i64) {
    open_with_args(
        WindowSpec::new("favorites", "收藏", (960.0, 640.0)).min_size((720.0, 420.0)),
        pages::favorites::FavoritesWindow,
        account,
        |dom| dom,
    );
}

/// 文件。窗口内可以切换账号（`AccountSwitcher`），参数是账号 ID。
pub fn open_files(account: i64) {
    open_with_args(
        WindowSpec::new("files", "文件", (960.0, 640.0)).min_size((760.0, 420.0)),
        pages::files::FilesWindow,
        account,
        |dom| dom,
    );
}

/// 会话的聊天记录，`query` 是打开时的搜索词。
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
        move |dom| dom.with_root_context(AccountScope(account)),
    );
}

pub fn open_reminders(account: i64) {
    open_with_args(
        WindowSpec::new(format!("reminders:{account}"), "消息提醒", (760.0, 560.0))
            .min_size((640.0, 420.0))
            .bounds_key("reminders"),
        pages::reminders::RemindersWindow,
        (),
        move |dom| dom.with_root_context(AccountScope(account)),
    );
}

pub fn open_friend_manager(account: i64) {
    open_with_args(
        WindowSpec::new(
            format!("friend-manager:{account}"),
            "好友管理器",
            (840.0, 600.0),
        )
        .min_size((680.0, 460.0))
        .bounds_key("friend-manager"),
        pages::friend_manager::FriendManagerWindow,
        (),
        move |dom| dom.with_root_context(AccountScope(account)),
    );
}

/// 系统通知：默认模式放在主窗口右侧，否则开窗口。
pub fn open_system_notices(account: i64) {
    if uses_chat_windows() {
        open_with_args(
            WindowSpec::new(
                format!("system-notices:{account}"),
                "系统通知",
                (720.0, 600.0),
            )
            .min_size((480.0, 400.0))
            .bounds_key("system-notices"),
            pages::system_notices::SystemNoticesWindow,
            (),
            move |dom| dom.with_root_context(AccountScope(account)),
        );
    } else {
        send_command(AppCommand::ShowNotices { account });
        send_command(AppCommand::ShowMain);
    }
}

/// 搜索结果的“更多”。
pub fn open_search_more(account: i64, args: SearchMoreArgs) {
    let title = args.window_title();
    open_with_args(
        WindowSpec::new(format!("search-more:{account}"), title, (380.0, 560.0))
            .min_size((300.0, 300.0))
            .bounds_key("search-more"),
        pages::search::SearchMoreWindow,
        args,
        move |dom| dom.with_root_context(AccountScope(account)),
    );
}

/// 添加账号（已有主窗口时）。`account` 不为空时预填这个账号，`error` 显示在表单中。
pub fn open_add_account(args: AddAccountArgs) {
    open_with_args(
        WindowSpec::new("add-account", "添加账号", (320.0, 448.0)).fixed(),
        pages::login::AddAccountWindow,
        args,
        |dom| dom,
    );
}

/// 桌面通知弹窗。新通知替换旧的。
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
        |dom| dom,
    );
}

/// 托盘消息列表（闪烁时悬停托盘图标），`anchor` 是图标的屏幕位置。
pub fn open_tray_messages(anchor: PhysicalPosition<f64>) {
    open_with_args(
        WindowSpec::new("tray-messages", "YaZ", (300.0, 200.0))
            .fixed()
            .popup(),
        pages::tray_messages::TrayMessagesWindow,
        (anchor.x, anchor.y),
        |dom| dom,
    );
}
