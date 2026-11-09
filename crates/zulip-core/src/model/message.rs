use serde::{Deserialize, Serialize};

use super::MentionKind;
use crate::conversation::ConversationKey;
use crate::notice::SystemNoticeKind;

/// 表情回应。
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reaction {
    pub emoji_name: String,
    pub emoji_code: String,
    pub reaction_type: String,
    pub user_id: i64,
}

/// 投票、待办等小部件的子消息。第一条的 `content` 是小部件定义，之后每一条是一次操作；
/// `content` 是 JSON 字符串。
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Submessage {
    pub id: i64,
    pub message_id: i64,
    pub sender_id: i64,
    pub msg_type: String,
    pub content: String,
}

/// 服务器 flags 中本客户端用到的部分。`starred` 是星标的唯一来源。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageFlags {
    pub read: bool,
    pub starred: bool,
    pub mentioned: bool,
    /// `stream_wildcard_mentioned`、`topic_wildcard_mentioned`、`wildcard_mentioned` 之一。
    pub wildcard_mentioned: bool,
    pub has_alert_word: bool,
}

impl MessageFlags {
    pub fn from_names<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Self {
        let mut flags = Self::default();
        for name in names {
            flags.set_named(name.as_ref(), true);
        }
        flags
    }

    /// 按服务器的名字设置。返回是否有变化，不关心的 flag 返回 false。
    pub fn set_named(&mut self, name: &str, on: bool) -> bool {
        let field = match name {
            "read" => &mut self.read,
            "starred" => &mut self.starred,
            "mentioned" => &mut self.mentioned,
            "stream_wildcard_mentioned" | "topic_wildcard_mentioned" | "wildcard_mentioned" => {
                &mut self.wildcard_mentioned
            }
            "has_alert_word" => &mut self.has_alert_word,
            _ => return false,
        };
        let changed = *field != on;
        *field = on;
        changed
    }

    pub fn mention(&self) -> MentionKind {
        if self.mentioned {
            MentionKind::Personal
        } else if self.wildcard_mentioned {
            MentionKind::Wildcard
        } else {
            MentionKind::None
        }
    }
}

/// 本地回显：尚未收到服务器确认的消息的发送信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalEcho {
    /// 发送时的 `local_id`，用于 `retry_send`、`discard_failed`。
    pub local_id: String,
    /// Markdown 原文：重发时使用。
    pub raw_content: String,
}

/// 消息的生命周期状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MessageState {
    /// 服务器上的消息。
    #[default]
    Normal,
    /// 本地回显，发送中。
    Sending(LocalEcho),
    /// 本地回显，发送失败。
    Failed(LocalEcho),
    /// 已在服务器上被删除，只是本机保存的副本：聊天中显示为“已被删除”，点击后展开。
    Deleted,
}

/// 一条消息。服务器上的消息 `id` 为正数；本地回显的消息 `id` 为负数，
/// 收到服务器的 message 事件后被真正的消息替换。
#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    pub id: i64,
    pub key: ConversationKey,
    pub sender_id: i64,
    /// 发送时的名字快照。显示用 `Session::display_name`（含备注和改名）；
    /// 这里用于引用回复，以及用户已不在用户列表时的兜底。
    pub sender_name: String,
    pub sender_email: String,
    /// 发送时的头像快照。显示优先用当前用户资料，用户已不在用户列表时用它兜底。
    pub avatar_url: Option<String>,
    /// 服务器渲染的 HTML。
    pub content_html: String,
    /// Unix 秒。
    pub timestamp: i64,
    pub flags: MessageFlags,
    pub reactions: Vec<Reaction>,
    pub last_edit_timestamp: Option<i64>,
    pub is_me_message: bool,
    /// 话题名（原始大小写）；私聊为空字符串。
    pub topic: String,
    pub stream_id: Option<i64>,
    /// 按 id 升序；submessage 事件追加。
    pub submessages: Vec<Submessage>,
    pub state: MessageState,
    /// Notification Bot 消息的显示方式。
    pub notice: SystemNoticeKind,
}

impl ChatMessage {
    pub fn new(id: i64, key: ConversationKey, sender_id: i64, timestamp: i64) -> Self {
        Self {
            id,
            key,
            sender_id,
            timestamp,
            sender_name: String::new(),
            sender_email: String::new(),
            avatar_url: None,
            content_html: String::new(),
            flags: MessageFlags::default(),
            reactions: Vec::new(),
            last_edit_timestamp: None,
            is_me_message: false,
            topic: String::new(),
            stream_id: None,
            submessages: Vec::new(),
            state: MessageState::Normal,
            notice: SystemNoticeKind::None,
        }
    }

    /// 已归入系统通知或消息提醒，不在聊天中显示。
    pub fn is_system_notice(&self) -> bool {
        matches!(
            self.notice,
            SystemNoticeKind::Notice | SystemNoticeKind::Reminder
        )
    }

    pub fn is_deleted(&self) -> bool {
        self.state == MessageState::Deleted
    }

    /// 本地回显的发送信息；服务器消息为 `None`。
    pub fn local_echo(&self) -> Option<&LocalEcho> {
        match &self.state {
            MessageState::Sending(echo) | MessageState::Failed(echo) => Some(echo),
            MessageState::Normal | MessageState::Deleted => None,
        }
    }

    pub fn is_read(&self) -> bool {
        self.flags.read
    }

    pub fn is_starred(&self) -> bool {
        self.flags.starred
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_notice() {
        let mut m = ChatMessage::new(1, ConversationKey::topic(1, "t"), 2, 3);
        assert!(!m.is_read() && !m.is_system_notice());
        m.flags = MessageFlags::from_names(["read", "collapsed"]);
        assert!(m.is_read());
        m.notice = SystemNoticeKind::Inline;
        assert!(!m.is_system_notice());
        m.notice = SystemNoticeKind::Reminder;
        assert!(m.is_system_notice());
    }

    #[test]
    fn named_flags_report_changes_and_mention_kind() {
        let mut flags = MessageFlags::default();
        assert_eq!(flags.mention(), MentionKind::None);
        assert!(flags.set_named("topic_wildcard_mentioned", true));
        assert!(!flags.set_named("stream_wildcard_mentioned", true));
        assert_eq!(flags.mention(), MentionKind::Wildcard);
        assert!(flags.set_named("mentioned", true));
        assert_eq!(flags.mention(), MentionKind::Personal);
        assert!(!flags.set_named("collapsed", true));
    }

    #[test]
    fn local_echo_only_on_unconfirmed_messages() {
        let mut m = ChatMessage::new(-1, ConversationKey::topic(1, "t"), 2, 3);
        assert!(m.local_echo().is_none());
        let echo = LocalEcho {
            local_id: "1.1".into(),
            raw_content: "x".into(),
        };
        m.state = MessageState::Sending(echo.clone());
        assert_eq!(m.local_echo(), Some(&echo));
        m.state = MessageState::Failed(echo.clone());
        assert_eq!(m.local_echo(), Some(&echo));
        m.state = MessageState::Deleted;
        assert!(m.is_deleted() && m.local_echo().is_none());
    }
}
