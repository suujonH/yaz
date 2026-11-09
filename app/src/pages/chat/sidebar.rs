//! Topic right sidebar: channel announcement, members. The member list and its ordering are also
//! used by the more panel.

use super::channel_dialogs::{can_manage_channel, edit_notice};
use super::message::ChannelNotice;
use super::store::Chat;
use crate::state::use_session_data;
use crate::ui::{
    IconButton, IconName, ListRow, Rect, RoleBadge, RowDensity, TextInput, Tooltip, VirtualList,
    VirtualListConfig, use_virtual_list,
};
use crate::widgets::{RemoteAvatar, open_user_card, plugin_context_menus};
use dioxus::prelude::*;
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::model::{Channel, PresenceState};
use zulip_core::permission::ROLE_ADMINISTRATOR;
use zulip_core::{ConversationKey, contains_ignore_case};
use zulip_plugin_host::ContextMenuTarget;
use zulip_session::{Session, SessionEvent};

const MEMBER_ROW_HEIGHT: f64 = 36.0;

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

#[component]
pub fn MemberRow(member: MemberItem) -> Element {
    let chat = use_context::<Chat>();
    let id = member.id;
    let badge = member.badge;
    let owner = member.owner;
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            leading: rsx! { RemoteAvatar { text: member.name.clone(), url: member.avatar.clone(), size: 24.0, online: member.online } },
            title: member.name.clone(),
            trailing: badge.map(|text| rsx! { RoleBadge { owner, "{text}" } }),
            onclick: move |e: MouseEvent| {
                let at = e.client_coordinates();
                open_user_card(chat.overlays, Rect { x: at.x, y: at.y, w: 4.0, h: 4.0 }, id);
            },
            menu: move |_| plugin_context_menus(&chat.session(), &ContextMenuTarget::User(id)),
        }
    }
}

#[component]
pub fn TopicSidebar() -> Element {
    let chat = use_context::<Chat>();
    let ids = use_channel_members(chat);
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
            div { class: "sidebar-members",
                div { class: "sidebar-title",
                    span { "频道成员 {count}" }
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
