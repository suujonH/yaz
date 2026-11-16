//! Data kept for offline viewing: the server's last `register` response and a copy of every message
//! received. Both are caches of Zulip data, used to show the account while it cannot log in, and are
//! replaced by the server's data once online.

use rusqlite::params;

use crate::db::Db;
use crate::error::Result;

/// The server's last `register` response (raw JSON), one per account.
pub struct RegisterSnapshotRepo {
    db: Db,
    account_id: i64,
}

impl RegisterSnapshotRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    /// Replaces the saved response. `saved_at`: Unix seconds.
    pub fn save(&self, json: String, saved_at: i64) {
        self.db.write(
            "INSERT INTO register_snapshot(account_id, json, saved_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(account_id) DO UPDATE SET json = excluded.json, saved_at = excluded.saved_at",
            params![self.account_id, json, saved_at],
        );
    }

    pub fn load(&self) -> Result<Option<String>> {
        self.db.query_one(
            "SELECT json FROM register_snapshot WHERE account_id = ?1",
            [self.account_id],
            |r| r.get(0),
        )
    }
}

/// One stored message: the storage string of its conversation and the message as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub message_id: i64,
    pub conv_key: String,
    pub json: String,
}

/// Where a page of stored messages starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredAnchor {
    /// The newest messages.
    Newest,
    /// Around a message id: `before` messages with smaller ids, `after` with larger ids, and the
    /// anchor itself when `include` is true.
    Id { id: i64, include: bool },
}

/// Copies of the messages received (the latest state known: edits, reactions, flags, deletion).
pub struct StoredMessageRepo {
    db: Db,
    account_id: i64,
}

impl StoredMessageRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    /// Inserts or replaces messages (queued, in one transaction).
    pub fn put(&self, messages: Vec<StoredMessage>) {
        if messages.is_empty() {
            return;
        }
        let account_id = self.account_id;
        self.db.submit(move |db| {
            db.with(|conn| {
                let tx = conn.transaction()?;
                {
                    let mut statement = tx.prepare_cached(
                        "INSERT INTO stored_message(account_id, message_id, conv_key, json) VALUES(?1, ?2, ?3, ?4)
                         ON CONFLICT(account_id, message_id) DO UPDATE SET conv_key = excluded.conv_key, json = excluded.json",
                    )?;
                    for m in &messages {
                        statement.execute(params![account_id, m.message_id, m.conv_key, m.json])?;
                    }
                }
                tx.commit()
            })
        });
    }

    /// Changes a stored message on the database thread: `update` receives its JSON and returns the
    /// new conversation storage string and JSON, or `None` to leave it. Nothing happens when the
    /// message is not stored.
    pub fn update(
        &self,
        message_id: i64,
        update: impl FnOnce(&str) -> Option<(String, String)> + Send + 'static,
    ) {
        let account_id = self.account_id;
        self.db.submit(move |db| {
            let json: Option<String> = db.query_one(
                "SELECT json FROM stored_message WHERE account_id = ?1 AND message_id = ?2",
                params![account_id, message_id],
                |r| r.get(0),
            )?;
            if let Some((conv_key, json)) = json.as_deref().and_then(update) {
                db.execute(
                    "UPDATE stored_message SET conv_key = ?3, json = ?4 WHERE account_id = ?1 AND message_id = ?2",
                    params![account_id, message_id, conv_key, json],
                )?;
            }
            Ok(())
        });
    }

    pub fn remove(&self, message_id: i64) {
        self.db.write(
            "DELETE FROM stored_message WHERE account_id = ?1 AND message_id = ?2",
            params![self.account_id, message_id],
        );
    }

    /// Up to `limit` stored messages older than `before` (all when `None`), newest first, of one
    /// conversation or (`conv_key` `None`) of all: `(message id, JSON)`.
    pub fn older(
        &self,
        conv_key: Option<&str>,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<(i64, String)>> {
        self.db.query(
            "SELECT message_id, json FROM stored_message
             WHERE account_id = ?1 AND (?2 IS NULL OR conv_key = ?2) AND message_id < ?3
             ORDER BY message_id DESC LIMIT ?4",
            params![self.account_id, conv_key, before.unwrap_or(i64::MAX), limit],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    /// A page of one conversation's messages in ascending id order. Returns the messages and
    /// whether the oldest and the newest stored ones were reached.
    pub fn page(
        &self,
        conv_key: &str,
        anchor: StoredAnchor,
        before: u32,
        after: u32,
    ) -> Result<(Vec<String>, bool, bool)> {
        let (anchor_id, include) = match anchor {
            StoredAnchor::Newest => (i64::MAX, true),
            StoredAnchor::Id { id, include } => (id, include),
        };
        let mut older: Vec<String> = self.db.query(
            "SELECT json FROM stored_message WHERE account_id = ?1 AND conv_key = ?2 AND message_id < ?3
             ORDER BY message_id DESC LIMIT ?4",
            params![self.account_id, conv_key, anchor_id, i64::from(before) + 1],
            |r| r.get(0),
        )?;
        let found_oldest = older.len() <= before as usize;
        older.truncate(before as usize);
        older.reverse();
        if include {
            older.extend(self.db.query_one(
                "SELECT json FROM stored_message WHERE account_id = ?1 AND conv_key = ?2 AND message_id = ?3",
                params![self.account_id, conv_key, anchor_id],
                |r| r.get(0),
            )?);
        }
        let mut newer: Vec<String> = if matches!(anchor, StoredAnchor::Newest) {
            Vec::new()
        } else {
            self.db.query(
                "SELECT json FROM stored_message WHERE account_id = ?1 AND conv_key = ?2 AND message_id > ?3
                 ORDER BY message_id LIMIT ?4",
                params![self.account_id, conv_key, anchor_id, i64::from(after) + 1],
                |r| r.get(0),
            )?
        };
        let found_newest = newer.len() <= after as usize;
        newer.truncate(after as usize);
        older.extend(newer);
        Ok((older, found_oldest, found_newest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::add_account;

    fn message(id: i64, conv_key: &str) -> StoredMessage {
        StoredMessage {
            message_id: id,
            conv_key: conv_key.into(),
            json: format!("m{id}"),
        }
    }

    #[test]
    fn snapshot_is_replaced() {
        let db = Db::open_in_memory().unwrap();
        let repo = RegisterSnapshotRepo::new(&db, add_account(&db));
        assert_eq!(repo.load().unwrap(), None);
        repo.save("a".into(), 1);
        repo.save("b".into(), 2);
        assert_eq!(repo.load().unwrap().as_deref(), Some("b"));
    }

    #[test]
    fn pages_follow_the_anchor() {
        let db = Db::open_in_memory().unwrap();
        let repo = StoredMessageRepo::new(&db, add_account(&db));
        repo.put((1..=5).map(|id| message(id, "t:1:a")).collect());
        repo.put(vec![message(6, "t:1:b")]);
        let ids = |page: (Vec<String>, bool, bool)| (page.0.join(","), page.1, page.2);

        assert_eq!(
            ids(repo.page("t:1:a", StoredAnchor::Newest, 3, 0).unwrap()),
            ("m3,m4,m5".into(), false, true)
        );
        assert_eq!(
            ids(repo
                .page(
                    "t:1:a",
                    StoredAnchor::Id {
                        id: 3,
                        include: false
                    },
                    5,
                    0
                )
                .unwrap()),
            ("m1,m2".into(), true, false)
        );
        assert_eq!(
            ids(repo
                .page(
                    "t:1:a",
                    StoredAnchor::Id {
                        id: 3,
                        include: true
                    },
                    1,
                    1
                )
                .unwrap()),
            ("m2,m3,m4".into(), false, false)
        );
    }

    #[test]
    fn older_pages_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let repo = StoredMessageRepo::new(&db, add_account(&db));
        repo.put((1..=4).map(|id| message(id, "t:1:a")).collect());
        repo.put(vec![message(5, "t:1:b")]);
        let ids = |rows: Vec<(i64, String)>| rows.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        assert_eq!(ids(repo.older(None, None, 3).unwrap()), vec![5, 4, 3]);
        assert_eq!(
            ids(repo.older(Some("t:1:a"), Some(3), 9).unwrap()),
            vec![2, 1]
        );
    }

    #[test]
    fn update_and_remove() {
        let db = Db::open_in_memory().unwrap();
        let repo = StoredMessageRepo::new(&db, add_account(&db));
        repo.put(vec![message(1, "t:1:a"), message(2, "t:1:a")]);
        repo.update(1, |json| Some(("t:1:b".into(), format!("{json}!"))));
        repo.update(9, |_| Some(("x".into(), "x".into())));
        repo.remove(2);
        assert_eq!(
            repo.page("t:1:b", StoredAnchor::Newest, 9, 0).unwrap().0,
            vec!["m1!".to_owned()]
        );
        assert!(
            repo.page("t:1:a", StoredAnchor::Newest, 9, 0)
                .unwrap()
                .0
                .is_empty()
        );
    }
}
