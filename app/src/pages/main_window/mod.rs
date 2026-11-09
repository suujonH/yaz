//! 主窗口：标题栏、左侧导航、中间页面、右侧内容区。经典模式只留前两栏，会话开独立窗口。

use crate::pages::chat::ChatPane;
use crate::pages::contacts::ContactsPage;
use crate::pages::dialogs::plus_menu_items;
use crate::pages::login::AddAccountArgs;
use crate::pages::messages::MessagesPage;
use crate::pages::search::SearchPanel;
use crate::pages::settings::SettingsTarget;
use crate::pages::system_notices::SystemNoticesPane;
use crate::platform::use_edge_hide;
use crate::root::{Stage, apply_main_bounds, request_close};
use crate::services::{services, write_setting};
use crate::shell::{DragRegion, WindowShell};
use crate::state::{
    AppCommand, FocusRequest, use_app_command, use_app_state, use_local_setting, use_session_data,
    use_session_event,
};
use crate::ui::*;
use crate::widgets::{
    ProfilePane, RemoteAvatar, logout_confirm, open_user_card, use_plugin_key_shortcuts,
};
use crate::windows;
use dioxus::desktop::use_window;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use zulip_core::ConversationKey;
use zulip_core::model::PresenceState;
use zulip_session::{AccountEntry, AccountStatus, HubEvent, Session, SessionEvent, badge_text};

const SIGNATURE_MAX: usize = 60;

/// 中间栏显示的页面。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavPage {
    #[default]
    Messages,
    Contacts,
}

/// 右侧内容区。
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RightPane {
    #[default]
    Empty,
    Chat {
        key: ConversationKey,
        focus: Option<FocusRequest>,
    },
    User(i64),
    Topic(ConversationKey),
    Notices,
}

/// 主窗口各页面共享的状态。
#[derive(Clone, Copy)]
pub struct MainUi {
    pub page: Signal<NavPage>,
    pub right: Signal<RightPane>,
    /// 搜索框内容；不为空时搜索结果盖住中间页面。
    pub search: Signal<String>,
    /// 联系人页要切到频道并展开的频道。
    pub reveal_channel: Signal<Option<i64>>,
}

pub fn use_main_ui() -> MainUi {
    use_context::<MainUi>()
}

impl MainUi {
    /// 账号切换时清空和账号相关的内容。
    fn reset(mut self) {
        self.search.set(String::new());
        self.right.set(RightPane::Empty);
    }
}

/// 根窗口的主界面阶段。`children` 是窗口内不显示内容的后台组件。
#[component]
pub fn MainWindowPage(children: Element) -> Element {
    let window = use_window();
    let classic = use_local_setting(|s| s.classic_mode());
    rsx! {
        WindowShell {
            on_close: move |_| request_close(&window, &Stage::Main { restore: false }),
            caption_left: rsx! { TitleLeft {} },
            caption_right: rsx! { TitleRight { classic: classic() } },
            {children}
            MainBody { classic: classic() }
        }
    }
}

/// 退出当前账号的统一流程：标题栏菜单和托盘都走这里。
async fn logout_active(overlays: Overlays) {
    let hub = services().hub.clone();
    let Some(session) = hub.active_session() else {
        return;
    };
    let multiple = hub.online_sessions().len() > 1;
    let name = session.display_name(session.self_id());
    let account = session.account_id();
    drop(session);
    if overlays.confirm(logout_confirm(&name, multiple)).await {
        hub.logout(account, false).await;
    }
}

#[component]
fn MainBody(classic: bool) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let window = use_window();
    use_plugin_key_shortcuts();
    let ui = use_context_provider(|| MainUi {
        page: Signal::new(NavPage::default()),
        right: Signal::new(RightPane::default()),
        search: Signal::new(String::new()),
        reveal_channel: Signal::new(None),
    });

    let edge_setting = use_local_setting(|s| s.edge_auto_hide());
    let mut edge_enabled = use_signal(move || classic && edge_setting());
    use_effect(use_reactive!(|classic| {
        edge_enabled.set(classic && edge_setting());
    }));
    let edge = use_edge_hide(
        edge_enabled.into(),
        use_callback(move |()| overlays.any_open()),
    );

    // 切换账号时清空搜索和右侧内容
    let mut last_account = use_signal(|| *state.account.peek());
    use_effect(move || {
        let account = (state.account)();
        if *last_account.peek() != account {
            last_account.set(account);
            ui.reset();
        }
    });

    // 模式切换：恢复这个模式的窗口位置，右侧的聊天移到独立窗口
    let mut last_classic = use_signal(|| classic);
    {
        let window = window.clone();
        use_effect(use_reactive!(|classic| {
            if *last_classic.peek() == classic {
                return;
            }
            last_classic.set(classic);
            let mut right = ui.right;
            if classic {
                if let (RightPane::Chat { key, focus }, Some(account)) =
                    (right.peek().clone(), *state.account.peek())
                {
                    windows::open_chat_window(account, key, focus.map(|f| f.id));
                }
                right.set(RightPane::Empty);
            }
            apply_main_bounds(&window);
        }));
    }

    use_app_command(move |command| {
        let mut ui = ui;
        match command {
            AppCommand::ShowMain => edge.reveal(),
            AppCommand::ShowChat {
                account,
                key,
                focus,
            } => {
                let hub = &services().hub;
                if hub.active_account() != Some(account) {
                    hub.set_active(Some(account));
                }
                ui.page.set(NavPage::Messages);
                ui.search.set(String::new());
                ui.right.set(RightPane::Chat { key, focus });
            }
            AppCommand::ShowChannel { account, stream_id } => {
                let hub = &services().hub;
                if hub.active_account() != Some(account) {
                    hub.set_active(Some(account));
                }
                ui.page.set(NavPage::Contacts);
                ui.search.set(String::new());
                ui.reveal_channel.set(Some(stream_id));
            }
            AppCommand::ShowNotices { account } => {
                let hub = &services().hub;
                if hub.active_account() != Some(account) {
                    hub.set_active(Some(account));
                }
                ui.right.set(RightPane::Notices);
            }
            AppCommand::Logout => {
                spawn(logout_active(overlays));
            }
            _ => {}
        }
    });

    // 打开中的话题被改名、移动后跟着新键走
    use_session_event(move |event| {
        let mut right = ui.right;
        let next = match (event, right.peek().clone()) {
            (SessionEvent::ConversationRekeyed { old, new }, RightPane::Chat { key, focus })
                if key == old =>
            {
                Some(RightPane::Chat { key: new, focus })
            }
            (SessionEvent::ConversationRekeyed { old, new }, RightPane::Topic(key))
                if key == old =>
            {
                Some(RightPane::Topic(new))
            }
            _ => None,
        };
        if let Some(next) = next {
            right.set(next);
        }
    });

    let has_session = state.session.read().is_some();
    let searching = !ui.search.read().is_empty();
    // 点击搜索结果以外的地方收起
    let close_search = move || {
        let mut search = ui.search;
        if !search.peek().is_empty() {
            search.set(String::new());
        }
    };
    rsx! {
        div { class: "main-page",
            LeftNav { has_session }
            div { class: if classic { "main-middle classic" } else { "main-middle" },
                if has_session {
                    SearchBar {}
                    div { class: "main-middle-page",
                        match (ui.page)() {
                            NavPage::Messages => rsx! { MessagesPage {} },
                            NavPage::Contacts => rsx! { ContactsPage {} },
                        }
                        if searching {
                            SearchPanel {}
                        }
                    }
                } else {
                    EmptyAccounts {}
                }
            }
            if !classic {
                div {
                    class: "main-right",
                    onmousedown: move |_| close_search(),
                    RightContent {}
                }
            }
        }
    }
}

#[component]
fn RightContent() -> Element {
    let ui = use_main_ui();
    let right = (ui.right)();
    match right {
        RightPane::Empty => rsx! {
            div { class: "main-placeholder",
                Icon { name: IconName::Logo, size: 90.0 }
            }
        },
        RightPane::Chat { key, focus } => rsx! {
            ChatPane { key: "{key}", conversation: key.clone(), focus }
        },
        RightPane::User(user_id) => rsx! { ProfilePane { user: user_id } },
        RightPane::Topic(key) => rsx! { ProfilePane { topic: key } },
        RightPane::Notices => rsx! { SystemNoticesPane {} },
    }
}

#[component]
fn EmptyAccounts() -> Element {
    rsx! {
        div { class: "main-empty",
            div { class: "text-title", "还没有登录的账号" }
            div { class: "text-sub", "在设置的“Zulip 账号”中启用账号，或添加一个账号" }
            Button {
                kind: ButtonKind::Primary,
                onclick: move |_| windows::open_add_account(AddAccountArgs::default()),
                "添加账号"
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct SelfInfo {
    id: i64,
    name: String,
    avatar: Option<String>,
    online: bool,
    status: String,
}

fn read_self(session: &std::sync::Arc<Session>) -> SelfInfo {
    let id = session.self_id();
    SelfInfo {
        id,
        name: session.display_name(id),
        avatar: session.user(id).and_then(|u| u.avatar_url),
        online: session.presence(id) == PresenceState::Online,
        status: session.status_text(id).unwrap_or_default(),
    }
}

/// 标题栏左侧：图标、自己的头像和名字、个性签名。
#[component]
fn TitleLeft() -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let classic = use_local_setting(|s| s.classic_mode());
    let me = use_session_data(
        |e| {
            matches!(
                e,
                SessionEvent::UserChanged(_)
                    | SessionEvent::UsersChanged
                    | SessionEvent::PresenceChanged(_)
                    | SessionEvent::ContactsChanged
            )
        },
        read_self,
    );
    let mut editing = use_signal(|| false);
    let info = me.read().clone();
    let has_session = state.session.read().is_some();
    rsx! {
        div { class: "title-logo", Icon { name: IconName::Logo, size: 18.0 } }
        if has_session {
            div {
                class: "title-self no-drag",
                onmousedown: move |e| e.stop_propagation(),
                RemoteAvatar {
                    text: info.name.clone(),
                    url: info.avatar.clone(),
                    size: 24.0,
                    online: info.online,
                    onclick: move |e: MouseEvent| {
                        let point = e.client_coordinates();
                        open_user_card(overlays, Rect::point(point.x, point.y), me.peek().id);
                    },
                }
            }
            span { class: "title-name", "{info.name}" }
            if !classic() {
                div {
                    class: "title-signature no-drag",
                    onmousedown: move |e| e.stop_propagation(),
                    if editing() {
                        InlineEdit {
                            value: info.status.clone(),
                            oncommit: move |text: String| {
                                editing.set(false);
                                let text: String = text.trim().chars().take(SIGNATURE_MAX).collect();
                                if text == me.peek().status {
                                    return;
                                }
                                let session = state.session.peek().clone();
                                spawn(async move {
                                    if let Some(session) = session {
                                        overlays.attempt("修改失败", session.set_status_text(&text)).await;
                                    }
                                });
                            },
                            oncancel: move |_| editing.set(false),
                        }
                    } else {
                        Tooltip { text: "编辑个性签名",
                            button {
                                class: "title-signature-text",
                                r#type: "button",
                                onclick: move |_| editing.set(true),
                                if info.status.is_empty() { "编辑个性签名" } else { "{info.status}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 标题栏右侧：布局切换。
#[component]
fn TitleRight(classic: bool) -> Element {
    let tip = if classic {
        "切换为默认模式"
    } else {
        "切换为经典模式"
    };
    rsx! {
        Tooltip { text: tip,
            button {
                class: "caption-btn",
                r#type: "button",
                tabindex: "-1",
                onclick: move |_| {
                    write_setting(|s| s.set_classic_mode(!classic));
                },
                Icon { name: IconName::Layout, size: 14.0 }
            }
        }
    }
}

/// 中间栏顶部：搜索框和“+”。
#[component]
fn SearchBar() -> Element {
    let mut ui = use_main_ui();
    let state = use_app_state();
    let overlays = use_overlays();
    let search = ui.search;
    rsx! {
        div {
            class: "main-search",
            onkeydown: move |e| {
                if e.key() == Key::Escape {
                    ui.search.set(String::new());
                }
            },
            TextInput {
                value: search(),
                placeholder: "搜索",
                style_kind: InputStyle::Search,
                clearable: true,
                icon: IconName::Search,
                oninput: move |v| ui.search.set(v),
            }
            IconButton {
                icon: IconName::Add,
                size: 30.0,
                onclick: move |e: MouseEvent| {
                    let Some(session) = state.session.peek().clone() else { return };
                    let point = e.client_coordinates();
                    overlays.show_menu(point.x, point.y, plus_menu_items(session, overlays));
                },
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct NavCounts {
    unread: usize,
    contact_alert: bool,
}

fn read_counts(session: &std::sync::Arc<Session>) -> NavCounts {
    NavCounts {
        unread: session.total_unread_for_badge(),
        contact_alert: session.unread_notice_count() + session.unread_reminder_count() > 0,
    }
}

/// 点击账号：在线的切换；失败的打开登录窗口填好错误；停用的启用。
fn select_account(entry: &AccountEntry, overlays: Overlays) {
    let id = entry.account.id;
    match &entry.status {
        AccountStatus::Online => services().hub.set_active(Some(id)),
        AccountStatus::Failed(error) => windows::open_add_account(AddAccountArgs {
            account: Some(id),
            error: Some(error.clone()),
        }),
        AccountStatus::Disabled => {
            spawn(async move {
                overlays
                    .attempt("启用失败", services().hub.set_login_on_startup(id, true))
                    .await;
            });
        }
        AccountStatus::Connecting => {}
    }
}

/// 导航栏的账号头像：头像、未读角标、失败标记。
#[component]
fn AccountButton(account_id: i64, active: bool, tick: u64) -> Element {
    let _ = tick;
    let overlays = use_overlays();
    let state = use_app_state();
    let accounts = state.accounts.read().clone();
    let Some(entry) = accounts
        .iter()
        .find(|e| e.account.id == account_id)
        .cloned()
    else {
        return rsx! {};
    };
    let name = entry.account.full_name.clone();
    let unread = entry
        .session
        .as_ref()
        .map_or(0, |s| s.total_unread_for_badge());
    let failed = matches!(entry.status, AccountStatus::Failed(_));
    let tip = format!("{name}（{}）", entry.status.text());
    let on_select = entry.clone();
    rsx! {
        Tooltip { text: tip,
            button {
                class: if active { "nav-account active" } else { "nav-account" },
                r#type: "button",
                onclick: move |_| select_account(&on_select, overlays),
                Avatar { text: name, size: 30.0 }
                if failed {
                    span { class: "nav-account-failed", "!" }
                } else if unread > 0 {
                    span { class: "nav-badge", Badge { count: unread as u32 } }
                }
            }
        }
    }
}

#[component]
fn NavButton(
    icon: IconName,
    filled: IconName,
    active: bool,
    #[props(into)] tip: String,
    badge: Element,
    onclick: EventHandler<()>,
) -> Element {
    rsx! {
        Tooltip { text: tip,
            button {
                class: if active { "nav-btn active" } else { "nav-btn" },
                r#type: "button",
                onclick: move |_| onclick.call(()),
                Icon { name: icon, size: 24.0 }
                CircleReveal { open: active,
                    Icon { name: filled, size: 24.0 }
                }
                span { class: "nav-badge", {badge} }
            }
        }
    }
}

#[component]
fn LeftNav(has_session: bool) -> Element {
    let state = use_app_state();
    let mut ui = use_main_ui();
    let counts = use_session_data(
        |e| {
            matches!(
                e,
                SessionEvent::UnreadChanged(_)
                    | SessionEvent::ConversationsChanged
                    | SessionEvent::SystemNoticesChanged
                    | SessionEvent::RemindersChanged
            )
        },
        read_counts,
    );
    // 账号按钮的角标包括不是当前账号的未读
    let mut unread_tick = use_signal(|| 0u64);
    use_future(move || async move {
        let mut rx = services().hub.subscribe();
        loop {
            match rx.recv().await {
                Ok(HubEvent::Session(_, SessionEvent::UnreadChanged(_)))
                | Err(RecvError::Lagged(_)) => {
                    unread_tick += 1;
                }
                Ok(_) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
    let accounts = state.accounts.read().clone();
    let active = *state.account.read();
    let tick = unread_tick();
    let muted = use_local_setting(|s| s.notification_muted());
    let page = (ui.page)();
    let NavCounts {
        unread,
        contact_alert,
    } = counts();
    let badge = badge_text(unread).map_or(0, |_| unread as u32);
    rsx! {
        DragRegion { class: "main-nav",
            div {
                class: "main-nav-items no-drag",
                onmousedown: move |e| e.stop_propagation(),
                if accounts.len() > 1 {
                    for entry in accounts.iter() {
                        AccountButton {
                            key: "{entry.account.id}",
                            tick,
                            active: active == Some(entry.account.id),
                            account_id: entry.account.id,
                        }
                    }
                    div { class: "main-nav-divider" }
                }
                if has_session {
                    NavButton {
                        icon: IconName::Chat,
                        filled: IconName::ChatFilled,
                        active: page == NavPage::Messages,
                        tip: "消息",
                        badge: rsx! { Badge { count: badge } },
                        onclick: move |_| ui.page.set(NavPage::Messages),
                    }
                    NavButton {
                        icon: IconName::User,
                        filled: IconName::UserFilled,
                        active: page == NavPage::Contacts,
                        tip: "联系人",
                        badge: rsx! { Badge { dot: contact_alert } },
                        onclick: move |_| ui.page.set(NavPage::Contacts),
                    }
                }
            }
            div { class: "main-nav-spacer" }
            div {
                class: "main-nav-items no-drag",
                onmousedown: move |e| e.stop_propagation(),
                Tooltip { text: if muted() { "取消静音" } else { "静音" },
                    IconButton {
                        icon: if muted() { IconName::NotificationOff } else { IconName::Notification },
                        size: 36.0,
                        icon_size: 20.0,
                        onclick: move |_| {
                            let next = !*muted.peek();
                            write_setting(|s| s.set_notification_muted(next));
                        },
                    }
                }
                Tooltip { text: "主菜单",
                    Popover {
                        options: PopoverOptions { placement: Placement::RightStart, gap: 8.0, min_width: 200.0 },
                        content: move |handle: PopoverHandle| rsx! { MainMenu { handle } },
                        IconButton { icon: IconName::ViewList, size: 36.0, icon_size: 20.0, class: "main-menu-button" }
                    }
                }
            }
        }
    }
}

/// 主菜单：收藏、文件、调色盘，设置、添加账号、退出账号。
#[component]
fn MainMenu(handle: PopoverHandle) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let account = *state.account.read();
    let open = move |action: Box<dyn Fn()>| {
        handle.close();
        // 弹层随即销毁，动作放到不随它取消的任务里
        spawn_forever(async move {
            tokio::time::sleep(Duration::ZERO).await;
            action();
        });
    };
    let tile = |icon: IconName, text: &'static str, action: Box<dyn Fn()>| {
        let action = std::rc::Rc::new(action);
        rsx! {
            button {
                class: "main-menu-tile",
                r#type: "button",
                onclick: move |_| {
                    let action = action.clone();
                    open(Box::new(move || action()));
                },
                Icon { name: icon, size: 22.0 }
                span { "{text}" }
            }
        }
    };
    let row = |icon: IconName, text: &'static str, action: Box<dyn Fn()>| {
        let action = std::rc::Rc::new(action);
        rsx! {
            ListRow {
                density: RowDensity::Compact,
                leading: rsx! { Icon { name: icon, size: 18.0 } },
                title: text,
                onclick: move |_| {
                    let action = action.clone();
                    open(Box::new(move || action()));
                },
            }
        }
    };
    rsx! {
        div { class: "main-menu",
            if let Some(account) = account {
                div { class: "main-menu-tiles",
                    {tile(IconName::Star, "收藏", Box::new(move || windows::open_favorites(account)))}
                    {tile(IconName::Folder, "文件", Box::new(move || windows::open_files(account)))}
                    {tile(IconName::Palette, "调色盘", Box::new(|| windows::open_settings(SettingsTarget::Palette)))}
                }
            } else {
                div { class: "main-menu-tiles",
                    {tile(IconName::Palette, "调色盘", Box::new(|| windows::open_settings(SettingsTarget::Palette)))}
                }
            }
            {row(IconName::Setting, "设置", Box::new(|| windows::open_settings(SettingsTarget::General)))}
            {row(IconName::Add, "添加账号", Box::new(|| windows::open_add_account(AddAccountArgs::default())))}
            if account.is_some() {
                {row(IconName::Logout, "退出账号", Box::new(move || { spawn_forever(logout_active(overlays)); }))}
            }
        }
    }
}
