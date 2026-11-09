use std::collections::HashMap;

use zulip_core::model::FriendGroup;
use zulip_store::ContactRepo;

use crate::error::{Result, SessionError};

/// 本地的好友分组。这些数据只保存在本机。
pub struct Contacts {
    repo: ContactRepo,
    groups: Vec<FriendGroup>,
    membership: HashMap<i64, i64>,
}

impl Contacts {
    pub fn new(repo: ContactRepo, groups: Vec<FriendGroup>, membership: HashMap<i64, i64>) -> Self {
        Self {
            repo,
            groups,
            membership,
        }
    }

    pub fn groups(&self) -> &[FriendGroup] {
        &self.groups
    }

    pub fn group_of(&self, user_id: i64) -> Option<i64> {
        self.membership.get(&user_id).copied()
    }

    /// 数据库中已创建的分组（新分组的 id 由数据库生成）。
    pub fn push_group(&mut self, group: FriendGroup) {
        self.groups.push(group);
    }

    pub fn rename_group(&mut self, id: i64, name: &str) {
        self.repo.rename_group(id, name);
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
            name.clone_into(&mut group.name);
        }
    }

    pub fn delete_group(&mut self, id: i64) {
        self.repo.delete_group(id);
        self.groups.retain(|g| g.id != id);
        self.membership.retain(|_, group| *group != id);
    }

    /// 把用户移入分组；`None` 移回“我的好友”。
    pub fn move_users(&mut self, user_ids: &[i64], group: Option<i64>) -> Result<()> {
        if let Some(id) = group
            && !self.groups.iter().any(|g| g.id == id)
        {
            return Err(SessionError::invalid("分组不存在"));
        }
        self.repo.set_membership(user_ids, group);
        for user in user_ids {
            match group {
                Some(id) => self.membership.insert(*user, id),
                None => self.membership.remove(user),
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use zulip_store::Db;

    use super::*;
    use crate::state::test_support::test_db;

    fn open(db: &Db) -> Contacts {
        let repo = ContactRepo::new(db, 1);
        let groups = repo.load_groups().unwrap();
        let membership = repo.load_membership().unwrap();
        Contacts::new(repo, groups, membership)
    }

    #[test]
    fn groups_and_membership() {
        let db = test_db();
        let mut c = open(&db);
        let group = ContactRepo::new(&db, 1).add_group("同事").unwrap();
        c.push_group(group.clone());
        c.rename_group(group.id, "朋友");
        assert_eq!(c.groups()[0].name, "朋友");
        c.move_users(&[1, 2], Some(group.id)).unwrap();
        assert_eq!(c.group_of(1), Some(group.id));
        assert!(c.move_users(&[3], Some(999)).is_err());
        c.move_users(&[1], None).unwrap();
        assert_eq!(c.group_of(1), None);
        assert_eq!(open(&db).group_of(2), Some(group.id));
        c.delete_group(group.id);
        assert_eq!(c.group_of(2), None);
        assert!(open(&db).groups().is_empty());
    }
}
