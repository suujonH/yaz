//! Per-message attached content (translations, analysis results etc.) that a plugin keeps in its
//! data directory: a SQLite database, whose table stores text (usually the JSON of
//! `set_annotation`) keyed by (account, message id). The same database may also hold the plugin's
//! own tables (`extra_schema`); the plugin thread and worker threads can access it at the same
//! time.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use rusqlite::{Connection, params, params_from_iter};

use crate::abi::LOG_WARNING;
use crate::{data_dir, log};

pub struct AnnotationStore {
    connection: Mutex<Connection>,
    table: &'static str,
}

impl AnnotationStore {
    /// Opens the database file under the data directory (creating it if missing) and creates the
    /// attached-content table `table` (`account`, `message_id`, `content`, `updated_at`, primary
    /// key the first two columns) and `extra_schema`. Returns `None` when there is no data
    /// directory; when the database cannot be opened, writes to the plugin log and returns `None`.
    pub fn open(file_name: &str, table: &'static str, extra_schema: &str) -> Option<Self> {
        let dir = data_dir().ok()?;
        let path = Path::new(&dir).join(file_name);
        let opened = Connection::open(path).and_then(|c| Self::create(c, table, extra_schema));
        match opened {
            Ok(store) => Some(store),
            Err(error) => {
                log(LOG_WARNING, &format!("打开数据库失败：{error}"));
                None
            }
        }
    }

    /// In-memory database (for tests).
    pub fn open_in_memory(table: &'static str, extra_schema: &str) -> rusqlite::Result<Self> {
        Self::create(Connection::open_in_memory()?, table, extra_schema)
    }

    fn create(
        connection: Connection,
        table: &'static str,
        extra_schema: &str,
    ) -> rusqlite::Result<Self> {
        connection.execute_batch(&format!(
            "{extra_schema}
CREATE TABLE IF NOT EXISTS {table}(
    account INTEGER NOT NULL, message_id INTEGER NOT NULL, content TEXT NOT NULL, updated_at INTEGER NOT NULL,
    PRIMARY KEY(account, message_id));
CREATE TABLE IF NOT EXISTS {table}_hidden(
    account INTEGER NOT NULL, message_id INTEGER NOT NULL, PRIMARY KEY(account, message_id));"
        ))?;
        Ok(Self {
            connection: Mutex::new(connection),
            table,
        })
    }

    /// Runs a query or update while holding the connection (to access tables in `extra_schema`).
    pub fn with<T>(
        &self,
        run: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        run(&self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner))
    }

    /// Saves the content of a message, overwriting any existing one.
    pub fn save(&self, account: i64, message_id: i64, content: &str) -> rusqlite::Result<()> {
        let sql = format!(
            "INSERT INTO {}(account, message_id, content, updated_at) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(account, message_id) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
            self.table
        );
        self.with(|db| {
            db.execute(
                &sql,
                params![account, message_id, content, chrono::Utc::now().timestamp()],
            )
            .map(drop)
        })
    }

    /// Among these messages, those with saved content: (message id, content), in no particular
    /// order.
    pub fn load(&self, account: i64, message_ids: &[i64]) -> rusqlite::Result<Vec<(i64, String)>> {
        if message_ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; message_ids.len()].join(",");
        let sql = format!(
            "SELECT message_id, content FROM {} WHERE account = ? AND message_id IN ({marks})",
            self.table
        );
        let args = std::iter::once(account).chain(message_ids.iter().copied());
        self.with(|db| {
            db.prepare(&sql)?
                .query_map(params_from_iter(args), |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })
    }

    /// Like `load`, without the ones the user removed from below the message (`set_hidden`).
    pub fn load_visible(
        &self,
        account: i64,
        message_ids: &[i64],
    ) -> rusqlite::Result<Vec<(i64, String)>> {
        let mut rows = self.load(account, message_ids)?;
        if rows.is_empty() {
            return Ok(rows);
        }
        let sql = format!(
            "SELECT 1 FROM {}_hidden WHERE account = ?1 AND message_id = ?2",
            self.table
        );
        let hidden = self.with(|db| {
            let mut query = db.prepare(&sql)?;
            let mut hidden = Vec::new();
            for (id, _) in &rows {
                if query.exists(params![account, id])? {
                    hidden.push(*id);
                }
            }
            Ok(hidden)
        })?;
        rows.retain(|(id, _)| !hidden.contains(id));
        Ok(rows)
    }

    /// Whether the user removed the content from below the message (it stays saved, and `load`
    /// still returns it).
    pub fn set_hidden(&self, account: i64, message_id: i64, hidden: bool) -> rusqlite::Result<()> {
        let sql = if hidden {
            format!(
                "INSERT OR IGNORE INTO {}_hidden(account, message_id) VALUES(?1, ?2)",
                self.table
            )
        } else {
            format!(
                "DELETE FROM {}_hidden WHERE account = ?1 AND message_id = ?2",
                self.table
            )
        };
        self.with(|db| db.execute(&sql, params![account, message_id]).map(drop))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_per_account_and_replaces() {
        let store = AnnotationStore::open_in_memory("note", "").unwrap();
        store.save(1, 10, "旧").unwrap();
        store.save(1, 10, "新").unwrap();
        store.save(2, 10, "别的账号").unwrap();
        store.save(1, 11, "b").unwrap();
        let mut rows = store.load(1, &[10, 11, 12]).unwrap();
        rows.sort();
        assert_eq!(rows, vec![(10, "新".to_owned()), (11, "b".to_owned())]);
        assert!(store.load(1, &[]).unwrap().is_empty());
    }

    #[test]
    fn hidden_are_left_out_of_visible() {
        let store = AnnotationStore::open_in_memory("note", "").unwrap();
        store.save(1, 10, "a").unwrap();
        store.save(1, 11, "b").unwrap();
        store.set_hidden(1, 10, true).unwrap();
        store.set_hidden(1, 10, true).unwrap();
        store.set_hidden(2, 11, true).unwrap();
        assert_eq!(
            store.load_visible(1, &[10, 11]).unwrap(),
            vec![(11, "b".to_owned())]
        );
        assert_eq!(store.load(1, &[10]).unwrap().len(), 1);
        store.set_hidden(1, 10, false).unwrap();
        assert_eq!(store.load_visible(1, &[10]).unwrap().len(), 1);
    }

    #[test]
    fn extra_schema_shares_the_connection() {
        let store = AnnotationStore::open_in_memory(
            "note",
            "CREATE TABLE IF NOT EXISTS other(id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        store
            .with(|db| db.execute("INSERT INTO other(id) VALUES(1)", []).map(drop))
            .unwrap();
        let count: i64 = store
            .with(|db| db.query_row("SELECT COUNT(*) FROM other", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(count, 1);
    }
}
