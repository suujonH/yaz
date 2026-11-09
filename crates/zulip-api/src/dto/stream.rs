use std::ops::Deref;

use serde::Deserialize;
use serde_json::Value;

use super::null_default;
use zulip_core::permission::GroupSettingValue;

/// A channel (`streams`, `never_subscribed`). Each missing `can_*_group` is `None`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct StreamDto {
    pub stream_id: i64,
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub description: String,
    #[serde(deserialize_with = "null_default")]
    pub rendered_description: String,
    pub invite_only: bool,
    pub is_web_public: bool,
    pub is_archived: bool,
    pub history_public_to_subscribers: bool,
    pub subscriber_count: i32,
    pub creator_id: Option<i64>,
    pub date_created: i64,
    pub folder_id: Option<i64>,
    /// Topic policy: `inherit` (follow the organization setting) / `allow_empty_topic` /
    /// `disable_empty_topic` / `empty_topic_only`.
    pub topics_policy: String,

    pub can_administer_channel_group: Option<GroupSettingValue>,
    pub can_add_subscribers_group: Option<GroupSettingValue>,
    pub can_remove_subscribers_group: Option<GroupSettingValue>,
    pub can_send_message_group: Option<GroupSettingValue>,
    pub can_move_messages_out_of_channel_group: Option<GroupSettingValue>,
    pub can_move_messages_within_channel_group: Option<GroupSettingValue>,
    pub can_resolve_topics_group: Option<GroupSettingValue>,
    pub can_delete_any_message_group: Option<GroupSettingValue>,
    pub can_delete_own_message_group: Option<GroupSettingValue>,
    pub can_create_topic_group: Option<GroupSettingValue>,
    pub can_subscribe_group: Option<GroupSettingValue>,
}

impl Default for StreamDto {
    fn default() -> Self {
        Self {
            stream_id: 0,
            name: String::new(),
            description: String::new(),
            rendered_description: String::new(),
            invite_only: false,
            is_web_public: false,
            is_archived: false,
            history_public_to_subscribers: true,
            subscriber_count: 0,
            creator_id: None,
            date_created: 0,
            folder_id: None,
            topics_policy: "inherit".to_owned(),
            can_administer_channel_group: None,
            can_add_subscribers_group: None,
            can_remove_subscribers_group: None,
            can_send_message_group: None,
            can_move_messages_out_of_channel_group: None,
            can_move_messages_within_channel_group: None,
            can_resolve_topics_group: None,
            can_delete_any_message_group: None,
            can_delete_own_message_group: None,
            can_create_topic_group: None,
            can_subscribe_group: None,
        }
    }
}

/// A channel that is or was subscribed (`subscriptions`, `unsubscribed`), including personal
/// settings.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SubscriptionDto {
    #[serde(flatten)]
    pub stream: StreamDto,
    #[serde(deserialize_with = "null_default")]
    pub color: String,
    pub is_muted: bool,
    pub desktop_notifications: Option<bool>,
    pub audible_notifications: Option<bool>,
    pub push_notifications: Option<bool>,
    pub email_notifications: Option<bool>,
    pub wildcard_mentions_notify: Option<bool>,
}

impl Deref for SubscriptionDto {
    type Target = StreamDto;

    fn deref(&self) -> &StreamDto {
        &self.stream
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UserTopicDto {
    pub stream_id: i64,
    pub topic_name: String,
    /// 0 none / 1 muted / 2 unmuted / 3 followed.
    pub visibility_policy: i32,
    pub last_updated: i64,
}

/// One item of `GET /users/me/{stream_id}/topics`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TopicDto {
    pub name: String,
    pub max_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RecentPrivateConversationDto {
    /// The other party's id (excluding self); an empty array for a DM with self.
    pub user_ids: Vec<i64>,
    pub max_message_id: i64,
}

/// One item of `POST /users/me/subscriptions/properties`.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionPropertyChange {
    pub stream_id: i64,
    pub property: String,
    pub value: Value,
}
