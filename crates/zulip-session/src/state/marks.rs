//! 备注和置顶：只保存在本机的标记。所有种类的目标共用一套读写。

use std::collections::HashMap;

use zulip_core::ConversationKey;
use zulip_core::model::{PinItem, PinScope, RemarkTarget};
use zulip_core::text::remark;
use zulip_store::{PinRepo, RemarkRepo};

/// 用户、频道、话题和多人私聊的备注。
pub struct Remarks {
    repo: RemarkRepo,
    entries: HashMap<RemarkTarget, String>,
}

impl Remarks {
    pub fn new(repo: RemarkRepo, entries: HashMap<RemarkTarget, String>) -> Self {
        Self { repo, entries }
    }

    pub fn get(&self, target: &RemarkTarget) -> Option<&str> {
        self.entries
            .get(&target.clone().canonical())
            .map(String::as_str)
    }

    pub fn of_user(&self, user_id: i64) -> Option<&str> {
        self.entries
            .get(&RemarkTarget::User(user_id))
            .map(String::as_str)
    }

    pub fn of_channel(&self, stream_id: i64) -> Option<&str> {
        self.entries
            .get(&RemarkTarget::Channel(stream_id))
            .map(String::as_str)
    }

    /// 返回是否有变化。备注去掉首尾空白，空白视为没有备注。
    pub fn set(&mut self, target: &RemarkTarget, text: Option<&str>) -> bool {
        let target = target.clone().canonical();
        let value = remark::normalize(text);
        if self.entries.get(&target) == value.as_ref() {
            return false;
        }
        self.repo.set(&target, value.as_deref());
        match value {
            Some(value) => self.entries.insert(target, value),
            None => self.entries.remove(&target),
        };
        true
    }

    /// 话题改名或整体移动后备注跟着走。目标已有备注时保留目标的。
    pub fn rekey(&mut self, old: &ConversationKey, new: &ConversationKey) {
        let old = RemarkTarget::Conversation(old.clone());
        let new = RemarkTarget::Conversation(new.clone());
        if old == new {
            return;
        }
        let Some(text) = self.entries.get(&old).cloned() else {
            return;
        };
        self.set(&old, None);
        if !self.entries.contains_key(&new) {
            self.set(&new, Some(&text));
        }
    }
}

/// 消息页和联系人页的置顶。
pub struct Pins {
    repo: PinRepo,
    entries: HashMap<(PinScope, PinItem), i64>,
}

impl Pins {
    pub fn new(repo: PinRepo, entries: HashMap<(PinScope, PinItem), i64>) -> Self {
        Self { repo, entries }
    }

    /// 置顶时间（Unix 毫秒）；没有置顶时为 `None`。
    pub fn get(&self, scope: PinScope, item: &PinItem) -> Option<i64> {
        self.entries.get(&(scope, item.clone())).copied()
    }

    /// 返回是否有变化。
    pub fn set(&mut self, scope: PinScope, item: &PinItem, pinned: bool, now: i64) -> bool {
        let entry = (scope, item.clone());
        if self.entries.contains_key(&entry) == pinned {
            return false;
        }
        if pinned {
            // 同一毫秒内连续置顶时也要保持先后顺序
            let at = self
                .entries
                .iter()
                .filter(|((s, _), _)| *s == scope)
                .map(|(_, at)| *at)
                .max()
                .map_or(now, |max| now.max(max + 1));
            self.repo.set(scope, item, Some(at));
            self.entries.insert(entry, at);
        } else {
            self.repo.set(scope, item, None);
            self.entries.remove(&entry);
        }
        true
    }

    /// 最新置顶的在前。
    pub fn items(&self, scope: PinScope) -> Vec<&PinItem> {
        let mut items: Vec<(&PinItem, i64)> = self
            .entries
            .iter()
            .filter(|((s, _), _)| *s == scope)
            .map(|((_, item), at)| (item, *at))
            .collect();
        items.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
        items.into_iter().map(|(item, _)| item).collect()
    }

    /// 话题改名后置顶跟着走，时间不变。目标已置顶时只去掉旧的。返回是否有变化。
    pub fn rekey(&mut self, old: &ConversationKey, new: &ConversationKey) -> bool {
        if old == new {
            return false;
        }
        let (old, new) = (
            PinItem::Conversation(old.clone()),
            PinItem::Conversation(new.clone()),
        );
        let mut changed = false;
        for scope in [PinScope::Messages, PinScope::Contacts] {
            let Some(at) = self.entries.remove(&(scope, old.clone())) else {
                continue;
            };
            changed = true;
            self.repo.set(scope, &old, None);
            if !self.entries.contains_key(&(scope, new.clone())) {
                self.repo.set(scope, &new, Some(at));
                self.entries.insert((scope, new.clone()), at);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use zulip_store::Db;

    use super::*;
    use crate::state::test_support::test_db;

    fn remarks(db: &Db, account: i64) -> Remarks {
        let repo = RemarkRepo::new(db, account);
        let entries = repo.load_all().unwrap();
        Remarks::new(repo, entries)
    }

    fn pins(db: &Db) -> Pins {
        let repo = PinRepo::new(db, 1);
        let entries = repo.load_all().unwrap();
        Pins::new(repo, entries)
    }

    #[test]
    fn remarks_are_normalized_and_saved() {
        let db = test_db();
        let mut r = remarks(&db, 1);
        let user = RemarkTarget::User(5);
        assert!(r.set(&user, Some("  小王  ")));
        assert_eq!(r.get(&user), Some("小王"));
        assert!(!r.set(&user, Some("小王")));
        assert!(r.set(&user, Some("   ")));
        assert_eq!(r.get(&user), None);
        r.set(&RemarkTarget::User(6), Some("老李"));
        assert_eq!(remarks(&db, 1).of_user(6), Some("老李"));
    }

    #[test]
    fn targets_of_different_kinds_are_separate() {
        let db = test_db();
        let mut r = remarks(&db, 1);
        r.set(&RemarkTarget::Channel(5), Some(" 频道 "));
        assert_eq!(r.of_channel(5), Some("频道"));
        assert_eq!(r.of_user(5), None);
    }

    #[test]
    fn one_to_one_dm_shares_the_users_remark() {
        let db = test_db();
        let mut r = remarks(&db, 1);
        let dm = RemarkTarget::Conversation(ConversationKey::dm([2], 1));
        r.set(&dm, Some("杰哥"));
        assert_eq!(r.of_user(2), Some("杰哥"));
        assert_eq!(r.get(&dm), Some("杰哥"));
        r.set(&RemarkTarget::User(2), Some("阿杰"));
        assert_eq!(r.get(&dm), Some("阿杰"));
    }

    #[test]
    fn remark_follows_the_topic() {
        let db = test_db();
        let mut r = remarks(&db, 1);
        let (a, b) = (
            ConversationKey::topic(1, "a"),
            ConversationKey::topic(1, "b"),
        );
        r.set(&RemarkTarget::Conversation(a.clone()), Some("备注"));
        r.rekey(&a, &b);
        assert_eq!(r.get(&RemarkTarget::Conversation(a)), None);
        assert_eq!(r.get(&RemarkTarget::Conversation(b)), Some("备注"));
    }

    #[test]
    fn pins_order_newest_first_even_in_same_millisecond() {
        let db = test_db();
        let mut p = pins(&db);
        let (a, b, c) = (PinItem::User(1), PinItem::User(2), PinItem::User(3));
        p.set(PinScope::Contacts, &a, true, 100);
        p.set(PinScope::Contacts, &b, true, 100);
        p.set(PinScope::Contacts, &c, true, 50);
        assert_eq!(p.items(PinScope::Contacts), [&c, &b, &a]);
        assert_eq!(pins(&db).items(PinScope::Contacts), [&c, &b, &a]);
    }

    #[test]
    fn pin_reports_change_only_when_state_changes() {
        let db = test_db();
        let mut p = pins(&db);
        let item = PinItem::User(1);
        assert!(p.set(PinScope::Contacts, &item, true, 100));
        assert!(!p.set(PinScope::Contacts, &item, true, 200));
        assert!(p.set(PinScope::Contacts, &item, false, 300));
        assert!(!p.set(PinScope::Contacts, &item, false, 400));
    }

    #[test]
    fn scopes_are_independent() {
        let db = test_db();
        let mut p = pins(&db);
        let topic = PinItem::Conversation(ConversationKey::topic(1, "接口"));
        p.set(PinScope::Messages, &topic, true, 1);
        assert!(p.get(PinScope::Messages, &topic).is_some());
        assert!(p.get(PinScope::Contacts, &topic).is_none());
        assert!(p.items(PinScope::Contacts).is_empty());
    }

    #[test]
    fn rekey_keeps_pin_time_in_every_scope() {
        let db = test_db();
        let mut p = pins(&db);
        let (a, b, new) = (
            ConversationKey::topic(1, "a"),
            ConversationKey::topic(1, "b"),
            ConversationKey::topic(1, "new"),
        );
        let item = |k: &ConversationKey| PinItem::Conversation(k.clone());
        p.set(PinScope::Messages, &item(&a), true, 1);
        p.set(PinScope::Contacts, &item(&a), true, 1);
        p.set(PinScope::Contacts, &item(&b), true, 2);
        assert!(p.rekey(&a, &new));
        assert!(p.get(PinScope::Messages, &item(&new)).is_some());
        assert_eq!(
            pins(&db).items(PinScope::Contacts),
            [&item(&b), &item(&new)]
        );
        assert!(!p.rekey(&a, &new));
        // 目标已置顶
        assert!(p.rekey(&b, &new));
        assert_eq!(p.items(PinScope::Contacts), [&item(&new)]);
    }
}
