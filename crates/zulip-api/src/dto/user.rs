use std::collections::HashMap;

use serde::Deserialize;
use zulip_core::model::ProfileValue;

use super::{int_key_map, lenient_i64, null_default};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct UserDto {
    pub user_id: i64,
    pub email: String,
    /// Real email; appears only when you have permission to see it.
    pub delivery_email: Option<String>,
    pub full_name: String,
    pub avatar_url: Option<String>,
    pub is_bot: bool,
    pub bot_type: Option<i32>,
    pub bot_owner_id: Option<i64>,
    /// 100 owner / 200 administrator / 300 moderator / 400 member / 600 guest.
    pub role: i32,
    /// Absent in `realm_users`, defaults to true; false in `realm_non_active_users`.
    pub is_active: bool,
    #[serde(deserialize_with = "null_default")]
    pub timezone: String,
    pub date_joined: Option<String>,
    pub profile_data: Option<HashMap<String, ProfileValue>>,
}

impl Default for UserDto {
    fn default() -> Self {
        Self {
            user_id: 0,
            email: String::new(),
            delivery_email: None,
            full_name: String::new(),
            avatar_url: None,
            is_bot: false,
            bot_type: None,
            bot_owner_id: None,
            role: 400,
            is_active: true,
            timezone: String::new(),
            date_joined: None,
            profile_data: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PresenceDto {
    #[serde(deserialize_with = "lenient_i64")]
    pub active_timestamp: i64,
    #[serde(deserialize_with = "lenient_i64")]
    pub idle_timestamp: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UserStatusDto {
    pub status_text: Option<String>,
    pub emoji_name: Option<String>,
    pub emoji_code: Option<String>,
    pub reaction_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MutedUserDto {
    pub id: i64,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RealmEmojiDto {
    pub id: String,
    pub name: String,
    pub source_url: String,
    pub still_url: Option<String>,
    pub deactivated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PresenceUpdateResult {
    #[serde(deserialize_with = "int_key_map")]
    pub presences: HashMap<i64, PresenceDto>,
    pub presence_last_update_id: Option<i64>,
    pub server_timestamp: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ApiKeyResultDto {
    pub api_key: String,
    pub email: String,
    pub user_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ServerSettingsDto {
    pub zulip_feature_level: i32,
    pub zulip_version: String,
    pub realm_name: String,
    pub realm_icon: Option<String>,
    pub realm_url: String,
    pub email_auth_enabled: bool,
    pub require_email_format_usernames: bool,
}
