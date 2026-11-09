//! 定时消息、消息提醒、常用语、编辑历史和附件。

use serde::{Deserialize, Serialize};

use crate::conversation::ConversationKey;

/// 定时消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledMessage {
    pub id: i64,
    pub key: ConversationKey,
    /// 话题名（原始大小写）；私聊为空。
    pub topic: String,
    /// Markdown 原文。
    pub content: String,
    pub rendered_content: String,
    /// Unix 秒。
    pub deliver_at: i64,
    /// 到时间后发送失败。
    pub failed: bool,
}

impl ScheduledMessage {
    /// 显示用的 HTML：服务器渲染的内容，没有时是原文。
    pub fn html(&self) -> &str {
        if self.rendered_content.is_empty() {
            &self.content
        } else {
            &self.rendered_content
        }
    }
}

/// 尚未送达的消息提醒。到时间后服务器由 Notification Bot 私信自己。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    pub id: i64,
    /// 要提醒的消息。
    pub target_message_id: i64,
    pub content: String,
    pub rendered_content: String,
    /// Unix 秒。
    pub deliver_at: i64,
    pub failed: bool,
}

impl Reminder {
    /// 显示用的 HTML：服务器渲染的内容，没有时是原文。
    pub fn html(&self) -> &str {
        if self.rendered_content.is_empty() {
            &self.content
        } else {
            &self.rendered_content
        }
    }
}

/// 常用语。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSnippet {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub date_created: i64,
}

/// 服务器上消息编辑历史中的一个版本。第一项是原始版本，之后每一项是一次编辑：
/// 改了内容时有 `prev_content` 与 `content_html_diff`，改了话题或频道时有 `prev_topic` / `prev_stream`。
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
    /// 与上一版本的差异：新增的文字在 `span.highlight_text_inserted` 中，删除的在 `span.highlight_text_deleted` 中。
    pub content_html_diff: Option<String>,
    /// 编辑者；系统自动修改时为 `None`。
    pub user_id: Option<i64>,
    pub timestamp: i64,
}

/// 上传成功的文件。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadedFile {
    /// 形如 `/user_uploads/…` 的相对地址。
    pub url: String,
    pub filename: Option<String>,
}

/// 自己上传的文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub id: i64,
    /// 文件名。
    pub name: String,
    /// `/user_uploads/` 之后的路径。
    pub path_id: String,
    /// 字节。
    pub size: i64,
    /// 上传时间，Unix 秒。
    pub created_at: i64,
    /// 引用这个文件的消息。
    pub message_ids: Vec<i64>,
}

/// 我上传的全部文件。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentList {
    pub attachments: Vec<Attachment>,
    /// 已使用的上传存储空间（字节）。
    pub upload_space_used: i64,
}
