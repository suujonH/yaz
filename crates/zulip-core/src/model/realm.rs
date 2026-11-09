use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Channel, positive_id};

/// 自定义资料字段的定义。
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomProfileField {
    pub id: i64,
    #[serde(rename = "type")]
    pub field_type: i32,
    pub name: String,
    pub hint: String,
    pub order: i32,
    pub display_in_profile_summary: bool,
    pub field_data: Value,
}

/// 组织信息、服务器限制与时间参数。register 时整体读取，收到 realm 事件时更新对应字段。
#[derive(Clone, Debug, PartialEq)]
pub struct RealmInfo {
    pub url: String,
    pub feature_level: i32,
    pub zulip_version: String,

    pub name: String,
    pub icon_url: Option<String>,

    pub max_message_length: i32,
    pub max_topic_length: i32,
    pub max_file_upload_size_mib: i32,
    /// 组织的上传存储空间配额（MiB）；`None` 为不限。
    pub upload_quota_mib: Option<i64>,

    /// 空话题（`""`）的显示名。
    pub empty_topic_display_name: String,
    pub server_emoji_data_url: Option<String>,
    pub allow_message_editing: bool,
    /// 服务器是否能发送手机推送；为 false 时手机推送设置不起作用。
    pub push_notifications_enabled: bool,

    pub new_channels_announcements_channel_id: Option<i64>,
    pub signup_announcements_channel_id: Option<i64>,
    pub zulip_update_announcements_channel_id: Option<i64>,
    pub moderation_request_channel_id: Option<i64>,

    /// 编辑时限（秒）；`None` 表示不限。
    pub edit_limit_seconds: Option<i64>,
    /// 删除自己消息的时限（秒）；`None` 表示不限。
    pub delete_limit_seconds: Option<i64>,

    pub enable_read_receipts: bool,
    pub edit_history_visibility_policy: String,
    /// 组织的话题策略（频道为 inherit 时使用）。
    pub topics_policy: String,
    /// custom_profile_fields 事件整体替换。
    pub profile_fields: Vec<CustomProfileField>,

    pub longpoll_timeout_seconds: i32,
    pub typing_started_wait_ms: i32,
    pub typing_stopped_wait_ms: i32,
    pub typing_started_expiry_ms: i32,
    pub presence_ping_interval_seconds: i32,
    pub presence_offline_threshold_seconds: i32,
}

impl Default for RealmInfo {
    fn default() -> Self {
        Self {
            url: String::new(),
            feature_level: 0,
            zulip_version: String::new(),
            name: String::new(),
            icon_url: None,
            max_message_length: 0,
            max_topic_length: 0,
            max_file_upload_size_mib: 0,
            upload_quota_mib: None,
            empty_topic_display_name: String::new(),
            server_emoji_data_url: None,
            allow_message_editing: false,
            push_notifications_enabled: true,
            new_channels_announcements_channel_id: None,
            signup_announcements_channel_id: None,
            zulip_update_announcements_channel_id: None,
            moderation_request_channel_id: None,
            edit_limit_seconds: None,
            delete_limit_seconds: None,
            enable_read_receipts: false,
            edit_history_visibility_policy: "all".to_owned(),
            topics_policy: "allow_empty_topic".to_owned(),
            profile_fields: Vec::new(),
            longpoll_timeout_seconds: 0,
            typing_started_wait_ms: 0,
            typing_stopped_wait_ms: 0,
            typing_started_expiry_ms: 0,
            presence_ping_interval_seconds: 0,
            presence_offline_threshold_seconds: 0,
        }
    }
}

impl RealmInfo {
    /// 设置为公告频道的频道（Notification Bot 在其中发公告）。
    pub fn announcement_channel_ids(&self) -> HashSet<i64> {
        [
            self.new_channels_announcements_channel_id,
            self.signup_announcements_channel_id,
            self.zulip_update_announcements_channel_id,
            self.moderation_request_channel_id,
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// 频道是否允许有名字的话题：话题策略为 empty_topic_only（只有“综合聊天”）时不允许。
    pub fn allows_named_topics(&self, channel: &Channel) -> bool {
        let policy = if channel.topics_policy == "inherit" {
            &self.topics_policy
        } else {
            &channel.topics_policy
        };
        policy != "empty_topic_only"
    }

    /// 应用 realm 事件中的一个设置（名字不带 `realm_` 前缀）。不认识的设置忽略。
    pub fn apply(&mut self, name: &str, value: &Value) {
        let set_string = |target: &mut String| {
            if let Some(s) = value.as_str() {
                s.clone_into(target);
            }
        };
        let set_bool = |target: &mut bool| {
            if let Some(b) = value.as_bool() {
                *target = b;
            }
        };
        match name {
            "name" => set_string(&mut self.name),
            "icon_url" => self.icon_url = value.as_str().map(str::to_owned),
            "allow_message_editing" => set_bool(&mut self.allow_message_editing),
            "push_notifications_enabled" => set_bool(&mut self.push_notifications_enabled),
            "new_stream_announcements_stream_id" => {
                self.new_channels_announcements_channel_id = positive_id(value);
            }
            "signup_announcements_stream_id" => {
                self.signup_announcements_channel_id = positive_id(value)
            }
            "zulip_update_announcements_stream_id" => {
                self.zulip_update_announcements_channel_id = positive_id(value);
            }
            "moderation_request_channel_id" => {
                self.moderation_request_channel_id = positive_id(value)
            }
            "message_content_edit_limit_seconds" => self.edit_limit_seconds = value.as_i64(),
            "message_content_delete_limit_seconds" => self.delete_limit_seconds = value.as_i64(),
            "enable_read_receipts" => set_bool(&mut self.enable_read_receipts),
            "message_edit_history_visibility_policy" => {
                set_string(&mut self.edit_history_visibility_policy)
            }
            "empty_topic_display_name" => set_string(&mut self.empty_topic_display_name),
            "upload_quota_mib" => self.upload_quota_mib = value.as_i64(),
            "topics_policy" => set_string(&mut self.topics_policy),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn apply_settings() {
        let mut realm = RealmInfo::default();
        realm.apply("name", &json!("组织"));
        assert_eq!(realm.name, "组织");
        realm.apply("signup_announcements_stream_id", &json!(5));
        realm.apply("moderation_request_channel_id", &json!(-1));
        assert_eq!(realm.announcement_channel_ids(), HashSet::from([5]));
        realm.apply("message_content_edit_limit_seconds", &json!(600));
        assert_eq!(realm.edit_limit_seconds, Some(600));
        realm.apply("message_content_edit_limit_seconds", &json!("unlimited"));
        assert_eq!(realm.edit_limit_seconds, None);
        realm.apply("unknown", &json!(1));
    }

    #[test]
    fn named_topics_follow_policy() {
        let mut realm = RealmInfo::default();
        let mut channel = Channel::default();
        assert!(realm.allows_named_topics(&channel));
        realm.topics_policy = "empty_topic_only".into();
        assert!(!realm.allows_named_topics(&channel));
        channel.topics_policy = "allow_empty_topic".into();
        assert!(realm.allows_named_topics(&channel));
    }
}
