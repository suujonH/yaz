use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permission::ROLE_MEMBER;

/// 用户的自定义资料字段值。
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileValue {
    pub value: Value,
    pub rendered_value: Option<String>,
}

/// Zulip 用户（含机器人、已停用用户）。收到 realm_user 事件时就地更新。
#[derive(Clone, Debug, PartialEq)]
pub struct ZulipUser {
    pub id: i64,
    pub email: String,
    /// 真实邮箱，只在有权查看时有值。
    pub delivery_email: Option<String>,
    pub full_name: String,
    pub avatar_url: Option<String>,
    pub is_bot: bool,
    pub bot_type: Option<i32>,
    /// 机器人的所有者；不是机器人或跨组织机器人为 `None`。自己的机器人发的消息自己可以删除。
    pub bot_owner_id: Option<i64>,
    /// `permission::ROLE_*`。
    pub role: i32,
    pub is_active: bool,
    pub timezone: String,
    /// 键为资料字段 id。
    pub profile_data: HashMap<String, ProfileValue>,
}

impl Default for ZulipUser {
    fn default() -> Self {
        Self {
            id: 0,
            email: String::new(),
            delivery_email: None,
            full_name: String::new(),
            avatar_url: None,
            is_bot: false,
            bot_type: None,
            bot_owner_id: None,
            role: ROLE_MEMBER,
            is_active: true,
            timezone: String::new(),
            profile_data: HashMap::new(),
        }
    }
}

impl ZulipUser {
    /// 应用 realm_user update 事件的 `person`（只包含变化的字段）。
    pub fn apply_update(&mut self, person: &Value) {
        let Some(person) = person.as_object() else {
            return;
        };
        let string = |v: &Value| v.as_str().map(str::to_owned);
        for (name, value) in person {
            match name.as_str() {
                "full_name" => {
                    if let Some(s) = string(value) {
                        self.full_name = s;
                    }
                }
                "avatar_url" => self.avatar_url = string(value),
                "role" => {
                    if let Some(role) = value.as_i64() {
                        self.role = role as i32;
                    }
                }
                "timezone" => self.timezone = string(value).unwrap_or_default(),
                "new_email" => {
                    if let Some(s) = string(value) {
                        self.email = s;
                    }
                }
                "delivery_email" => self.delivery_email = string(value),
                "is_active" => {
                    if let Some(active) = value.as_bool() {
                        self.is_active = active;
                    }
                }
                "bot_owner_id" => self.bot_owner_id = value.as_i64(),
                "custom_profile_field" => self.apply_profile_field(value),
                _ => {}
            }
        }
    }

    // value 为 null 表示清除该字段
    fn apply_profile_field(&mut self, field: &Value) {
        let Some(id) = field.get("id").and_then(Value::as_i64) else {
            return;
        };
        let id = id.to_string();
        match field.get("value") {
            None | Some(Value::Null) => {
                self.profile_data.remove(&id);
            }
            Some(_) => {
                if let Ok(value) = serde_json::from_value(field.clone()) {
                    self.profile_data.insert(id, value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn apply_update_changes_only_given_fields() {
        let mut user = ZulipUser {
            full_name: "甲".into(),
            email: "a@example.com".into(),
            avatar_url: Some("/a.png".into()),
            ..Default::default()
        };
        user.apply_update(
            &json!({"full_name": "乙", "avatar_url": null, "role": 200, "is_active": false}),
        );
        assert_eq!(user.full_name, "乙");
        assert_eq!(user.avatar_url, None);
        assert_eq!(user.role, 200);
        assert!(!user.is_active);
        assert_eq!(user.email, "a@example.com");
    }

    #[test]
    fn apply_update_profile_field() {
        let mut user = ZulipUser::default();
        user.apply_update(
            &json!({"custom_profile_field": {"id": 3, "value": "x", "rendered_value": "<p>x</p>"}}),
        );
        assert_eq!(
            user.profile_data["3"].rendered_value.as_deref(),
            Some("<p>x</p>")
        );
        user.apply_update(&json!({"custom_profile_field": {"id": 3, "value": null}}));
        assert!(user.profile_data.is_empty());
    }
}
