use rusqlite::{Row, params};

use crate::db::Db;
use crate::error::Result;

/// 保存到本机的文件记录（默认目录或另存为）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRecord {
    /// 以 `/user_uploads/` 开头的路径（不含域名）。
    pub upload_path: String,
    pub local_path: String,
    pub file_name: String,
    pub size: i64,
    pub message_id: i64,
    /// 所属会话的存储字符串。
    pub conv_key: String,
    /// Unix 秒。
    pub downloaded_at: i64,
    /// Unix 秒；从未打开为 `None`。
    pub last_opened_at: Option<i64>,
}

const COLUMNS: &str =
    "upload_path, local_path, file_name, size, message_id, conv_key, downloaded_at, last_opened_at";

fn map(r: &Row<'_>) -> rusqlite::Result<DownloadRecord> {
    Ok(DownloadRecord {
        upload_path: r.get(0)?,
        local_path: r.get(1)?,
        file_name: r.get(2)?,
        size: r.get(3)?,
        message_id: r.get(4)?,
        conv_key: r.get(5)?,
        downloaded_at: r.get(6)?,
        last_opened_at: r.get(7)?,
    })
}

pub struct DownloadRepo {
    db: Db,
    account_id: i64,
}

impl DownloadRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    pub fn find(&self, upload_path: &str) -> Result<Option<DownloadRecord>> {
        self.db.query_one(
            &format!(
                "SELECT {COLUMNS} FROM downloaded_file WHERE account_id = ?1 AND upload_path = ?2"
            ),
            params![self.account_id, upload_path],
            map,
        )
    }

    pub fn upsert(&self, r: &DownloadRecord) {
        self.db.write(
            "INSERT INTO downloaded_file(account_id, upload_path, local_path, file_name, size, message_id, conv_key,
               downloaded_at, last_opened_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(account_id, upload_path) DO UPDATE SET
               local_path = excluded.local_path, file_name = excluded.file_name, size = excluded.size,
               message_id = excluded.message_id, conv_key = excluded.conv_key, downloaded_at = excluded.downloaded_at,
               last_opened_at = excluded.last_opened_at",
            params![
                self.account_id,
                r.upload_path,
                r.local_path,
                r.file_name,
                r.size,
                r.message_id,
                r.conv_key,
                r.downloaded_at,
                r.last_opened_at
            ],
        );
    }

    pub fn mark_opened(&self, upload_path: &str, opened_at: i64) {
        self.db.write(
            "UPDATE downloaded_file SET last_opened_at = ?3 WHERE account_id = ?1 AND upload_path = ?2",
            params![self.account_id, upload_path, opened_at],
        );
    }

    pub fn delete(&self, upload_path: &str) {
        self.db.write(
            "DELETE FROM downloaded_file WHERE account_id = ?1 AND upload_path = ?2",
            params![self.account_id, upload_path],
        );
    }

    /// 最近使用：按最近打开时间（没有则按下载时间）降序。
    pub fn recent(&self, limit: usize) -> Result<Vec<DownloadRecord>> {
        self.db.query(
            &format!(
                "SELECT {COLUMNS} FROM downloaded_file WHERE account_id = ?1
                 ORDER BY COALESCE(last_opened_at, downloaded_at) DESC LIMIT ?2"
            ),
            params![self.account_id, limit as i64],
            map,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str, local: &str, at: i64) -> DownloadRecord {
        DownloadRecord {
            upload_path: path.into(),
            local_path: local.into(),
            file_name: "f".into(),
            size: 1,
            message_id: 1,
            conv_key: "d:9".into(),
            downloaded_at: at,
            last_opened_at: None,
        }
    }

    #[test]
    fn recent_orders_by_last_use() {
        let db = Db::open_in_memory().unwrap();
        let repo = DownloadRepo::new(&db, crate::account::tests::add_account(&db));
        repo.upsert(&record("/user_uploads/a", r"C:\a", 100));
        repo.upsert(&record("/user_uploads/b", r"C:\b", 200));
        let paths = |limit| {
            repo.recent(limit)
                .unwrap()
                .into_iter()
                .map(|r| r.upload_path)
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(10), ["/user_uploads/b", "/user_uploads/a"]);

        repo.mark_opened("/user_uploads/a", 300);
        assert_eq!(paths(10), ["/user_uploads/a", "/user_uploads/b"]);
        assert_eq!(paths(1), ["/user_uploads/a"]);
        assert_eq!(
            repo.find("/user_uploads/a")
                .unwrap()
                .unwrap()
                .last_opened_at,
            Some(300)
        );

        repo.upsert(&record("/user_uploads/a", r"D:\moved", 400));
        assert_eq!(
            repo.find("/user_uploads/a").unwrap().unwrap().local_path,
            r"D:\moved"
        );
        assert!(DownloadRepo::new(&db, 2).recent(10).unwrap().is_empty());

        repo.delete("/user_uploads/a");
        assert!(repo.find("/user_uploads/a").unwrap().is_none());
    }
}
