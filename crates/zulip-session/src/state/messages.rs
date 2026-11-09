use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use zulip_core::ConversationKey;
use zulip_core::html::encode;
use zulip_core::model::{ChatMessage, LocalEcho, MessageFlags, MessageState, Reaction, Submessage};
use zulip_core::notice::SystemNoticeKind;
use zulip_store::{DeletedMessage, DeletedMessageRepo};

const MAX_TIMELINES: usize = 30;
const MAX_INDEXED: usize = 5000;
const PRUNED_INDEXED: usize = 4000;
/// 缓存至少有这么多条才算够打开会话时直接显示。
pub const ENOUGH_CACHED: usize = 30;

/// 一个会话已加载的窗口：消息按 id 升序，本地回显（负 id）排在最后。
#[derive(Clone, Default)]
pub struct Timeline {
    pub messages: Vec<Arc<ChatMessage>>,
    pub found_oldest: bool,
    pub found_newest: bool,
    pub version: u64,
    last_access: u64,
}

impl Timeline {
    fn first_id(&self) -> Option<i64> {
        self.messages.iter().map(|m| m.id).find(|id| *id > 0)
    }

    fn last_id(&self) -> Option<i64> {
        self.messages.iter().rev().map(|m| m.id).find(|id| *id > 0)
    }

    fn sort(&mut self) {
        self.messages.sort_by_key(|m| (m.id < 0, m.id.abs()));
    }

    pub fn has_enough_cached(&self) -> bool {
        self.found_newest && (self.messages.len() >= ENOUGH_CACHED || self.found_oldest)
    }
}

/// 分页方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    Newest,
    Older,
    Newer,
    Around,
}

/// `update_message` 事件中的内容修改。
pub struct ContentEdit<'a> {
    pub id: i64,
    pub html: Option<&'a str>,
    pub flags: Option<&'a [String]>,
    pub edit_timestamp: Option<i64>,
}

pub struct MessageCache {
    timelines: HashMap<ConversationKey, Timeline>,
    index: HashMap<i64, ConversationKey>,
    /// 打开着的会话及窗口数。
    open: HashMap<ConversationKey, usize>,
    clock: u64,
    /// 本机保存的已删除消息，打开数据库时读入内存。
    deleted: HashMap<ConversationKey, Vec<DeletedMessage>>,
    deleted_repo: DeletedMessageRepo,
    /// 被插件拦截的消息（只在本次运行内记住，重新拉取的消息也不显示）。
    hidden: HashSet<i64>,
    account_id: i64,
    next_local_id: i64,
}

impl MessageCache {
    pub fn new(
        deleted_repo: DeletedMessageRepo,
        stored_deleted: Vec<DeletedMessage>,
        account_id: i64,
    ) -> Self {
        let mut deleted: HashMap<ConversationKey, Vec<DeletedMessage>> = HashMap::new();
        for row in stored_deleted {
            if let Ok(key) = row.conv_key.parse() {
                deleted.entry(key).or_default().push(row);
            }
        }
        Self {
            timelines: HashMap::new(),
            index: HashMap::new(),
            open: HashMap::new(),
            clock: 0,
            deleted,
            deleted_repo,
            hidden: HashSet::new(),
            account_id,
            next_local_id: 0,
        }
    }

    /// 重新注册后接管上一个缓存中与消息内容无关的状态。
    pub fn inherit(&mut self, previous: MessageCache) {
        self.open = previous.open;
        self.hidden = previous.hidden;
        self.next_local_id = previous.next_local_id;
    }

    /// 不在聊天中显示：已归入系统通知，或被插件拦截。
    pub fn is_hidden(&self, message: &ChatMessage) -> bool {
        message.is_system_notice() || self.hidden.contains(&message.id)
    }

    /// 插件拦截这条消息。
    pub fn hide(&mut self, id: i64) {
        self.hidden.insert(id);
    }

    pub fn clear(&mut self) {
        self.timelines.clear();
        self.index.clear();
    }

    pub fn is_open(&self, key: &ConversationKey) -> bool {
        self.open.contains_key(key)
    }

    /// 同一会话可以开多个窗口。返回这个会话是否从关闭变为打开或从打开变为全部关闭。
    pub fn set_open(&mut self, key: &ConversationKey, open: bool) -> bool {
        if open {
            let count = self.open.entry(key.clone()).or_default();
            *count += 1;
            *count == 1
        } else {
            match self.open.get_mut(key) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => {
                    self.open.remove(key);
                    true
                }
                None => false,
            }
        }
    }

    /// 话题改名后打开的窗口跟着走。
    pub fn rekey_open(&mut self, old: &ConversationKey, new: &ConversationKey) {
        if let Some(count) = self.open.remove(old) {
            *self.open.entry(new.clone()).or_default() += count;
        }
    }

    pub fn timeline(&self, key: &ConversationKey) -> Option<&Timeline> {
        self.timelines.get(key)
    }

    pub fn get(&self, id: i64) -> Option<&Arc<ChatMessage>> {
        let key = self.index.get(&id)?;
        self.timelines
            .get(key)?
            .messages
            .iter()
            .find(|m| m.id == id)
    }

    /// 写入一页。`Newest` / `Around` 与现有窗口不相连时替换窗口，本地回显总是保留。
    pub fn merge_page(
        &mut self,
        key: &ConversationKey,
        page: Vec<ChatMessage>,
        kind: PageKind,
        found_oldest: bool,
        found_newest: bool,
    ) {
        self.clock += 1;
        let clock = self.clock;
        let timeline = self.timelines.entry(key.clone()).or_default();
        timeline.last_access = clock;
        timeline.version += 1;

        let page_first = page.first().map(|m| m.id);
        let page_last = page.last().map(|m| m.id);
        let connected = match (
            page_first,
            page_last,
            timeline.first_id(),
            timeline.last_id(),
        ) {
            (Some(pf), Some(pl), Some(tf), Some(tl)) => {
                pf <= tl + 1 && tf <= pl + 1 || overlaps(&page, timeline)
            }
            _ => false,
        };
        if matches!(kind, PageKind::Newest | PageKind::Around) && !connected {
            let old: Vec<i64> = timeline
                .messages
                .iter()
                .filter(|m| m.id > 0)
                .map(|m| m.id)
                .collect();
            for id in old {
                self.index.remove(&id);
            }
            timeline.messages.retain(|m| m.id < 0);
            timeline.found_oldest = false;
            timeline.found_newest = false;
        }

        let mut by_id: HashMap<i64, Arc<ChatMessage>> =
            timeline.messages.drain(..).map(|m| (m.id, m)).collect();
        for message in page {
            self.index.insert(message.id, key.clone());
            by_id.insert(message.id, Arc::new(message));
        }
        timeline.messages = by_id.into_values().collect();
        timeline.sort();
        timeline.found_oldest |= found_oldest;
        timeline.found_newest |= found_newest;
        self.merge_deleted(key);
        self.evict();
    }

    /// 把本机保存的已删除消息放回窗口范围内。
    fn merge_deleted(&mut self, key: &ConversationKey) {
        let (Some(rows), Some(timeline)) = (self.deleted.get(key), self.timelines.get_mut(key))
        else {
            return;
        };
        let lower = if timeline.found_oldest {
            i64::MIN
        } else {
            timeline.first_id().unwrap_or(i64::MAX)
        };
        let upper = if timeline.found_newest {
            i64::MAX
        } else {
            timeline.last_id().unwrap_or(i64::MIN)
        };
        let mut added = false;
        for row in rows {
            if row.message_id < lower
                || row.message_id > upper
                || timeline.messages.iter().any(|m| m.id == row.message_id)
            {
                continue;
            }
            self.index.insert(row.message_id, key.clone());
            timeline
                .messages
                .push(Arc::new(message_from_deleted(row, key)));
            added = true;
        }
        if added {
            timeline.sort();
        }
    }

    fn evict(&mut self) {
        while self.timelines.len() > MAX_TIMELINES {
            let oldest = self
                .timelines
                .iter()
                .filter(|(key, _)| !self.open.contains_key(*key))
                .min_by_key(|(_, t)| t.last_access)
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else { break };
            self.remove_timeline(&key);
        }
        if self.index.len() > MAX_INDEXED {
            let mut keys: Vec<(u64, ConversationKey)> = self
                .timelines
                .iter()
                .filter(|(key, _)| !self.open.contains_key(*key))
                .map(|(key, t)| (t.last_access, key.clone()))
                .collect();
            keys.sort_by_key(|(at, _)| *at);
            for (_, key) in keys {
                if self.index.len() <= PRUNED_INDEXED {
                    break;
                }
                self.remove_timeline(&key);
            }
        }
    }

    fn remove_timeline(&mut self, key: &ConversationKey) {
        if let Some(timeline) = self.timelines.remove(key) {
            for message in timeline.messages {
                self.index.remove(&message.id);
            }
        }
    }

    /// 新消息。窗口未到最新时不加入（不相连），`local_id` 对应的本地回显总是被替换。
    /// 返回 `(是否进入时间线, 被替换的本地回显 id)`。
    pub fn on_new_message(
        &mut self,
        message: ChatMessage,
        local_id: Option<&str>,
    ) -> (bool, Option<i64>) {
        let key = message.key.clone();
        let id = message.id;
        let Some(timeline) = self.timelines.get_mut(&key) else {
            return (false, None);
        };
        timeline.version += 1;
        let mut replaced = None;
        if let Some(local) = local_id
            && let Some(pos) = timeline
                .messages
                .iter()
                .position(|m| m.local_echo().is_some_and(|e| e.local_id == local))
        {
            replaced = Some(timeline.messages[pos].id);
            timeline.messages.remove(pos);
        }
        // 比窗口还旧的事件（加载期间到达）会在时间线中造成空洞
        let before_window = !timeline.found_oldest && timeline.first_id().is_some_and(|f| id < f);
        if !timeline.found_newest || before_window || timeline.messages.iter().any(|m| m.id == id) {
            return (false, replaced);
        }
        timeline.messages.push(Arc::new(message));
        timeline.sort();
        self.index.insert(id, key);
        (true, replaced)
    }

    fn with_message(
        &mut self,
        id: i64,
        f: impl FnOnce(&mut ChatMessage),
    ) -> Option<ConversationKey> {
        let key = self.index.get(&id)?.clone();
        let timeline = self.timelines.get_mut(&key)?;
        let slot = timeline.messages.iter_mut().find(|m| m.id == id)?;
        f(Arc::make_mut(slot));
        timeline.version += 1;
        Some(key)
    }

    /// 内容、flags 或编辑时间变化。返回会话和被替换的旧内容（内容没有变化时为 `None`）。
    pub fn apply_edit(&mut self, edit: &ContentEdit) -> Option<(ConversationKey, Option<String>)> {
        let mut previous = None;
        let key = self.with_message(edit.id, |m| {
            if let Some(html) = edit.html {
                if m.content_html != html {
                    previous = Some(m.content_html.clone());
                }
                m.content_html = html.to_owned();
            }
            if let Some(flags) = edit.flags {
                m.flags = MessageFlags::from_names(flags);
            }
            if edit.edit_timestamp.is_some() {
                m.last_edit_timestamp = edit.edit_timestamp;
            }
        })?;
        Some((key, previous))
    }

    /// 消息移动到新会话：从旧窗口移除；目标窗口不连续，直接失效等待重新加载。
    /// 返回受影响的旧会话和新会话。
    pub fn apply_move(
        &mut self,
        ids: &[i64],
        new_key: &ConversationKey,
    ) -> HashSet<ConversationKey> {
        let mut affected = HashSet::new();
        for id in ids {
            let Some(old_key) = self.index.remove(id) else {
                continue;
            };
            if let Some(timeline) = self.timelines.get_mut(&old_key) {
                timeline.messages.retain(|m| m.id != *id);
                timeline.version += 1;
            }
            affected.insert(old_key);
        }
        if self.timelines.contains_key(new_key) {
            self.remove_timeline(new_key);
            affected.insert(new_key.clone());
        }
        affected
    }

    /// 删除：普通消息保存到数据库并标记为已删除，留在窗口中；通知和隐藏的消息直接移除。
    /// 返回 `(会话, 是否只是标记)`。
    pub fn on_deleted(&mut self, id: i64) -> Option<(ConversationKey, bool)> {
        let message = self.get(id)?.clone();
        let key = message.key.clone();
        if self.hidden.contains(&id) || message.notice != SystemNoticeKind::None {
            if let Some(timeline) = self.timelines.get_mut(&key) {
                timeline.messages.retain(|m| m.id != id);
                timeline.version += 1;
            }
            self.index.remove(&id);
            return Some((key, false));
        }
        let row = DeletedMessage {
            message_id: id,
            conv_key: key.to_string(),
            sender_id: message.sender_id,
            sender_name: message.sender_name.clone(),
            avatar_url: message.avatar_url.clone(),
            content: message.content_html.clone(),
            timestamp: message.timestamp,
            last_edit_timestamp: message.last_edit_timestamp,
            is_me_message: message.is_me_message,
            topic: message.topic.clone(),
        };
        self.deleted_repo.add(&row);
        let rows = self.deleted.entry(key.clone()).or_default();
        if !rows.iter().any(|r| r.message_id == id) {
            rows.push(row);
        }
        self.with_message(id, |m| {
            m.state = MessageState::Deleted;
            m.reactions.clear();
            m.submessages.clear();
            m.flags.read = true;
        });
        Some((key, true))
    }

    pub fn apply_reaction(
        &mut self,
        id: i64,
        reaction: Reaction,
        add: bool,
    ) -> Option<ConversationKey> {
        self.with_message(id, |m| {
            if add {
                if !m.reactions.contains(&reaction) {
                    m.reactions.push(reaction);
                }
            } else {
                m.reactions.retain(|r| {
                    !(r.user_id == reaction.user_id
                        && r.emoji_code == reaction.emoji_code
                        && r.reaction_type == reaction.reaction_type)
                });
            }
        })
    }

    pub fn apply_submessage(&mut self, submessage: Submessage) -> Option<ConversationKey> {
        self.with_message(submessage.message_id, |m| {
            if !m.submessages.iter().any(|s| s.id == submessage.id) {
                m.submessages.push(submessage);
                m.submessages.sort_by_key(|s| s.id);
            }
        })
    }

    /// 已缓存的消息 flag 变化，返回实际变化的 `(id, 会话)`。
    pub fn apply_flag(
        &mut self,
        ids: &[i64],
        flag: &str,
        add: bool,
    ) -> Vec<(i64, ConversationKey)> {
        let mut changed = Vec::new();
        for id in ids {
            let Some(current) = self.get(*id) else {
                continue;
            };
            let mut flags = current.flags;
            if !flags.set_named(flag, add) {
                continue;
            }
            if let Some(key) = self.with_message(*id, |m| m.flags = flags) {
                changed.push((*id, key));
            }
        }
        changed
    }

    /// `all: true` 的 flag 事件：给所有缓存的消息加上。
    pub fn apply_flag_all(&mut self, flag: &str) -> Vec<(i64, ConversationKey)> {
        let ids: Vec<i64> = self.index.keys().copied().collect();
        self.apply_flag(&ids, flag, true)
    }

    /// 本地回显。`text` 是 Markdown 原文。返回负 id 和发送时使用的 `local_id`。
    pub fn add_pending(&mut self, mut message: ChatMessage, text: &str) -> (i64, String) {
        self.next_local_id -= 1;
        let id = self.next_local_id;
        let local_id = format!("{}.{}", self.account_id, -id);
        message.id = id;
        message.state = MessageState::Sending(LocalEcho {
            local_id: local_id.clone(),
            raw_content: text.to_owned(),
        });
        message.flags.read = true;
        message.content_html = format!("<p>{}</p>", encode(text).replace('\n', "<br>"));
        let key = message.key.clone();
        let timeline = self.timelines.entry(key.clone()).or_default();
        timeline.messages.push(Arc::new(message));
        timeline.version += 1;
        self.index.insert(id, key);
        (id, local_id)
    }

    pub fn find_pending(&self, local_id: &str) -> Option<&Arc<ChatMessage>> {
        self.timelines
            .values()
            .flat_map(|t| t.messages.iter())
            .find(|m| m.local_echo().is_some_and(|e| e.local_id == local_id))
    }

    /// 本地回显变为发送中或发送失败。
    pub fn set_pending_failed(
        &mut self,
        local_id: &str,
        failed: bool,
    ) -> Option<(ConversationKey, i64)> {
        let pending = self.find_pending(local_id)?;
        let id = pending.id;
        let echo = pending.local_echo()?.clone();
        let key = self.with_message(id, |m| {
            m.state = if failed {
                MessageState::Failed(echo)
            } else {
                MessageState::Sending(echo)
            };
        })?;
        Some((key, id))
    }

    pub fn remove_pending(&mut self, local_id: &str) -> Option<(ConversationKey, i64)> {
        let message = self.find_pending(local_id)?.clone();
        if let Some(timeline) = self.timelines.get_mut(&message.key) {
            timeline.messages.retain(|m| m.id != message.id);
            timeline.version += 1;
        }
        self.index.remove(&message.id);
        Some((message.key.clone(), message.id))
    }

    /// 窗口里最后一条可以作为预览的消息。
    pub fn latest_visible(&self, key: &ConversationKey) -> Option<&Arc<ChatMessage>> {
        self.timelines
            .get(key)?
            .messages
            .iter()
            .rev()
            .find(|m| m.id > 0 && !self.is_hidden(m) && !m.is_deleted())
    }
}

fn overlaps(page: &[ChatMessage], timeline: &Timeline) -> bool {
    page.iter()
        .any(|m| timeline.messages.iter().any(|t| t.id == m.id))
}

fn message_from_deleted(row: &DeletedMessage, key: &ConversationKey) -> ChatMessage {
    let mut message = ChatMessage::new(row.message_id, key.clone(), row.sender_id, row.timestamp);
    message.sender_name.clone_from(&row.sender_name);
    message.avatar_url.clone_from(&row.avatar_url);
    message.content_html.clone_from(&row.content);
    message.last_edit_timestamp = row.last_edit_timestamp;
    message.is_me_message = row.is_me_message;
    message.topic.clone_from(&row.topic);
    if let ConversationKey::Topic(topic) = key {
        message.stream_id = Some(topic.stream_id);
    }
    message.state = MessageState::Deleted;
    message.flags.read = true;
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::test_db;

    fn cache() -> MessageCache {
        MessageCache::new(DeletedMessageRepo::new(&test_db(), 1), Vec::new(), 1)
    }

    fn key() -> ConversationKey {
        ConversationKey::topic(1, "t")
    }

    fn msg(id: i64) -> ChatMessage {
        let mut m = ChatMessage::new(id, key(), 2, id);
        m.content_html = format!("<p>{id}</p>");
        m
    }

    fn ids(c: &MessageCache) -> Vec<i64> {
        c.timeline(&key())
            .unwrap()
            .messages
            .iter()
            .map(|m| m.id)
            .collect()
    }

    #[test]
    fn paging_merges_in_order() {
        let mut c = cache();
        c.merge_page(&key(), vec![msg(5), msg(6)], PageKind::Newest, false, true);
        c.merge_page(&key(), vec![msg(3), msg(4)], PageKind::Older, true, false);
        assert_eq!(ids(&c), [3, 4, 5, 6]);
        let t = c.timeline(&key()).unwrap();
        assert!(t.found_oldest && t.found_newest && t.has_enough_cached());
    }

    #[test]
    fn around_page_replaces_disconnected_window() {
        let mut c = cache();
        c.merge_page(
            &key(),
            vec![msg(50), msg(51)],
            PageKind::Newest,
            false,
            true,
        );
        c.merge_page(&key(), vec![msg(5), msg(6)], PageKind::Around, false, false);
        assert_eq!(ids(&c), [5, 6]);
        assert!(!c.timeline(&key()).unwrap().found_newest);
        // 窗口未到最新，新消息不加入
        let (added, _) = c.on_new_message(msg(60), None);
        assert!(!added);
        assert_eq!(ids(&c), [5, 6]);
        assert!(c.get(50).is_none());
    }

    #[test]
    fn pending_is_replaced_by_echo_and_stays_last() {
        let mut c = cache();
        c.merge_page(&key(), vec![msg(5)], PageKind::Newest, true, true);
        let (local, local_id) = c.add_pending(msg(0), "你<好\n世界");
        assert!(local < 0);
        let pending = c.find_pending(&local_id).unwrap();
        assert_eq!(pending.content_html, "<p>你&lt;好<br>世界</p>");
        assert!(
            matches!(&pending.state, MessageState::Sending(e) if e.raw_content == "你<好\n世界")
        );
        assert!(pending.is_read());
        assert_eq!(ids(&c), [5, local]);

        c.on_new_message(msg(6), None);
        assert_eq!(ids(&c), [5, 6, local]);

        let (added, replaced) = c.on_new_message(msg(7), Some(&local_id));
        assert!(added);
        assert_eq!(replaced, Some(local));
        assert_eq!(ids(&c), [5, 6, 7]);
    }

    #[test]
    fn failed_pending_can_be_marked_and_removed() {
        let mut c = cache();
        c.merge_page(&key(), vec![], PageKind::Newest, true, true);
        let (_, local_id) = c.add_pending(msg(0), "x");
        c.set_pending_failed(&local_id, true);
        let failed = c.find_pending(&local_id).unwrap();
        assert!(matches!(&failed.state, MessageState::Failed(e) if e.raw_content == "x"));
        assert!(c.remove_pending(&local_id).is_some());
        assert!(ids(&c).is_empty());
    }

    #[test]
    fn deleted_message_stays_marked_and_returns_on_reload() {
        let mut c = cache();
        let mut m = msg(5);
        m.reactions.push(Reaction::default());
        c.merge_page(&key(), vec![m, msg(6)], PageKind::Newest, true, true);
        assert_eq!(c.on_deleted(5), Some((key(), true)));
        let deleted = c.get(5).unwrap();
        assert!(deleted.is_deleted() && deleted.is_read() && deleted.reactions.is_empty());

        c.remove_timeline(&key());
        c.merge_page(&key(), vec![msg(6)], PageKind::Newest, true, true);
        assert_eq!(ids(&c), [5, 6]);
        assert!(c.get(5).unwrap().is_deleted());
    }

    #[test]
    fn notice_is_removed_instead_of_marked() {
        let mut c = cache();
        let mut m = msg(5);
        m.notice = SystemNoticeKind::Notice;
        c.merge_page(&key(), vec![m], PageKind::Newest, true, true);
        assert_eq!(c.on_deleted(5), Some((key(), false)));
        assert!(ids(&c).is_empty());
    }

    #[test]
    fn edit_returns_previous_content_and_applies_flags() {
        let mut c = cache();
        c.merge_page(&key(), vec![msg(5)], PageKind::Newest, true, true);
        let flags = vec!["starred".to_owned()];
        let (k, previous) = c
            .apply_edit(&ContentEdit {
                id: 5,
                html: Some("<p>新</p>"),
                flags: Some(&flags),
                edit_timestamp: Some(99),
            })
            .unwrap();
        assert_eq!(k, key());
        assert_eq!(previous.as_deref(), Some("<p>5</p>"));
        let m = c.get(5).unwrap();
        assert!(m.is_starred());
        assert_eq!(m.last_edit_timestamp, Some(99));
    }

    #[test]
    fn move_removes_from_old_and_invalidates_new() {
        let mut c = cache();
        c.merge_page(&key(), vec![msg(5), msg(6)], PageKind::Newest, true, true);
        let other = ConversationKey::topic(1, "o");
        c.merge_page(&other, vec![], PageKind::Newest, true, true);
        let affected = c.apply_move(&[5], &other);
        assert!(affected.contains(&key()) && affected.contains(&other));
        assert_eq!(ids(&c), [6]);
        assert!(c.timeline(&other).is_none());
    }

    #[test]
    fn reactions_and_flags() {
        let mut c = cache();
        c.merge_page(&key(), vec![msg(5)], PageKind::Newest, true, true);
        let r = Reaction {
            emoji_name: "a".into(),
            emoji_code: "1".into(),
            reaction_type: "u".into(),
            user_id: 1,
        };
        c.apply_reaction(5, r.clone(), true);
        c.apply_reaction(5, r.clone(), true);
        assert_eq!(c.get(5).unwrap().reactions.len(), 1);
        c.apply_reaction(5, r, false);
        assert!(c.get(5).unwrap().reactions.is_empty());
        assert_eq!(c.apply_flag(&[5, 9], "read", true), [(5, key())]);
        assert!(c.apply_flag(&[5], "read", true).is_empty());
    }

    #[test]
    fn event_older_than_the_window_is_ignored() {
        let mut c = cache();
        c.merge_page(&key(), vec![msg(50)], PageKind::Newest, false, true);
        assert_eq!(c.on_new_message(msg(7), None), (false, None));
        assert_eq!(c.on_new_message(msg(51), None), (true, None));
    }

    #[test]
    fn plugin_hidden_message_is_hidden_after_refetch_and_removed_on_delete() {
        let mut c = cache();
        c.hide(5);
        c.merge_page(&key(), vec![msg(5), msg(6)], PageKind::Newest, true, true);
        assert!(c.is_hidden(c.get(5).unwrap()));
        assert_eq!(c.latest_visible(&key()).unwrap().id, 6);
        c.hide(6);
        assert!(c.latest_visible(&key()).is_none());
        assert_eq!(c.on_deleted(5), Some((key(), false)));
        assert!(c.get(5).is_none());
    }

    #[test]
    fn open_counts_multiple_windows() {
        let mut c = cache();
        assert!(c.set_open(&key(), true));
        assert!(!c.set_open(&key(), true));
        assert!(!c.set_open(&key(), false));
        assert!(c.is_open(&key()));
        assert!(c.set_open(&key(), false));
        assert!(!c.is_open(&key()));
    }

    #[test]
    fn evicts_least_recently_used_but_not_open() {
        let mut c = cache();
        let first = ConversationKey::topic(1, "first");
        let second = ConversationKey::topic(1, "second");
        c.set_open(&first, true);
        c.merge_page(&first, vec![], PageKind::Newest, true, true);
        c.merge_page(&second, vec![], PageKind::Newest, true, true);
        for i in 0..MAX_TIMELINES {
            c.merge_page(
                &ConversationKey::topic(2, format!("t{i}")),
                vec![],
                PageKind::Newest,
                true,
                true,
            );
        }
        assert!(c.timeline(&first).is_some());
        assert!(c.timeline(&second).is_none());
    }
}
