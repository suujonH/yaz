//! 本机标记：备注和置顶。所有种类的目标共用一张表和一套读写。

use std::collections::HashMap;

use rusqlite::params;
use zulip_core::model::{PinItem, PinScope, RemarkTarget};

use crate::db::Db;
use crate::error::Result;

pub struct RemarkRepo {
    db: Db,
    account_id: i64,
}

impl RemarkRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    pub fn load_all(&self) -> Result<HashMap<RemarkTarget, String>> {
        let rows = self.db.query(
            "SELECT target_kind, target_id, remark FROM remark WHERE account_id = ?1",
            [self.account_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|(kind, id, remark)| {
                Some((RemarkTarget::from_storage(&kind, &id)?, remark))
            })
            .collect())
    }

    /// `None` 表示清除。
    pub fn set(&self, target: &RemarkTarget, remark: Option<&str>) {
        let (kind, id) = target.storage();
        match remark {
            Some(remark) => self.db.write(
                "INSERT INTO remark(account_id, target_kind, target_id, remark) VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(account_id, target_kind, target_id) DO UPDATE SET remark = excluded.remark",
                params![self.account_id, kind, id, remark],
            ),
            None => self.db.write(
                "DELETE FROM remark WHERE account_id = ?1 AND target_kind = ?2 AND target_id = ?3",
                params![self.account_id, kind, id],
            ),
        }
    }
}

pub struct PinRepo {
    db: Db,
    account_id: i64,
}

impl PinRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    /// (范围, 条目) → 置顶时间（Unix 毫秒）。
    pub fn load_all(&self) -> Result<HashMap<(PinScope, PinItem), i64>> {
        let rows = self.db.query(
            "SELECT scope, item_key, pinned_at FROM pin WHERE account_id = ?1",
            [self.account_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|(scope, item, at)| {
                Some(((PinScope::parse(&scope)?, item.parse().ok()?), at))
            })
            .collect())
    }

    /// `None` 表示取消置顶。
    pub fn set(&self, scope: PinScope, item: &PinItem, pinned_at: Option<i64>) {
        let key = item.to_string();
        match pinned_at {
            Some(at) => self.db.write(
                "INSERT INTO pin(account_id, scope, item_key, pinned_at) VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(account_id, scope, item_key) DO UPDATE SET pinned_at = excluded.pinned_at",
                params![self.account_id, scope.as_str(), key, at],
            ),
            None => self.db.write(
                "DELETE FROM pin WHERE account_id = ?1 AND scope = ?2 AND item_key = ?3",
                params![self.account_id, scope.as_str(), key],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use zulip_core::{ConversationFolder, ConversationKey};

    use super::*;
    use crate::account::tests::add_account;

    #[test]
    fn remarks_of_every_kind_share_one_path() {
        let db = Db::open_in_memory().unwrap();
        let repo = RemarkRepo::new(&db, add_account(&db));
        let user = RemarkTarget::User(1);
        let channel = RemarkTarget::Channel(1);
        let topic = RemarkTarget::Conversation(ConversationKey::topic(1, "接口"));
        repo.set(&user, Some("甲"));
        repo.set(&channel, Some("乙"));
        repo.set(&topic, Some("丙"));
        repo.set(&user, Some("丁"));
        let all = repo.load_all().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[&user], "丁");
        assert_eq!(all[&channel], "乙");
        assert_eq!(all[&topic], "丙");

        repo.set(&channel, None);
        assert_eq!(repo.load_all().unwrap().len(), 2);
    }

    #[test]
    fn pins_are_separate_per_scope() {
        let db = Db::open_in_memory().unwrap();
        let repo = PinRepo::new(&db, add_account(&db));
        let topic = PinItem::Conversation(ConversationKey::topic(1, "接口"));
        let folder = PinItem::Folder(ConversationFolder::Helper);
        let user = PinItem::User(9);
        repo.set(PinScope::Messages, &topic, Some(1));
        repo.set(PinScope::Contacts, &topic, Some(2));
        repo.set(PinScope::Messages, &folder, Some(3));
        repo.set(PinScope::Contacts, &user, Some(4));
        let all = repo.load_all().unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[&(PinScope::Messages, topic.clone())], 1);
        assert_eq!(all[&(PinScope::Contacts, topic.clone())], 2);
        assert_eq!(all[&(PinScope::Messages, folder)], 3);

        repo.set(PinScope::Messages, &topic, None);
        let all = repo.load_all().unwrap();
        assert!(!all.contains_key(&(PinScope::Messages, topic.clone())));
        assert!(all.contains_key(&(PinScope::Contacts, topic)));
    }
}
