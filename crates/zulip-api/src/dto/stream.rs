use std::ops::Deref;

use serde::Deserialize;
use serde_json::Value;

use super::null_default;
use zulip_core::permission::GroupSettingValue;

/// 频道（`streams`、`never_subscribed`）。各 `can_*_group` 缺失时为 `None`。
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
    /// 话题策略：`inherit`（沿用组织设置）/ `allow_empty_topic` / `disable_empty_topic` / `empty_topic_only`。
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

/// 已订阅或曾订阅的频道（`subscriptions`、`unsubscribed`），包含个人设置。
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
    /// 0 无 / 1 静音 / 2 取消静音 / 3 关注。
    pub visibility_policy: i32,
    pub last_updated: i64,
}

/// `GET /users/me/{stream_id}/topics` 的一项。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TopicDto {
    pub name: String,
    pub max_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RecentPrivateConversationDto {
    /// 对方的 id（不含自己）；与自己的私聊为空数组。
    pub user_ids: Vec<i64>,
    pub max_message_id: i64,
}

/// `POST /users/me/subscriptions/properties` 的一项。
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionPropertyChange {
    pub stream_id: i64,
    pub property: String,
    pub value: Value,
}
