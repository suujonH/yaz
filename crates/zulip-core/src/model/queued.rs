//! Scheduled messages, message reminders, canned replies, edit history and attachments.

use serde::{Deserialize, Serialize};

use crate::conversation::ConversationKey;

/// Scheduled message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledMessage {
    pub id: i64,
    pub key: ConversationKey,
    /// Topic name (original case); empty for DMs.
    pub topic: String,
    /// Original Markdown.
    pub content: String,
    pub rendered_content: String,
    /// Unix seconds.
    pub deliver_at: i64,
    /// Sending failed after the time came.
    pub failed: bool,
}

impl ScheduledMessage {
    /// HTML for display: the server-rendered content, or the original text when there is none.
    pub fn html(&self) -> &str {
        if self.rendered_content.is_empty() {
            &self.content
        } else {
            &self.rendered_content
        }
    }
}

/// A message reminder not yet delivered. When the time comes, the server sends oneself a DM via Notification Bot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    pub id: i64,
    /// The message to remind about.
    pub target_message_id: i64,
    pub content: String,
    pub rendered_content: String,
    /// Unix seconds.
    pub deliver_at: i64,
    pub failed: bool,
}

impl Reminder {
    /// HTML for display: the server-rendered content, or the original text when there is none.
    pub fn html(&self) -> &str {
        if self.rendered_content.is_empty() {
            &self.content
        } else {
            &self.rendered_content
        }
    }
}

/// Canned reply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSnippet {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub date_created: i64,
}

/// One version in a message's edit history on the server. The first entry is the original version, and each later entry is one edit:
/// when the content changed there are `prev_content` and `content_html_diff`; when the topic or channel changed there are `prev_topic` / `prev_stream`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageRevision {
    pub topic: String,
    pub prev_topic: Option<String>,
    pub stream: Option<i64>,
    pub prev_stream: Option<i64>,
    pub content: String,
    pub rendered_content: String,
    pub prev_content: Option<String>,
    pub prev_rendered_content: Option<String>,
    /// Difference from the previous version: inserted text is in `span.highlight_text_inserted`, deleted text in `span.highlight_text_deleted`.
    pub content_html_diff: Option<String>,
    /// The editor; `None` for automatic system changes.
    pub user_id: Option<i64>,
    pub timestamp: i64,
}

/// A successfully uploaded file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadedFile {
    /// A relative address like `/user_uploads/...`.
    pub url: String,
    pub filename: Option<String>,
}

/// A file uploaded by oneself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub id: i64,
    /// File name.
    pub name: String,
    /// The path after `/user_uploads/`.
    pub path_id: String,
    /// Bytes.
    pub size: i64,
    /// Upload time, Unix seconds.
    pub created_at: i64,
    /// Messages that reference this file.
    pub message_ids: Vec<i64>,
}

/// All files I uploaded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentList {
    pub attachments: Vec<Attachment>,
    /// Upload storage space used (bytes).
    pub upload_space_used: i64,
}
