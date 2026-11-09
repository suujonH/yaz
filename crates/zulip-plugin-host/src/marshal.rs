//! Converts models into the C structs passed to plugins. Pointers are valid only while the
//! [`Scope`] is alive.

use std::ffi::{CStr, CString, c_char};
use std::mem::offset_of;
use std::ptr::null;

use serde_json::{Value, json};
use zulip_core::ConversationKey;
use zulip_core::html::{extract_uploads, to_plain_text};
use zulip_core::model::{ChatMessage, RemarkTarget, ZulipUser};
use zulip_plugin_sdk::abi::{
    CONV_CHANNEL, CONV_DIRECT, MSG_HISTORICAL, MSG_MENTIONED, MSG_OUTGOING, MSG_READ, MSG_STARRED,
    MSG_WILDCARD_MENTION, RawAccount, RawConversation, RawMessage, RawUser,
};
use zulip_session::Session;

#[derive(Default)]
pub(crate) struct Scope {
    strings: Vec<CString>,
    // Pointers are handed to the plugin, so the structs' addresses must not change when the Vec
    // grows.
    #[allow(clippy::vec_box)]
    conversations: Vec<Box<RawConversation>>,
}

impl Scope {
    pub fn text(&mut self, text: &str) -> *const c_char {
        let owned = CString::new(text.replace('\0', "")).unwrap_or_default();
        let pointer = owned.as_ptr();
        self.strings.push(owned);
        pointer
    }

    pub fn conversation(&mut self, raw: RawConversation) -> *const RawConversation {
        let boxed = Box::new(raw);
        let pointer: *const RawConversation = &raw const *boxed;
        self.conversations.push(boxed);
        pointer
    }
}

/// # Safety
/// `p` is null or a NUL-terminated string.
pub(crate) unsafe fn read_text(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: see the function docs.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

// The conversation passed by the plugin must cover at least up to here (all fields of v1).
const CONVERSATION_V1_SIZE: usize = offset_of!(RawConversation, dm_user_count) + size_of::<i32>();

/// Conversation snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Conversation {
    Channel {
        stream_id: i64,
        stream_name: String,
        topic: String,
    },
    Direct {
        user_ids: Vec<i64>,
    },
}

impl Conversation {
    /// `None`: not a channel topic or a direct message.
    pub fn from_key(key: &ConversationKey, session: &Session) -> Option<Self> {
        match key {
            ConversationKey::Topic(topic) => Some(Self::Channel {
                stream_id: topic.stream_id,
                stream_name: session
                    .channel(topic.stream_id)
                    .map(|c| c.name)
                    .unwrap_or_default(),
                topic: session.topic_name(key),
            }),
            ConversationKey::Dm(dm) => Some(Self::Direct {
                user_ids: dm.other_user_ids().to_vec(),
            }),
        }
    }

    /// # Safety
    /// The pointers in `raw` are valid.
    pub unsafe fn from_raw(raw: &RawConversation) -> Option<Self> {
        // Fields appended later: before reading, check offset_of!(RawConversation, field) <
        // struct_size.
        if (raw.struct_size as usize) < CONVERSATION_V1_SIZE {
            return None;
        }
        match raw.kind {
            CONV_CHANNEL if raw.stream_id > 0 => Some(Self::Channel {
                stream_id: raw.stream_id,
                stream_name: String::new(),
                // SAFETY: see the function docs.
                topic: unsafe { read_text(raw.topic) }?,
            }),
            CONV_DIRECT if raw.dm_user_count >= 0 => {
                let count = raw.dm_user_count as usize;
                if raw.dm_user_ids.is_null() && count > 0 {
                    return None;
                }
                let user_ids = if count == 0 {
                    Vec::new()
                } else {
                    // SAFETY: see the function docs.
                    unsafe { std::slice::from_raw_parts(raw.dm_user_ids, count) }.to_vec()
                };
                Some(Self::Direct { user_ids })
            }
            _ => None,
        }
    }

    pub fn to_raw(&self, scope: &mut Scope) -> RawConversation {
        match self {
            Self::Channel {
                stream_id,
                stream_name,
                topic,
            } => RawConversation {
                struct_size: size_of::<RawConversation>() as u32,
                kind: CONV_CHANNEL,
                dm_user_count: 0,
                stream_id: *stream_id,
                stream_name: scope.text(stream_name),
                topic: scope.text(topic),
                dm_user_ids: null(),
            },
            Self::Direct { user_ids } => RawConversation {
                struct_size: size_of::<RawConversation>() as u32,
                kind: CONV_DIRECT,
                dm_user_count: user_ids.len() as i32,
                stream_id: 0,
                stream_name: null(),
                topic: null(),
                dm_user_ids: if user_ids.is_empty() {
                    null()
                } else {
                    user_ids.as_ptr()
                },
            },
        }
    }
}

impl Conversation {
    /// A direct message with empty `user_ids` means a direct message to yourself.
    pub fn to_key(&self, self_id: i64) -> ConversationKey {
        match self {
            Self::Channel {
                stream_id, topic, ..
            } => ConversationKey::topic(*stream_id, topic.as_str()),
            Self::Direct { user_ids } if user_ids.is_empty() => {
                ConversationKey::dm([self_id], self_id)
            }
            Self::Direct { user_ids } => ConversationKey::dm(user_ids.iter().copied(), self_id),
        }
    }

    /// The conversation in callback JSON.
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
}

/// The conversation in callback JSON; null when unknown.
pub(crate) fn conversation_json(key: Option<&ConversationKey>, session: &Session) -> Value {
    key.and_then(|key| Conversation::from_key(key, session))
        .map_or(Value::Null, |conv| conv.to_json())
}

/// Message snapshot: taken from the state at that time, can be handed to the plugin thread.
#[derive(Clone, Debug)]
pub(crate) struct MessageSnapshot {
    id: i64,
    account_id: i64,
    timestamp: i64,
    sender_id: i64,
    sender_name: String,
    sender_email: String,
    conv: Conversation,
    html: String,
    text: String,
    flags: u32,
    reactions: String,
    uploads: String,
    last_edit_timestamp: i64,
}

impl MessageSnapshot {
    /// `None`: the message does not belong to a channel topic or a direct message.
    pub fn new(message: &ChatMessage, session: &Session, historical: bool) -> Option<Self> {
        let mut flags = 0;
        for (on, bit) in [
            (message.sender_id == session.self_id(), MSG_OUTGOING),
            (message.flags.mentioned, MSG_MENTIONED),
            (message.flags.wildcard_mentioned, MSG_WILDCARD_MENTION),
            (message.flags.starred, MSG_STARRED),
            (message.flags.read, MSG_READ),
            (historical, MSG_HISTORICAL),
        ] {
            if on {
                flags |= bit;
            }
        }
        Some(Self {
            id: message.id,
            account_id: session.account_id(),
            timestamp: message.timestamp,
            sender_id: message.sender_id,
            sender_name: message.sender_name.clone(),
            sender_email: message.sender_email.clone(),
            conv: Conversation::from_key(&message.key, session)?,
            html: message.content_html.clone(),
            text: to_plain_text(&message.content_html),
            flags,
            reactions: serde_json::to_string(&message.reactions)
                .unwrap_or_else(|_| "[]".to_owned()),
            uploads: Value::from_iter(extract_uploads(&message.content_html).iter().map(
                |u| json!({"path": u.path, "file_name": u.file_name, "is_image": u.is_image}),
            ))
            .to_string(),
            last_edit_timestamp: message.last_edit_timestamp.unwrap_or_default(),
        })
    }

    pub fn to_raw(&self, scope: &mut Scope) -> RawMessage {
        let conv = self.conv.to_raw(scope);
        RawMessage {
            struct_size: size_of::<RawMessage>() as u32,
            message_id: self.id,
            account_id: self.account_id,
            timestamp: self.timestamp,
            sender_id: self.sender_id,
            sender_name: scope.text(&self.sender_name),
            sender_email: scope.text(&self.sender_email),
            conv: scope.conversation(conv),
            content_html: scope.text(&self.html),
            content_text: scope.text(&self.text),
            flags: self.flags,
            reactions_json: scope.text(&self.reactions),
            uploads_json: scope.text(&self.uploads),
            last_edit_timestamp: self.last_edit_timestamp,
        }
    }
}

pub(crate) fn user_to_raw(user: &ZulipUser, session: &Session, scope: &mut Scope) -> RawUser {
    RawUser {
        struct_size: size_of::<RawUser>() as u32,
        user_id: user.id,
        full_name: scope.text(&user.full_name),
        email: scope.text(&user.email),
        remark: session
            .remark(&RemarkTarget::User(user.id))
            .map_or(null(), |remark| scope.text(&remark)),
        role: user.role,
        is_bot: i32::from(user.is_bot),
        is_active: i32::from(user.is_active),
    }
}

pub(crate) fn account_to_raw(session: &Session, scope: &mut Scope) -> RawAccount {
    let me = session.user(session.self_id());
    RawAccount {
        struct_size: size_of::<RawAccount>() as u32,
        account_id: session.account_id(),
        user_id: session.self_id(),
        full_name: scope.text(me.as_ref().map_or("", |u| u.full_name.as_str())),
        email: scope.text(session.email()),
        realm_url: scope.text(session.realm_url()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(struct_size: u32) -> RawConversation {
        RawConversation {
            struct_size,
            kind: CONV_DIRECT,
            stream_id: 0,
            stream_name: null(),
            topic: null(),
            dm_user_ids: null(),
            dm_user_count: 0,
        }
    }

    #[test]
    fn conversation_only_needs_the_v1_fields() {
        let direct = Some(Conversation::Direct {
            user_ids: Vec::new(),
        });
        // SAFETY: no pointer fields are read.
        unsafe {
            assert_eq!(Conversation::from_raw(&raw(44)), direct);
            assert_eq!(Conversation::from_raw(&raw(48)), direct);
            assert_eq!(Conversation::from_raw(&raw(64)), direct);
            assert_eq!(Conversation::from_raw(&raw(43)), None);
        }
    }
}
