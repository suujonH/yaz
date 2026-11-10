//! Main window: title bar, left navigation, middle page, right content area. Classic mode keeps
//! only the first two columns, and conversations open in standalone windows.

use crate::pages::chat::ChatPane;
use crate::pages::contacts::ContactsPage;
use crate::pages::dialogs::plus_menu_items;
use crate::pages::login::AddAccountArgs;
use crate::pages::messages::MessagesPage;
use crate::pages::reminders::RemindersPane;
use crate::pages::search::SearchPanel;
use crate::pages::settings::SettingsTarget;
use crate::pages::system_notices::SystemNoticesPane;
use crate::platform::use_edge_hide;
use crate::root::{Stage, StartupLogin, apply_main_bounds, request_close};
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
use crate::windows::{self, WindowTarget};
use dioxus::desktop::use_window;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use zulip_core::ConversationKey;
use zulip_core::model::PresenceState;
use zulip_session::{AccountEntry, AccountStatus, HubEvent, Session, SessionEvent, badge_text};

const SIGNATURE_MAX: usize = 60;

/// The page shown in the middle column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavPage {
    #[default]
    Messages,
    Contacts,
}

/// The right content area.
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
    Reminders,
}

/// State shared by the pages of the main window.
#[derive(Clone, Copy)]
pub struct MainUi {
    pub page: Signal<NavPage>,
    pub right: Signal<RightPane>,
    /// Search box content; when non-empty, search results cover the middle page.
    pub search: Signal<String>,
    /// Whether the search results are shown: a press anywhere else hides them (the text stays),
    /// pressing the search box shows them again.
    pub search_open: Signal<bool>,
    /// The channel the contacts page should switch to and expand.
    pub reveal_channel: Signal<Option<i64>>,
    /// What the right side showed on the messages page when switching to the contacts page; shown
    /// again when switching back.
    messages_right: Signal<RightPane>,
}

pub fn use_main_ui() -> MainUi {
    use_context::<MainUi>()
}

impl MainUi {
    /// When the account switches, clear account-related content.
    fn reset(mut self) {
        self.search.set(String::new());
        self.right.set(RightPane::Empty);
        self.messages_right.set(RightPane::Empty);
    }

    /// Switch the middle page. The contacts page starts with nothing selected on the right; the
    /// messages page gets back the conversation it had open (without its scroll position).
    pub fn show_page(mut self, next: NavPage) {
        let current = *self.page.peek();
        if current == next {
            return;
        }
        if current == NavPage::Messages {
            let kept = match self.right.peek().clone() {
                RightPane::Chat { key, .. } => RightPane::Chat { key, focus: None },
                RightPane::Notices => RightPane::Notices,
                _ => RightPane::Empty,
            };
            self.messages_right.set(kept);
        }
        let right = match next {
            NavPage::Messages => std::mem::take(&mut *self.messages_right.write()),
            NavPage::Contacts => RightPane::Empty,
        };
        self.right.set(right);
        self.page.set(next);
    }
}

/// Main UI phase of the root window. `children` are background components in the window that show
/// no content.
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

/// Unified flow for logging out of the current account: the title bar menu and the tray both go
/// through here.
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
        search_open: Signal::new(false),
        reveal_channel: Signal::new(None),
        messages_right: Signal::new(RightPane::Empty),
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

    // When the account switches, clear search and the right-side content
    let mut last_account = use_signal(|| *state.account.peek());
    use_effect(move || {
        let account = (state.account)();
        if *last_account.peek() != account {
            last_account.set(account);
            ui.reset();
        }
    });

    // Mode switch: restore this mode's window position, and move the chat on the right into a
    // standalone window
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
                ui.show_page(NavPage::Messages);
                ui.search.set(String::new());
                ui.search_open.set(false);
                ui.right.set(RightPane::Chat { key, focus });
            }
            AppCommand::ShowChannel { account, stream_id } => {
                let hub = &services().hub;
                if hub.active_account() != Some(account) {
                    hub.set_active(Some(account));
                }
                ui.show_page(NavPage::Contacts);
                ui.search.set(String::new());
                ui.search_open.set(false);
                ui.reveal_channel.set(Some(stream_id));
            }
            AppCommand::ShowNotices { account } => {
                let hub = &services().hub;
                if hub.active_account() != Some(account) {
                    hub.set_active(Some(account));
                }
                ui.right.set(RightPane::Notices);
            }
            AppCommand::ShowReminders { account } => {
                let hub = &services().hub;
                if hub.active_account() != Some(account) {
                    hub.set_active(Some(account));
                }
                ui.right.set(RightPane::Reminders);
            }
            AppCommand::Logout => {
                spawn(logout_active(overlays));
            }
            _ => {}
        }
    });

    // When an open topic is renamed or moved, follow the new key
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
    let logging_in = use_logging_in();
    // Pressing the search box opens the panel: results, or recent searches while it is empty
    let searching = (ui.search_open)();
    rsx! {
        div {
            class: "main-page",
            // A press outside the search box and its results hides the results (both stop the
            // event themselves)
            onmousedown: move |_| {
                let mut open = ui.search_open;
                if *open.peek() {
                    open.set(false);
                }
            },
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
                } else if logging_in() {
                    div { class: "main-empty",
                        Icon { name: IconName::Loading, size: 22.0, class: "spin" }
                    }
                } else {
                    NoSession {}
                }
            }
            if !classic {
                div { class: "main-right", RightContent {} }
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
            ChatPane { key: "{key}", conversation: key.clone(), focus, restore_scroll: false }
        },
        RightPane::User(user_id) => rsx! { ProfilePane { user: user_id } },
        RightPane::Topic(key) => rsx! { ProfilePane { topic: key } },
        RightPane::Notices => rsx! { SystemNoticesPane {} },
        RightPane::Reminders => rsx! { RemindersPane {} },
    }
}

/// Logging in: at startup (plugins load first, then the accounts set to log in at startup), or the
/// current account is connecting. An empty list then means "not loaded yet".
pub fn use_logging_in() -> Memo<bool> {
    let state = use_app_state();
    let startup = try_use_context::<StartupLogin>();
    use_memo(move || {
        startup.is_some_and(|s| (s.0)())
            || state.accounts.read().iter().any(|a| {
                Some(a.account.id) == (state.account)() && a.status == AccountStatus::Connecting
            })
    })
}

/// No account online: accounts the server could not be reached for (retried in the background,
/// or connecting again) are listed grey with the reason and a retry button; otherwise
/// [`EmptyAccounts`].
#[component]
fn NoSession() -> Element {
    let state = use_app_state();
    let waiting: Vec<(i64, String, Option<String>)> = state
        .accounts
        .read()
        .iter()
        .filter_map(|e| {
            let error = match &e.status {
                AccountStatus::Unreachable(error) => Some(error.clone()),
                AccountStatus::Connecting if services().hub.is_unreachable(e.account.id) => None,
                _ => return None,
            };
            Some((e.account.id, e.account.full_name.clone(), error))
        })
        .collect();
    if waiting.is_empty() {
        return rsx! { EmptyAccounts {} };
    }
    rsx! {
        div { class: "main-empty",
            div { class: "text-title", "无法连接服务器" }
            div { class: "text-sub", "连接恢复后会自动登录" }
            div { class: "unreachable-list",
                for (id, name, error) in waiting {
                    div { key: "{id}", class: "unreachable-row",
                        div { class: "unreachable-avatar", Avatar { text: name.clone(), size: 36.0 } }
                        div { class: "unreachable-text",
                            div { class: "unreachable-name", "{name}" }
                            div { class: "text-sub",
                                {error.clone().unwrap_or_else(|| "正在重新连接…".to_owned())}
                            }
                        }
                        Button {
                            disabled: error.is_none(),
                            onclick: move |_| retry_account(id),
                            "重试"
                        }
                    }
                }
            }
        }
    }
}

/// Logs in an unreachable account now (instead of waiting for the background retry).
fn retry_account(id: i64) {
    spawn(async move {
        let _ = services().hub.login_account(id).await;
    });
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

/// Left side of the title bar: icon, own avatar and name, status text.
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

/// Right side of the title bar: layout switch.
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
                // Default mode: fold the right side away; classic mode: bring it back
                Icon {
                    name: if classic { IconName::PanelArrowLeft } else { IconName::PanelArrowRight },
                    size: 14.0,
                }
            }
        }
    }
}

/// Top of the middle column: search box and "+".
#[component]
fn SearchBar() -> Element {
    let mut ui = use_main_ui();
    let state = use_app_state();
    let overlays = use_overlays();
    let search = ui.search;
    rsx! {
        div {
            class: "main-search",
            // Pressing here does not hide the panel (see the main page); only the box opens it
            onmousedown: move |e| e.stop_propagation(),
            onkeydown: move |e| {
                if e.key() == Key::Escape {
                    ui.search.set(String::new());
                    ui.search_open.set(false);
                }
            },
            div {
                class: "main-search-box",
                onmousedown: move |_| {
                    let mut open = ui.search_open;
                    if !*open.peek() {
                        open.set(true);
                    }
                },
                TextInput {
                    value: search(),
                    placeholder: "搜索",
                    style_kind: InputStyle::Search,
                    clearable: true,
                    icon: IconName::Search,
                    oninput: move |v| {
                        ui.search.set(v);
                        ui.search_open.set(true);
                    },
                }
            }
            IconButton {
                icon: IconName::Add,
                size: 30.0,
                class: "main-search-add",
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

/// Clicking an account: an online one is switched to; a failed one opens the login window with the
/// error filled in; a disabled one is enabled.
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
        AccountStatus::Unreachable(_) => retry_account(id),
        AccountStatus::Connecting => {}
    }
}

/// Account avatar in the navigation bar: avatar, unread badge, failure marker.
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
    // Unreachable (also while connecting again): grey
    let offline = entry.session.is_none() && services().hub.is_unreachable(account_id);
    let tip = format!("{name}（{}）", entry.status.text());
    let on_select = entry.clone();
    rsx! {
        Tooltip { text: tip,
            button {
                class: format!(
                    "nav-account{}{}",
                    if active { " active" } else { "" },
                    if offline { " offline" } else { "" },
                ),
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
    let ui = use_main_ui();
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
    // The account button's badge includes unread of accounts that are not the current one
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
                        icon: IconName::ChatBubble,
                        filled: IconName::ChatFilled,
                        active: page == NavPage::Messages,
                        tip: "消息",
                        badge: rsx! { Badge { count: badge } },
                        onclick: move |_| ui.show_page(NavPage::Messages),
                    }
                    NavButton {
                        icon: IconName::User,
                        filled: IconName::UserFilled,
                        active: page == NavPage::Contacts,
                        tip: "联系人",
                        badge: rsx! { Badge { dot: contact_alert } },
                        onclick: move |_| ui.show_page(NavPage::Contacts),
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

/// Main menu: favorites, files, palette; settings, add account, log out of account.
#[component]
fn MainMenu(handle: PopoverHandle) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let account = *state.account.read();
    let open = move |action: Box<dyn Fn()>| {
        handle.close();
        // The popover is destroyed right away, so the action goes into a task that is not cancelled
        // with it
        spawn_forever(async move {
            tokio::time::sleep(Duration::ZERO).await;
            action();
        });
    };
    let tile = |icon: IconName, text: &'static str, target: WindowTarget, action: Box<dyn Fn()>| {
        let action = std::rc::Rc::new(action);
        rsx! {
            button {
                class: "main-menu-tile",
                r#type: "button",
                onclick: move |_| {
                    let action = action.clone();
                    open(Box::new(move || action()));
                },
                onmouseenter: move |_| windows::hover(Some(target.clone())),
                onmouseleave: move |_| windows::hover(None),
                Icon { name: icon, size: 22.0 }
                span { "{text}" }
            }
        }
    };
    let row = |icon: IconName,
               text: &'static str,
               target: Option<WindowTarget>,
               action: Box<dyn Fn()>| {
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
                onhover: move |entered: bool| windows::hover(target.clone().filter(|_| entered)),
            }
        }
    };
    rsx! {
        div { class: "main-menu",
            if let Some(account) = account {
                div { class: "main-menu-tiles",
                    {tile(IconName::Star, "收藏", WindowTarget::Favorites(account), Box::new(move || windows::open_favorites(account)))}
                    {tile(IconName::Folder, "文件", WindowTarget::Files(account), Box::new(move || windows::open_files(account)))}
                    {tile(IconName::Palette, "调色盘", WindowTarget::Settings(SettingsTarget::Palette), Box::new(|| windows::open_settings(SettingsTarget::Palette)))}
                }
            } else {
                div { class: "main-menu-tiles",
                    {tile(IconName::Palette, "调色盘", WindowTarget::Settings(SettingsTarget::Palette), Box::new(|| windows::open_settings(SettingsTarget::Palette)))}
                }
            }
            {row(IconName::Setting, "设置", Some(WindowTarget::Settings(SettingsTarget::General)), Box::new(|| windows::open_settings(SettingsTarget::General)))}
            {row(IconName::Add, "添加账号", Some(WindowTarget::AddAccount), Box::new(|| windows::open_add_account(AddAccountArgs::default())))}
            if account.is_some() {
                {row(IconName::Logout, "退出账号", None, Box::new(move || { spawn_forever(logout_active(overlays)); }))}
            }
        }
    }
}
