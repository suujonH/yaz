use serde::Deserialize;
use serde_json::Value;
use zulip_core::model::{Reaction, Submessage};

use crate::narrow::NarrowTerm;

/// A message (`GET /messages`, `message` event).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MessageDto {
    pub id: i64,
    pub sender_id: i64,
    pub sender_full_name: String,
    pub sender_email: String,
    pub avatar_url: Option<String>,
    /// Server-rendered HTML (Markdown source when `apply_markdown=false`).
    pub content: String,
    /// Unix seconds.
    pub timestamp: i64,
    /// `"stream"` or `"private"`.
    #[serde(rename = "type")]
    pub kind: String,
    pub stream_id: Option<i64>,
    /// Topic name; empty string for DMs.
    pub subject: String,
    /// For channel messages the channel name (string); for DMs `[{id,email,full_name}]`, including
    /// self.
    pub display_recipient: Value,
    pub flags: Vec<String>,
    pub reactions: Vec<Reaction>,
    pub last_edit_timestamp: Option<i64>,
    /// Records of edits and moves, newest to oldest; `None` if never edited or the organization
    /// does not allow viewing edit history.
    pub edit_history: Option<Vec<MessageEditDto>>,
    pub is_me_message: bool,
    pub submessages: Vec<Submessage>,
    /// Highlighted content returned by the server when searching.
    pub match_content: Option<String>,
    pub match_subject: Option<String>,
}

impl MessageDto {
    pub fn is_stream(&self) -> bool {
        self.kind == "stream"
    }

    /// IDs of all DM participants (including self); an empty list for channel messages.
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

/// One edit or move in a message's `edit_history`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MessageEditDto {
    pub timestamp: i64,
    /// HTML before this edit; `None` if only the topic / channel was moved, or the organization
    /// only allows viewing move records.
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

/// A message to send. Channel messages set `stream_id` and `topic`; DMs set `user_ids`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutgoingMessage {
    pub stream_id: Option<i64>,
    pub topic: Option<String>,
    pub user_ids: Vec<i64>,
    pub content: String,
    /// When set together with `local_id`, the server echoes the send result into events of that
    /// queue.
    pub queue_id: Option<String>,
    pub local_id: Option<String>,
}

/// Query parameters of `GET /messages`.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageQuery {
    pub narrow: Vec<NarrowTerm>,
    /// `newest`, `oldest`, `first_unread`, a message id, or `date` (then `anchor_date` is an ISO
    /// 8601 time; needs feature level >= 445).
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

/// Changes for `PATCH /messages/{id}`; `None` means no change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageUpdate {
    pub topic: Option<String>,
    /// Move to another channel.
    pub stream_id: Option<i64>,
    /// `change_one` / `change_later` / `change_all`.
    pub propagate_mode: Option<String>,
    pub content: Option<String>,
}

/// How to modify message flags.
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

/// Message flags.
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

/// Parameters of `POST /messages/flags/narrow`.
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
