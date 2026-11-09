//! 消息页和联系人页的分组、排序规则。输入是当前状态，输出是界面直接显示的数据。

use std::cmp::Reverse;

use zulip_core::markdown;
use zulip_core::model::{ConversationNotice, PinItem, PinScope, PresenceState, TopicInfo};
use zulip_core::push::DndLevel;
use zulip_core::{ConversationFolder, ConversationKey, ListEntry};

use crate::notify::{GROUP_AVATAR_COLOR, GROUP_AVATAR_TEXT};
use crate::state::{State, folder_title};

/// 会话的头像。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationAvatar {
    Image(Option<String>),
    Text { text: String, color: String },
}

/// 消息页的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationRow {
    pub entry: ListEntry,
    pub title: String,
    /// 带发送者前缀的预览。
    pub preview: String,
    /// 有草稿时的草稿预览，优先于 `preview` 显示。
    pub draft_preview: Option<String>,
    /// Unix 秒。
    pub time: i64,
    pub unread_count: usize,
    pub notice: ConversationNotice,
    pub pinned: bool,
    pub level: DndLevel,
    pub avatar: ConversationAvatar,
}

impl ConversationRow {
    /// 这一行的会话；会话夹为 `None`。
    pub fn key(&self) -> Option<&ConversationKey> {
        self.entry.conversation()
    }
}

/// 未读数的显示文字，超过 99 为“99+”，0 为 `None`。
pub fn badge_text(count: usize) -> Option<String> {
    match count {
        0 => None,
        1..=99 => Some(count.to_string()),
        _ => Some("99+".to_owned()),
    }
}

fn avatar_of(state: &State, key: &ConversationKey, title: &str) -> ConversationAvatar {
    match key {
        ConversationKey::Topic(t) => ConversationAvatar::Text {
            text: title.to_owned(),
            color: state
                .channels
                .get(t.stream_id)
                .map(|c| c.color.clone())
                .unwrap_or_default(),
        },
        ConversationKey::Dm(d) if d.is_group_dm() => ConversationAvatar::Text {
            text: GROUP_AVATAR_TEXT.to_owned(),
            color: GROUP_AVATAR_COLOR.to_owned(),
        },
        ConversationKey::Dm(d) => {
            let id = d
                .other_user_ids()
                .first()
                .copied()
                .unwrap_or(state.users.self_id);
            ConversationAvatar::Image(state.users.get(id).and_then(|u| u.avatar_url.clone()))
        }
    }
}

fn preview_with_sender(
    state: &State,
    key: &ConversationKey,
    sender_id: i64,
    preview: &str,
) -> String {
    let needs_prefix = match key {
        ConversationKey::Topic(_) => true,
        ConversationKey::Dm(d) => d.is_group_dm(),
    };
    if needs_prefix && sender_id != state.users.self_id && sender_id != 0 && !preview.is_empty() {
        format!("{}：{preview}", state.display_name(sender_id))
    } else {
        preview.to_owned()
    }
}

/// 草稿的单行预览。
pub fn draft_preview(state: &State, content: &str) -> String {
    let emoji = state.emoji.clone();
    markdown::to_preview(content, state, &move |name: &str| {
        emoji.get(name).and_then(|e| e.text.clone())
    })
}

fn row_of(state: &State, key: &ConversationKey, in_folder: bool) -> Option<ConversationRow> {
    let entry = state.conversations.get(key)?;
    let title = state.display_title(key, false);
    let draft = state
        .drafts
        .get(key)
        .filter(|_| !in_folder && !state.messages.is_open(key))
        .map(|d| draft_preview(state, &d.content));
    let avatar_title = state.title(key);
    Some(ConversationRow {
        entry: ListEntry::Conversation(key.clone()),
        preview: preview_with_sender(state, key, entry.last_sender_id, &entry.last_preview),
        draft_preview: draft,
        time: entry.last_message_time,
        unread_count: state.unread.count(key),
        notice: state.unread.notice(key),
        pinned: state
            .pins
            .get(PinScope::Messages, &PinItem::Conversation(key.clone()))
            .is_some(),
        level: state.conversations.level(key),
        avatar: avatar_of(state, key, &avatar_title),
        title,
    })
}

fn sort_rows(state: &State, rows: &mut [ConversationRow]) {
    let pinned_at = |row: &ConversationRow| {
        state
            .pins
            .get(PinScope::Messages, &PinItem::from(&row.entry))
    };
    rows.sort_by(|a, b| {
        let (pa, pb) = (pinned_at(a), pinned_at(b));
        pb.is_some()
            .cmp(&pa.is_some())
            .then(pb.cmp(&pa))
            .then(b.time.cmp(&a.time))
            .then_with(|| a.entry.to_string().cmp(&b.entry.to_string()))
    });
}

/// 这个会话归入的会话夹；`None` 表示在主列表。
fn folder_of(state: &State, key: &ConversationKey) -> Option<ConversationFolder> {
    if state.local.collapse_resolved_topics() && state.conversations.is_resolved(key) {
        Some(ConversationFolder::Resolved)
    } else if state.conversations.level(key) == DndLevel::Assistant {
        Some(ConversationFolder::Helper)
    } else {
        None
    }
}

fn listed_keys(state: &State) -> Vec<ConversationKey> {
    state
        .conversations
        .iter()
        .filter(|(_, e)| e.in_list)
        .map(|(key, _)| key.clone())
        .collect()
}

fn folder_row(
    state: &State,
    folder: ConversationFolder,
    rows: &[ConversationRow],
) -> ConversationRow {
    let title = folder_title(folder).to_owned();
    let latest = rows.iter().max_by_key(|r| r.time);
    let unread_topics = rows.iter().filter(|r| r.unread_count > 0).count();
    let preview = if unread_topics > 0 {
        format!("{unread_topics}个话题有新消息")
    } else {
        latest
            .map(|r| format!("{}：{}", r.title, r.preview))
            .unwrap_or_default()
    };
    let notice = if rows.iter().any(|r| r.notice == ConversationNotice::AtMe) {
        ConversationNotice::AtMe
    } else if rows.iter().any(|r| r.notice == ConversationNotice::AtAll) {
        ConversationNotice::AtAll
    } else {
        ConversationNotice::None
    };
    ConversationRow {
        preview,
        draft_preview: None,
        time: latest.map_or(0, |r| r.time),
        unread_count: 0,
        notice,
        pinned: state
            .pins
            .get(PinScope::Messages, &PinItem::Folder(folder))
            .is_some(),
        level: DndLevel::Notify,
        avatar: ConversationAvatar::Text {
            text: title.clone(),
            color: String::new(),
        },
        entry: ListEntry::Folder(folder),
        title,
    }
}

/// 消息页的主列表：普通会话加上有内容的会话夹。置顶在前（后置顶的在上），然后按时间倒序。
pub fn message_rows(state: &State) -> Vec<ConversationRow> {
    let mut main = Vec::new();
    let mut resolved = Vec::new();
    let mut helper = Vec::new();
    for key in listed_keys(state) {
        match folder_of(state, &key) {
            None => main.extend(row_of(state, &key, false)),
            Some(ConversationFolder::Resolved) => resolved.extend(row_of(state, &key, true)),
            Some(ConversationFolder::Helper) => helper.extend(row_of(state, &key, true)),
        }
    }
    if !resolved.is_empty() {
        main.push(folder_row(state, ConversationFolder::Resolved, &resolved));
    }
    if !helper.is_empty() {
        main.push(folder_row(state, ConversationFolder::Helper, &helper));
    }
    sort_rows(state, &mut main);
    main
}

/// 会话夹里的会话。
pub fn folder_rows(state: &State, folder: ConversationFolder) -> Vec<ConversationRow> {
    let mut rows: Vec<ConversationRow> = listed_keys(state)
        .into_iter()
        .filter(|key| folder_of(state, key) == Some(folder))
        .filter_map(|key| row_of(state, &key, false))
        .collect();
    sort_rows(state, &mut rows);
    rows
}

/// 联系人页的好友。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendItem {
    pub user_id: i64,
    pub display_name: String,
    pub name_with_original: String,
    pub avatar_url: Option<String>,
    pub presence: PresenceState,
    pub status_text: Option<String>,
    pub is_bot: bool,
    pub pinned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FriendSectionKind {
    Pinned,
    All,
    Group(i64),
    Bots,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendSection {
    pub kind: FriendSectionKind,
    pub title: String,
    /// “在线数/总数”；机器人分组只有总数。
    pub count_text: String,
    pub items: Vec<FriendItem>,
}

fn friend_item(state: &State, user_id: i64, now: i64) -> Option<FriendItem> {
    let user = state.users.get(user_id)?;
    Some(FriendItem {
        user_id,
        display_name: state.display_name(user_id),
        name_with_original: state.name_with_original(user_id),
        avatar_url: user.avatar_url.clone(),
        presence: state.users.presence(user_id, now),
        status_text: state.users.status_text(user_id).map(str::to_owned),
        is_bot: user.is_bot,
        pinned: state
            .pins
            .get(PinScope::Contacts, &PinItem::User(user_id))
            .is_some(),
    })
}

fn sorted_section(
    kind: FriendSectionKind,
    title: &str,
    mut items: Vec<FriendItem>,
) -> FriendSection {
    items.sort_by(|a, b| {
        b.presence
            .cmp(&a.presence)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    let online = items
        .iter()
        .filter(|i| i.presence != PresenceState::Offline)
        .count();
    FriendSection {
        kind,
        title: title.to_owned(),
        count_text: if kind == FriendSectionKind::Bots {
            items.len().to_string()
        } else {
            format!("{online}/{}", items.len())
        },
        items,
    }
}

/// 好友页：置顶好友、我的好友、自定义分组、机器人。没有置顶时不显示置顶分组。
pub fn friend_sections(state: &State, now: i64) -> Vec<FriendSection> {
    let self_id = state.users.self_id;
    let bot_id = state.users.notification_bot_id;
    let humans: Vec<i64> = state
        .users
        .all()
        .filter(|u| u.is_active && !u.is_bot && u.id != self_id)
        .map(|u| u.id)
        .collect();
    let items_of = |ids: &[i64]| -> Vec<FriendItem> {
        ids.iter()
            .filter_map(|id| friend_item(state, *id, now))
            .collect()
    };

    let mut sections = Vec::new();
    let pinned: Vec<i64> = state
        .pins
        .items(PinScope::Contacts)
        .into_iter()
        .filter_map(|item| match item {
            PinItem::User(id) => Some(*id),
            _ => None,
        })
        .filter(|id| state.users.get(*id).is_some_and(|u| u.is_active))
        .collect();
    if !pinned.is_empty() {
        // 置顶好友保持置顶顺序
        let items = items_of(&pinned);
        let mut section = sorted_section(FriendSectionKind::Pinned, "置顶好友", items.clone());
        section.items = items;
        sections.push(section);
    }
    sections.push(sorted_section(
        FriendSectionKind::All,
        "我的好友",
        items_of(&humans),
    ));
    for group in state.contacts.groups() {
        let members: Vec<i64> = humans
            .iter()
            .copied()
            .filter(|id| state.contacts.group_of(*id) == Some(group.id))
            .collect();
        sections.push(sorted_section(
            FriendSectionKind::Group(group.id),
            &group.name,
            items_of(&members),
        ));
    }
    let bots: Vec<i64> = state
        .users
        .all()
        .filter(|u| u.is_active && u.is_bot && Some(u.id) != bot_id)
        .map(|u| u.id)
        .collect();
    sections.push(sorted_section(
        FriendSectionKind::Bots,
        "机器人",
        items_of(&bots),
    ));
    sections
}

/// 联系人页的频道。话题按需用 `Session::load_topics` 加载，`topics` 为 `None` 表示还没有加载。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSection {
    pub stream_id: i64,
    pub name: String,
    pub name_with_original: String,
    pub color: String,
    pub locked: bool,
    pub dimmed: bool,
    pub topics: Option<Vec<TopicInfo>>,
}

/// 置顶话题。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedTopicRow {
    pub key: ConversationKey,
    pub title: String,
}

/// 置顶话题：只保留已订阅频道中的。
pub fn pinned_topic_rows(state: &State) -> Vec<PinnedTopicRow> {
    state
        .pins
        .items(PinScope::Contacts)
        .into_iter()
        .filter_map(|item| match item {
            PinItem::Conversation(key @ ConversationKey::Topic(t))
                if state
                    .channels
                    .get(t.stream_id)
                    .is_some_and(|c| c.is_subscribed()) =>
            {
                Some(PinnedTopicRow {
                    title: state.display_title(key, false),
                    key: key.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

/// 已订阅且未归档的频道，按名称排序。
pub fn channel_sections(state: &State) -> Vec<ChannelSection> {
    let mut sections: Vec<ChannelSection> = state
        .channels
        .subscribed()
        .into_iter()
        .filter(|c| !c.is_archived)
        .map(|c| ChannelSection {
            stream_id: c.stream_id,
            name: state.channel_display_name(c.stream_id),
            name_with_original: zulip_core::text::remark::with_original(
                state.remarks.of_channel(c.stream_id),
                &c.name,
            ),
            color: c.color.clone(),
            locked: c.invite_only,
            dimmed: c.is_muted(),
            topics: state
                .channels
                .topics(c.stream_id)
                .map(<[TopicInfo]>::to_vec),
        })
        .collect();
    sections.sort_by_key(|s| (s.name.clone(), Reverse(s.stream_id)));
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::conversations::Latest;
    use crate::state::test_support::*;
    use zulip_core::model::ChatMessage;

    fn add(state: &mut State, key: &ConversationKey, id: i64, sender: i64, title: &str) {
        state.conversations.apply_latest(
            key,
            &Latest {
                id,
                time: id,
                sender_id: sender,
                preview: format!("消息{id}"),
                title: title.to_owned(),
            },
        );
    }

    #[test]
    fn badge() {
        assert_eq!(badge_text(0), None);
        assert_eq!(badge_text(99).as_deref(), Some("99"));
        assert_eq!(badge_text(100).as_deref(), Some("99+"));
    }

    #[test]
    fn rows_sort_pinned_then_time() {
        let mut s = state();
        let (a, b, c) = (
            ConversationKey::topic(10, "a"),
            ConversationKey::topic(10, "b"),
            ConversationKey::dm([2], 1),
        );
        add(&mut s, &a, 1, 2, "a");
        add(&mut s, &b, 2, 2, "b");
        add(&mut s, &c, 3, 2, "");
        let keys = |s: &State| -> Vec<ConversationKey> {
            message_rows(s)
                .iter()
                .filter_map(|r| r.key().cloned())
                .collect()
        };
        assert_eq!(keys(&s), [c.clone(), b.clone(), a.clone()]);
        s.pins.set(
            PinScope::Messages,
            &PinItem::Conversation(a.clone()),
            true,
            5,
        );
        s.pins.set(
            PinScope::Messages,
            &PinItem::Conversation(b.clone()),
            true,
            6,
        );
        assert_eq!(keys(&s), [b, a, c]);
    }

    #[test]
    fn preview_prefix_only_for_others_in_shared_conversations() {
        let mut s = state();
        let topic = ConversationKey::topic(10, "a");
        add(&mut s, &topic, 1, 2, "a");
        assert_eq!(message_rows(&s)[0].preview, "阿杰：消息1");
        add(&mut s, &topic, 2, 1, "a");
        assert_eq!(message_rows(&s)[0].preview, "消息2");
        let dm = ConversationKey::dm([2], 1);
        add(&mut s, &dm, 3, 2, "");
        assert_eq!(message_rows(&s)[0].preview, "消息3");
        let group = ConversationKey::dm([2, 3], 1);
        add(&mut s, &group, 4, 3, "");
        assert_eq!(message_rows(&s)[0].preview, "小雨：消息4");
    }

    #[test]
    fn folders_collect_assistant_and_resolved() {
        let mut s = state();
        let (plain, helper, done) = (
            ConversationKey::topic(10, "普通"),
            ConversationKey::topic(10, "助手"),
            ConversationKey::topic(10, "✔ 完成"),
        );
        add(&mut s, &plain, 1, 2, "普通");
        add(&mut s, &helper, 2, 2, "助手");
        add(&mut s, &done, 3, 2, "✔ 完成");
        s.conversations.set_level(&helper, DndLevel::Assistant);
        s.local.set_collapse_resolved_topics(true);
        let mut m = ChatMessage::new(2, helper.clone(), 2, 2);
        m.flags.mentioned = true;
        s.unread.add_message(&m);

        let rows = message_rows(&s);
        let entries: Vec<_> = rows.iter().map(|r| r.entry.clone()).collect();
        assert_eq!(
            entries,
            [
                ListEntry::Folder(ConversationFolder::Resolved),
                ListEntry::Folder(ConversationFolder::Helper),
                ListEntry::Conversation(plain)
            ]
        );
        let folder = &rows[1];
        assert_eq!(folder.preview, "1个话题有新消息");
        assert_eq!(folder.unread_count, 0);
        assert_eq!(folder.notice, ConversationNotice::AtMe);
        assert!(rows[0].preview.contains("完成"));
        assert_eq!(
            folder_rows(&s, ConversationFolder::Helper)[0].key(),
            Some(&helper)
        );
        assert_eq!(
            folder_rows(&s, ConversationFolder::Resolved)[0].key(),
            Some(&done)
        );
        s.pins.set(
            PinScope::Messages,
            &PinItem::Folder(ConversationFolder::Helper),
            true,
            1,
        );
        let rows = message_rows(&s);
        assert!(rows[0].pinned);
        assert_eq!(rows[0].entry, ListEntry::Folder(ConversationFolder::Helper));
    }

    #[test]
    fn draft_preview_hidden_while_open() {
        let mut s = state();
        let key = ConversationKey::topic(10, "a");
        add(&mut s, &key, 1, 2, "a");
        s.drafts.set_visible(&key, "**草稿**", 1);
        assert_eq!(message_rows(&s)[0].draft_preview.as_deref(), Some("草稿"));
        s.set_open(&key, true);
        assert_eq!(message_rows(&s)[0].draft_preview, None);
    }

    #[test]
    fn avatars() {
        let mut s = state();
        let (topic, group, dm) = (
            ConversationKey::topic(10, "主题"),
            ConversationKey::dm([2, 3], 1),
            ConversationKey::dm([2], 1),
        );
        add(&mut s, &topic, 1, 2, "主题");
        add(&mut s, &group, 2, 2, "");
        add(&mut s, &dm, 3, 2, "");
        let rows = message_rows(&s);
        assert_eq!(
            rows[0].avatar,
            ConversationAvatar::Image(Some("/a2.png".into()))
        );
        assert_eq!(
            rows[1].avatar,
            ConversationAvatar::Text {
                text: "群".into(),
                color: "#0052D9".into()
            }
        );
        assert_eq!(
            rows[2].avatar,
            ConversationAvatar::Text {
                text: "主题".into(),
                color: "#123456".into()
            }
        );
    }

    #[test]
    fn friend_sections_group_and_sort() {
        let mut s = state();
        s.users.apply_presences(
            &std::iter::once((
                3,
                zulip_api::PresenceDto {
                    active_timestamp: 1000,
                    idle_timestamp: 1000,
                },
            ))
            .collect(),
        );
        let group = zulip_core::model::FriendGroup {
            id: 77,
            name: "同事".to_owned(),
        };
        s.contacts.push_group(group.clone());
        s.contacts.move_users(&[2], Some(group.id)).unwrap();
        s.pins.set(PinScope::Contacts, &PinItem::User(4), true, 1);

        let sections = friend_sections(&s, 1000);
        let kinds: Vec<_> = sections.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            [
                FriendSectionKind::Pinned,
                FriendSectionKind::All,
                FriendSectionKind::Group(group.id),
                FriendSectionKind::Bots
            ]
        );
        assert_eq!(sections[0].items[0].user_id, 4);
        let all = &sections[1];
        assert_eq!(all.count_text, "1/3");
        assert_eq!(all.items[0].user_id, 3);
        assert_eq!(sections[2].count_text, "0/1");
        // 通知机器人不显示
        assert!(sections[3].items.is_empty());
        assert_eq!(sections[3].count_text, "0");
    }

    #[test]
    fn channel_sections_and_pinned_topics() {
        let mut s = state();
        s.remarks
            .set(&zulip_core::model::RemarkTarget::Channel(10), Some("备注"));
        let sections = channel_sections(&s);
        let names: Vec<_> = sections
            .iter()
            .map(|c| c.name_with_original.as_str())
            .collect();
        assert!(names.contains(&"备注(开发)"));
        assert!(sections.iter().all(|c| c.topics.is_none()));

        let key = ConversationKey::topic(10, "a");
        s.pins.set(
            PinScope::Contacts,
            &PinItem::Conversation(key.clone()),
            true,
            1,
        );
        s.pins.set(
            PinScope::Contacts,
            &PinItem::Conversation(ConversationKey::topic(99, "x")),
            true,
            2,
        );
        let pinned = pinned_topic_rows(&s);
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].key, key);
    }
}
