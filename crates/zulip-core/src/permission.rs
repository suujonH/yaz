//! Zulip 组设置与当前用户的权限判断。没有权限的功能在界面上完全不显示。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

pub const ROLE_OWNER: i32 = 100;
pub const ROLE_ADMINISTRATOR: i32 = 200;
pub const ROLE_MODERATOR: i32 = 300;
pub const ROLE_MEMBER: i32 = 400;
pub const ROLE_GUEST: i32 = 600;

/// 角色名；普通成员、不认识的角色为 `None`（名片上不显示）。
pub const fn special_role_label(role: i32) -> Option<&'static str> {
    match role {
        ROLE_OWNER => Some("所有者"),
        ROLE_ADMINISTRATOR => Some("管理员"),
        ROLE_MODERATOR => Some("版主"),
        role if role >= ROLE_GUEST => Some("访客"),
        _ => None,
    }
}

/// 角色名，普通成员是“成员”，不认识的角色是“未知”。
pub const fn role_label(role: i32) -> &'static str {
    match special_role_label(role) {
        Some(label) => label,
        None if role == ROLE_MEMBER => "成员",
        None => "未知",
    }
}

/// 组设置值：一个用户组 id，或匿名组（直接成员 + 直接子组）。
/// JSON 中分别是数字和 `{"direct_members":[…],"direct_subgroups":[…]}`。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GroupSettingValue {
    Group(i64),
    Anonymous {
        #[serde(default)]
        direct_members: Vec<i64>,
        #[serde(default)]
        direct_subgroups: Vec<i64>,
    },
}

impl GroupSettingValue {
    pub fn anonymous(members: impl Into<Vec<i64>>, subgroups: impl Into<Vec<i64>>) -> Self {
        Self::Anonymous {
            direct_members: members.into(),
            direct_subgroups: subgroups.into(),
        }
    }

    /// 匿名组的成员与子组忽略顺序和重复。
    fn normalized(&self) -> (Option<i64>, Vec<i64>, Vec<i64>) {
        fn sorted(ids: &[i64]) -> Vec<i64> {
            let mut ids = ids.to_vec();
            ids.sort_unstable();
            ids.dedup();
            ids
        }
        match self {
            Self::Group(id) => (Some(*id), Vec::new(), Vec::new()),
            Self::Anonymous {
                direct_members,
                direct_subgroups,
            } => (None, sorted(direct_members), sorted(direct_subgroups)),
        }
    }
}

impl PartialEq for GroupSettingValue {
    fn eq(&self, other: &Self) -> bool {
        self.normalized() == other.normalized()
    }
}

impl Eq for GroupSettingValue {}

impl std::hash::Hash for GroupSettingValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.normalized().hash(state);
    }
}

/// 用户组。
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserGroup {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub members: Vec<i64>,
    pub direct_subgroup_ids: Vec<i64>,
    pub is_system_group: bool,
    pub deactivated: bool,
}

/// 频道级的组设置，字段名与 Zulip 的 JSON 一致，缺失为 `None`。
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelGroupSettings {
    pub can_administer_channel_group: Option<GroupSettingValue>,
    pub can_add_subscribers_group: Option<GroupSettingValue>,
    pub can_remove_subscribers_group: Option<GroupSettingValue>,
    pub can_send_message_group: Option<GroupSettingValue>,
    pub can_move_messages_out_of_channel_group: Option<GroupSettingValue>,
    pub can_move_messages_within_channel_group: Option<GroupSettingValue>,
    pub can_resolve_topics_group: Option<GroupSettingValue>,
    pub can_create_topic_group: Option<GroupSettingValue>,
    pub can_delete_any_message_group: Option<GroupSettingValue>,
    pub can_delete_own_message_group: Option<GroupSettingValue>,
    pub can_subscribe_group: Option<GroupSettingValue>,
}

/// 频道设置中可以修改的一项权限：设置名、中文名、当前值。
#[derive(Clone, Copy, Debug)]
pub struct ChannelPermission {
    pub name: &'static str,
    pub label: &'static str,
    pub value: fn(&ChannelGroupSettings) -> Option<&GroupSettingValue>,
}

/// 频道设置中可以修改的权限，按显示顺序。
pub const CHANNEL_PERMISSIONS: [ChannelPermission; 10] = [
    ChannelPermission {
        name: "can_send_message_group",
        label: "谁可以发言",
        value: |c| c.can_send_message_group.as_ref(),
    },
    ChannelPermission {
        name: "can_add_subscribers_group",
        label: "谁可以邀请成员",
        value: |c| c.can_add_subscribers_group.as_ref(),
    },
    ChannelPermission {
        name: "can_remove_subscribers_group",
        label: "谁可以移出成员",
        value: |c| c.can_remove_subscribers_group.as_ref(),
    },
    ChannelPermission {
        name: "can_administer_channel_group",
        label: "频道管理员",
        value: |c| c.can_administer_channel_group.as_ref(),
    },
    ChannelPermission {
        name: "can_move_messages_within_channel_group",
        label: "谁可以在频道内移动话题",
        value: |c| c.can_move_messages_within_channel_group.as_ref(),
    },
    ChannelPermission {
        name: "can_move_messages_out_of_channel_group",
        label: "谁可以把话题移出频道",
        value: |c| c.can_move_messages_out_of_channel_group.as_ref(),
    },
    ChannelPermission {
        name: "can_resolve_topics_group",
        label: "谁可以标记已解决",
        value: |c| c.can_resolve_topics_group.as_ref(),
    },
    ChannelPermission {
        name: "can_create_topic_group",
        label: "谁可以新建话题",
        value: |c| c.can_create_topic_group.as_ref(),
    },
    ChannelPermission {
        name: "can_delete_any_message_group",
        label: "谁可以删除任何消息",
        value: |c| c.can_delete_any_message_group.as_ref(),
    },
    ChannelPermission {
        name: "can_delete_own_message_group",
        label: "谁可以删除自己的消息",
        value: |c| c.can_delete_own_message_group.as_ref(),
    },
];

/// `server_supported_permission_settings` 的分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingScope {
    Realm,
    Stream,
    Group,
}

/// 构造 [`PermissionEvaluator`] 所需的数据。
#[derive(Clone, Debug, Default)]
pub struct PermissionInputs {
    pub self_id: i64,
    /// `ROLE_*`。
    pub role: i32,
    pub groups: HashMap<i64, UserGroup>,
    /// 组织级组设置，键不带 `realm_` 前缀。
    pub realm_group_settings: HashMap<String, GroupSettingValue>,
    /// 来自 `server_supported_permission_settings`：该设置是否允许“所有人”组（含访客）。
    pub allow_everyone_group: HashMap<(SettingScope, String), bool>,
    pub allow_message_editing: bool,
    /// 消息编辑时限（秒）；`None` 表示不限。
    pub edit_limit_seconds: Option<i64>,
    /// 删除自己消息的时限（秒）；`None` 表示不限。
    pub delete_limit_seconds: Option<i64>,
}

/// 收到 realm、user_group 事件或自己的角色变化后，整体重新构造。
#[derive(Clone, Debug)]
pub struct PermissionEvaluator {
    inputs: PermissionInputs,
}

use SettingScope::{Realm, Stream};

impl PermissionEvaluator {
    pub fn new(inputs: PermissionInputs) -> Self {
        Self { inputs }
    }

    pub fn self_id(&self) -> i64 {
        self.inputs.self_id
    }

    pub fn role(&self) -> i32 {
        self.inputs.role
    }

    pub fn is_guest(&self) -> bool {
        self.inputs.role >= ROLE_GUEST
    }

    /// 组织所有者或管理员。
    pub fn is_org_admin(&self) -> bool {
        self.inputs.role <= ROLE_ADMINISTRATOR
    }

    /// 自己是否属于该组（递归检查子组）。
    pub fn is_member(&self, group_id: i64) -> bool {
        self.user_is_member(self.inputs.self_id, group_id)
    }

    /// 任意用户是否属于该组（递归检查子组，防止循环）。
    pub fn user_is_member(&self, user_id: i64, group_id: i64) -> bool {
        let mut visited = HashSet::new();
        let mut pending = vec![group_id];
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let Some(group) = self.inputs.groups.get(&id) else {
                continue;
            };
            if group.members.contains(&user_id) {
                return true;
            }
            pending.extend(&group.direct_subgroup_ids);
        }
        false
    }

    /// 任意用户是否满足该组设置值（不考虑访客限制）。
    pub fn user_allows(&self, user_id: i64, value: Option<&GroupSettingValue>) -> bool {
        match value {
            None => false,
            Some(GroupSettingValue::Group(id)) => self.user_is_member(user_id, *id),
            Some(GroupSettingValue::Anonymous {
                direct_members,
                direct_subgroups,
            }) => {
                direct_members.contains(&user_id)
                    || direct_subgroups
                        .iter()
                        .any(|&g| self.user_is_member(user_id, g))
            }
        }
    }

    /// 自己是否满足该组设置值。访客对不允许“所有人”组的设置一律没有权限。
    pub fn allows(
        &self,
        value: Option<&GroupSettingValue>,
        scope: SettingScope,
        name: &str,
    ) -> bool {
        if value.is_none() {
            return false;
        }
        if self.is_guest()
            && self
                .inputs
                .allow_everyone_group
                .get(&(scope, name.to_owned()))
                == Some(&false)
        {
            return false;
        }
        self.user_allows(self.inputs.self_id, value)
    }

    /// 组织级设置是否允许自己。
    pub fn allows_realm(&self, name: &str) -> bool {
        self.allows(self.inputs.realm_group_settings.get(name), Realm, name)
    }

    pub fn can_administer_channel(&self, ch: &ChannelGroupSettings) -> bool {
        self.is_org_admin()
            || self.allows(
                ch.can_administer_channel_group.as_ref(),
                Stream,
                "can_administer_channel_group",
            )
    }

    pub fn can_move_topic_within_channel(&self, ch: &ChannelGroupSettings) -> bool {
        self.allows_realm("can_move_messages_between_topics_group")
            || self.allows(
                ch.can_move_messages_within_channel_group.as_ref(),
                Stream,
                "can_move_messages_within_channel_group",
            )
            || self.can_administer_channel(ch)
    }

    pub fn can_move_topic_between_channels(&self, ch: &ChannelGroupSettings) -> bool {
        self.allows_realm("can_move_messages_between_channels_group")
            || self.allows(
                ch.can_move_messages_out_of_channel_group.as_ref(),
                Stream,
                "can_move_messages_out_of_channel_group",
            )
            || self.can_administer_channel(ch)
    }

    pub fn can_resolve_topic(&self, ch: &ChannelGroupSettings) -> bool {
        self.allows_realm("can_resolve_topics_group")
            || self.allows(
                ch.can_resolve_topics_group.as_ref(),
                Stream,
                "can_resolve_topics_group",
            )
    }

    /// 删除话题：Zulip 文档说明只有组织管理员可以。
    pub fn can_delete_topic(&self) -> bool {
        self.is_org_admin()
    }

    /// 设置缺失视为不限制。
    pub fn can_send_to_channel(&self, ch: &ChannelGroupSettings) -> bool {
        ch.can_send_message_group.is_none()
            || self.allows(
                ch.can_send_message_group.as_ref(),
                Stream,
                "can_send_message_group",
            )
    }

    /// 设置缺失视为不限制。
    pub fn can_create_topic(&self, ch: &ChannelGroupSettings) -> bool {
        ch.can_create_topic_group.is_none()
            || self.allows(
                ch.can_create_topic_group.as_ref(),
                Stream,
                "can_create_topic_group",
            )
    }

    /// 自己加入频道：公开频道非访客都可以；否则需要在 can_subscribe_group 或频道的 can_add_subscribers_group 中。
    /// 私有频道的管理权限不包含加入权限（Zulip 将两者分开），所以组织管理员、频道管理员也不例外。
    pub fn can_subscribe(&self, ch: &ChannelGroupSettings, invite_only: bool) -> bool {
        (!invite_only && !self.is_guest())
            || self.allows(
                ch.can_subscribe_group.as_ref(),
                Stream,
                "can_subscribe_group",
            )
            || self.allows(
                ch.can_add_subscribers_group.as_ref(),
                Stream,
                "can_add_subscribers_group",
            )
    }

    pub fn can_add_subscribers(&self, ch: &ChannelGroupSettings) -> bool {
        self.allows_realm("can_add_subscribers_group")
            || self.allows(
                ch.can_add_subscribers_group.as_ref(),
                Stream,
                "can_add_subscribers_group",
            )
            || self.can_administer_channel(ch)
    }

    /// 频道管理员也可以移出成员（feature level 349 起）。
    pub fn can_remove_subscribers(&self, ch: &ChannelGroupSettings) -> bool {
        self.is_org_admin()
            || self.allows(
                ch.can_remove_subscribers_group.as_ref(),
                Stream,
                "can_remove_subscribers_group",
            )
            || self.can_administer_channel(ch)
    }

    pub fn can_create_public_channel(&self) -> bool {
        self.allows_realm("can_create_public_channel_group")
    }

    pub fn can_create_private_channel(&self) -> bool {
        self.allows_realm("can_create_private_channel_group")
    }

    pub fn can_invite_users(&self) -> bool {
        self.allows_realm("can_invite_users_group")
    }

    pub fn can_mention_everyone(&self) -> bool {
        self.allows_realm("can_mention_many_users_group")
    }

    /// 删除消息（与网页端的 get_deletability 一致）：已归档频道中的消息不能删除；可以删除任何消息的（组织或频道设置）都可以删除；
    /// 否则只能删除自己或自己的机器人发的消息，需要组织或频道允许删除自己的消息，且在删除时限内（时间均为 Unix 秒）。
    /// 私聊消息 `channel` 为 `None`，只看组织设置。
    pub fn can_delete_message(
        &self,
        channel: Option<&ChannelGroupSettings>,
        channel_archived: bool,
        sent_by_me_or_my_bot: bool,
        message_timestamp: i64,
        now: i64,
    ) -> bool {
        if channel.is_some() && channel_archived {
            return false;
        }
        let in_channel = |value: fn(&ChannelGroupSettings) -> Option<&GroupSettingValue>,
                          name: &str| {
            channel.is_some_and(|ch| self.allows(value(ch), Stream, name))
        };
        if self.allows_realm_or_admin("can_delete_any_message_group")
            || in_channel(
                |c| c.can_delete_any_message_group.as_ref(),
                "can_delete_any_message_group",
            )
        {
            return true;
        }
        if !sent_by_me_or_my_bot {
            return false;
        }
        if !self.allows_realm_or_admin("can_delete_own_message_group")
            && !in_channel(
                |c| c.can_delete_own_message_group.as_ref(),
                "can_delete_own_message_group",
            )
        {
            return false;
        }
        self.inputs
            .delete_limit_seconds
            .is_none_or(|limit| limit + (message_timestamp - now) > 0)
    }

    // 服务器没有这个组设置时只有组织管理员有权限
    fn allows_realm_or_admin(&self, name: &str) -> bool {
        if self.inputs.realm_group_settings.contains_key(name) {
            self.allows_realm(name)
        } else {
            self.is_org_admin()
        }
    }

    /// 自己的消息，且组织允许编辑，且在编辑时限内（时间均为 Unix 秒）。
    pub fn can_edit_message(&self, sender_id: i64, message_timestamp: i64, now: i64) -> bool {
        sender_id == self.inputs.self_id
            && self.inputs.allow_message_editing
            && self
                .inputs
                .edit_limit_seconds
                .is_none_or(|limit| now - message_timestamp < limit)
    }
}

/// `server_supported_permission_settings` 中一个设置的限制。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionSettingInfo {
    pub allow_nobody: bool,
    pub allow_everyone: bool,
    /// 允许的系统组；空表示不限制。
    pub allowed_system_groups: Vec<String>,
}

impl PermissionSettingInfo {
    /// 服务器没有给出该设置的信息时使用：不限制。
    pub fn unrestricted() -> Self {
        Self {
            allow_nobody: true,
            allow_everyone: true,
            allowed_system_groups: Vec::new(),
        }
    }

    /// 该设置可以选择的系统组。
    pub fn system_group_choices(&self) -> Vec<SystemGroup> {
        SYSTEM_GROUPS
            .iter()
            .copied()
            .filter(|g| {
                (g.name != ROLE_EVERYONE || self.allow_everyone)
                    && (g.name != ROLE_NOBODY || self.allow_nobody)
                    && (self.allowed_system_groups.is_empty()
                        || self.allowed_system_groups.iter().any(|a| a == g.name))
            })
            .collect()
    }
}

pub const ROLE_EVERYONE: &str = "role:everyone";
pub const ROLE_NOBODY: &str = "role:nobody";

/// 系统组名与中文显示名。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemGroup {
    pub name: &'static str,
    pub text: &'static str,
}

const fn group(name: &'static str, text: &'static str) -> SystemGroup {
    SystemGroup { name, text }
}

/// 权限设置下拉框中可选的系统组，按范围从大到小排列。不包含 role:internet（只用于网页公开频道）。
pub const SYSTEM_GROUPS: [SystemGroup; 7] = [
    group(ROLE_EVERYONE, "所有人"),
    group("role:members", "成员"),
    group("role:fullmembers", "正式成员"),
    group("role:moderators", "版主"),
    group("role:administrators", "管理员"),
    group("role:owners", "所有者"),
    group(ROLE_NOBODY, "无人"),
];

#[cfg(test)]
mod tests {
    use super::GroupSettingValue as G;
    use super::*;

    fn g(id: i64, members: &[i64], subs: &[i64]) -> (i64, UserGroup) {
        (
            id,
            UserGroup {
                id,
                members: members.to_vec(),
                direct_subgroup_ids: subs.to_vec(),
                ..Default::default()
            },
        )
    }

    // 系统组：9 nobody，10 owners，11 administrators，12 moderators，13 fullmembers，14 members，15 everyone
    fn groups() -> HashMap<i64, UserGroup> {
        HashMap::from([
            g(9, &[], &[]),
            g(10, &[8], &[]),
            g(11, &[20], &[10]),
            g(12, &[], &[11]),
            g(13, &[9], &[12]),
            g(14, &[9, 10], &[13]),
            g(15, &[30], &[14]),
            // 循环引用的自定义组
            g(50, &[], &[51]),
            g(51, &[10], &[50]),
        ])
    }

    fn realm(entries: &[(&str, G)]) -> HashMap<String, G> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn inputs(self_id: i64, role: i32) -> PermissionInputs {
        PermissionInputs {
            self_id,
            role,
            groups: groups(),
            allow_message_editing: true,
            edit_limit_seconds: Some(600),
            ..Default::default()
        }
    }

    fn create(self_id: i64, role: i32) -> PermissionEvaluator {
        PermissionEvaluator::new(inputs(self_id, role))
    }

    #[test]
    fn group_setting_json() {
        assert_eq!(serde_json::from_str::<G>("14").unwrap(), G::Group(14));
        assert_eq!(
            serde_json::from_str::<G>(r#"{"direct_members":[1,2],"direct_subgroups":[3]}"#)
                .unwrap(),
            G::anonymous([2, 1], [3])
        );
        assert_eq!(serde_json::from_str::<Option<G>>("null").unwrap(), None);
        assert!(serde_json::from_str::<G>("\"x\"").is_err());
        assert_eq!(serde_json::to_string(&G::Group(5)).unwrap(), "5");
        assert_eq!(
            serde_json::to_string(&G::anonymous([1], [])).unwrap(),
            r#"{"direct_members":[1],"direct_subgroups":[]}"#
        );
    }

    #[test]
    fn membership_is_recursive() {
        let owner = create(8, 100);
        assert!(owner.is_member(10) && owner.is_member(14) && owner.is_member(15));
        assert!(!owner.is_member(9));

        let member = create(10, 400);
        assert!(member.is_member(14));
        assert!(!member.is_member(13) && !member.is_member(11));
    }

    #[test]
    fn group_cycle_terminates() {
        assert!(create(10, 400).is_member(50));
        assert!(!create(9, 400).is_member(50));
    }

    #[test]
    fn allows_anonymous_group() {
        let p = create(30, 400);
        assert!(p.allows(Some(&G::anonymous([30], [])), Stream, "x"));
        assert!(create(9, 400).allows(Some(&G::anonymous([], [13])), Stream, "x"));
        assert!(!create(10, 400).allows(Some(&G::anonymous([30], [13])), Stream, "x"));
        assert!(!p.allows(None, Stream, "x"));
    }

    #[test]
    fn guest_denied_when_everyone_not_allowed() {
        let allow = HashMap::from([
            ((Realm, "can_add_custom_emoji_group".to_owned()), false),
            ((Stream, "can_send_message_group".to_owned()), true),
        ]);
        let with = |role| {
            PermissionEvaluator::new(PermissionInputs {
                allow_everyone_group: allow.clone(),
                ..inputs(30, role)
            })
        };
        let guest = with(600);
        assert!(!guest.allows(Some(&G::Group(15)), Realm, "can_add_custom_emoji_group"));
        assert!(guest.allows(Some(&G::Group(15)), Stream, "can_send_message_group"));
        assert!(with(400).allows(Some(&G::Group(15)), Realm, "can_add_custom_emoji_group"));
    }

    #[test]
    fn channel_permissions() {
        let realm = realm(&[
            ("can_move_messages_between_topics_group", G::Group(14)),
            ("can_move_messages_between_channels_group", G::Group(12)),
            ("can_resolve_topics_group", G::Group(9)),
            ("can_add_subscribers_group", G::Group(9)),
        ]);
        let ch = ChannelGroupSettings {
            can_administer_channel_group: Some(G::anonymous([10], [])),
            can_send_message_group: Some(G::Group(13)),
            can_remove_subscribers_group: Some(G::Group(11)),
            can_resolve_topics_group: Some(G::anonymous([9], [])),
            ..Default::default()
        };
        let with = |id, role| {
            PermissionEvaluator::new(PermissionInputs {
                realm_group_settings: realm.clone(),
                ..inputs(id, role)
            })
        };

        let member = with(9, 400);
        assert!(member.can_move_topic_within_channel(&ch));
        assert!(!member.can_move_topic_between_channels(&ch));
        assert!(member.can_resolve_topic(&ch));
        assert!(member.can_send_to_channel(&ch));
        assert!(!member.can_delete_topic());
        assert!(!member.can_add_subscribers(&ch));
        assert!(!member.can_remove_subscribers(&ch));
        assert!(member.can_create_topic(&ch)); // 缺失视为不限制

        let channel_admin = with(10, 400);
        assert!(channel_admin.can_administer_channel(&ch));
        assert!(channel_admin.can_move_topic_between_channels(&ch));
        assert!(channel_admin.can_add_subscribers(&ch));
        assert!(channel_admin.can_remove_subscribers(&ch));
        assert!(!channel_admin.can_send_to_channel(&ch));

        let owner = with(8, 100);
        assert!(owner.is_org_admin());
        assert!(owner.can_delete_topic());
        assert!(owner.can_remove_subscribers(&ch));
    }

    #[test]
    fn can_subscribe() {
        let ch = ChannelGroupSettings::default();
        assert!(create(10, 400).can_subscribe(&ch, false));
        assert!(!create(30, 600).can_subscribe(&ch, false));
        assert!(!create(10, 400).can_subscribe(&ch, true));
        let subscribable = ChannelGroupSettings {
            can_subscribe_group: Some(G::anonymous([10], [])),
            ..Default::default()
        };
        assert!(create(10, 400).can_subscribe(&subscribable, true));
        assert!(!create(8, 100).can_subscribe(&ch, true));
        let addable = ChannelGroupSettings {
            can_add_subscribers_group: Some(G::anonymous([10], [])),
            ..Default::default()
        };
        assert!(create(10, 400).can_subscribe(&addable, true));
    }

    #[test]
    fn can_edit_message() {
        let p = create(9, 400);
        assert!(p.can_edit_message(9, 1000, 1599));
        assert!(!p.can_edit_message(9, 1000, 1600));
        assert!(!p.can_edit_message(10, 1000, 1001));
        let unlimited = PermissionEvaluator::new(PermissionInputs {
            edit_limit_seconds: None,
            ..inputs(9, 400)
        });
        assert!(unlimited.can_edit_message(9, 0, 999_999));
    }

    #[test]
    fn can_delete_message_follows_webapp_rules() {
        // 组织：管理员可以删除任何消息，成员可以删除自己的消息；删除时限 600 秒
        let settings = realm(&[
            ("can_delete_any_message_group", G::Group(11)),
            ("can_delete_own_message_group", G::Group(14)),
        ]);
        let with = |id, role, settings: &HashMap<String, G>, limit| {
            PermissionEvaluator::new(PermissionInputs {
                realm_group_settings: settings.clone(),
                delete_limit_seconds: limit,
                ..inputs(id, role)
            })
        };
        let channel = ChannelGroupSettings::default();
        let ch = Some(&channel);

        let member = with(10, 400, &settings, Some(600));
        assert!(member.can_delete_message(ch, false, true, 1000, 1599));
        assert!(!member.can_delete_message(ch, false, true, 1000, 1600));
        assert!(!member.can_delete_message(ch, false, false, 1000, 1001));
        assert!(member.can_delete_message(None, false, true, 1000, 1001));
        // 已归档频道中的消息谁都不能删除
        assert!(!member.can_delete_message(ch, true, true, 1000, 1001));

        // 可以删除任何消息：不看发送人和时限
        let admin = with(20, 200, &settings, Some(600));
        assert!(admin.can_delete_message(ch, false, false, 0, 999_999));
        assert!(!admin.can_delete_message(ch, true, false, 0, 1));

        // 组织不允许删除自己的消息，频道允许
        let mut strict = settings.clone();
        strict.insert("can_delete_own_message_group".to_owned(), G::Group(11));
        let limited = with(10, 400, &strict, None);
        assert!(!limited.can_delete_message(ch, false, true, 0, 999_999));
        assert!(!limited.can_delete_message(None, false, true, 0, 999_999));
        let own_in_channel = ChannelGroupSettings {
            can_delete_own_message_group: Some(G::Group(14)),
            ..Default::default()
        };
        assert!(limited.can_delete_message(Some(&own_in_channel), false, true, 0, 999_999));
        // 频道允许删除任何消息
        let any_in_channel = ChannelGroupSettings {
            can_delete_any_message_group: Some(G::Group(14)),
            ..Default::default()
        };
        assert!(limited.can_delete_message(Some(&any_in_channel), false, false, 0, 999_999));
    }

    #[test]
    fn delete_without_realm_setting_needs_org_admin() {
        assert!(create(8, 100).can_delete_message(None, false, false, 0, 1));
        assert!(!create(9, 400).can_delete_message(None, false, true, 0, 1));
    }

    fn names(info: &PermissionSettingInfo) -> Vec<&'static str> {
        info.system_group_choices().iter().map(|c| c.name).collect()
    }

    #[test]
    fn choices_unrestricted_lists_all() {
        assert_eq!(
            names(&PermissionSettingInfo::unrestricted()).len(),
            SYSTEM_GROUPS.len()
        );
    }

    #[test]
    fn choices_honor_nobody_and_everyone() {
        let names = names(&PermissionSettingInfo {
            allow_nobody: false,
            allow_everyone: false,
            allowed_system_groups: vec![],
        });
        assert!(!names.contains(&ROLE_EVERYONE));
        assert!(!names.contains(&ROLE_NOBODY));
        assert!(names.contains(&"role:members"));
    }

    #[test]
    fn choices_restricted_by_allowed_groups() {
        let info = PermissionSettingInfo {
            allow_nobody: true,
            allow_everyone: true,
            allowed_system_groups: vec!["role:administrators".into(), "role:owners".into()],
        };
        assert_eq!(names(&info), ["role:administrators", "role:owners"]);
    }
}
