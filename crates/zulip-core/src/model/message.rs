use serde::{Deserialize, Serialize};

use super::MentionKind;
use crate::conversation::ConversationKey;
use crate::notice::SystemNoticeKind;

/// Reaction.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reaction {
    pub emoji_name: String,
    pub emoji_code: String,
    pub reaction_type: String,
    pub user_id: i64,
}

/// Sub-messages of widgets such as polls and to-dos. The `content` of the first one is the widget definition, and each later one is one operation;
/// `content` is a JSON string.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Submessage {
    pub id: i64,
    pub message_id: i64,
    pub sender_id: i64,
    pub msg_type: String,
    pub content: String,
}

/// The part of the server flags used by this client. `starred` is the only source of starring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageFlags {
    pub read: bool,
    pub starred: bool,
    pub mentioned: bool,
    /// One of `stream_wildcard_mentioned`, `topic_wildcard_mentioned`, `wildcard_mentioned`.
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

    /// Set by the server's name. Returns whether anything changed; flags we do not care about return false.
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

/// Local echo: send information of a message not yet confirmed by the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalEcho {
    /// The `local_id` at send time, used by `retry_send` and `discard_failed`.
    pub local_id: String,
    /// Original Markdown: used when resending.
    pub raw_content: String,
}

/// Lifecycle state of a message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MessageState {
    /// A message on the server.
    #[default]
    Normal,
    /// Local echo, sending.
    Sending(LocalEcho),
    /// Local echo, sending failed.
    Failed(LocalEcho),
    /// Already deleted on the server and only a locally kept copy: shown in the chat as "deleted" and expanded on click.
    Deleted,
}

/// A message. The `id` of a server message is positive; the `id` of a local echo message is negative,
/// and it is replaced by the real message after the server's message event arrives.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    pub id: i64,
    pub key: ConversationKey,
    pub sender_id: i64,
    /// Snapshot of the name at send time. Display uses `Session::display_name` (including remarks and renames);
    /// this one is used for quote replies, and as a fallback when the user is no longer in the user list.
    pub sender_name: String,
    pub sender_email: String,
    /// Snapshot of the avatar at send time. Display prefers the current user profile; this is the fallback when the user is no longer in the user list.
    pub avatar_url: Option<String>,
    /// HTML rendered by the server.
    pub content_html: String,
    /// Unix seconds.
    pub timestamp: i64,
    pub flags: MessageFlags,
    pub reactions: Vec<Reaction>,
    pub last_edit_timestamp: Option<i64>,
    pub is_me_message: bool,
    /// Topic name (original case); empty string for DMs.
    pub topic: String,
    pub stream_id: Option<i64>,
    /// Ascending by id; appended by submessage events.
    pub submessages: Vec<Submessage>,
    pub state: MessageState,
    /// How a Notification Bot message is displayed.
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

    /// Already sorted into system notifications or message reminders, not shown in the chat.
    pub fn is_system_notice(&self) -> bool {
        matches!(
            self.notice,
            SystemNoticeKind::Notice | SystemNoticeKind::Reminder
        )
    }

    pub fn is_deleted(&self) -> bool {
        self.state == MessageState::Deleted
    }

    /// Send information of a local echo; `None` for server messages.
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
