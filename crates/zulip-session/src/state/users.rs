use std::collections::{HashMap, HashSet};

use serde_json::Value;
use zulip_api::{PresenceDto, RegisterResult};
use zulip_core::model::{PresenceState, ZulipUser};
use zulip_core::permission::UserGroup;

use crate::convert::user_from_dto;

const UNKNOWN_USER: &str = "未知用户";
const NOTIFICATION_BOT_PREFIX: &str = "notification-bot@";

/// 用户、在线状态、个性签名和静音用户。
#[derive(Default)]
pub struct Users {
    pub self_id: i64,
    by_id: HashMap<i64, ZulipUser>,
    pub notification_bot_id: Option<i64>,
    presences: HashMap<i64, PresenceDto>,
    pub presence_last_update_id: i64,
    offline_threshold: i64,
    statuses: HashMap<i64, String>,
    muted: HashSet<i64>,
    pub groups: HashMap<i64, UserGroup>,
}

impl Users {
    pub fn load(reg: &RegisterResult) -> Self {
        let mut by_id = HashMap::new();
        for dto in reg
            .realm_users
            .iter()
            .chain(&reg.realm_non_active_users)
            .chain(&reg.cross_realm_bots)
        {
            by_id.insert(dto.user_id, user_from_dto(dto));
        }
        let statuses = reg
            .user_status
            .iter()
            .filter_map(|(id, s)| Some((*id, s.status_text.clone().filter(|t| !t.is_empty())?)))
            .collect();
        Self {
            self_id: reg.user_id,
            by_id,
            notification_bot_id: reg
                .cross_realm_bots
                .iter()
                .find(|u| u.email.starts_with(NOTIFICATION_BOT_PREFIX))
                .map(|u| u.user_id),
            presences: reg.presences.clone(),
            presence_last_update_id: reg.presence_last_update_id,
            offline_threshold: i64::from(reg.server_presence_offline_threshold_seconds),
            statuses,
            muted: reg.muted_users.iter().map(|m| m.id).collect(),
            groups: reg
                .realm_user_groups
                .iter()
                .map(|g| (g.id, g.clone()))
                .collect(),
        }
    }

    pub fn get(&self, id: i64) -> Option<&ZulipUser> {
        self.by_id.get(&id)
    }

    pub fn all(&self) -> impl Iterator<Item = &ZulipUser> {
        self.by_id.values()
    }

    pub fn name_of(&self, id: i64) -> &str {
        self.get(id).map_or(UNKNOWN_USER, |u| u.full_name.as_str())
    }

    pub fn is_muted(&self, id: i64) -> bool {
        self.muted.contains(&id)
    }

    pub fn muted_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.muted.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn status_text(&self, id: i64) -> Option<&str> {
        self.statuses.get(&id).map(String::as_str)
    }

    pub fn presence(&self, id: i64, now: i64) -> PresenceState {
        let Some(p) = self.presences.get(&id) else {
            return PresenceState::Offline;
        };
        if now - p.active_timestamp <= self.offline_threshold {
            PresenceState::Online
        } else if now - p.idle_timestamp <= self.offline_threshold {
            PresenceState::Idle
        } else {
            PresenceState::Offline
        }
    }

    /// 合并服务器返回的在线状态，返回有变化的用户。
    pub fn apply_presences(&mut self, presences: &HashMap<i64, PresenceDto>) -> Vec<i64> {
        let mut changed = Vec::new();
        for (id, value) in presences {
            if self.presences.get(id) != Some(value) {
                self.presences.insert(*id, value.clone());
                changed.push(*id);
            }
        }
        changed.sort_unstable();
        changed
    }

    pub fn set_status(&mut self, id: i64, text: Option<&str>) {
        match text.filter(|t| !t.is_empty()) {
            Some(text) => self.statuses.insert(id, text.to_owned()),
            None => self.statuses.remove(&id),
        };
    }

    pub fn set_muted(&mut self, ids: impl IntoIterator<Item = i64>) {
        self.muted = ids.into_iter().collect();
    }

    pub fn upsert(&mut self, user: ZulipUser) {
        self.by_id.insert(user.id, user);
    }

    pub fn apply_update(&mut self, person: &Value) -> Option<i64> {
        let id = person.get("user_id")?.as_i64()?;
        self.by_id.get_mut(&id)?.apply_update(person);
        Some(id)
    }

    pub fn deactivate(&mut self, id: i64) {
        if let Some(user) = self.by_id.get_mut(&id) {
            user.is_active = false;
        }
    }

    /// `user_group` 事件。
    pub fn apply_group_event(&mut self, event: &Value) {
        let group_id = event.get("group_id").and_then(Value::as_i64).unwrap_or(0);
        let ids = |name: &str| -> Vec<i64> {
            event
                .get(name)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default()
        };
        match event.get("op").and_then(Value::as_str).unwrap_or_default() {
            "add" => {
                if let Some(group) = event
                    .get("group")
                    .and_then(|g| serde_json::from_value::<UserGroup>(g.clone()).ok())
                {
                    self.groups.insert(group.id, group);
                }
            }
            "remove" => {
                self.groups.remove(&group_id);
            }
            "update" => {
                if let Some(group) = self.groups.get_mut(&group_id)
                    && let Some(data) = event.get("data")
                {
                    if let Some(name) = data.get("name").and_then(Value::as_str) {
                        group.name = name.to_owned();
                    }
                    if let Some(text) = data.get("description").and_then(Value::as_str) {
                        group.description = text.to_owned();
                    }
                    if let Some(flag) = data.get("deactivated").and_then(Value::as_bool) {
                        group.deactivated = flag;
                    }
                }
            }
            "add_members" => {
                if let Some(group) = self.groups.get_mut(&group_id) {
                    group.members.extend(ids("user_ids"));
                }
            }
            "remove_members" => {
                let removed = ids("user_ids");
                if let Some(group) = self.groups.get_mut(&group_id) {
                    group.members.retain(|m| !removed.contains(m));
                }
            }
            "add_subgroups" => {
                if let Some(group) = self.groups.get_mut(&group_id) {
                    group.direct_subgroup_ids.extend(ids("direct_subgroup_ids"));
                }
            }
            "remove_subgroups" => {
                let removed = ids("direct_subgroup_ids");
                if let Some(group) = self.groups.get_mut(&group_id) {
                    group.direct_subgroup_ids.retain(|m| !removed.contains(m));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn users() -> Users {
        let reg: RegisterResult = serde_json::from_value(json!({
            "user_id": 1,
            "realm_users": [{"user_id": 1, "full_name": "我"}, {"user_id": 2, "full_name": "阿杰"}],
            "cross_realm_bots": [{"user_id": 9, "email": "notification-bot@x", "full_name": "Bot", "is_bot": true}],
            "presences": {"2": {"active_timestamp": 1000, "idle_timestamp": 1100}},
            "server_presence_offline_threshold_seconds": 140,
            "user_status": {"2": {"status_text": "忙"}},
            "muted_users": [{"id": 2, "timestamp": 1}],
        }))
        .unwrap();
        Users::load(&reg)
    }

    #[test]
    fn load_finds_bot_status_and_muted() {
        let u = users();
        assert_eq!(u.notification_bot_id, Some(9));
        assert_eq!(u.status_text(2), Some("忙"));
        assert!(u.is_muted(2));
        assert_eq!(u.name_of(77), "未知用户");
    }

    #[test]
    fn presence_uses_threshold() {
        let u = users();
        assert_eq!(u.presence(2, 1100), PresenceState::Online);
        assert_eq!(u.presence(2, 1200), PresenceState::Idle);
        assert_eq!(u.presence(2, 2000), PresenceState::Offline);
        assert_eq!(u.presence(1, 2000), PresenceState::Offline);
    }

    #[test]
    fn group_membership_events() {
        let mut u = users();
        u.apply_group_event(&json!({"op": "add", "group": {"id": 5, "name": "g", "members": [1]}}));
        u.apply_group_event(&json!({"op": "add_members", "group_id": 5, "user_ids": [2]}));
        u.apply_group_event(&json!({"op": "remove_members", "group_id": 5, "user_ids": [1]}));
        assert_eq!(u.groups[&5].members, [2]);
        u.apply_group_event(&json!({"op": "remove", "group_id": 5}));
        assert!(u.groups.is_empty());
    }
}
