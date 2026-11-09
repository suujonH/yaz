//! Chat: the [`ChatPane`] on the right of the main window and the standalone window [`ChatWindow`].

mod actions;
mod channel_dialogs;
mod content;
mod dialogs;
mod events;
mod message;
mod more_panel;
mod sidebar;
mod store;
mod widget_card;

use crate::pages::composer::{Composer, ComposerHandle};
use crate::platform;
use crate::shell::{WindowShell, use_presented, use_window_args};
use crate::state::{
    FocusRequest, use_app_state, use_plugin_events, use_session_data, use_session_event,
};
use crate::ui::{
    Button, ButtonKind, IconButton, IconName, ListViewport, MenuEntry, ScrollAlign, SlideBar,
    VirtualList, VirtualListConfig, use_overlays, use_virtual_list,
};
use crate::widgets::SessionRef;
use crate::windows;
pub use actions::table_menu_entries;
use dioxus::prelude::*;
pub use message::quote_preview_html;
use message::{DeletedRow, MessageRow, NoticeRow};
use more_panel::ChatMorePanel;
use sidebar::TopicSidebar;
use std::rc::Rc;
use std::time::Duration;
use store::{Chat, Item, Target, initial_target, item_key, message_key, unseen_above};
use zulip_core::ConversationKey;
use zulip_core::links;
use zulip_core::model::MentionKind;
use zulip_plugin_host::PluginEvent;
use zulip_session::SessionEvent;

const READ_POLL: Duration = Duration::from_millis(500);
const READ_RETRY: Duration = Duration::from_secs(5);
const PEER_READ_POLL_TICKS: u32 = 60;

/// Parameters of the standalone chat window.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatArgs {
    pub key: ConversationKey,
    pub focus: Option<FocusRequest>,
}

/// The chat content of one conversation; remounted when the conversation or account changes.
/// `focus` is the message to locate after opening. `restore_scroll`: go back to where the
/// conversation was last scrolled to (standalone windows); otherwise it opens at the latest
/// messages or the first unread one.
#[component]
pub fn ChatPane(
    conversation: ConversationKey,
    #[props(default)] focus: Option<FocusRequest>,
    #[props(default = true)] restore_scroll: bool,
) -> Element {
    let state = use_app_state();
    let session = state.session.read().clone();
    // Dioxus only remounts by key inside a list, so this is written as a one-item list
    rsx! {
        for session in session {
            ChatRoot {
                key: "{session.account_id()}:{conversation}",
                session: SessionRef(session),
                conversation: conversation.clone(),
                focus,
                restore_scroll,
            }
        }
    }
}

/// `presented`: false while the window is rendered in advance (hidden); the conversation is then
/// only peeked and opened once the window is shown.
fn opened_target(chat: Chat, focus: Option<i64>, presented: bool, restore_scroll: bool) -> Target {
    let mut chat = chat;
    let session = chat.session();
    let key = chat.current_key();
    let opened = if presented {
        session.open_conversation(&key)
    } else {
        session.peek_conversation(&key)
    };
    chat.first_unread.set(opened.first_unread_id);
    chat.unread_at_open.set(Rc::new(opened.unread.ids.clone()));
    chat.mentions.set(opened.mention_queue.clone());
    let saved = if restore_scroll { opened.scroll } else { None };
    let target = initial_target(focus, opened.first_unread_id, saved);
    chat.at_bottom.set(target == Target::Bottom);
    target
}

/// When the target message is already in memory there is no need to wait for the network, and the
/// first screen appears immediately.
fn needs_loading(chat: Chat, target: Target, first_unread: Option<i64>) -> Option<i64> {
    let session = chat.session();
    let snapshot = session.timeline(&chat.current_key());
    let has = |id: i64| snapshot.messages.iter().any(|m| m.id == id);
    match target {
        Target::Message(id) | Target::Restore { id, .. } => (!has(id)).then_some(id),
        Target::Divider => first_unread.filter(|id| !has(*id)),
        Target::Bottom => (snapshot.messages.is_empty() || !snapshot.found_newest).then_some(0),
    }
}

async fn load_initial(mut chat: Chat, missing: i64) {
    let session = chat.session();
    let key = chat.current_key();
    let result = if missing > 0 {
        session
            .locate_message(Some(&key), missing)
            .await
            .map(|_| ())
    } else {
        session.load_newest(&key).await
    };
    match result {
        Ok(()) => {
            chat.load_failed.set(false);
            chat.reload();
            chat.loaded.set(true);
        }
        Err(error) => {
            log::warn!("消息加载失败：{error}");
            chat.load_failed.set(true);
        }
    }
}

fn rekey(mut chat: Chat, old: ConversationKey, new: ConversationKey) {
    let session = chat.session();
    session.close_conversation(&old);
    session.open_conversation(&new);
    chat.key.set(new.clone());
    chat.spawn(async move {
        if let Err(error) = session.load_newest(&new).await {
            log::warn!("消息加载失败：{error}");
        }
        chat.reload();
    });
}

fn on_session_event(mut chat: Chat, event: SessionEvent) {
    let current = chat.current_key();
    match event {
        SessionEvent::MessageAdded { key, id } if key == current => {
            on_message_added(chat, id);
            chat.schedule_reload();
            chat.spawn(chat.refresh_peer_read());
        }
        SessionEvent::MessageConfirmed { key, .. }
        | SessionEvent::MessageUpdated { key, .. }
        | SessionEvent::MessageRemoved { key, .. }
        | SessionEvent::TimelineChanged(key)
            if key == current =>
        {
            chat.schedule_reload();
        }
        SessionEvent::ConversationRekeyed { old, new } if old == current => rekey(chat, old, new),
        SessionEvent::Resynced => {
            let session = chat.session();
            chat.spawn(async move {
                if let Err(error) = session.load_newest(&current).await {
                    log::warn!("消息加载失败：{error}");
                }
                chat.reload();
            });
        }
        SessionEvent::MutedUsersChanged
        | SessionEvent::UserChanged(_)
        | SessionEvent::UsersChanged => {
            chat.users_tick += 1;
        }
        _ => {}
    }
}

fn on_message_added(mut chat: Chat, id: i64) {
    let session = chat.session();
    let own = session
        .message(id)
        .is_some_and(|m| m.sender_id == session.self_id());
    if own {
        chat.spawn(chat.show_latest());
    } else if !*chat.at_bottom.peek() || !*chat.found_newest.peek() {
        chat.new_count += 1;
        if chat.new_first.peek().is_none() {
            chat.new_first.set(Some(id));
        }
    }
}

/// Automatically mark as read when the window is in the foreground and the latest message is
/// visible; paused after "mark as unread from here". Content is not visible while edge-hidden, so
/// it does not count as viewing; the next round catches up after it moves back.
async fn read_loop(chat: Chat, window: dioxus::desktop::DesktopContext) {
    let mut ticks = 0u32;
    loop {
        let session = chat.session();
        let key = chat.current_key();
        let foreground = platform::is_foreground(&window) && !platform::is_hidden_at_edge(&window);
        let suspended = *chat.auto_read_suspended.peek();
        let latest = *chat.at_bottom.peek() && *chat.found_newest.peek();
        let loaded = *chat.loaded.peek();
        session.set_actively_viewing(&key, foreground && latest && loaded && !suspended);
        let mut wait = READ_POLL;
        if foreground && loaded && !suspended {
            let has_unread = latest && chat.messages.peek().values().any(|m| !m.flags.read);
            let result = if has_unread {
                session.mark_conversation_read(&key).await
            } else {
                let visible = chat.visible_unread();
                if visible.is_empty() {
                    Ok(())
                } else {
                    session.mark_read(&visible).await
                }
            };
            if let Err(error) = result {
                log::warn!("标记已读失败：{error}");
                wait = READ_RETRY;
            }
        }
        ticks += 1;
        if ticks >= PEER_READ_POLL_TICKS {
            ticks = 0;
            chat.refresh_peer_read().await;
        }
        tokio::time::sleep(wait).await;
    }
}

#[component]
fn ChatRoot(
    session: SessionRef,
    conversation: ConversationKey,
    focus: Option<FocusRequest>,
    restore_scroll: bool,
) -> Element {
    let overlays = use_overlays();
    let composer = ComposerHandle {
        insert: use_signal(|| None),
        editing: use_signal(|| None),
    };
    use_context_provider(|| composer);
    let session = session.0;
    let chat = use_context_provider({
        let session = session.clone();
        let conversation = conversation.clone();
        move || Chat::new(session, conversation, overlays, composer)
    });
    let presented = use_presented();
    // Whether the conversation is opened in the session (not yet while rendered in advance)
    let opened = use_hook(|| CopyValue::new(*presented.peek()));
    let target = use_hook(|| {
        let target = opened_target(chat, focus.map(|f| f.id), *presented.peek(), restore_scroll);
        let first_unread = *chat.first_unread.peek();
        let missing = needs_loading(chat, target, first_unread);
        chat.reload();
        match missing {
            Some(id) => chat.spawn(load_initial(chat, id)),
            None => {
                let mut loaded = chat.loaded;
                loaded.set(true);
            }
        }
        chat.spawn(chat.refresh_peer_read());
        target
    });
    use_effect(move || {
        if presented() && !*opened.peek() {
            let mut opened = opened;
            opened.set(true);
            chat.session().open_conversation(&chat.current_key());
        }
    });
    use_drop(move || {
        if !*opened.peek() {
            return;
        }
        let session = chat.session();
        let key = chat.current_key();
        session.set_actively_viewing(&key, false);
        session.save_scroll_position(&key, *chat.position.peek());
        session.close_conversation(&key);
    });
    use_session_event(move |event| on_session_event(chat, event));
    let window = dioxus::desktop::use_window();
    use_future(move || read_loop(chat, window.clone()));
    let mut plugin_tick = chat.plugin_tick;
    use_plugin_events(move |event| match event {
        PluginEvent::AnnotationChanged { account_id, .. } if account_id == chat.account => {
            plugin_tick += 1;
        }
        PluginEvent::PluginsChanged => plugin_tick += 1,
        _ => {}
    });
    let mut last_focus = use_hook(|| CopyValue::new(focus));
    use_effect(use_reactive!(|focus| {
        if *last_focus.peek() != focus {
            last_focus.set(focus);
            if let Some(request) = focus {
                chat.spawn(chat.jump_to(request.id, true, ScrollAlign::Center));
            }
        }
    }));
    let root_id = events::use_chat_events(chat);

    let key = chat.key.read().clone();
    let is_topic = matches!(key, ConversationKey::Topic(_));
    let multi = (chat.multi)();
    let more_open = (chat.more_open)();
    rsx! {
        div { id: "{root_id}", class: if multi { "chat-pane multi" } else { "chat-pane" },
            ChatHead {}
            div { class: "chat-body",
                div { class: "chat-main",
                    div { class: "chat-list-wrap",
                        if (chat.loaded)() {
                            MessageList { target }
                            Banners {}
                        } else if (chat.load_failed)() {
                            div { class: "chat-state",
                                div { "消息加载失败" }
                                Button {
                                    kind: ButtonKind::Secondary,
                                    onclick: move |_| {
                                        let mut failed = chat.load_failed;
                                        failed.set(false);
                                        chat.spawn(load_initial(chat, 0));
                                    },
                                    "重新加载"
                                }
                            }
                        } else {
                            div { class: "chat-state", "加载中…" }
                        }
                    }
                    SlideBar { open: multi, MultiBar {} }
                    div { class: if multi { "chat-composer hidden" } else { "chat-composer" },
                        Composer { key: "{key}", conversation: key.clone() }
                    }
                    if more_open {
                        ChatMorePanel {}
                    }
                }
                if is_topic {
                    TopicSidebar {}
                }
            }
        }
    }
}

#[component]
fn ChatHead() -> Element {
    let chat = use_context::<Chat>();
    let title = use_session_data(
        |e| {
            matches!(
                e,
                SessionEvent::ConversationChanged(_)
                    | SessionEvent::ConversationsChanged
                    | SessionEvent::ChannelChanged(_)
                    | SessionEvent::UsersChanged
                    | SessionEvent::UserChanged(_)
            )
        },
        move |s| s.chat_title(&chat.key.peek()),
    );
    let typing = use_session_data(
        |e| matches!(e, SessionEvent::TypingChanged(_)),
        move |s| s.typing_text(&chat.key.peek()),
    );
    let key = chat.key.read().clone();
    let is_topic = matches!(key, ConversationKey::Topic(_));
    let overlays = chat.overlays;
    rsx! {
        div { class: "chat-head",
            div {
                class: "chat-title",
                onclick: move |e| actions::open_title_card(chat, &e),
                span { class: "chat-title-text", "{title}" }
            }
            if let Some(text) = typing() {
                span { class: "chat-typing", "{text}" }
            }
            div { class: "chat-head-actions",
                if is_topic {
                    IconButton {
                        icon: IconName::App,
                        onclick: move |e: MouseEvent| {
                            let at = e.client_coordinates();
                            overlays.show_menu(at.x, at.y, group_apps(chat));
                        },
                    }
                }
                IconButton {
                    icon: IconName::OpenExternal,
                    onclick: move |_| actions::open_in_browser(chat),
                }
                IconButton {
                    icon: IconName::More,
                    onclick: move |_| {
                        let mut open = chat.more_open;
                        open.toggle();
                    },
                }
            }
        }
    }
}

fn group_apps(chat: Chat) -> Vec<MenuEntry> {
    let account = chat.account;
    let insert = chat.composer.insert;
    vec![
        MenuEntry::item("频道文件", move || windows::open_files(account)).icon(IconName::Folder),
        MenuEntry::item("投票", move || {
            let mut insert = insert;
            insert.set(Some("/poll ".to_owned()));
        })
        .icon(IconName::Poll),
    ]
}

#[component]
fn MessageList(target: Target) -> Element {
    let chat = use_context::<Chat>();
    let handle = use_virtual_list(VirtualListConfig {
        estimate: 64.0,
        overscan: 1000.0,
        stick_to_bottom: true,
        start_at_bottom: target == Target::Bottom,
    });
    let start = handle.clone();
    use_hook(move || {
        let mut list = chat.list;
        list.set(Some(start.clone()));
        let keys = chat.keys.peek().clone();
        match target {
            Target::Bottom => {}
            Target::Divider => {
                let divider = chat
                    .items
                    .peek()
                    .iter()
                    .find(|i| **i == Item::Divider)
                    .map(item_key);
                if let Some(divider) = divider {
                    start.start_at(&keys, divider, ScrollAlign::Start, 0.0);
                }
            }
            Target::Message(id) => {
                start.start_at(&keys, message_key(id), ScrollAlign::Center, 0.0);
                chat.flash_message(id);
            }
            Target::Restore { id, offset } => {
                start.start_at(&keys, message_key(id), ScrollAlign::Start, offset);
            }
        }
    });
    let keys = chat.keys.read().clone();
    rsx! {
        VirtualList {
            handle,
            keys,
            class: "chat-list",
            render: move |index: usize| {
                let item = chat.items.peek().get(index).cloned();
                match item {
                    Some(Item::Message(id)) => rsx! { MessageRow { id } },
                    Some(Item::Notice(id)) => rsx! { NoticeRow { id } },
                    Some(Item::Deleted(id)) => rsx! { DeletedRow { id } },
                    Some(Item::Time { text, .. }) => rsx! { div { class: "chat-time", span { "{text}" } } },
                    Some(Item::Divider) => rsx! { div { class: "chat-divider", span { "新消息" } } },
                    Some(Item::Loading) => rsx! { div { class: "chat-loading", "加载中…" } },
                    None => rsx! {},
                }
            },
            on_reach_top: move |()| chat.spawn(chat.load_older()),
            on_reach_bottom: move |()| chat.spawn(chat.load_newer()),
            on_viewport: move |v: ListViewport| chat.on_viewport(v.first, v.offset, v.last, v.at_bottom),
        }
    }
}

#[component]
fn Banners() -> Element {
    let chat = use_context::<Chat>();
    let first_mention = chat.mentions.read().first().copied();
    let above = unseen_above(&chat.unread_at_open.read(), (chat.min_top_seen)());
    let top_text = match first_mention {
        Some((_, MentionKind::Personal)) => Some("@我".to_owned()),
        Some((_, _)) => Some("@全体成员".to_owned()),
        None if above > 0 => Some(format!("{above}条新消息")),
        None => None,
    };
    let new_count = (chat.new_count)();
    rsx! {
        if let Some(text) = top_text {
            div {
                class: "chat-banner top",
                onclick: move |_| {
                    let mut chat = chat;
                    let mention = chat.mentions.peek().first().copied();
                    if let Some((id, _)) = mention {
                        chat.mentions.write().remove(0);
                        chat.spawn(chat.jump_to(id, true, ScrollAlign::Center));
                    } else {
                        chat.spawn(chat.jump_to_first_unread());
                    }
                },
                crate::ui::Icon { name: IconName::ChevronUpDouble, size: 14.0 }
                "{text}"
            }
        }
        if new_count > 0 {
            div {
                class: "chat-banner bottom",
                onclick: move |_| {
                    let mut chat = chat;
                    let first = *chat.new_first.peek();
                    chat.new_count.set(0);
                    chat.new_first.set(None);
                    match first {
                        Some(id) => chat.spawn(chat.jump_to(id, false, ScrollAlign::Start)),
                        None => chat.spawn(chat.show_latest()),
                    }
                },
                crate::ui::Icon { name: IconName::ChevronDown, size: 14.0 }
                "{new_count}条新消息"
            }
        }
    }
}

#[component]
fn MultiBar() -> Element {
    let chat = use_context::<Chat>();
    let count = chat.selected.read().len();
    let disabled = count == 0;
    rsx! {
        div { class: "multi-bar",
            span { class: "multi-count", "已选择{count}条消息" }
            div { class: "multi-actions",
                Button {
                    kind: ButtonKind::Secondary,
                    disabled,
                    onclick: move |_| dialogs::forward(chat, actions::selected_ids(chat), false),
                    "逐条转发"
                }
                Button {
                    kind: ButtonKind::Secondary,
                    disabled,
                    onclick: move |_| dialogs::forward(chat, actions::selected_ids(chat), true),
                    "合并转发"
                }
                Button {
                    kind: ButtonKind::Secondary,
                    disabled,
                    onclick: move |_| actions::star_selected(chat),
                    "收藏"
                }
                Button {
                    kind: ButtonKind::Secondary,
                    disabled,
                    onclick: move |_| actions::copy_selected(chat),
                    "复制"
                }
                IconButton {
                    icon: IconName::Close,
                    onclick: move |_| chat.exit_multi(),
                }
            }
        }
    }
}

#[component]
pub fn ChatWindow() -> Element {
    crate::widgets::use_plugin_key_shortcuts();
    let args = use_window_args::<ChatArgs>();
    let ChatArgs { key, focus } = args();
    rsx! {
        WindowShell {
            ChatPane { key: "{key}", conversation: key.clone(), focus }
        }
    }
}

/// Web address of a topic or direct message.
fn web_link(chat: Chat) -> String {
    let session = chat.session();
    match chat.current_key() {
        ConversationKey::Topic(t) => {
            let channel = session
                .channel(t.stream_id)
                .map_or_else(String::new, |c| c.name);
            links::topic_link(
                session.realm_url(),
                t.stream_id,
                &channel,
                &session.topic_name(&ConversationKey::Topic(t.clone())),
            )
        }
        ConversationKey::Dm(d) => links::dm_link(session.realm_url(), &d, session.self_id()),
    }
}
