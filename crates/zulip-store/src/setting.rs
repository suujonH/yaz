use std::collections::HashMap;

use rusqlite::params;

use crate::db::Db;

/// Key-value settings: app-level or per-account. Reads only touch memory (everything is read in
/// when the database is opened) and writes are queued for saving; neither blocks.
#[derive(Clone)]
pub struct SettingRepo {
    db: Db,
    account_id: Option<i64>,
}

impl SettingRepo {
    pub fn app(db: &Db) -> Self {
        Self {
            db: db.clone(),
            account_id: None,
        }
    }

    pub fn account(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id: Some(account_id),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.db
            .settings()
            .read()
            .get(&self.account_id)?
            .get(key)
            .cloned()
    }

    /// Holds the write lock until the write is queued: with concurrent writes to the same key, the
    /// order in memory and in the database is consistent.
    pub fn set(&self, key: &str, value: &str) {
        let mut settings = self.db.settings().write();
        settings
            .entry(self.account_id)
            .or_default()
            .insert(key.to_owned(), value.to_owned());
        self.db.write(
            "INSERT INTO setting(account_id, key, value) VALUES(?1, ?2, ?3)
             ON CONFLICT(COALESCE(account_id, 0), key) DO UPDATE SET value = excluded.value",
            params![self.account_id, key, value],
        );
    }

    /// Like [`set`](Self::set), holds the write lock until the write is queued.
    pub fn remove(&self, key: &str) {
        let mut settings = self.db.settings().write();
        if let Some(values) = settings.get_mut(&self.account_id) {
            values.remove(key);
        }
        self.db.write(
            "DELETE FROM setting WHERE account_id IS ?1 AND key = ?2",
            params![self.account_id, key],
        );
    }

    pub fn get_all(&self) -> HashMap<String, String> {
        self.db
            .settings()
            .read()
            .get(&self.account_id)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::add_account;

    #[test]
    fn app_and_account_settings_are_separate_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Db::open(&path).unwrap();
        let account = add_account(&db);
        let other = crate::account::tests::add_named_account(&db, "other@x");
        let app = SettingRepo::app(&db);
        let a = SettingRepo::account(&db, account);
        let b = SettingRepo::account(&db, other);

        assert_eq!(app.get("k"), None);
        app.set("k", "app");
        a.set("k", "a");
        a.set("k", "a2");
        a.set("gone", "x");
        a.remove("gone");
        assert_eq!(app.get("k").as_deref(), Some("app"));
        assert_eq!(a.get("k").as_deref(), Some("a2"));
        assert_eq!(b.get("k"), None);
        assert_eq!(a.get_all().len(), 1);

        drop((db, app, a, b));
        let db = Db::open(&path).unwrap();
        assert_eq!(SettingRepo::app(&db).get("k").as_deref(), Some("app"));
        assert_eq!(
            SettingRepo::account(&db, account).get("k").as_deref(),
            Some("a2")
        );
        assert_eq!(SettingRepo::account(&db, account).get("gone"), None);
    }

    #[test]
    fn concurrent_writes_leave_memory_and_database_equal() {
        let db = Db::open_in_memory().unwrap();
        let threads: Vec<_> = (0..8)
            .map(|thread| {
                let repo = SettingRepo::app(&db);
                std::thread::spawn(move || {
                    for i in 0..200 {
                        if thread == 0 && i % 2 == 0 {
                            repo.remove("k");
                        } else {
                            repo.set("k", &format!("{thread}-{i}"));
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let stored = db
            .query(
                "SELECT value FROM setting WHERE account_id IS NULL AND key = 'k'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(stored.first().cloned(), SettingRepo::app(&db).get("k"));
    }
}
