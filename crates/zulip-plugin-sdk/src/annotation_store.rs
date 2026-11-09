//! 插件在数据目录中保存的按消息附加的内容（译文、分析结果等）：一个 SQLite 数据库，
//! 其中的表按（账号，消息 id）保存文字（通常是 `set_annotation` 的 JSON）。
//! 同一个数据库可以再放插件自己的表（`extra_schema`）；插件线程与工作线程可以同时访问。

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
    /// 打开（不存在时创建）数据目录下的数据库文件，并创建附加内容的表 `table`
    /// （`account`、`message_id`、`content`、`updated_at`，主键为前两列）和 `extra_schema`。
    /// 没有数据目录时返回 `None`；数据库打不开时写入插件日志并返回 `None`。
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

    /// 内存数据库（测试用）。
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
    PRIMARY KEY(account, message_id));"
        ))?;
        Ok(Self {
            connection: Mutex::new(connection),
            table,
        })
    }

    /// 在持有连接的情况下执行查询或更新（访问 `extra_schema` 中的表）。
    pub fn with<T>(
        &self,
        run: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        run(&self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner))
    }

    /// 保存一条消息的内容，已有时覆盖。
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

    /// 这些消息中保存过内容的：(消息 id, 内容)，顺序不定。
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
