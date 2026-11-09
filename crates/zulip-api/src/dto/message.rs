use serde::Deserialize;
use serde_json::Value;
use zulip_core::model::{Reaction, Submessage};

use crate::narrow::NarrowTerm;

/// 一条消息（`GET /messages`、`message` 事件）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MessageDto {
    pub id: i64,
    pub sender_id: i64,
    pub sender_full_name: String,
    pub sender_email: String,
    pub avatar_url: Option<String>,
    /// 服务器渲染的 HTML（`apply_markdown=false` 时为 Markdown 原文）。
    pub content: String,
    /// Unix 秒。
    pub timestamp: i64,
    /// `"stream"` 或 `"private"`。
    #[serde(rename = "type")]
    pub kind: String,
    pub stream_id: Option<i64>,
    /// 话题名；私聊为空字符串。
    pub subject: String,
    /// 频道消息为频道名（字符串）；私聊为 `[{id,email,full_name}]`，包含自己。
    pub display_recipient: Value,
    pub flags: Vec<String>,
    pub reactions: Vec<Reaction>,
    pub last_edit_timestamp: Option<i64>,
    /// 编辑和移动的记录，按时间从新到旧；没有编辑过、或组织不允许查看编辑历史时为 `None`。
    pub edit_history: Option<Vec<MessageEditDto>>,
    pub is_me_message: bool,
    pub submessages: Vec<Submessage>,
    /// 搜索时服务器返回的带高亮的内容。
    pub match_content: Option<String>,
    pub match_subject: Option<String>,
}

impl MessageDto {
    pub fn is_stream(&self) -> bool {
        self.kind == "stream"
    }

    /// 私聊的全部参与者 id（包含自己）；频道消息返回空列表。
    pub fn recipient_user_ids(&self) -> Vec<i64> {
        match &self.display_recipient {
            Value::Array(recipients) => recipients
                .iter()
                .filter_map(|r| r.get("id")?.as_i64())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// 消息 `edit_history` 中的一次编辑或移动。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MessageEditDto {
    pub timestamp: i64,
    /// 这次编辑之前的 HTML；只移动了话题 / 频道，或组织只允许查看移动记录时为 `None`。
    pub prev_rendered_content: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct GetMessagesResult {
    pub anchor: i64,
    pub found_newest: bool,
    pub found_oldest: bool,
    pub found_anchor: bool,
    pub history_limited: bool,
    pub messages: Vec<MessageDto>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FlagsNarrowResult {
    pub processed_count: i32,
    pub updated_count: i32,
    pub first_processed_id: Option<i64>,
    pub last_processed_id: Option<i64>,
    pub found_oldest: bool,
    pub found_newest: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SendMessageResult {
    pub id: i64,
}

/// 要发送的消息。频道消息设置 `stream_id` 与 `topic`，私聊设置 `user_ids`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutgoingMessage {
    pub stream_id: Option<i64>,
    pub topic: Option<String>,
    pub user_ids: Vec<i64>,
    pub content: String,
    /// 与 `local_id` 同时设置时，服务器把发送结果回显到该队列的事件中。
    pub queue_id: Option<String>,
    pub local_id: Option<String>,
}

/// `GET /messages` 的查询参数。
#[derive(Debug, Clone, PartialEq)]
pub struct MessageQuery {
    pub narrow: Vec<NarrowTerm>,
    /// `newest`、`oldest`、`first_unread`、消息 id，或 `date`（此时 `anchor_date` 为 ISO 8601 时间，需要 feature level ≥ 445）。
    pub anchor: String,
    pub num_before: u32,
    pub num_after: u32,
    pub include_anchor: bool,
    pub apply_markdown: bool,
    pub anchor_date: Option<String>,
}

impl MessageQuery {
    pub fn new(
        narrow: Vec<NarrowTerm>,
        anchor: impl ToString,
        num_before: u32,
        num_after: u32,
    ) -> Self {
        Self {
            narrow,
            anchor: anchor.to_string(),
            num_before,
            num_after,
            include_anchor: true,
            apply_markdown: true,
            anchor_date: None,
        }
    }
}

/// `PATCH /messages/{id}` 的修改项，`None` 表示不修改。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageUpdate {
    pub topic: Option<String>,
    /// 移动到另一个频道。
    pub stream_id: Option<i64>,
    /// `change_one` / `change_later` / `change_all`。
    pub propagate_mode: Option<String>,
    pub content: Option<String>,
}

/// 消息标志的修改方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagOp {
    Add,
    Remove,
}

impl FlagOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
        }
    }
}

/// 消息标志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageFlag {
    Read,
    Starred,
}

impl MessageFlag {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Starred => "starred",
        }
    }
}

/// `POST /messages/flags/narrow` 的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct FlagsNarrowUpdate {
    pub narrow: Vec<NarrowTerm>,
    pub anchor: String,
    pub include_anchor: bool,
    pub num_before: u32,
    pub num_after: u32,
    pub op: FlagOp,
    pub flag: MessageFlag,
}
