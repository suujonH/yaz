use std::ffi::{CStr, c_char};

use chrono::{Local, TimeZone};
use serde_json::{Value, json};

use crate::abi::{
    CONV_CHANNEL, MSG_MENTIONED, MSG_OUTGOING, OUTGOING_FILE, OUTGOING_FORWARD, RawAccount,
    RawChannel, RawConversation, RawMessage, RawOutgoing, RawTopic, RawUser,
};

/// Result of the event chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Ignore,
    Block,
}

/// # Safety
/// `p` is null or a 0-terminated string.
pub(crate) unsafe fn read_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: see the function docs.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// # Safety
/// `p` is null or points to `count` numbers.
unsafe fn read_ids(p: *const i64, count: i32) -> Vec<i64> {
    if p.is_null() || count <= 0 {
        return Vec::new();
    }
    // SAFETY: see the function docs.
    unsafe { std::slice::from_raw_parts(p, count as usize) }.to_vec()
}

/// A string field of a JSON object; an empty string if missing or of the wrong type.
pub fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Extracts a JSON object from a model reply (it may be wrapped in a code block or have text before
/// or after it).
pub fn extract_json(reply: &str) -> Option<Value> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&reply[start..=end])
        .ok()
        .filter(Value::is_object)
}

/// `MM-dd HH:mm` (local time).
pub fn format_time(timestamp: i64) -> String {
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map_or_else(String::new, |t| t.format("%m-%d %H:%M").to_string())
}

/// A conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conversation {
    Channel {
        stream_id: i64,
        stream_name: String,
        topic: String,
    },
    /// `user_ids` are the other party (excluding yourself); empty for a DM with yourself.
    Direct { user_ids: Vec<i64> },
}

impl Conversation {
    /// # Safety
    /// The pointers in `raw` must be valid.
    unsafe fn from_raw(raw: &RawConversation) -> Self {
        // SAFETY: see the function docs.
        unsafe {
            if raw.kind == CONV_CHANNEL {
                Self::Channel {
                    stream_id: raw.stream_id,
                    stream_name: read_string(raw.stream_name),
                    topic: read_string(raw.topic),
                }
            } else {
                Self::Direct {
                    user_ids: read_ids(raw.dm_user_ids, raw.dm_user_count),
                }
            }
        }
    }

    /// The conversation in callbacks and query JSON: `{"type": "channel", "stream", "stream_name",
    /// "topic"}` or `{"type": "direct", "users": [...]}`.
    pub fn from_json(value: &Value) -> Option<Self> {
        match str_of(value, "type") {
            "channel" => Some(Self::Channel {
                stream_id: value["stream"].as_i64()?,
                stream_name: str_of(value, "stream_name").to_owned(),
                topic: str_of(value, "topic").to_owned(),
            }),
            "direct" => Some(Self::Direct {
                user_ids: ids_of(&value["users"]),
            }),
            _ => None,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Channel {
                stream_id,
                stream_name,
                topic,
            } => {
                json!({"type": "channel", "stream": stream_id, "stream_name": stream_name, "topic": topic})
            }
            Self::Direct { user_ids } => json!({"type": "direct", "users": user_ids}),
        }
    }

    /// Description of a conversation, e.g. 'topic "release" of channel "dev"' or 'direct message'.
    pub fn describe(&self) -> String {
        match self {
            Self::Channel {
                stream_name, topic, ..
            } => format!("频道“{stream_name}”的话题“{topic}”"),
            Self::Direct { .. } => "私聊".to_owned(),
        }
    }
}

/// A copy of the callback arguments, still usable after the callback returns.
#[derive(Clone, Debug)]
pub struct Message {
    pub id: i64,
    pub account_id: i64,
    pub timestamp: i64,
    pub flags: u32,
    pub sender_id: i64,
    pub sender_name: String,
    pub sender_email: String,
    pub conv: Conversation,
    pub html: String,
    pub text: String,
    pub reactions: Vec<Reaction>,
    pub uploads: Vec<Upload>,
    pub last_edit_timestamp: Option<i64>,
}

/// A reaction on a message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reaction {
    pub emoji_name: String,
    pub emoji_code: String,
    pub reaction_type: String,
    pub user_id: i64,
}

impl Reaction {
    fn from_json(value: &Value) -> Self {
        Self {
            emoji_name: str_of(value, "emoji_name").to_owned(),
            emoji_code: str_of(value, "emoji_code").to_owned(),
            reaction_type: str_of(value, "reaction_type").to_owned(),
            user_id: value["user_id"].as_i64().unwrap_or_default(),
        }
    }
}

/// An uploaded file in a message; `path` is passed to
/// [`download_attachment`](crate::download_attachment).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Upload {
    pub path: String,
    pub file_name: String,
    pub is_image: bool,
}

// JSON array string; empty if the string is empty or malformed.
fn parse_list<T>(text: &str, item: impl Fn(&Value) -> T) -> Vec<T> {
    serde_json::from_str::<Value>(text)
        .ok()
        .as_ref()
        .and_then(Value::as_array)
        .map(|list| list.iter().map(item).collect())
        .unwrap_or_default()
}

impl Message {
    /// # Safety
    /// The pointers in `raw` must be valid.
    pub(crate) unsafe fn from_raw(raw: &RawMessage) -> Self {
        // SAFETY: see the function docs.
        unsafe {
            Self {
                id: raw.message_id,
                account_id: raw.account_id,
                timestamp: raw.timestamp,
                flags: raw.flags,
                sender_id: raw.sender_id,
                sender_name: read_string(raw.sender_name),
                sender_email: read_string(raw.sender_email),
                conv: Conversation::from_raw(&*raw.conv),
                html: read_string(raw.content_html),
                text: read_string(raw.content_text),
                reactions: parse_list(&read_string(raw.reactions_json), Reaction::from_json),
                uploads: parse_list(&read_string(raw.uploads_json), |u| Upload {
                    path: str_of(u, "path").to_owned(),
                    file_name: str_of(u, "file_name").to_owned(),
                    is_image: u["is_image"].as_bool().unwrap_or(false),
                }),
                last_edit_timestamp: (raw.last_edit_timestamp > 0)
                    .then_some(raw.last_edit_timestamp),
            }
        }
    }

    pub fn is_outgoing(&self) -> bool {
        self.flags & MSG_OUTGOING != 0
    }

    pub fn is_mentioned(&self) -> bool {
        self.flags & MSG_MENTIONED != 0
    }
}

/// Source of the content being sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutgoingKind {
    #[default]
    Text,
    File,
    Forward,
}

/// Content being filtered before sending.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub account_id: i64,
    pub kind: OutgoingKind,
    pub conv: Conversation,
    pub content: String,
}

impl Outgoing {
    /// # Safety
    /// The pointers in `raw` must be valid.
    pub(crate) unsafe fn from_raw(raw: &RawOutgoing) -> Self {
        // SAFETY: see the function docs.
        unsafe {
            Self {
                account_id: raw.account_id,
                kind: match raw.kind {
                    OUTGOING_FILE => OutgoingKind::File,
                    OUTGOING_FORWARD => OutgoingKind::Forward,
                    _ => OutgoingKind::Text,
                },
                conv: Conversation::from_raw(&*raw.conv),
                content: read_string(raw.content),
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct User {
    pub id: i64,
    pub full_name: String,
    pub email: String,
    pub remark: Option<String>,
    pub role: i32,
    pub is_bot: bool,
    pub is_active: bool,
}

impl User {
    /// # Safety
    /// The pointers in `raw` must be valid.
    pub(crate) unsafe fn from_raw(raw: &RawUser) -> Self {
        // SAFETY: see the function docs.
        unsafe {
            Self {
                id: raw.user_id,
                full_name: read_string(raw.full_name),
                email: read_string(raw.email),
                remark: (!raw.remark.is_null()).then(|| read_string(raw.remark)),
                role: raw.role,
                is_bot: raw.is_bot != 0,
                is_active: raw.is_active != 0,
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub is_private: bool,
    pub is_subscribed: bool,
}

impl Channel {
    /// # Safety
    /// The pointers in `raw` must be valid.
    pub(crate) unsafe fn from_raw(raw: &RawChannel) -> Self {
        // SAFETY: see the function docs.
        unsafe {
            Self {
                id: raw.stream_id,
                name: read_string(raw.name),
                description: read_string(raw.description),
                is_private: raw.is_private != 0,
                is_subscribed: raw.is_subscribed != 0,
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Topic {
    pub stream_id: i64,
    pub max_message_id: i64,
    pub name: String,
}

impl Topic {
    /// # Safety
    /// The pointers in `raw` must be valid.
    pub(crate) unsafe fn from_raw(raw: &RawTopic) -> Self {
        Self {
            stream_id: raw.stream_id,
            max_message_id: raw.max_message_id,
            // SAFETY: see the function docs.
            name: unsafe { read_string(raw.name) },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Account {
    pub id: i64,
    pub user_id: i64,
    pub full_name: String,
    pub email: String,
    pub realm_url: String,
}

impl Account {
    /// # Safety
    /// The pointers in `raw` must be valid.
    pub(crate) unsafe fn from_raw(raw: &RawAccount) -> Self {
        // SAFETY: see the function docs.
        unsafe {
            Self {
                id: raw.account_id,
                user_id: raw.user_id,
                full_name: read_string(raw.full_name),
                email: read_string(raw.email),
                realm_url: read_string(raw.realm_url),
            }
        }
    }
}

fn ids_of(value: &Value) -> Vec<i64> {
    value
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

fn account_of(value: &Value) -> i64 {
    value["account"].as_i64().unwrap_or_default()
}

/// A message was edited.
#[derive(Clone, Debug, Default)]
pub struct MessageEdited {
    pub account_id: i64,
    pub message_id: i64,
    /// `None` for a DM whose message is not in the cache.
    pub conv: Option<Conversation>,
    pub edit_timestamp: Option<i64>,
    pub html: String,
    pub text: String,
}

impl MessageEdited {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            message_id: value["message_id"].as_i64().unwrap_or_default(),
            conv: Conversation::from_json(&value["conv"]),
            edit_timestamp: value["edit_timestamp"].as_i64(),
            html: str_of(value, "content_html").to_owned(),
            text: str_of(value, "content_text").to_owned(),
        }
    }
}

/// A message was deleted.
#[derive(Clone, Debug, Default)]
pub struct MessagesDeleted {
    pub account_id: i64,
    pub message_ids: Vec<i64>,
    pub conv: Option<Conversation>,
}

impl MessagesDeleted {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            message_ids: ids_of(&value["message_ids"]),
            conv: Conversation::from_json(&value["conv"]),
        }
    }
}

/// A reaction was added or removed.
#[derive(Clone, Debug, Default)]
pub struct ReactionChanged {
    pub account_id: i64,
    pub message_id: i64,
    pub conv: Option<Conversation>,
    pub added: bool,
    pub reaction: Reaction,
}

impl ReactionChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            message_id: value["message_id"].as_i64().unwrap_or_default(),
            conv: Conversation::from_json(&value["conv"]),
            added: value["added"].as_bool().unwrap_or(false),
            reaction: Reaction::from_json(&value["reaction"]),
        }
    }
}

/// Read state changed.
#[derive(Clone, Debug, Default)]
pub struct ReadStateChanged {
    pub account_id: i64,
    pub message_ids: Vec<i64>,
    pub read: bool,
    /// All messages (`message_ids` may be empty in this case).
    pub all: bool,
}

impl ReadStateChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            message_ids: ids_of(&value["message_ids"]),
            read: value["read"].as_bool().unwrap_or(false),
            all: value["all"].as_bool().unwrap_or(false),
        }
    }
}

/// A topic was renamed or moved.
#[derive(Clone, Debug)]
pub struct TopicMoved {
    pub account_id: i64,
    pub old: Option<Conversation>,
    pub new: Option<Conversation>,
    pub message_ids: Vec<i64>,
    /// The whole topic; otherwise only some of its messages.
    pub whole: bool,
}

impl TopicMoved {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            old: Conversation::from_json(&value["old"]),
            new: Conversation::from_json(&value["new"]),
            message_ids: ids_of(&value["message_ids"]),
            whole: value["whole"].as_bool().unwrap_or(false),
        }
    }
}

/// A channel changed. `op`: the operation of the server event (create, update, add, remove, etc.).
#[derive(Clone, Debug, Default)]
pub struct ChannelsChanged {
    pub account_id: i64,
    pub stream_ids: Vec<i64>,
    pub op: String,
}

impl ChannelsChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            stream_ids: ids_of(&value["streams"]),
            op: str_of(value, "op").to_owned(),
        }
    }
}

/// A user's profile or status text changed.
#[derive(Clone, Debug, Default)]
pub struct UserChanged {
    pub account_id: i64,
    pub user_id: i64,
}

impl UserChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            user_id: value["user"].as_i64().unwrap_or_default(),
        }
    }
}

/// These users' presence changed; read it with [`presence`](crate::presence).
#[derive(Clone, Debug, Default)]
pub struct PresenceChanged {
    pub account_id: i64,
    pub user_ids: Vec<i64>,
}

impl PresenceChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            user_ids: ids_of(&value["users"]),
        }
    }
}

/// The user switched to this conversation.
#[derive(Clone, Debug)]
pub struct ConversationOpened {
    pub account_id: i64,
    pub conv: Option<Conversation>,
}

impl ConversationOpened {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            conv: Conversation::from_json(&value["conv"]),
        }
    }
}

/// Connection state. `state`: connected, reconnecting, auth_failed. Events may have been missed
/// after recovery; re-read when needed.
#[derive(Clone, Debug, Default)]
pub struct ConnectionChanged {
    pub account_id: i64,
    pub state: String,
}

impl ConnectionChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            state: str_of(value, "state").to_owned(),
        }
    }
}

/// Someone started or stopped typing. `typing` false means stopped; it may not be received when the
/// other side leaves abnormally, so apply your own timeout.
#[derive(Clone, Debug)]
pub struct TypingChanged {
    pub account_id: i64,
    pub conv: Option<Conversation>,
    pub user_id: i64,
    pub typing: bool,
}

impl TypingChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            conv: Conversation::from_json(&value["conv"]),
            user_id: value["user"].as_i64().unwrap_or_default(),
            typing: value["typing"].as_bool().unwrap_or_default(),
        }
    }
}

/// Scheduled messages changed; read with [`scheduled_messages`](crate::scheduled_messages).
#[derive(Clone, Debug, Default)]
pub struct ScheduledChanged {
    pub account_id: i64,
}

impl ScheduledChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
        }
    }
}

/// Message reminders changed; read with [`reminders`](crate::reminders).
#[derive(Clone, Debug, Default)]
pub struct RemindersChanged {
    pub account_id: i64,
}

impl RemindersChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
        }
    }
}

/// Draft content changed (local save/delete when sync is off, changes synced from the server when
/// sync is on); read with [`drafts`](crate::drafts).
#[derive(Clone, Debug)]
pub struct DraftChanged {
    pub account_id: i64,
    pub conv: Option<Conversation>,
}

impl DraftChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            conv: Conversation::from_json(&value["conv"]),
        }
    }
}

/// A topic's follow state changed; `visibility` is `abi::TOPIC_*`.
#[derive(Clone, Debug)]
pub struct UserTopicChanged {
    pub account_id: i64,
    pub conv: Option<Conversation>,
    pub visibility: i32,
}

impl UserTopicChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            conv: Conversation::from_json(&value["conv"]),
            visibility: value["visibility"].as_i64().unwrap_or_default() as i32,
        }
    }
}

/// Muted users changed; `user_ids` are all currently muted users.
#[derive(Clone, Debug, Default)]
pub struct MutedUsersChanged {
    pub account_id: i64,
    pub user_ids: Vec<i64>,
}

impl MutedUsersChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            user_ids: ids_of(&value["users"]),
        }
    }
}

/// Organization settings changed; `properties` are the names of the changed settings; read with
/// [`realm_info`](crate::realm_info).
#[derive(Clone, Debug, Default)]
pub struct RealmChanged {
    pub account_id: i64,
    pub properties: Vec<String>,
}

impl RealmChanged {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            properties: value["properties"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        }
    }
}

/// Target of a context menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextTarget {
    Conversation(Conversation),
    User { user_id: i64, name: String },
    Channel { stream_id: i64, name: String },
}

/// A context menu item was clicked.
#[derive(Clone, Debug)]
pub struct ContextMenuClick {
    pub account_id: i64,
    /// `None` when the format sent by the host is not recognized.
    pub target: Option<ContextTarget>,
}

impl ContextMenuClick {
    pub(crate) fn from_json(value: &Value) -> Self {
        let target = &value["target"];
        let name = || str_of(target, "name").to_owned();
        Self {
            account_id: account_of(value),
            target: match str_of(value, "kind") {
                "conversation" => Conversation::from_json(target).map(ContextTarget::Conversation),
                "user" => target["user"].as_i64().map(|user_id| ContextTarget::User {
                    user_id,
                    name: name(),
                }),
                "channel" => target["stream"]
                    .as_i64()
                    .map(|stream_id| ContextTarget::Channel {
                        stream_id,
                        name: name(),
                    }),
                _ => None,
            },
        }
    }
}

/// A composer toolbar button was clicked.
#[derive(Clone, Debug)]
pub struct ComposerClick {
    pub account_id: i64,
    pub conv: Option<Conversation>,
    pub text: String,
}

impl ComposerClick {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: account_of(value),
            conv: Conversation::from_json(&value["conv"]),
            text: str_of(value, "text").to_owned(),
        }
    }
}

/// These messages were displayed in the chat window.
#[derive(Clone, Debug, Default)]
pub struct MessagesShown {
    pub account_id: i64,
    pub message_ids: Vec<i64>,
}

impl MessagesShown {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            account_id: value["account"].as_i64().unwrap_or_default(),
            message_ids: value["messages"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_i64)
                        .filter(|id| *id > 0)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// A button in the settings was clicked.
#[derive(Clone, Debug, Default)]
pub struct SettingsButton {
    pub key: String,
    pub row: Option<i64>,
    pub item: Option<String>,
    /// Current value (not yet saved) of the level it is in (top level or this row).
    pub values: Value,
    /// All current values (not yet saved).
    pub settings: Value,
}

impl SettingsButton {
    pub(crate) fn from_json(value: &Value) -> Self {
        Self {
            key: str_of(value, "key").to_owned(),
            row: value["row"].as_i64(),
            item: value["item"].as_str().map(str::to_owned),
            values: value["values"].clone(),
            settings: value["settings"].clone(),
        }
    }
}

/// A panel request. `action`: get, set, click, list, remove.
#[derive(Clone, Debug, Default)]
pub struct PanelRequest {
    pub action: String,
    pub is_user: bool,
    pub account_id: i64,
    pub user_id: i64,
    pub stream_id: i64,
    pub topic: String,
    pub name: String,
    pub key: String,
    pub value: Value,
}

impl PanelRequest {
    pub(crate) fn from_json(value: &Value) -> Self {
        let target = &value["target"];
        Self {
            action: str_of(value, "action").to_owned(),
            is_user: str_of(value, "kind") == "user",
            account_id: target["account"].as_i64().unwrap_or_default(),
            user_id: target["user"].as_i64().unwrap_or_default(),
            stream_id: target["stream"].as_i64().unwrap_or_default(),
            topic: str_of(target, "topic").to_owned(),
            name: str_of(target, "name").to_owned(),
            key: str_of(value, "key").to_owned(),
            value: value["value"].clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_json_from_reply() {
        let reply = "好的：\n```json\n{\"a\": 1}\n```";
        assert_eq!(extract_json(reply), Some(json!({"a": 1})));
        assert_eq!(extract_json("没有"), None);
        assert_eq!(extract_json("} {"), None);
        assert_eq!(extract_json("{坏}"), None);
    }

    #[test]
    fn parses_callback_arguments() {
        let shown = MessagesShown::from_json(&json!({"account": 2, "messages": [1, 0, "x", 3]}));
        assert_eq!((shown.account_id, shown.message_ids), (2, vec![1, 3]));

        let panel = PanelRequest::from_json(&json!({
            "action": "set", "kind": "topic", "key": "scene", "value": "work",
            "target": {"account": 1, "stream": 2, "topic": "发布", "name": "开发 / 发布"},
        }));
        assert!(!panel.is_user);
        assert_eq!((panel.stream_id, panel.topic.as_str()), (2, "发布"));

        let button =
            SettingsButton::from_json(&json!({"key": "fetch", "row": 3, "values": {"a": 1}}));
        assert_eq!((button.row, button.item), (Some(3), None));
    }

    #[test]
    fn describes_conversation() {
        let channel = Conversation::Channel {
            stream_id: 1,
            stream_name: "开发".into(),
            topic: "发布".into(),
        };
        assert_eq!(channel.describe(), "频道“开发”的话题“发布”");
        assert_eq!(Conversation::Direct { user_ids: vec![] }.describe(), "私聊");
    }

    #[test]
    fn conversation_json_round_trip() {
        let channel = Conversation::Channel {
            stream_id: 1,
            stream_name: "开发".into(),
            topic: "发布".into(),
        };
        assert_eq!(Conversation::from_json(&channel.to_json()), Some(channel));
        let direct = Conversation::Direct {
            user_ids: vec![2, 3],
        };
        assert_eq!(Conversation::from_json(&direct.to_json()), Some(direct));
        assert_eq!(Conversation::from_json(&json!({})), None);
    }

    #[test]
    fn parses_notifications() {
        let moved = TopicMoved::from_json(&json!({
            "account": 1, "whole": true, "message_ids": [5],
            "old": {"type": "channel", "stream": 2, "topic": "旧"},
            "new": {"type": "channel", "stream": 3, "topic": "新"},
        }));
        assert!(moved.whole);
        assert!(matches!(
            moved.new,
            Some(Conversation::Channel { stream_id: 3, .. })
        ));

        let click = ContextMenuClick::from_json(&json!({
            "account": 1, "kind": "user", "target": {"user": 9, "name": "小雨"},
        }));
        assert_eq!(
            click.target,
            Some(ContextTarget::User {
                user_id: 9,
                name: "小雨".into()
            })
        );

        let reaction = ReactionChanged::from_json(&json!({
            "account": 1, "message_id": 7, "added": true, "reaction": {"emoji_name": "smile", "user_id": 2},
        }));
        assert_eq!(reaction.reaction.emoji_name, "smile");
        assert!(parse_list("坏", Reaction::from_json).is_empty());
    }
}
