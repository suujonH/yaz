use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use super::{
    DraftDto, MutedUserDto, PresenceDto, RealmEmojiDto, RecentPrivateConversationDto, ReminderDto,
    ScheduledMessageDto, StreamDto, SubscriptionDto, UnreadMsgsDto, UserDto, UserStatusDto,
    UserTopicDto, int_key_map,
};
use zulip_core::model::{CustomProfileField, SavedSnippet, UserSettings};
use zulip_core::permission::{GroupSettingValue, UserGroup};

/// `server_supported_permission_settings` 中一个设置的限制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSettingInfo {
    pub allow_nobody: bool,
    pub allow_everyone: bool,
    /// 允许的系统组；空表示不限制。
    pub allowed_system_groups: Vec<String>,
}

/// `POST /register` 的结果。未列出的字段保留在 `extra` 中。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct RegisterResult {
    pub queue_id: String,
    pub last_event_id: i64,
    pub zulip_feature_level: i32,
    pub zulip_version: String,
    pub event_queue_longpoll_timeout_seconds: u32,

    pub user_id: i64,
    pub email: String,
    pub full_name: String,
    pub avatar_url: Option<String>,
    pub is_admin: bool,

    pub realm_users: Vec<UserDto>,
    pub realm_non_active_users: Vec<UserDto>,
    pub cross_realm_bots: Vec<UserDto>,

    pub subscriptions: Vec<SubscriptionDto>,
    pub unsubscribed: Vec<SubscriptionDto>,
    pub never_subscribed: Vec<StreamDto>,
    pub streams: Vec<StreamDto>,

    pub recent_private_conversations: Vec<RecentPrivateConversationDto>,
    pub unread_msgs: UnreadMsgsDto,
    pub user_topics: Vec<UserTopicDto>,

    pub drafts: Vec<DraftDto>,
    pub starred_messages: Vec<i64>,
    pub muted_users: Vec<MutedUserDto>,
    #[serde(deserialize_with = "int_key_map")]
    pub presences: HashMap<i64, PresenceDto>,
    pub presence_last_update_id: i64,
    #[serde(deserialize_with = "int_key_map")]
    pub user_status: HashMap<i64, UserStatusDto>,
    pub user_settings: UserSettings,
    pub alert_words: Vec<String>,
    pub scheduled_messages: Vec<ScheduledMessageDto>,
    pub saved_snippets: Vec<SavedSnippet>,
    pub reminders: Vec<ReminderDto>,

    pub realm_user_groups: Vec<UserGroup>,
    pub realm_emoji: HashMap<String, RealmEmojiDto>,
    pub custom_profile_fields: Vec<CustomProfileField>,
    pub server_emoji_data_url: Option<String>,

    pub max_message_length: i32,
    pub max_topic_length: i32,
    pub max_file_upload_size_mib: i32,
    /// 组织的上传存储空间配额（MiB）；`None` 为不限。
    pub realm_upload_quota_mib: Option<i64>,
    pub realm_empty_topic_display_name: String,
    pub server_supported_permission_settings: Value,

    pub realm_name: String,
    pub realm_icon_url: Option<String>,
    pub realm_allow_message_editing: bool,
    pub realm_push_notifications_enabled: bool,

    /// 公告频道；-1 表示未设置。
    pub realm_new_stream_announcements_stream_id: i64,
    pub realm_signup_announcements_stream_id: i64,
    pub realm_zulip_update_announcements_stream_id: i64,
    pub realm_moderation_request_channel_id: i64,

    /// 编辑时限（秒）；`None` 表示不限。
    pub realm_message_content_edit_limit_seconds: Option<i32>,
    /// 删除自己消息的时限（秒）；`None` 表示不限。
    pub realm_message_content_delete_limit_seconds: Option<i32>,
    pub realm_enable_read_receipts: bool,
    pub realm_move_messages_between_streams_limit_seconds: Option<i32>,
    pub realm_move_messages_within_stream_limit_seconds: Option<i32>,
    /// `all`、`moves` 或 `none`。
    pub realm_message_edit_history_visibility_policy: String,
    pub realm_topics_policy: String,

    pub server_typing_started_wait_period_milliseconds: u32,
    pub server_typing_stopped_wait_period_milliseconds: u32,
    pub server_typing_started_expiry_period_milliseconds: u32,
    pub server_presence_ping_interval_seconds: u32,
    pub server_presence_offline_threshold_seconds: u32,

    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Default for RegisterResult {
    fn default() -> Self {
        Self {
            queue_id: String::new(),
            last_event_id: 0,
            zulip_feature_level: 0,
            zulip_version: String::new(),
            event_queue_longpoll_timeout_seconds: 90,
            user_id: 0,
            email: String::new(),
            full_name: String::new(),
            avatar_url: None,
            is_admin: false,
            realm_users: Vec::new(),
            realm_non_active_users: Vec::new(),
            cross_realm_bots: Vec::new(),
            subscriptions: Vec::new(),
            unsubscribed: Vec::new(),
            never_subscribed: Vec::new(),
            streams: Vec::new(),
            recent_private_conversations: Vec::new(),
            unread_msgs: UnreadMsgsDto::default(),
            user_topics: Vec::new(),
            drafts: Vec::new(),
            starred_messages: Vec::new(),
            muted_users: Vec::new(),
            presences: HashMap::new(),
            presence_last_update_id: -1,
            user_status: HashMap::new(),
            user_settings: UserSettings::default(),
            alert_words: Vec::new(),
            scheduled_messages: Vec::new(),
            saved_snippets: Vec::new(),
            reminders: Vec::new(),
            realm_user_groups: Vec::new(),
            realm_emoji: HashMap::new(),
            custom_profile_fields: Vec::new(),
            server_emoji_data_url: None,
            max_message_length: 10000,
            max_topic_length: 60,
            max_file_upload_size_mib: 25,
            realm_upload_quota_mib: None,
            realm_empty_topic_display_name: String::new(),
            server_supported_permission_settings: Value::Null,
            realm_name: String::new(),
            realm_icon_url: None,
            realm_allow_message_editing: true,
            realm_push_notifications_enabled: true,
            realm_new_stream_announcements_stream_id: -1,
            realm_signup_announcements_stream_id: -1,
            realm_zulip_update_announcements_stream_id: -1,
            realm_moderation_request_channel_id: -1,
            realm_message_content_edit_limit_seconds: None,
            realm_message_content_delete_limit_seconds: None,
            realm_enable_read_receipts: false,
            realm_move_messages_between_streams_limit_seconds: None,
            realm_move_messages_within_stream_limit_seconds: None,
            realm_message_edit_history_visibility_policy: "all".to_owned(),
            realm_topics_policy: "allow_empty_topic".to_owned(),
            server_typing_started_wait_period_milliseconds: 10000,
            server_typing_stopped_wait_period_milliseconds: 5000,
            server_typing_started_expiry_period_milliseconds: 45000,
            server_presence_ping_interval_seconds: 60,
            server_presence_offline_threshold_seconds: 140,
            extra: HashMap::new(),
        }
    }
}

impl RegisterResult {
    /// 自己的角色，取自 `realm_users` 中自己那一行；找不到时为成员（400）。
    pub fn self_role(&self) -> i32 {
        self.realm_users
            .iter()
            .find(|u| u.user_id == self.user_id)
            .map_or(400, |u| u.role)
    }

    /// 组织级的组设置，键为去掉 `realm_` 前缀后的名字（如 `can_create_groups`）。
    /// 设置名以服务器的 `server_supported_permission_settings.realm` 为准；
    /// 部分名字不以 `_group` 结尾（`can_create_groups`、`can_manage_all_groups`）。
    pub fn realm_group_settings(&self) -> HashMap<String, GroupSettingValue> {
        self.permission_setting_names("realm")
            .into_iter()
            .filter_map(|name| {
                let value = self.extra.get(&format!("realm_{name}"))?;
                let setting = serde_json::from_value::<GroupSettingValue>(value.clone()).ok()?;
                Some((name, setting))
            })
            .collect()
    }

    /// 某一范围（`realm`、`stream`、`group`）下服务器支持的权限设置名。
    pub fn permission_setting_names(&self, scope: &str) -> Vec<String> {
        self.permission_scope(scope)
            .map(|settings| settings.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 每个 (范围, 设置名) 可以设置的组的限制。
    pub fn permission_setting_infos(&self) -> HashMap<(String, String), PermissionSettingInfo> {
        self.permission_settings()
            .map(|(scope, name, setting)| {
                let flag = |key: &str| setting.get(key).is_none_or(|v| v.as_bool() == Some(true));
                let allowed_system_groups = setting
                    .get("allowed_system_groups")
                    .and_then(Value::as_array)
                    .map(|groups| {
                        groups
                            .iter()
                            .map(|g| g.as_str().unwrap_or_default().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                (
                    (scope.to_owned(), name.to_owned()),
                    PermissionSettingInfo {
                        allow_nobody: flag("allow_nobody_group"),
                        allow_everyone: flag("allow_everyone_group"),
                        allowed_system_groups,
                    },
                )
            })
            .collect()
    }

    /// 每个 (范围, 设置名) 是否允许设为“所有人”组（含访客）；服务器没有给出的设置不在其中。
    pub fn allow_everyone_group(&self) -> HashMap<(String, String), bool> {
        self.permission_settings()
            .filter_map(|(scope, name, setting)| {
                let allow = setting.get("allow_everyone_group")?;
                Some((
                    (scope.to_owned(), name.to_owned()),
                    allow.as_bool() == Some(true),
                ))
            })
            .collect()
    }

    fn permission_scope(&self, scope: &str) -> Option<&serde_json::Map<String, Value>> {
        self.server_supported_permission_settings
            .get(scope)?
            .as_object()
    }

    fn permission_settings(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.server_supported_permission_settings
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(scope, settings)| Some((scope.as_str(), settings.as_object()?)))
            .flat_map(|(scope, settings)| {
                settings
                    .iter()
                    .map(move |(name, setting)| (scope, name.as_str(), setting))
            })
    }
}
