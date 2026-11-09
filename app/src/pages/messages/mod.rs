//! Messages page: conversation list, conversation folders (topic assistant, resolved), connection
//! notice.

use crate::pages::main_window::{RightPane, use_logging_in, use_main_ui};
use crate::services::plugins;
use crate::state::{use_app_state, use_plugin_events, use_session_data, use_session_event};
use crate::ui::*;
use crate::widgets::{RemoteAvatar, conversation_menu, notice_label, plugin_text, text_id};
use crate::windows;
use dioxus::prelude::*;
use std::collections::HashMap;
use std::rc::Rc;
use zulip_core::push::DndLevel;
use zulip_core::text::time::{from_unix, list_time};
use zulip_core::{ConversationFolder, ListEntry};
use zulip_plugin_host::{PluginEvent, PluginStatusText};
use zulip_session::{
    ConnectionState, ConversationAvatar, ConversationRow, SessionEvent, folder_title,
};

/// List data: the order and the by-key lookup are kept separate, so the list needs no relayout when
/// the order is unchanged.
#[derive(Clone, Default, PartialEq)]
struct RowsData {
    keys: Rc<Vec<u64>>,
    rows: Rc<HashMap<u64, ConversationRow>>,
    use_24h: bool,
}

fn rows_data(rows: Vec<ConversationRow>, use_24h: bool) -> RowsData {
    let id = |r: &ConversationRow| text_id(&r.entry.to_string());
    let keys = rows.iter().map(id).collect();
    let rows = rows.into_iter().map(|r| (id(&r), r)).collect();
    RowsData {
        keys: Rc::new(keys),
        rows: Rc::new(rows),
        use_24h,
    }
}

fn list_changed(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ConversationsChanged
            | SessionEvent::ConversationChanged(_)
            | SessionEvent::ConversationRekeyed { .. }
            | SessionEvent::UnreadChanged(_)
            | SessionEvent::DraftChanged(_)
            | SessionEvent::ContactsChanged
            | SessionEvent::UserChanged(_)
            | SessionEvent::UsersChanged
            | SessionEvent::ChannelChanged(_)
            | SessionEvent::ChannelsChanged
            | SessionEvent::UserTopicChanged { .. }
            | SessionEvent::MessageAdded { .. }
            | SessionEvent::MessageUpdated { .. }
            | SessionEvent::MessageRemoved { .. }
            | SessionEvent::SettingsChanged(_)
    )
}

/// Category of the notice bar: errors use red text, information uses the secondary text color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BannerKind {
    Error,
    Info,
}

/// Content at the connection notice position: connection problems take priority, otherwise the
/// plugin's text is shown.
fn banner_text(
    connection: ConnectionState,
    plugin: Option<&PluginStatusText>,
) -> Option<(BannerKind, String)> {
    match connection {
        ConnectionState::Connected => {
            plugin.map(|p| (BannerKind::Info, plugin_text(&p.plugin_name, &p.text)))
        }
        ConnectionState::Reconnecting => {
            Some((BannerKind::Error, "连接已断开，正在重连…".to_owned()))
        }
        ConnectionState::AuthFailed => Some((BannerKind::Error, "登录已失效".to_owned())),
    }
}

#[component]
fn MessagesBanner(kind: BannerKind, text: String) -> Element {
    rsx! {
        div {
            class: match kind {
                BannerKind::Error => "messages-banner",
                BannerKind::Info => "messages-banner info",
            },
            "{text}"
        }
    }
}

#[component]
pub fn MessagesPage() -> Element {
    let state = use_app_state();
    let folder = use_signal(|| None::<ConversationFolder>);
    use_context_provider(|| FolderContext(folder));
    let mut connection = use_signal(|| {
        state
            .session
            .peek()
            .as_ref()
            .map_or(ConnectionState::Connected, |s| s.connection())
    });
    use_session_event(move |event| match event {
        SessionEvent::Connection(next) => connection.set(next),
        SessionEvent::Resynced => {
            if let Some(session) = state.session.peek().as_ref() {
                connection.set(session.connection());
            }
        }
        _ => {}
    });
    let mut plugin_status = use_signal(|| plugins().status_text());
    use_plugin_events(move |event| {
        if matches!(
            event,
            PluginEvent::StatusTextChanged | PluginEvent::PluginsChanged
        ) {
            plugin_status.set(plugins().status_text());
        }
    });
    let banner = banner_text(connection(), plugin_status.read().as_ref());
    rsx! {
        div { class: "messages-page",
            if let Some((kind, text)) = banner {
                MessagesBanner { kind, text }
            }
            ConversationList { folder: None }
            if let Some(current) = folder() {
                FolderView { key: "{current}", current, folder }
            }
        }
    }
}

/// Sub-list of a conversation folder: slides in above the main list.
#[component]
fn FolderView(current: ConversationFolder, folder: Signal<Option<ConversationFolder>>) -> Element {
    let title = folder_title(current);
    rsx! {
        div { class: "messages-folder",
            div { class: "messages-folder-head",
                IconButton {
                    icon: IconName::ChevronLeft,
                    size: 26.0,
                    onclick: move |_| folder.set(None),
                }
                span { class: "text-sub", "{title}" }
            }
            ConversationList { folder: Some(current) }
        }
    }
}

/// Context read by row components: rows do not re-render with the list, they only read their own
/// row.
#[derive(Clone, Copy)]
struct ListContext {
    data: Signal<RowsData>,
}

#[component]
fn ConversationList(folder: Option<ConversationFolder>) -> Element {
    let data = use_session_data(list_changed, move |session| {
        let rows = match folder {
            Some(folder) => session.folder_rows(folder),
            None => session.message_rows(),
        };
        rows_data(rows, session.use_24h())
    });
    use_context_provider(|| ListContext { data });
    let list = use_virtual_list(VirtualListConfig {
        estimate: 64.0,
        ..Default::default()
    });
    let keys = data.read().keys.clone();
    let order = keys.clone();
    let logging_in = use_logging_in();
    let loading = folder.is_none() && keys.is_empty() && logging_in();
    rsx! {
        if loading {
            div { class: "messages-loading",
                Icon { name: IconName::Loading, size: 22.0, class: "spin" }
            }
        }
        div { class: "messages-list",
            VirtualList {
                handle: list,
                keys,
                render: move |i: usize| rsx! {
                    ConversationRowItem { id: order[i] }
                },
            }
        }
    }
}

fn avatar_fill(entry: &ListEntry, color: &str) -> Option<String> {
    match entry {
        ListEntry::Folder(ConversationFolder::Helper) => Some("var(--helper-avatar)".to_owned()),
        ListEntry::Folder(ConversationFolder::Resolved) => {
            Some("var(--resolved-avatar)".to_owned())
        }
        ListEntry::Conversation(_) => (!color.is_empty()).then(|| color.to_owned()),
    }
}

#[component]
fn ConversationRowItem(id: u64) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let ui = use_main_ui();
    let context = use_context::<ListContext>();
    let mut folder = use_context::<FolderContext>().0;
    let data = context.data.read();
    let Some(row) = data.rows.get(&id).cloned() else {
        return rsx! {};
    };
    let use_24h = data.use_24h;
    drop(data);

    let selected =
        matches!(&*ui.right.read(), RightPane::Chat { key, .. } if row.key() == Some(key));
    let blocked = row.level == DndLevel::Blocked;
    let muted = row.level != DndLevel::Notify;
    let draft = row.draft_preview.is_some();
    let notice = if draft || blocked {
        None
    } else {
        notice_label(row.notice)
    };
    let time = from_unix(row.time)
        .filter(|_| row.time > 0)
        .map(|t| list_time(t, chrono::Local::now().naive_local(), use_24h));
    let subtitle = row
        .draft_preview
        .clone()
        .unwrap_or_else(|| row.preview.clone());
    let leading = match &row.avatar {
        ConversationAvatar::Image(url) => rsx! {
            RemoteAvatar { text: row.title.clone(), url: url.clone(), size: 44.0 }
        },
        ConversationAvatar::Text { text, color } => rsx! {
            Avatar { text: text.clone(), fill: avatar_fill(&row.entry, color), size: 44.0 }
        },
    };
    let entry = row.entry.clone();
    let hover_entry = row.entry.clone();
    let menu_row = row.clone();
    rsx! {
        ListRow {
            density: RowDensity::Large,
            leading,
            title: row.title.clone(),
            meta: time,
            subtitle,
            selected,
            pinned: row.pinned,
            subtitle_prefix: rsx! {
                if let Some(text) = notice {
                    span { class: "messages-notice", "{text}" }
                }
                if draft {
                    span { class: "messages-notice", "[草稿]" }
                }
            },
            trailing: rsx! {
                if blocked {
                    Icon { name: IconName::Forbid, size: 14.0 }
                } else if row.unread_count > 0 {
                    Badge { count: row.unread_count as u32, muted }
                } else if matches!(row.level, DndLevel::Silent | DndLevel::Assistant) {
                    Icon { name: IconName::NotificationOff, size: 14.0 }
                }
            },
            onclick: move |_| {
                match &entry {
                    ListEntry::Folder(target) => folder.set(Some(*target)),
                    ListEntry::Conversation(key) => {
                        // Clicking the conversation shown on the right again closes it
                        let open = matches!(&*ui.right.peek(), RightPane::Chat { key: open, .. } if open == key);
                        if open {
                            let mut right = ui.right;
                            right.set(RightPane::Empty);
                        } else if let Some(account) = *state.account.peek() {
                            windows::open_chat(account, key.clone(), None);
                        }
                    }
                }
            },
            onhover: move |entered: bool| {
                let target = match (&hover_entry, *state.account.peek()) {
                    (ListEntry::Conversation(key), Some(account)) if entered => {
                        Some(windows::WindowTarget::Chat { account, key: key.clone() })
                    }
                    _ => None,
                };
                windows::hover(target);
            },
            menu: move |_| {
                let Some(session) = state.session.peek().clone() else {
                    return Vec::new();
                };
                conversation_menu(&session, overlays, &menu_row, move |key| {
                    let open = matches!(&*ui.right.peek(), RightPane::Chat { key: open, .. } if open == key);
                    if open {
                        let mut right = ui.right;
                        right.set(RightPane::Empty);
                    }
                })
            },
        }
    }
}

/// The open conversation folder.
#[derive(Clone, Copy)]
struct FolderContext(Signal<Option<ConversationFolder>>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_problem_hides_plugin_text() {
        let status = PluginStatusText {
            plugin_name: "回声".to_owned(),
            text: "运行中".to_owned(),
        };
        assert_eq!(banner_text(ConnectionState::Connected, None), None);
        assert_eq!(
            banner_text(ConnectionState::Connected, Some(&status)),
            Some((BannerKind::Info, "回声：运行中".to_owned()))
        );
        assert_eq!(
            banner_text(ConnectionState::Reconnecting, Some(&status)),
            Some((BannerKind::Error, "连接已断开，正在重连…".to_owned()))
        );
        assert_eq!(
            banner_text(ConnectionState::AuthFailed, Some(&status)),
            Some((BannerKind::Error, "登录已失效".to_owned()))
        );
    }
}
