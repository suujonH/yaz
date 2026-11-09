use std::collections::HashMap;

use rusqlite::params;

use crate::db::Db;
use crate::error::Result;

/// 本地草稿，只在 Zulip 用户关闭了草稿同步时使用。会话键是会话的存储字符串。
pub struct LocalDraftRepo {
    db: Db,
    account_id: i64,
}

impl LocalDraftRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    /// 会话键 → (草稿内容, 保存时间)。
    pub fn load_all(&self) -> Result<HashMap<String, (String, i64)>> {
        let rows = self.db.query(
            "SELECT conv_key, content, updated_at FROM local_draft WHERE account_id = ?1",
            [self.account_id],
            |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))),
        )?;
        Ok(rows.into_iter().collect())
    }

    pub fn set(&self, conv_key: &str, content: &str, updated_at: i64) {
        self.db.write(
            "INSERT INTO local_draft(account_id, conv_key, content, updated_at) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, conv_key) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
            params![self.account_id, conv_key, content, updated_at],
        );
    }

    pub fn delete(&self, conv_key: &str) {
        self.db.write(
            "DELETE FROM local_draft WHERE account_id = ?1 AND conv_key = ?2",
            params![self.account_id, conv_key],
        );
    }

    pub fn delete_all(&self) {
        self.db.write(
            "DELETE FROM local_draft WHERE account_id = ?1",
            params![self.account_id],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::add_account;

    #[test]
    fn drafts_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let repo = LocalDraftRepo::new(&db, add_account(&db));
        repo.set("a", "一", 1);
        repo.set("a", "二", 2);
        repo.set("b", "三", 3);
        let all = repo.load_all().unwrap();
        assert_eq!(all["a"], ("二".to_owned(), 2));
        repo.delete("a");
        assert_eq!(repo.load_all().unwrap().len(), 1);
        repo.delete_all();
        assert!(repo.load_all().unwrap().is_empty());
    }
}
