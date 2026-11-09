use serde::Deserialize;
use serde_json::Value;
use zulip_core::model::Attachment;

/// Unread summary returned by register.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UnreadMsgsDto {
    pub count: i32,
    pub pms: Vec<UnreadPm>,
    pub streams: Vec<UnreadStream>,
    pub huddles: Vec<UnreadHuddle>,
    pub mentions: Vec<i64>,
    pub old_unreads_missing: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UnreadPm {
    /// The other party's id; for a DM with self, your own id.
    pub other_user_id: i64,
    pub unread_message_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UnreadStream {
    pub stream_id: i64,
    pub topic: String,
    pub unread_message_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct UnreadHuddle {
    /// Comma-separated ids of all participants (including self).
    pub user_ids_string: String,
    pub unread_message_ids: Vec<i64>,
}

/// A draft. `kind` is `stream`, `private`, or an empty string (no recipient specified).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DraftDto {
    pub id: Option<i64>,
    #[serde(rename = "type")]
    pub kind: String,
    /// `[stream_id]` for channel drafts; the list of the other party's ids for DMs.
    pub to: Vec<i64>,
    pub topic: String,
    pub content: String,
    pub timestamp: Option<i64>,
}

/// A scheduled message (`GET /scheduled_messages`, `scheduled_messages` of register).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ScheduledMessageDto {
    pub scheduled_message_id: i64,
    /// `"stream"` or `"private"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Channel id (number) for channel messages; array of recipient ids for DMs.
    pub to: Value,
    pub topic: String,
    pub content: String,
    pub rendered_content: String,
    /// Unix seconds.
    pub scheduled_delivery_timestamp: i64,
    /// Sending failed at the due time.
    pub failed: bool,
}

/// A message reminder (`reminders` of register, reminders event). At the due time the server has
/// Notification Bot DM you.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ReminderDto {
    pub reminder_id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    /// Array of recipient ids (only yourself).
    pub to: Value,
    pub content: String,
    pub rendered_content: String,
    /// Unix seconds.
    pub scheduled_delivery_timestamp: i64,
    pub failed: bool,
    /// The message to remind about.
    pub reminder_target_message_id: i64,
}

/// A file you uploaded (`GET /attachments`, attachment event).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AttachmentDto {
    pub id: i64,
    /// File name.
    pub name: String,
    /// Path after `/user_uploads/`.
    pub path_id: String,
    /// Bytes.
    pub size: i64,
    /// Upload time. Unix seconds or Unix milliseconds depending on the server version; normalized
    /// to seconds when read.
    pub create_time: i64,
    /// Ids of messages that reference this file.
    pub message_ids: Vec<i64>,
    /// Messages that reference this file (some server versions return this instead of
    /// `message_ids`).
    pub messages: Vec<AttachmentMessageDto>,
}

impl From<AttachmentDto> for Attachment {
    fn from(dto: AttachmentDto) -> Self {
        // Seconds will not exceed 1e11 until after the year 5000; larger values are milliseconds.
        let created_at = if dto.create_time > 100_000_000_000 {
            dto.create_time / 1000
        } else {
            dto.create_time
        };
        let message_ids = if dto.message_ids.is_empty() {
            dto.messages.iter().map(|m| m.id).collect()
        } else {
            dto.message_ids
        };
        Self {
            id: dto.id,
            name: dto.name,
            path_id: dto.path_id,
            size: dto.size,
            created_at,
            message_ids,
        }
    }
}

/// A message that references an uploaded file.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AttachmentMessageDto {
    pub id: i64,
    /// Send time, Unix milliseconds.
    pub date_sent: i64,
}
