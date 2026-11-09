use std::collections::HashMap;

use rusqlite::params;
use zulip_core::model::FriendGroup;

use crate::db::Db;
use crate::error::Result;

/// 好友分组与分组成员关系。一个用户最多属于一个自定义分组。
pub struct ContactRepo {
    db: Db,
    account_id: i64,
}

impl ContactRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    /// 按 sort_order 排列的自定义分组。
    pub fn load_groups(&self) -> Result<Vec<FriendGroup>> {
        self.db.query(
            "SELECT id, name FROM contact_group WHERE account_id = ?1 ORDER BY sort_order, id",
            [self.account_id],
            |r| {
                Ok(FriendGroup {
                    id: r.get(0)?,
                    name: r.get(1)?,
                })
            },
        )
    }

    /// 用户 id → 分组 id。
    pub fn load_membership(&self) -> Result<HashMap<i64, i64>> {
        let rows = self.db.query(
            "SELECT user_id, group_id FROM contact_group_member WHERE account_id = ?1",
            [self.account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(rows.into_iter().collect())
    }

    /// 新分组排在最后。
    pub fn add_group(&self, name: &str) -> Result<FriendGroup> {
        let id = self.db.with(|conn| {
            conn.prepare_cached(
                "INSERT INTO contact_group(account_id, name, sort_order)
                 VALUES(?1, ?2, (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM contact_group WHERE account_id = ?1))
                 RETURNING id",
            )?
            .query_row(params![self.account_id, name], |r| r.get(0))
        })?;
        Ok(FriendGroup {
            id,
            name: name.to_owned(),
        })
    }

    pub fn rename_group(&self, group_id: i64, name: &str) {
        self.db.write(
            "UPDATE contact_group SET name = ?3 WHERE account_id = ?1 AND id = ?2",
            params![self.account_id, group_id, name],
        );
    }

    /// 删除分组；成员回到“我的好友”（外键级联删除成员关系）。
    pub fn delete_group(&self, group_id: i64) {
        self.db.write(
            "DELETE FROM contact_group WHERE account_id = ?1 AND id = ?2",
            params![self.account_id, group_id],
        );
    }

    /// 把用户移到分组；`group_id` 为 `None` 表示移回“我的好友”。
    pub fn set_membership(&self, user_ids: &[i64], group_id: Option<i64>) {
        let account_id = self.account_id;
        let user_ids = user_ids.to_vec();
        self.db.submit(move |db| {
            db.with(|conn| {
                let tx = conn.transaction()?;
                for user in user_ids {
                    match group_id {
                        Some(group) => tx.execute(
                            "INSERT INTO contact_group_member(account_id, user_id, group_id) VALUES(?1, ?2, ?3)
                             ON CONFLICT(account_id, user_id) DO UPDATE SET group_id = excluded.group_id",
                            params![account_id, user, group],
                        )?,
                        None => tx.execute(
                            "DELETE FROM contact_group_member WHERE account_id = ?1 AND user_id = ?2",
                            params![account_id, user],
                        )?,
                    };
                }
                tx.commit()
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::{add_account, add_named_account};

    #[test]
    fn groups_keep_order_and_membership_follows() {
        let db = Db::open_in_memory().unwrap();
        let repo = ContactRepo::new(&db, add_account(&db));
        let a = repo.add_group("甲").unwrap();
        let b = repo.add_group("乙").unwrap();
        repo.rename_group(a.id, "丙");
        assert_eq!(
            repo.load_groups()
                .unwrap()
                .iter()
                .map(|g| g.name.as_str())
                .collect::<Vec<_>>(),
            ["丙", "乙"]
        );

        repo.set_membership(&[1, 2], Some(a.id));
        repo.set_membership(&[2], Some(b.id));
        assert_eq!(
            repo.load_membership().unwrap(),
            HashMap::from([(1, a.id), (2, b.id)])
        );
        repo.set_membership(&[1], None);
        assert_eq!(repo.load_membership().unwrap(), HashMap::from([(2, b.id)]));

        // 删除分组，成员回到“我的好友”
        repo.delete_group(b.id);
        assert!(repo.load_membership().unwrap().is_empty());
        assert_eq!(repo.load_groups().unwrap().len(), 1);
    }

    #[test]
    fn accounts_are_separate() {
        let db = Db::open_in_memory().unwrap();
        let first = ContactRepo::new(&db, add_account(&db));
        let second = ContactRepo::new(&db, add_named_account(&db, "b@x"));
        first.add_group("甲").unwrap();
        assert!(second.load_groups().unwrap().is_empty());
    }
}
