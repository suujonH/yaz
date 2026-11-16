//! Right sidebar of a topic (channel announcement, members) or a group chat (members only). The member list and its ordering are also
//! used by the more panel.

use super::channel_dialogs::{can_manage_channel, edit_notice};
use super::message::ChannelNotice;
use super::store::Chat;
use crate::state::use_session_data;
use crate::ui::{
    IconButton, IconName, ListRow, MenuEntry, Placement, Rect, RoleBadge, RowDensity, TextInput,
    Tooltip, VirtualList, VirtualListConfig, client_rect, place, use_virtual_list, viewport,
};
use crate::widgets::{RemoteAvatar, UserCard, plugin_context_menus};
use crate::windows;
use dioxus::prelude::*;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use zulip_core::model::{Channel, PresenceState};
use zulip_core::permission::ROLE_ADMINISTRATOR;
use zulip_core::{ConversationKey, contains_ignore_case};
use zulip_plugin_host::ContextMenuTarget;
use zulip_session::{Session, SessionEvent};

const MEMBER_ROW_HEIGHT: f64 = 36.0;
/// The member card appears after the pointer rests on a member this long, and goes this long
/// after it leaves the member and the card.
const CARD_SHOW_DELAY: Duration = Duration::from_millis(1000);
const CARD_HIDE_DELAY: Duration = Duration::from_millis(500);
const CARD_GAP: f64 = 8.0;

#[derive(Clone, Debug, PartialEq)]
pub struct MemberItem {
    pub id: i64,
    pub name: String,
    pub avatar: Option<String>,
    pub email: String,
    pub badge: Option<&'static str>,
    pub owner: bool,
    pub online: bool,
}

/// Order: channel creator, administrators, online members, then by name within the same level.
pub fn member_rank(item: &MemberItem) -> (u8, u8) {
    let rank = if item.owner {
        0
    } else if item.badge.is_some() {
        1
    } else {
        2
    };
    (rank, u8::from(rank == 2 && !item.online))
}

pub fn build_members(
    session: &Arc<Session>,
    channel: Option<&Channel>,
    ids: &[i64],
) -> Vec<MemberItem> {
    let permissions = session.permissions();
    let mut items: Vec<MemberItem> = ids
        .iter()
        .filter_map(|id| session.user(*id))
        .map(|user| {
            let owner = channel.is_some_and(|c| c.creator_id == Some(user.id));
            let admin = channel.is_some_and(|c| {
                user.role <= ROLE_ADMINISTRATOR
                    || permissions
                        .user_allows(user.id, c.permissions.can_administer_channel_group.as_ref())
            });
            MemberItem {
                id: user.id,
                name: session.display_name(user.id),
                avatar: user.avatar_url.clone(),
                email: user.email.clone(),
                badge: if owner {
                    Some("频道创建者")
                } else if admin {
                    Some("管理员")
                } else {
                    None
                },
                owner,
                online: session.presence(user.id) != PresenceState::Offline,
            }
        })
        .collect();
    items.sort_by(|a, b| {
        member_rank(a)
            .cmp(&member_rank(b))
            .then_with(|| a.name.cmp(&b.name))
    });
    items
}

pub fn filter_members(items: &[MemberItem], query: &str) -> Vec<MemberItem> {
    let query = query.trim();
    items
        .iter()
        .filter(|m| contains_ignore_case(&m.name, query) || contains_ignore_case(&m.email, query))
        .cloned()
        .collect()
}

pub fn topic_stream(key: &ConversationKey) -> Option<i64> {
    match key {
        ConversationKey::Topic(t) => Some(t.stream_id),
        ConversationKey::Dm(_) => None,
    }
}

/// Channel announcement: channel id and the server-rendered HTML. `None` when there is no
/// announcement.
pub fn notice_of(channel: &Channel) -> Option<(i64, String)> {
    let html = &channel.rendered_description;
    (!html.trim().is_empty()).then(|| (channel.stream_id, html.clone()))
}

/// Members of the current channel.
pub fn use_channel_members(chat: Chat) -> Signal<Vec<i64>> {
    let mut ids = use_signal(Vec::new);
    use_effect(move || {
        let Some(stream) = topic_stream(&chat.key.read()) else {
            return;
        };
        spawn(async move {
            match chat.session().load_subscribers(stream).await {
                Ok(list) => ids.set(list),
                Err(error) => log::warn!("加载频道成员失败：{error}"),
            }
        });
    });
    ids
}

/// A member's card shown to the left of the member list. `pinned`: opened from "查看资料", it
/// stays when the pointer leaves (until another member's card shows or a click elsewhere).
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShownCard {
    /// Tells cards apart (a click elsewhere closes only the card it was waiting for).
    id: u64,
    user_id: i64,
    /// The member's row.
    anchor: Rect,
    pinned: bool,
}

/// The member card of the sidebar: which is shown, and the pending show / hide (each restarts
/// its wait when set again, and is dropped when set to `None`).
#[derive(Clone, Copy)]
struct MemberCards {
    shown: Signal<Option<ShownCard>>,
    next_id: Signal<u64>,
    /// The member and row the pointer rests on.
    pending_show: Signal<Option<(i64, Rect)>>,
    /// Counter: each leave starts the wait again.
    pending_hide: Signal<Option<u64>>,
}

impl MemberCards {
    fn show(mut self, user_id: i64, anchor: Rect, pinned: bool) {
        let id = *self.next_id.peek() + 1;
        self.next_id.set(id);
        self.pending_show.set(None);
        self.pending_hide.set(None);
        self.shown.set(Some(ShownCard {
            id,
            user_id,
            anchor,
            pinned,
        }));
    }

    /// The pointer entered a member's row: their card shows after a while (kept when already
    /// shown).
    fn enter_row(mut self, user_id: i64, anchor: Rect) {
        if self.shown.peek().is_some_and(|c| c.user_id == user_id) {
            self.pending_show.set(None);
            self.pending_hide.set(None);
        } else {
            self.pending_show.set(Some((user_id, anchor)));
        }
    }

    /// The pointer is over the card: it stays.
    fn enter_card(mut self) {
        self.pending_hide.set(None);
    }

    /// The pointer left a row or the card: nothing shows, and an unpinned card goes after a while.
    fn leave(mut self) {
        self.pending_show.set(None);
        let next = self.pending_hide.peek().map_or(0, |n| n + 1);
        self.pending_hide.set(Some(next));
    }

    fn close(mut self, id: u64) {
        if self.shown.peek().is_some_and(|c| c.id == id) {
            self.shown.set(None);
        }
    }
}

/// The member cards of a sidebar, with the waits for showing and hiding (in the sidebar's scope).
fn use_member_cards() -> MemberCards {
    let cards = MemberCards {
        shown: use_signal(|| None),
        next_id: use_signal(|| 0),
        pending_show: use_signal(|| None),
        pending_hide: use_signal(|| None),
    };
    use_context_provider(|| cards);
    // Each restarts when its signal changes (the earlier wait is dropped)
    use_resource(move || async move {
        let Some((user_id, anchor)) = (cards.pending_show)() else {
            return;
        };
        tokio::time::sleep(CARD_SHOW_DELAY).await;
        cards.show(user_id, anchor, false);
    });
    use_resource(move || async move {
        if (cards.pending_hide)().is_none() {
            return;
        }
        tokio::time::sleep(CARD_HIDE_DELAY).await;
        let mut shown = cards.shown;
        if shown.peek().is_some_and(|c| !c.pinned) {
            shown.set(None);
        }
        let mut pending = cards.pending_hide;
        pending.set(None);
    });
    // A mousedown anywhere but the card (and the menus / dropdowns / dialogs opened from it)
    // closes it
    use_resource(move || async move {
        let Some(id) = (cards.shown)().map(|c| c.id) else {
            return;
        };
        let mut eval = document::eval(
            r#"await new Promise((done) => {
                const inside = ".member-card, .popover-layer, .menu, .menu-bar, .dialog-layer";
                const down = (e) => {
                    if (e.target instanceof Element && e.target.closest(inside)) return;
                    document.removeEventListener("mousedown", down, true);
                    done();
                };
                document.addEventListener("mousedown", down, true);
            });
            dioxus.send(true);"#,
        );
        if eval.recv::<bool>().await.is_ok() {
            cards.close(id);
        }
    });
    cards
}

/// The shown member card, placed to the left of the member's row.
#[component]
fn MemberCard(card: ShownCard) -> Element {
    let cards = use_context::<MemberCards>();
    let mut pos = use_signal(|| None::<(f64, f64)>);
    let style = match pos() {
        Some((x, y)) => format!("left:{x}px;top:{y}px"),
        None => "left:0;top:0;visibility:hidden".to_owned(),
    };
    rsx! {
        div {
            class: "member-card scroll",
            style,
            onmounted: move |e: MountedEvent| async move {
                if let Some(rect) = client_rect(&e.data()).await {
                    pos.set(Some(place(card.anchor, (rect.w, rect.h), viewport(), Placement::LeftStart, CARD_GAP)));
                }
            },
            onmouseenter: move |_| cards.enter_card(),
            onmouseleave: move |_| cards.leave(),
            UserCard { user_id: card.user_id }
        }
    }
}

/// Context menu of a member: message them, mention them in the composer, their card (pinned),
/// then the plugins' items.
fn member_menu(chat: Chat, cards: MemberCards, user_id: i64, row: Option<Rect>) -> Vec<MenuEntry> {
    let session = chat.session();
    let name = session.display_name(user_id);
    let self_id = session.self_id();
    let mut items = vec![
        MenuEntry::item("发送消息", move || {
            windows::open_chat(chat.account, ConversationKey::dm([user_id], self_id), None);
        }),
        MenuEntry::item("@TA", move || {
            let mut insert = chat.composer.insert;
            insert.set(Some(format!("@**{name}|{user_id}** ")));
        }),
        MenuEntry::item("查看资料", move || {
            if let Some(anchor) = row {
                cards.show(user_id, anchor, true);
            }
        }),
    ];
    let plugin_items = plugin_context_menus(&session, &ContextMenuTarget::User(user_id));
    if !plugin_items.is_empty() {
        items.push(MenuEntry::Separator);
        items.extend(plugin_items);
    }
    items
}

/// A member: double-click opens the chat with them; resting on it shows their card on the left.
#[component]
pub fn MemberRow(member: MemberItem) -> Element {
    let chat = use_context::<Chat>();
    let cards = use_context::<MemberCards>();
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    // Where the row is, taken when the pointer enters it (the context menu places the card by it)
    let mut anchor = use_signal(|| None::<Rect>);
    let id = member.id;
    let badge = member.badge;
    let owner = member.owner;
    rsx! {
        div {
            class: "member-row",
            onmounted: move |e| mounted.set(Some(e.data())),
            onmouseenter: move |_| {
                let row = mounted.peek().clone();
                spawn(async move {
                    let Some(row) = row else { return };
                    if let Some(rect) = client_rect(&row).await {
                        anchor.set(Some(rect));
                        cards.enter_row(id, rect);
                    }
                });
            },
            onmouseleave: move |_| cards.leave(),
            ListRow {
                density: RowDensity::Compact,
                leading: rsx! { RemoteAvatar { text: member.name.clone(), url: member.avatar.clone(), size: 24.0, online: member.online } },
                title: member.name.clone(),
                trailing: badge.map(|text| rsx! { RoleBadge { owner, "{text}" } }),
                // Clicking does nothing (the row still shows hover and pressed)
                onclick: move |_| {},
                ondoubleclick: move |_| {
                    let self_id = chat.session().self_id();
                    windows::open_chat(chat.account, ConversationKey::dm([id], self_id), None);
                },
                menu: move |_| member_menu(chat, cards, id, *anchor.peek()),
            }
        }
    }
}

#[component]
pub fn TopicSidebar() -> Element {
    let chat = use_context::<Chat>();
    let cards = use_member_cards();
    let channel_ids = use_channel_members(chat);
    // Group chat: its participants, yourself included
    let self_id = chat.session().self_id();
    let ids = use_memo(move || match &*chat.key.read() {
        ConversationKey::Topic(_) => channel_ids(),
        ConversationKey::Dm(dm) => {
            let mut ids = dm.other_user_ids().to_vec();
            ids.push(self_id);
            ids
        }
    });
    let is_topic = matches!(*chat.key.read(), ConversationKey::Topic(_));
    let channel = use_session_data(
        |e| {
            matches!(
                e,
                SessionEvent::ChannelChanged(_) | SessionEvent::ChannelsChanged
            )
        },
        move |s| topic_stream(&chat.key.peek()).and_then(|id| s.channel(id)),
    );
    let can_edit = use_session_data(
        |e| {
            matches!(
                e,
                SessionEvent::ChannelChanged(_)
                    | SessionEvent::ChannelsChanged
                    | SessionEvent::PermissionsChanged
            )
        },
        move |s| {
            topic_stream(&chat.key.peek())
                .and_then(|id| s.channel(id))
                .is_some_and(|c| can_manage_channel(s, &c))
        },
    );
    let mut searching = use_signal(|| false);
    let mut query = use_signal(String::new);
    let session = chat.session();
    let members = use_memo(move || build_members(&session, channel.read().as_ref(), &ids.read()));
    let shown = use_memo(move || Rc::new(filter_members(&members.read(), &query.read())));
    let keys = use_memo(move || {
        Rc::new(
            shown
                .read()
                .iter()
                .map(|m| m.id as u64)
                .collect::<Vec<u64>>(),
        )
    });
    let handle = use_virtual_list(VirtualListConfig {
        estimate: MEMBER_ROW_HEIGHT,
        ..Default::default()
    });
    let notice = channel.read().as_ref().and_then(notice_of);
    let count = members.read().len();
    rsx! {
        div { class: "topic-sidebar",
            if let Some(card) = (cards.shown)() {
                MemberCard { key: "{card.id}", card }
            }
            if is_topic {
            div { class: "sidebar-section",
                div { class: "sidebar-title",
                    span { "频道公告" }
                    if can_edit() {
                        Tooltip { text: "编辑",
                            IconButton {
                                icon: IconName::Edit,
                                size: 22.0,
                                icon_size: 13.0,
                                onclick: move |_| {
                                    if let Some(channel) = channel.peek().clone() {
                                        edit_notice(chat, channel);
                                    }
                                },
                            }
                        }
                    }
                }
                match notice {
                    Some((stream_id, html)) => rsx! {
                        div { class: "sidebar-notice scroll",
                            ChannelNotice { stream_id, html }
                        }
                    },
                    None => rsx! { div { class: "sidebar-empty", "暂无公告" } },
                }
            }
            }
            div { class: "sidebar-members",
                div { class: "sidebar-title",
                    span { if is_topic { "频道成员 {count}" } else { "群成员 {count}" } }
                    IconButton {
                        icon: IconName::Search,
                        size: 22.0,
                        icon_size: 13.0,
                        onclick: move |_| {
                            searching.toggle();
                            query.set(String::new());
                        },
                    }
                }
                if searching() {
                    div { class: "sidebar-search",
                        TextInput {
                            value: query(),
                            placeholder: "搜索成员",
                            autofocus: true,
                            clearable: true,
                            oninput: move |v| query.set(v),
                        }
                    }
                }
                VirtualList {
                    handle,
                    keys: keys(),
                    class: "sidebar-list",
                    render: move |i: usize| {
                        let member = shown.peek().get(i).cloned();
                        rsx! {
                            if let Some(member) = member {
                                MemberRow { member }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, badge: Option<&'static str>, owner: bool, online: bool) -> MemberItem {
        MemberItem {
            id: 1,
            name: name.into(),
            avatar: None,
            email: format!("{name}@x"),
            badge,
            owner,
            online,
        }
    }

    #[test]
    fn notice_is_absent_when_the_description_is_blank() {
        let channel = |html: &str| Channel {
            stream_id: 7,
            rendered_description: html.to_owned(),
            ..Channel::default()
        };
        assert_eq!(notice_of(&channel("  \n")), None);
        assert_eq!(
            notice_of(&channel("<p>a</p>")),
            Some((7, "<p>a</p>".to_owned()))
        );
    }

    #[test]
    fn rank_orders_owner_admin_online_offline() {
        let owner = member_rank(&member("a", Some("频道创建者"), true, false));
        let admin = member_rank(&member("b", Some("管理员"), false, false));
        let online = member_rank(&member("c", None, false, true));
        let offline = member_rank(&member("d", None, false, false));
        assert!(owner < admin && admin < online && online < offline);
    }

    #[test]
    fn filter_matches_name_and_email() {
        let items = vec![
            member("张三", None, false, true),
            member("Li", None, false, true),
        ];
        assert_eq!(filter_members(&items, "li").len(), 1);
        assert_eq!(filter_members(&items, "张三@").len(), 1);
        assert_eq!(filter_members(&items, " ").len(), 2);
    }
}
