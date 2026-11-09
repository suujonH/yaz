use rusqlite::params;

use crate::db::Db;
use crate::error::Result;

/// The user's preferences for a conversation: do-not-disturb level and whether it is in the message
/// list. The conversation key is the conversation's storage string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationPref {
    pub key: String,
    /// Do-not-disturb level, whose meaning is defined by the caller; `None` is the default.
    pub dnd_level: Option<i32>,
    pub in_list: bool,
}

pub struct ConversationPrefRepo {
    db: Db,
    account_id: i64,
}

impl ConversationPrefRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    pub fn load_all(&self) -> Result<Vec<ConversationPref>> {
        self.db.query(
            "SELECT conv_key, dnd_level, in_list FROM conversation_pref WHERE account_id = ?1",
            [self.account_id],
            |r| {
                Ok(ConversationPref {
                    key: r.get(0)?,
                    dnd_level: r.get(1)?,
                    in_list: r.get(2)?,
                })
            },
        )
    }

    pub fn set(&self, pref: &ConversationPref) {
        self.db.write(
            "INSERT INTO conversation_pref(account_id, conv_key, dnd_level, in_list) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, conv_key) DO UPDATE SET
               dnd_level = excluded.dnd_level, in_list = excluded.in_list",
            params![self.account_id, pref.key, pref.dnd_level, pref.in_list],
        );
    }

    pub fn delete(&self, key: &str) {
        self.db.write(
            "DELETE FROM conversation_pref WHERE account_id = ?1 AND conv_key = ?2",
            params![self.account_id, key],
        );
    }
}

/// Cache of the message list: shows titles and previews at startup before messages arrive. Can be
/// discarded at any time and rebuilt from the messages on Zulip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationCache {
    pub key: String,
    /// The raw topic name seen most recently.
    pub title: String,
    pub last_message_id: i64,
    /// Unix seconds.
    pub last_message_time: i64,
    pub last_sender_id: i64,
    pub last_preview: String,
}

pub struct ConversationCacheRepo {
    db: Db,
    account_id: i64,
}

impl ConversationCacheRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    pub fn load_all(&self) -> Result<Vec<ConversationCache>> {
        self.db.query(
            "SELECT conv_key, title, last_message_id, last_message_time, last_sender_id, last_preview
             FROM conversation_cache WHERE account_id = ?1",
            [self.account_id],
            |r| {
                Ok(ConversationCache {
                    key: r.get(0)?,
                    title: r.get(1)?,
                    last_message_id: r.get(2)?,
                    last_message_time: r.get(3)?,
                    last_sender_id: r.get(4)?,
                    last_preview: r.get(5)?,
                })
            },
        )
    }

    pub fn set(&self, cache: &ConversationCache) {
        self.db.write(
            "INSERT INTO conversation_cache(account_id, conv_key, title, last_message_id, last_message_time, last_sender_id, last_preview)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account_id, conv_key) DO UPDATE SET
               title = excluded.title, last_message_id = excluded.last_message_id,
               last_message_time = excluded.last_message_time, last_sender_id = excluded.last_sender_id,
               last_preview = excluded.last_preview",
            params![
                self.account_id,
                cache.key,
                cache.title,
                cache.last_message_id,
                cache.last_message_time,
                cache.last_sender_id,
                cache.last_preview
            ],
        );
    }

    pub fn delete(&self, key: &str) {
        self.db.write(
            "DELETE FROM conversation_cache WHERE account_id = ?1 AND conv_key = ?2",
            params![self.account_id, key],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::add_account;

    #[test]
    fn prefs_and_cache_are_independent() {
        let db = Db::open_in_memory().unwrap();
        let account = add_account(&db);
        let prefs = ConversationPrefRepo::new(&db, account);
        let caches = ConversationCacheRepo::new(&db, account);
        let pref = ConversationPref {
            key: "t:1:a".into(),
            dnd_level: Some(2),
            in_list: true,
        };
        let cache = ConversationCache {
            key: "t:1:a".into(),
            title: "A".into(),
            last_message_id: 5,
            last_message_time: 6,
            last_sender_id: 7,
            last_preview: "p".into(),
        };
        prefs.set(&pref);
        caches.set(&cache);
        assert_eq!(prefs.load_all().unwrap(), std::slice::from_ref(&pref));
        assert_eq!(caches.load_all().unwrap(), std::slice::from_ref(&cache));

        let updated = ConversationCache {
            last_message_id: 9,
            ..cache
        };
        caches.set(&updated);
        assert_eq!(caches.load_all().unwrap(), [updated]);

        // Discarding the cache does not affect preferences
        caches.delete("t:1:a");
        assert!(caches.load_all().unwrap().is_empty());
        assert_eq!(prefs.load_all().unwrap(), [pref]);
        prefs.delete("t:1:a");
        assert!(prefs.load_all().unwrap().is_empty());
    }
}
