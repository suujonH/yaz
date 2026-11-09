use std::collections::{HashMap, HashSet, VecDeque};

use zulip_core::ConversationKey;
use zulip_core::html;
use zulip_core::push::DndLevel;
use zulip_store::{
    ConversationCache, ConversationCacheRepo, ConversationPref, ConversationPrefRepo, LocalSettings,
};

use crate::topic::is_resolved_name;

/// 一条消息作为预览时的字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Latest {
    pub id: i64,
    pub time: i64,
    pub sender_id: i64,
    pub preview: String,
    pub title: String,
}

/// 消息列表中的一个会话：用户偏好（`dnd_level`、`in_list`）和列表缓存（其余字段，可由消息重建）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationEntry {
    /// 免打扰级别；`None` 为默认。
    pub dnd_level: Option<i32>,
    pub in_list: bool,
    /// 最近一次看到的原始话题名。
    pub title: String,
    pub last_message_id: i64,
    /// Unix 秒。
    pub last_message_time: i64,
    pub last_sender_id: i64,
    pub last_preview: String,
}

/// 消息列表的状态。
pub struct Conversations {
    entries: HashMap<ConversationKey, ConversationEntry>,
    prefs: ConversationPrefRepo,
    cache: ConversationCacheRepo,
    local: LocalSettings,
    backfill: VecDeque<ConversationKey>,
}

impl Conversations {
    /// 缓存丢失的会话（在列表中却没有缓存行）进入补取队列重建。
    pub fn new(
        prefs: ConversationPrefRepo,
        cache: ConversationCacheRepo,
        local: LocalSettings,
        stored_prefs: Vec<ConversationPref>,
        stored_cache: Vec<ConversationCache>,
    ) -> Self {
        let mut entries: HashMap<ConversationKey, ConversationEntry> = HashMap::new();
        let mut backfill = VecDeque::new();
        for row in stored_cache {
            if let Ok(key) = row.key.parse() {
                entries.insert(
                    key,
                    ConversationEntry {
                        title: row.title,
                        last_message_id: row.last_message_id,
                        last_message_time: row.last_message_time,
                        last_sender_id: row.last_sender_id,
                        last_preview: row.last_preview,
                        ..ConversationEntry::default()
                    },
                );
            }
        }
        for row in stored_prefs {
            let Ok(key) = row.key.parse::<ConversationKey>() else {
                continue;
            };
            if row.in_list && !entries.contains_key(&key) {
                backfill.push_back(key.clone());
            }
            let entry = entries.entry(key).or_default();
            entry.dnd_level = row.dnd_level;
            entry.in_list = row.in_list;
        }
        Self {
            entries,
            prefs,
            cache,
            local,
            backfill,
        }
    }

    pub fn get(&self, key: &ConversationKey) -> Option<&ConversationEntry> {
        self.entries.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ConversationKey, &ConversationEntry)> {
        self.entries.iter()
    }

    fn save_pref(&self, key: &ConversationKey) {
        if let Some(e) = self.entries.get(key) {
            self.prefs.set(&ConversationPref {
                key: key.to_string(),
                dnd_level: e.dnd_level,
                in_list: e.in_list,
            });
        }
    }

    fn save_cache(&self, key: &ConversationKey) {
        if let Some(e) = self.entries.get(key) {
            self.cache.set(&ConversationCache {
                key: key.to_string(),
                title: e.title.clone(),
                last_message_id: e.last_message_id,
                last_message_time: e.last_message_time,
                last_sender_id: e.last_sender_id,
                last_preview: e.last_preview.clone(),
            });
        }
    }

    fn entry(&mut self, key: &ConversationKey) -> &mut ConversationEntry {
        self.entries.entry(key.clone()).or_default()
    }

    /// 存储的免打扰级别。
    pub fn level(&self, key: &ConversationKey) -> DndLevel {
        self.entries
            .get(key)
            .and_then(|e| e.dnd_level)
            .and_then(DndLevel::from_i32)
            .unwrap_or_default()
    }

    /// 实际生效的级别：折叠已解决话题时，已解决话题收进群助手。
    pub fn effective_level(&self, key: &ConversationKey) -> DndLevel {
        let level = self.level(key);
        if level != DndLevel::Blocked
            && self.is_resolved(key)
            && self.local.collapse_resolved_topics()
        {
            DndLevel::Assistant
        } else {
            level
        }
    }

    pub fn is_resolved(&self, key: &ConversationKey) -> bool {
        let ConversationKey::Topic(t) = key else {
            return false;
        };
        let name = self
            .entries
            .get(key)
            .map(|e| e.title.as_str())
            .filter(|title| !title.is_empty())
            .unwrap_or(&t.topic);
        is_resolved_name(name)
    }

    /// 写入最后一条消息。比现有的旧时忽略；同一条消息只刷新预览和标题，不改变是否在列表中。
    pub fn apply_latest(&mut self, key: &ConversationKey, latest: &Latest) -> bool {
        let blocked = self.level(key) == DndLevel::Blocked;
        let is_topic = matches!(key, ConversationKey::Topic(_));
        let is_new = !self.entries.contains_key(key);
        let entry = self.entry(key);
        if is_new && !is_topic {
            entry.title.clone_from(&latest.title);
        }
        if !is_new && latest.id < entry.last_message_id {
            return false;
        }
        if !is_new && latest.id == entry.last_message_id {
            let same_title = !is_topic || entry.title == latest.title;
            if entry.last_preview == latest.preview && same_title {
                return false;
            }
            entry.last_preview.clone_from(&latest.preview);
            if is_topic {
                entry.title.clone_from(&latest.title);
            }
            self.save_cache(key);
            return true;
        }
        entry.last_message_id = latest.id;
        entry.last_message_time = latest.time;
        entry.last_sender_id = latest.sender_id;
        entry.last_preview.clone_from(&latest.preview);
        if is_topic {
            entry.title.clone_from(&latest.title);
        }
        let listed = !blocked && !entry.in_list;
        if listed {
            entry.in_list = true;
            self.save_pref(key);
        }
        self.save_cache(key);
        true
    }

    /// 服务器提到有这个会话，但没有消息详情。需要补取预览时加入队列。
    pub fn bootstrap_known(
        &mut self,
        key: &ConversationKey,
        max_id: i64,
        title: &str,
        is_exact_max: bool,
    ) {
        let blocked = self.level(key) == DndLevel::Blocked;
        match self.entries.get_mut(key) {
            None => {
                self.entries.insert(
                    key.clone(),
                    ConversationEntry {
                        title: title.to_owned(),
                        in_list: true,
                        ..ConversationEntry::default()
                    },
                );
                self.save_pref(key);
                self.save_cache(key);
                self.queue_backfill(key);
            }
            Some(entry) if max_id > entry.last_message_id => {
                if !entry.in_list && !blocked {
                    entry.in_list = true;
                    self.save_pref(key);
                }
                self.queue_backfill(key);
            }
            Some(entry) if is_exact_max && max_id < entry.last_message_id => {
                self.queue_backfill(key);
            }
            Some(_) => {}
        }
    }

    pub fn queue_backfill(&mut self, key: &ConversationKey) {
        if !self.backfill.contains(key) {
            self.backfill.push_back(key.clone());
        }
    }

    pub fn next_backfill(&mut self) -> Option<ConversationKey> {
        self.backfill.pop_front()
    }

    /// 补取结果。补取期间有了更新的消息时丢弃；会话已没有消息时清空预览。
    pub fn apply_backfill(
        &mut self,
        key: &ConversationKey,
        asked_after: i64,
        latest: Option<Latest>,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        if entry.last_message_id > asked_after {
            match &latest {
                Some(l) if l.id >= entry.last_message_id => {}
                _ => return false,
            }
        }
        let (id, time, sender, preview) = match &latest {
            Some(l) => (l.id, l.time, l.sender_id, l.preview.clone()),
            None => (0, 0, 0, String::new()),
        };
        // 缓存丢失后标题也要重建
        let title = latest
            .as_ref()
            .filter(|l| entry.title.is_empty() && !l.title.is_empty())
            .map(|l| l.title.clone());
        if entry.last_message_id == id && entry.last_preview == preview && title.is_none() {
            return false;
        }
        entry.last_message_id = id;
        entry.last_message_time = time;
        entry.last_sender_id = sender;
        entry.last_preview.clone_from(&preview);
        if let Some(title) = title {
            entry.title = title;
        }
        self.save_cache(key);
        true
    }

    /// 最后一条消息被编辑。按 id 查找：预览可能来自补取，消息不一定在缓存中。
    pub fn on_edited(&mut self, id: i64, rendered_html: &str) -> Option<ConversationKey> {
        let key = self
            .entries
            .iter()
            .find(|(_, e)| e.last_message_id == id)
            .map(|(k, _)| k.clone())?;
        self.entries.get_mut(&key)?.last_preview = html::to_preview(rendered_html);
        self.save_cache(&key);
        Some(key)
    }

    /// 最后一条消息被删除的会话进入补取队列。
    pub fn on_deleted(&mut self, ids: &[i64]) {
        let hit: HashSet<i64> = ids.iter().copied().collect();
        let keys: Vec<ConversationKey> = self
            .entries
            .iter()
            .filter(|(_, e)| hit.contains(&e.last_message_id))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            self.queue_backfill(&key);
        }
    }

    pub fn set_level(&mut self, key: &ConversationKey, level: DndLevel) -> bool {
        let entry = self.entry(key);
        let value = (level != DndLevel::Notify).then_some(level as i32);
        if entry.dnd_level == value {
            return false;
        }
        entry.dnd_level = value;
        if level == DndLevel::Blocked {
            entry.in_list = false;
        }
        self.save_pref(key);
        true
    }

    pub fn remove_from_list(&mut self, key: &ConversationKey) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        if !entry.in_list {
            return false;
        }
        entry.in_list = false;
        self.save_pref(key);
        true
    }

    /// 打开会话时放进列表。屏蔽的会话打开后恢复到列表，级别不变。
    pub fn ensure_in_list(&mut self, key: &ConversationKey, title: &str) -> bool {
        let entry = self.entry(key);
        if entry.in_list {
            return false;
        }
        entry.in_list = true;
        let new_title = entry.title.is_empty();
        if new_title {
            title.clone_into(&mut entry.title);
        }
        self.save_pref(key);
        if new_title {
            self.save_cache(key);
        }
        true
    }

    pub fn set_title(&mut self, key: &ConversationKey, title: &str) {
        let entry = self.entry(key);
        if entry.title != title {
            title.clone_into(&mut entry.title);
            self.save_cache(key);
        }
    }

    /// 话题改名或整体移动：状态搬到新键。目标已存在时保留目标的状态。
    pub fn rename(&mut self, old: &ConversationKey, new: &ConversationKey, title: &str) {
        if old == new {
            self.set_title(new, title);
            return;
        }
        let Some(mut row) = self.entries.remove(old) else {
            return;
        };
        let old_text = old.to_string();
        self.prefs.delete(&old_text);
        self.cache.delete(&old_text);
        match self.entries.get_mut(new) {
            Some(existing) => {
                existing.in_list |= row.in_list;
                if row.last_message_id > existing.last_message_id {
                    existing.last_message_id = row.last_message_id;
                    existing.last_message_time = row.last_message_time;
                    existing.last_sender_id = row.last_sender_id;
                    existing.last_preview = row.last_preview;
                }
                title.clone_into(&mut existing.title);
                self.queue_backfill(new);
            }
            None => {
                title.clone_into(&mut row.title);
                self.entries.insert(new.clone(), row);
            }
        }
        self.save_pref(new);
        self.save_cache(new);
        self.backfill.retain(|k| k != old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::test_db;
    use zulip_store::Db;

    fn open(db: &Db) -> Conversations {
        let prefs = ConversationPrefRepo::new(db, 1);
        let cache = ConversationCacheRepo::new(db, 1);
        let (stored_prefs, stored_cache) = (prefs.load_all().unwrap(), cache.load_all().unwrap());
        Conversations::new(
            prefs,
            cache,
            LocalSettings::new(db),
            stored_prefs,
            stored_cache,
        )
    }

    fn store() -> Conversations {
        open(&test_db())
    }

    fn key() -> ConversationKey {
        ConversationKey::topic(1, "t")
    }

    fn latest(id: i64, preview: &str) -> Latest {
        Latest {
            id,
            time: id * 10,
            sender_id: 2,
            preview: preview.into(),
            title: "T".into(),
        }
    }

    #[test]
    fn latest_only_moves_forward_and_lists() {
        let mut c = store();
        assert!(c.apply_latest(&key(), &latest(5, "五")));
        let e = c.get(&key()).unwrap();
        assert!(e.in_list);
        assert_eq!(
            (e.last_message_id, e.last_preview.as_str(), e.title.as_str()),
            (5, "五", "T")
        );
        assert!(!c.apply_latest(&key(), &latest(4, "四")));
        assert_eq!(c.get(&key()).unwrap().last_message_id, 5);
    }

    #[test]
    fn same_message_refreshes_preview_without_relisting() {
        let mut c = store();
        c.apply_latest(&key(), &latest(5, "五"));
        c.remove_from_list(&key());
        assert!(c.apply_latest(&key(), &latest(5, "改")));
        let e = c.get(&key()).unwrap();
        assert_eq!(e.last_preview, "改");
        assert!(!e.in_list);
        assert!(c.apply_latest(&key(), &latest(6, "六")));
        assert!(c.get(&key()).unwrap().in_list);
    }

    #[test]
    fn blocked_conversation_stays_out_of_list() {
        let mut c = store();
        c.apply_latest(&key(), &latest(5, "五"));
        assert!(c.set_level(&key(), DndLevel::Blocked));
        assert!(!c.get(&key()).unwrap().in_list);
        c.apply_latest(&key(), &latest(6, "六"));
        assert!(!c.get(&key()).unwrap().in_list);
        assert_eq!(c.level(&key()), DndLevel::Blocked);
        c.bootstrap_known(&key(), 9, "T", false);
        assert!(!c.get(&key()).unwrap().in_list);
    }

    #[test]
    fn bootstrap_known_queues_backfill() {
        let mut c = store();
        c.bootstrap_known(&key(), 5, "T", false);
        assert!(c.get(&key()).unwrap().in_list);
        assert_eq!(c.next_backfill(), Some(key()));
        assert_eq!(c.next_backfill(), None);
        // 与本地相同：不补取
        c.apply_latest(&key(), &latest(5, "五"));
        c.bootstrap_known(&key(), 5, "T", true);
        assert_eq!(c.next_backfill(), None);
        // 最大 id 变小：最后一条被删
        c.bootstrap_known(&key(), 3, "T", true);
        assert_eq!(c.next_backfill(), Some(key()));
    }

    #[test]
    fn backfill_discards_stale_result_and_clears_empty() {
        let mut c = store();
        c.apply_latest(&key(), &latest(5, "五"));
        assert!(!c.apply_backfill(&key(), 3, Some(latest(4, "四"))));
        assert_eq!(c.get(&key()).unwrap().last_message_id, 5);
        assert!(c.apply_backfill(&key(), 5, Some(latest(4, "四"))));
        assert_eq!(c.get(&key()).unwrap().last_message_id, 4);
        assert!(c.apply_backfill(&key(), 4, None));
        assert_eq!(c.get(&key()).unwrap().last_preview, "");
    }

    #[test]
    fn edit_and_delete_find_entry_by_message_id() {
        let mut c = store();
        c.apply_latest(&key(), &latest(5, "五"));
        assert_eq!(c.on_edited(5, "<p>新内容</p>"), Some(key()));
        assert_eq!(c.get(&key()).unwrap().last_preview, "新内容");
        assert_eq!(c.on_edited(6, "<p>x</p>"), None);
        c.on_deleted(&[5]);
        assert_eq!(c.next_backfill(), Some(key()));
    }

    #[test]
    fn rename_moves_state_and_merges() {
        let mut c = store();
        c.apply_latest(&key(), &latest(5, "五"));
        c.set_level(&key(), DndLevel::Silent);
        let new = ConversationKey::topic(1, "n");
        c.rename(&key(), &new, "N");
        assert!(c.get(&key()).is_none());
        let e = c.get(&new).unwrap();
        assert_eq!(e.title, "N");
        assert_eq!(c.level(&new), DndLevel::Silent);
        // 合并到已存在的目标
        let other = ConversationKey::topic(1, "o");
        c.apply_latest(&other, &latest(9, "九"));
        c.rename(&new, &other, "O");
        assert_eq!(c.get(&other).unwrap().last_message_id, 9);
    }

    #[test]
    fn collapse_resolved_affects_effective_level() {
        let db = test_db();
        let mut c = open(&db);
        let resolved = ConversationKey::topic(1, "✔ 完成");
        let mut l = latest(5, "x");
        l.title = "✔ 完成".into();
        c.apply_latest(&resolved, &l);
        assert_eq!(c.effective_level(&resolved), DndLevel::Notify);
        LocalSettings::new(&db).set_collapse_resolved_topics(true);
        assert_eq!(c.effective_level(&resolved), DndLevel::Assistant);
        c.set_level(&resolved, DndLevel::Blocked);
        assert_eq!(c.effective_level(&resolved), DndLevel::Blocked);
    }

    #[test]
    fn state_survives_reload() {
        let db = test_db();
        let mut c = open(&db);
        c.apply_latest(&key(), &latest(5, "五"));
        c.set_level(&key(), DndLevel::Silent);
        let c = open(&db);
        assert_eq!(c.level(&key()), DndLevel::Silent);
        assert_eq!(c.get(&key()).unwrap().last_preview, "五");
    }

    #[test]
    fn lost_cache_is_rebuilt_and_prefs_survive() {
        let db = test_db();
        let mut c = open(&db);
        c.apply_latest(&key(), &latest(5, "五"));
        c.set_level(&key(), DndLevel::Silent);
        ConversationCacheRepo::new(&db, 1).delete(&key().to_string());

        let mut c = open(&db);
        assert_eq!(c.level(&key()), DndLevel::Silent);
        assert!(c.get(&key()).unwrap().in_list);
        assert_eq!(c.next_backfill(), Some(key()));
        assert!(c.apply_backfill(&key(), 0, Some(latest(5, "五"))));
        let e = c.get(&key()).unwrap();
        assert_eq!((e.title.as_str(), e.last_preview.as_str()), ("T", "五"));
    }
}
