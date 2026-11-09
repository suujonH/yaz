use std::collections::{BTreeSet, HashMap, HashSet};

use zulip_api::UnreadMsgsDto;
use zulip_core::ConversationKey;
use zulip_core::html;
use zulip_core::model::{ChatMessage, ConversationNotice, MentionKind};
use zulip_core::notice::SystemNoticeKind;

/// 打开会话时的未读快照。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnreadSnapshot {
    /// 升序。
    pub ids: Vec<i64>,
    /// 提及我的未读，升序。
    pub mentions: Vec<(i64, MentionKind)>,
}

#[derive(Default)]
pub struct Unread {
    by_key: HashMap<ConversationKey, BTreeSet<i64>>,
    key_of: HashMap<i64, ConversationKey>,
    mentions: HashMap<i64, MentionKind>,
    has_file: HashSet<i64>,
    alert_word: HashSet<i64>,
    /// 归入系统通知或消息提醒的消息，不进入未读。
    excluded: HashSet<i64>,
}

impl Unread {
    /// register 的未读摘要。提及先按“提到我”处理，详情用 [`apply_details`](Self::apply_details) 修正。
    pub fn load(summary: &UnreadMsgsDto, self_id: i64) -> Self {
        let mut unread = Self::default();
        for pm in &summary.pms {
            let key = ConversationKey::dm([pm.other_user_id], self_id);
            unread.add_all(&key, &pm.unread_message_ids);
        }
        for stream in &summary.streams {
            let key = ConversationKey::topic(stream.stream_id, stream.topic.clone());
            unread.add_all(&key, &stream.unread_message_ids);
        }
        for huddle in &summary.huddles {
            let ids = huddle
                .user_ids_string
                .split(',')
                .filter_map(|s| s.trim().parse::<i64>().ok());
            let key = ConversationKey::dm(ids, self_id);
            unread.add_all(&key, &huddle.unread_message_ids);
        }
        for id in &summary.mentions {
            if unread.key_of.contains_key(id) {
                unread.mentions.insert(*id, MentionKind::Personal);
            }
        }
        unread
    }

    fn add_all(&mut self, key: &ConversationKey, ids: &[i64]) {
        for id in ids {
            self.add_id(key, *id);
        }
    }

    fn add_id(&mut self, key: &ConversationKey, id: i64) {
        self.by_key.entry(key.clone()).or_default().insert(id);
        self.key_of.insert(id, key.clone());
    }

    /// 用加载到的消息修正提及、关键词和文件信息；通知类消息移出未读。
    pub fn apply_details(&mut self, messages: &[ChatMessage]) -> Vec<ConversationKey> {
        let mut changed = HashSet::new();
        for m in messages {
            if matches!(
                m.notice,
                SystemNoticeKind::Notice | SystemNoticeKind::Reminder
            ) {
                self.excluded.insert(m.id);
                changed.extend(self.remove(m.id));
                continue;
            }
            if !self.key_of.contains_key(&m.id) {
                continue;
            }
            self.set_details(m);
            changed.insert(m.key.clone());
        }
        changed.into_iter().collect()
    }

    fn set_details(&mut self, m: &ChatMessage) {
        match m.flags.mention() {
            MentionKind::None => {
                self.mentions.remove(&m.id);
            }
            kind => {
                self.mentions.insert(m.id, kind);
            }
        }
        if m.flags.has_alert_word {
            self.alert_word.insert(m.id);
        }
        if html::has_file_attachment(&m.content_html) {
            self.has_file.insert(m.id);
        } else {
            self.has_file.remove(&m.id);
        }
    }

    /// 新消息或重新变为未读。返回会话是否变化。
    pub fn add_message(&mut self, m: &ChatMessage) -> bool {
        if m.is_read() || m.id < 0 || self.excluded.contains(&m.id) {
            return false;
        }
        self.add_id(&m.key, m.id);
        self.set_details(m);
        true
    }

    fn remove(&mut self, id: i64) -> Option<ConversationKey> {
        let key = self.key_of.remove(&id)?;
        if let Some(set) = self.by_key.get_mut(&key) {
            set.remove(&id);
            if set.is_empty() {
                self.by_key.remove(&key);
            }
        }
        self.mentions.remove(&id);
        self.has_file.remove(&id);
        self.alert_word.remove(&id);
        Some(key)
    }

    /// 标记已读或删除，返回受影响的会话。
    pub fn remove_ids(&mut self, ids: &[i64]) -> Vec<ConversationKey> {
        let changed: HashSet<ConversationKey> =
            ids.iter().filter_map(|id| self.remove(*id)).collect();
        changed.into_iter().collect()
    }

    pub fn clear(&mut self) -> Vec<ConversationKey> {
        let keys = self.by_key.keys().cloned().collect();
        self.by_key.clear();
        self.key_of.clear();
        self.mentions.clear();
        self.has_file.clear();
        self.alert_word.clear();
        keys
    }

    /// 消息重新变为未读时已知 id 和会话，缺少消息详情。
    pub fn add_known(&mut self, id: i64, key: &ConversationKey, mention: MentionKind) -> bool {
        if self.excluded.contains(&id) {
            return false;
        }
        self.add_id(key, id);
        if mention != MentionKind::None {
            self.mentions.insert(id, mention);
        }
        true
    }

    /// 消息移动到新会话，返回受影响的会话。
    pub fn apply_move(&mut self, ids: &[i64], new_key: &ConversationKey) -> Vec<ConversationKey> {
        let mut changed = HashSet::new();
        for id in ids {
            let Some(old) = self.key_of.get(id).cloned() else {
                continue;
            };
            if &old == new_key {
                continue;
            }
            if let Some(set) = self.by_key.get_mut(&old) {
                set.remove(id);
                if set.is_empty() {
                    self.by_key.remove(&old);
                }
            }
            self.add_id(new_key, *id);
            changed.insert(old);
            changed.insert(new_key.clone());
        }
        changed.into_iter().collect()
    }

    /// 编辑后提及、文件信息可能变化。
    pub fn on_edited(&mut self, m: &ChatMessage) -> bool {
        if !self.key_of.contains_key(&m.id) {
            return false;
        }
        self.set_details(m);
        true
    }

    pub fn count(&self, key: &ConversationKey) -> usize {
        self.by_key.get(key).map_or(0, BTreeSet::len)
    }

    pub fn keys(&self) -> Vec<ConversationKey> {
        self.by_key.keys().cloned().collect()
    }

    pub fn key_of(&self, id: i64) -> Option<&ConversationKey> {
        self.key_of.get(&id)
    }

    pub fn max_id(&self, key: &ConversationKey) -> Option<i64> {
        self.by_key.get(key)?.iter().next_back().copied()
    }

    pub fn ids(&self, key: &ConversationKey) -> Vec<i64> {
        self.by_key
            .get(key)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn latest_mention(&self, key: &ConversationKey) -> Option<(i64, MentionKind)> {
        self.by_key
            .get(key)?
            .iter()
            .rev()
            .find_map(|id| Some((*id, *self.mentions.get(id)?)))
    }

    pub fn latest_alert_word(&self, key: &ConversationKey) -> Option<i64> {
        self.by_key
            .get(key)?
            .iter()
            .rev()
            .find(|id| self.alert_word.contains(id))
            .copied()
    }

    /// 优先级：@我 > @全体成员 > 新文件。
    pub fn notice(&self, key: &ConversationKey) -> ConversationNotice {
        let Some(ids) = self.by_key.get(key) else {
            return ConversationNotice::None;
        };
        let mut wildcard = false;
        for id in ids {
            match self.mentions.get(id) {
                Some(MentionKind::Personal) => return ConversationNotice::AtMe,
                Some(MentionKind::Wildcard) => wildcard = true,
                _ => {}
            }
        }
        if wildcard {
            ConversationNotice::AtAll
        } else if ids.iter().any(|id| self.has_file.contains(id)) {
            ConversationNotice::NewFile
        } else {
            ConversationNotice::None
        }
    }

    pub fn snapshot(&self, key: &ConversationKey) -> UnreadSnapshot {
        let ids = self.ids(key);
        let mentions = ids
            .iter()
            .filter_map(|id| Some((*id, *self.mentions.get(id)?)))
            .collect();
        UnreadSnapshot { ids, mentions }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn summary() -> Unread {
        let dto: UnreadMsgsDto = serde_json::from_value(json!({
            "pms": [{"other_user_id": 2, "unread_message_ids": [1, 2]}, {"other_user_id": 7, "unread_message_ids": [3]}],
            "streams": [{"stream_id": 5, "topic": "T", "unread_message_ids": [4, 5]}],
            "huddles": [{"user_ids_string": "2,3,7", "unread_message_ids": [6]}],
            "mentions": [4, 99],
        }))
        .unwrap();
        Unread::load(&dto, 7)
    }

    fn msg(id: i64, key: ConversationKey) -> ChatMessage {
        ChatMessage::new(id, key, 2, 1)
    }

    #[test]
    fn load_maps_conversations() {
        let u = summary();
        assert_eq!(u.count(&ConversationKey::dm([2], 7)), 2);
        assert_eq!(u.count(&ConversationKey::dm([7], 7)), 1);
        assert_eq!(u.count(&ConversationKey::topic(5, "t")), 2);
        assert_eq!(u.count(&ConversationKey::dm([2, 3], 7)), 1);
        assert_eq!(
            u.notice(&ConversationKey::topic(5, "t")),
            ConversationNotice::AtMe
        );
        assert!(u.key_of(99).is_none());
    }

    #[test]
    fn details_correct_initial_mention_guess() {
        let mut u = summary();
        let key = ConversationKey::topic(5, "t");
        let mut m = msg(4, key.clone());
        m.flags.wildcard_mentioned = true;
        u.apply_details(&[m]);
        assert_eq!(u.notice(&key), ConversationNotice::AtAll);
        assert_eq!(u.latest_mention(&key), Some((4, MentionKind::Wildcard)));
    }

    #[test]
    fn notices_leave_unread_and_stay_out() {
        let mut u = summary();
        let bot = ConversationKey::dm([2], 7);
        let mut m = msg(1, bot.clone());
        m.notice = SystemNoticeKind::Notice;
        assert_eq!(
            u.apply_details(std::slice::from_ref(&m)),
            std::slice::from_ref(&bot)
        );
        assert_eq!(u.count(&bot), 1);
        m.notice = SystemNoticeKind::None;
        assert!(!u.add_message(&m));
    }

    #[test]
    fn notice_priority_and_files() {
        let mut u = Unread::default();
        let key = ConversationKey::topic(1, "a");
        let mut file = msg(1, key.clone());
        file.content_html = "<p><a href=\"/user_uploads/1/a/x.txt\">x.txt</a></p>".into();
        u.add_message(&file);
        assert_eq!(u.notice(&key), ConversationNotice::NewFile);
        let mut all = msg(2, key.clone());
        all.flags.wildcard_mentioned = true;
        u.add_message(&all);
        assert_eq!(u.notice(&key), ConversationNotice::AtAll);
        let mut me = msg(3, key.clone());
        me.flags.mentioned = true;
        u.add_message(&me);
        assert_eq!(u.notice(&key), ConversationNotice::AtMe);
        assert_eq!(u.snapshot(&key).mentions.len(), 2);
    }

    #[test]
    fn read_flag_and_move() {
        let mut u = summary();
        let dm = ConversationKey::dm([2], 7);
        assert_eq!(u.remove_ids(&[1, 2, 1000]), std::slice::from_ref(&dm));
        assert_eq!(u.count(&dm), 0);
        let topic = ConversationKey::topic(5, "t");
        let target = ConversationKey::topic(5, "new");
        let changed = u.apply_move(&[4], &target);
        assert_eq!(changed.len(), 2);
        assert_eq!(u.ids(&target), [4]);
        assert_eq!(u.ids(&topic), [5]);
        assert_eq!(u.notice(&target), ConversationNotice::AtMe);
        assert!(!u.clear().is_empty());
        assert!(u.keys().is_empty());
    }

    #[test]
    fn own_read_is_not_added() {
        let mut u = Unread::default();
        let key = ConversationKey::topic(1, "a");
        let mut read = msg(1, key.clone());
        read.flags.read = true;
        assert!(!u.add_message(&read));
        assert!(u.add_message(&msg(2, key)));
    }
}
