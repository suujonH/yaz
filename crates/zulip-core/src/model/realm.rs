use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Channel, positive_id};

/// Definition of a custom profile field.
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

/// Organization info, server limits and time parameters. Read as a whole at register; the corresponding fields are updated on realm events.
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
    /// Upload storage quota of the organization (MiB); `None` means unlimited.
    pub upload_quota_mib: Option<i64>,

    /// Display name of the empty topic (`""`).
    pub empty_topic_display_name: String,
    pub server_emoji_data_url: Option<String>,
    pub allow_message_editing: bool,
    /// Whether the server can send mobile push notifications; when false the mobile push settings have no effect.
    pub push_notifications_enabled: bool,

    pub new_channels_announcements_channel_id: Option<i64>,
    pub signup_announcements_channel_id: Option<i64>,
    pub zulip_update_announcements_channel_id: Option<i64>,
    pub moderation_request_channel_id: Option<i64>,

    /// Edit time limit (seconds); `None` means unlimited.
    pub edit_limit_seconds: Option<i64>,
    /// Time limit for deleting one's own messages (seconds); `None` means unlimited.
    pub delete_limit_seconds: Option<i64>,

    pub enable_read_receipts: bool,
    pub edit_history_visibility_policy: String,
    /// Topic policy of the organization (used when the channel is inherit).
    pub topics_policy: String,
    /// Replaced as a whole by the custom_profile_fields event.
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
    /// The channel set as the announcement channel (Notification Bot posts announcements in it).
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

    /// Whether a channel allows named topics: not allowed when the topic policy is empty_topic_only (only the "general chat" topic).
    pub fn allows_named_topics(&self, channel: &Channel) -> bool {
        let policy = if channel.topics_policy == "inherit" {
            &self.topics_policy
        } else {
            &channel.topics_policy
        };
        policy != "empty_topic_only"
    }

    /// Applies one setting from a realm event (the name has no `realm_` prefix). Unknown settings are ignored.
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
