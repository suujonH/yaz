use std::collections::{HashMap, HashSet, VecDeque};

use zulip_core::ConversationKey;
use zulip_core::html;
use zulip_core::push::DndLevel;
use zulip_store::{
    ConversationCache, ConversationCacheRepo, ConversationPref, ConversationPrefRepo, LocalSettings,
};

use crate::topic::is_resolved_name;

/// Fields of a message when used as a preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Latest {
    pub id: i64,
    pub time: i64,
    pub sender_id: i64,
    pub preview: String,
    pub title: String,
}

/// A conversation in the message list: user preferences (`dnd_level`, `in_list`) plus the list
/// cache (the other fields, rebuildable from messages).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationEntry {
    /// Do-not-disturb level; `None` is the default.
    pub dnd_level: Option<i32>,
    pub in_list: bool,
    /// The raw topic name seen most recently.
    pub title: String,
    pub last_message_id: i64,
    /// Unix seconds.
    pub last_message_time: i64,
    pub last_sender_id: i64,
    pub last_preview: String,
}

/// State of the message list.
pub struct Conversations {
    entries: HashMap<ConversationKey, ConversationEntry>,
    prefs: ConversationPrefRepo,
    cache: ConversationCacheRepo,
    local: LocalSettings,
    backfill: VecDeque<ConversationKey>,
}

impl Conversations {
    /// Conversations whose cache was lost (in the list but with no cache row) go into the backfill
    /// queue to be rebuilt.
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

    /// The stored do-not-disturb level.
    pub fn level(&self, key: &ConversationKey) -> DndLevel {
        self.entries
            .get(key)
            .and_then(|e| e.dnd_level)
            .and_then(DndLevel::from_i32)
            .unwrap_or_default()
    }

    /// The effective level: when resolved topics are collapsed, resolved topics go into the group
    /// assistant.
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

    /// Writes the last message. Ignored if older than the existing one; for the same message only
    /// the preview and title are refreshed, and whether it is in the list is unchanged.
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

    /// The server mentioned this conversation but gave no message details. Queued when the preview
    /// needs backfilling.
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

    /// Backfill result. Discarded if a newer message arrived during the backfill; the preview is
    /// cleared if the conversation has no messages left.
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
        // The title must be rebuilt too after the cache is lost
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

    /// The last message was edited. Looked up by id: the preview may come from a backfill, so the
    /// message is not necessarily in the cache.
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

    /// A conversation whose last message was deleted goes into the backfill queue.
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

    /// Put into the list when the conversation is opened. A blocked conversation returns to the
    /// list when opened, with its level unchanged.
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

    /// Topic renamed or moved as a whole: the state moves to the new key. If the target already
    /// exists, the target's state is kept.
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
        // Same as local: no backfill
        c.apply_latest(&key(), &latest(5, "五"));
        c.bootstrap_known(&key(), 5, "T", true);
        assert_eq!(c.next_backfill(), None);
        // Max id decreased: the last message was deleted
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
        // Merged into the existing target
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
