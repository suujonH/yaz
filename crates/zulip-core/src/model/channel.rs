use serde_json::Value;

use crate::permission::{ChannelGroupSettings, GroupSettingValue};

/// 频道中的一个话题。`name` 为原始大小写。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicInfo {
    pub name: String,
    pub max_id: i64,
}

/// 对频道的订阅：个人的静音和通知设置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subscription {
    pub is_muted: bool,
    pub desktop_notifications: Option<bool>,
    pub audible_notifications: Option<bool>,
    pub push_notifications: Option<bool>,
    pub email_notifications: Option<bool>,
    pub wildcard_mentions_notify: Option<bool>,
}

/// 频道（界面上的“群分组”）。`subscription` 为 `None` 表示已退订或从未订阅。
/// 收到 subscription、stream 事件时就地更新。
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub stream_id: i64,

    pub name: String,
    pub description: String,
    pub rendered_description: String,
    pub invite_only: bool,
    pub is_web_public: bool,
    pub is_archived: bool,
    pub history_public_to_subscribers: bool,
    pub subscriber_count: i32,
    pub creator_id: Option<i64>,
    pub date_created: i64,
    pub folder_id: Option<i64>,
    pub topics_policy: String,

    /// 个人设置的颜色，退订后仍用于该频道话题的头像；从未订阅时为空。
    pub color: String,
    pub subscription: Option<Subscription>,

    pub permissions: ChannelGroupSettings,
}

impl Default for Channel {
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
            color: String::new(),
            subscription: None,
            permissions: ChannelGroupSettings::default(),
        }
    }
}

fn set_bool(target: &mut bool, value: Option<&Value>) {
    if let Some(v) = value.and_then(Value::as_bool) {
        *target = v;
    }
}

fn set_string(target: &mut String, value: &Value) {
    if let Some(s) = value.as_str() {
        s.clone_into(target);
    }
}

fn group(value: &Value) -> Option<GroupSettingValue> {
    serde_json::from_value(value.clone()).ok()
}

impl Channel {
    pub fn is_subscribed(&self) -> bool {
        self.subscription.is_some()
    }

    /// 已静音。没有订阅时为 false。
    pub fn is_muted(&self) -> bool {
        self.subscription.as_ref().is_some_and(|s| s.is_muted)
    }

    /// 应用 stream update 事件（`property` + `value`，部分属性附带额外字段）。未知属性忽略。
    pub fn apply_stream_update(&mut self, event: &Value) {
        let value = event.get("value").unwrap_or(&Value::Null);
        let permissions = &mut self.permissions;
        match event.get("property").and_then(Value::as_str).unwrap_or("") {
            "name" => set_string(&mut self.name, value),
            "description" => {
                self.description = value.as_str().unwrap_or("").to_owned();
                if let Some(rendered) = event.get("rendered_description") {
                    self.rendered_description = rendered.as_str().unwrap_or("").to_owned();
                }
            }
            "invite_only" => {
                set_bool(&mut self.invite_only, Some(value));
                set_bool(
                    &mut self.history_public_to_subscribers,
                    event.get("history_public_to_subscribers"),
                );
                set_bool(&mut self.is_web_public, event.get("is_web_public"));
            }
            "is_web_public" => set_bool(&mut self.is_web_public, Some(value)),
            "history_public_to_subscribers" => {
                set_bool(&mut self.history_public_to_subscribers, Some(value));
            }
            "is_archived" => set_bool(&mut self.is_archived, Some(value)),
            "folder_id" => self.folder_id = value.as_i64(),
            "topics_policy" => set_string(&mut self.topics_policy, value),
            "can_administer_channel_group" => {
                permissions.can_administer_channel_group = group(value)
            }
            "can_add_subscribers_group" => permissions.can_add_subscribers_group = group(value),
            "can_remove_subscribers_group" => {
                permissions.can_remove_subscribers_group = group(value)
            }
            "can_send_message_group" => permissions.can_send_message_group = group(value),
            "can_move_messages_out_of_channel_group" => {
                permissions.can_move_messages_out_of_channel_group = group(value);
            }
            "can_move_messages_within_channel_group" => {
                permissions.can_move_messages_within_channel_group = group(value);
            }
            "can_resolve_topics_group" => permissions.can_resolve_topics_group = group(value),
            "can_delete_any_message_group" => {
                permissions.can_delete_any_message_group = group(value)
            }
            "can_delete_own_message_group" => {
                permissions.can_delete_own_message_group = group(value)
            }
            "can_create_topic_group" => permissions.can_create_topic_group = group(value),
            "can_subscribe_group" => permissions.can_subscribe_group = group(value),
            _ => {}
        }
    }

    /// 应用 subscription update 事件中的个人设置。未知属性忽略。
    pub fn apply_subscription_property(&mut self, property: &str, value: &Value) {
        if property == "color" {
            set_string(&mut self.color, value);
            return;
        }
        let Some(subscription) = &mut self.subscription else {
            return;
        };
        match property {
            "is_muted" => set_bool(&mut subscription.is_muted, Some(value)),
            // 服务器仍可能发送的别名，含义与 is_muted 相反
            "in_home_view" => {
                if let Some(v) = value.as_bool() {
                    subscription.is_muted = !v;
                }
            }
            "desktop_notifications" => subscription.desktop_notifications = value.as_bool(),
            "audible_notifications" => subscription.audible_notifications = value.as_bool(),
            "push_notifications" => subscription.push_notifications = value.as_bool(),
            "email_notifications" => subscription.email_notifications = value.as_bool(),
            "wildcard_mentions_notify" => subscription.wildcard_mentions_notify = value.as_bool(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stream_update() {
        let mut ch = Channel::default();
        ch.apply_stream_update(&json!({"property": "name", "value": "开发"}));
        assert_eq!(ch.name, "开发");
        ch.apply_stream_update(
            &json!({"property": "description", "value": "d", "rendered_description": "<p>d</p>"}),
        );
        assert_eq!(ch.rendered_description, "<p>d</p>");
        ch.apply_stream_update(&json!({
            "property": "invite_only", "value": true,
            "history_public_to_subscribers": false, "is_web_public": false
        }));
        assert!(ch.invite_only && !ch.history_public_to_subscribers);
        ch.apply_stream_update(&json!({"property": "folder_id", "value": 7}));
        assert_eq!(ch.folder_id, Some(7));
        ch.apply_stream_update(&json!({"property": "folder_id", "value": null}));
        assert_eq!(ch.folder_id, None);
        ch.apply_stream_update(&json!({"property": "can_send_message_group", "value": 14}));
        assert_eq!(
            ch.permissions.can_send_message_group,
            Some(GroupSettingValue::Group(14))
        );
        ch.apply_stream_update(&json!({
            "property": "can_send_message_group",
            "value": {"direct_members": [1], "direct_subgroups": []}
        }));
        assert_eq!(
            ch.permissions.can_send_message_group,
            Some(GroupSettingValue::anonymous([1], []))
        );
        ch.apply_stream_update(&json!({"property": "unknown", "value": 1}));
    }

    #[test]
    fn subscription_property() {
        let mut ch = Channel {
            subscription: Some(Subscription::default()),
            ..Channel::default()
        };
        ch.apply_subscription_property("color", &json!("#fff"));
        assert_eq!(ch.color, "#fff");
        ch.apply_subscription_property("in_home_view", &json!(false));
        assert!(ch.is_muted());
        ch.apply_subscription_property("is_muted", &json!(false));
        assert!(!ch.is_muted());
        ch.apply_subscription_property("push_notifications", &json!(true));
        assert_eq!(
            ch.subscription.as_ref().unwrap().push_notifications,
            Some(true)
        );
        ch.apply_subscription_property("push_notifications", &json!(null));
        assert_eq!(ch.subscription.as_ref().unwrap().push_notifications, None);

        ch.subscription = None;
        ch.apply_subscription_property("is_muted", &json!(true));
        assert!(!ch.is_muted() && !ch.is_subscribed());
    }
}
