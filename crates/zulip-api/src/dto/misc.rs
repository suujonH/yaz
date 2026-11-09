use serde::Deserialize;
use serde_json::Value;
use zulip_core::model::Attachment;

/// register 返回的未读摘要。
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
    /// 对方 id；与自己的私聊时为自己的 id。
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
    /// 逗号分隔的全部参与者 id（包含自己）。
    pub user_ids_string: String,
    pub unread_message_ids: Vec<i64>,
}

/// 草稿。`kind` 为 `stream`、`private` 或空字符串（未指定收件人）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DraftDto {
    pub id: Option<i64>,
    #[serde(rename = "type")]
    pub kind: String,
    /// 频道草稿为 `[stream_id]`；私聊为对方 id 列表。
    pub to: Vec<i64>,
    pub topic: String,
    pub content: String,
    pub timestamp: Option<i64>,
}

/// 定时消息（`GET /scheduled_messages`、register 的 `scheduled_messages`）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ScheduledMessageDto {
    pub scheduled_message_id: i64,
    /// `"stream"` 或 `"private"`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 频道消息为频道 id（数字）；私聊为收件人 id 数组。
    pub to: Value,
    pub topic: String,
    pub content: String,
    pub rendered_content: String,
    /// Unix 秒。
    pub scheduled_delivery_timestamp: i64,
    /// 到时间后发送失败。
    pub failed: bool,
}

/// 消息提醒（register 的 `reminders`、reminders 事件）。到时间后服务器由 Notification Bot 私信自己。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ReminderDto {
    pub reminder_id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    /// 收件人 id 数组（只有自己）。
    pub to: Value,
    pub content: String,
    pub rendered_content: String,
    /// Unix 秒。
    pub scheduled_delivery_timestamp: i64,
    pub failed: bool,
    /// 要提醒的消息。
    pub reminder_target_message_id: i64,
}

/// 自己上传的文件（`GET /attachments`、attachment 事件）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AttachmentDto {
    pub id: i64,
    /// 文件名。
    pub name: String,
    /// `/user_uploads/` 之后的路径。
    pub path_id: String,
    /// 字节。
    pub size: i64,
    /// 上传时间。随服务器版本为 Unix 秒或 Unix 毫秒，读取时统一为秒。
    pub create_time: i64,
    /// 引用这个文件的消息 id。
    pub message_ids: Vec<i64>,
    /// 引用这个文件的消息（部分版本的服务器返回它而不是 `message_ids`）。
    pub messages: Vec<AttachmentMessageDto>,
}

impl From<AttachmentDto> for Attachment {
    fn from(dto: AttachmentDto) -> Self {
        // 秒数要到公元 5000 年后才会超过 1e11，更大的值是毫秒。
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

/// 引用上传文件的一条消息。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AttachmentMessageDto {
    pub id: i64,
    /// 发送时间，Unix 毫秒。
    pub date_sent: i64,
}
