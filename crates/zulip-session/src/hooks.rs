//! Hooks from the session layer to the upper layer (plugin host).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use zulip_core::ConversationKey;
use zulip_core::model::{ChatMessage, Reaction};

use crate::event::ConnectionState;
use crate::session::Session;

pub type HookFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Source of the content being sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutgoingKind {
    #[default]
    Text,
    File,
    Forward,
}

/// Notification-only event, cannot be intercepted. `key` of `None` means the message is not in the
/// cache and the event carried no conversation (DM).
#[derive(Clone, Debug, PartialEq)]
pub enum HookEvent {
    MessageEdited {
        key: Option<ConversationKey>,
        id: i64,
        edit_timestamp: Option<i64>,
        html: String,
    },
    MessagesDeleted {
        key: Option<ConversationKey>,
        ids: Vec<i64>,
    },
    ReactionChanged {
        key: Option<ConversationKey>,
        id: i64,
        reaction: Reaction,
        added: bool,
    },
    /// `all`: all messages (`ids` may be empty in this case).
    ReadChanged {
        ids: Vec<i64>,
        read: bool,
        all: bool,
    },
    /// A topic was renamed or moved. `whole`: the whole topic; otherwise only some of its messages.
    TopicMoved {
        old_stream: i64,
        old_topic: String,
        new_stream: i64,
        new_topic: String,
        ids: Vec<i64>,
        whole: bool,
    },
    /// `op`: the operation of the server event (create, update, add, remove, etc.).
    ChannelsChanged {
        stream_ids: Vec<i64>,
        op: String,
    },
    UserChanged(i64),
    PresenceChanged(Vec<i64>),
    /// The user switched to this conversation.
    ConversationOpened(ConversationKey),
    Connection(ConnectionState),
    /// The server's typing event. `typing` false means the other side stopped typing; this event is
    /// not always sent when the other side leaves abnormally.
    TypingChanged {
        key: ConversationKey,
        user_id: i64,
        typing: bool,
    },
    ScheduledChanged,
    RemindersChanged,
    /// Draft content changed: saved or deleted locally (when sync is off), or changes synced from the
    /// server.
    DraftChanged(ConversationKey),
    UserTopicChanged {
        stream_id: i64,
        topic: String,
    },
    MutedUsersChanged,
    /// `properties`: names of the organization settings that changed.
    RealmChanged {
        properties: Vec<String>,
    },
}

/// Installed through [`Hub::set_hooks`](crate::Hub::set_hooks); takes effect for accounts that log
/// in afterwards.
pub trait SessionHooks: Send + Sync {
    /// Account login finished.
    fn session_started(&self, session: &Arc<Session>);

    /// The account is about to end: the session is closed only after waiting for completion (at most
    /// 2 seconds).
    fn session_ending(&self, session: &Arc<Session>) -> HookFuture<()>;

    /// A new message from someone else. Returning true intercepts it: not shown, no notification,
    /// marked read, and also not shown when history is loaded during this run. The event loop waits
    /// for the result; implementations must be time-limited.
    fn filter_incoming(&self, session: &Arc<Session>, message: &ChatMessage) -> HookFuture<bool>;

    /// A new message entered the state (including messages you sent, excluding intercepted ones).
    fn on_message(&self, session: &Arc<Session>, message: &ChatMessage);

    /// Called after the state has been updated; must not block.
    fn on_event(&self, _session: &Arc<Session>, _event: HookEvent) {}

    /// Pre-send filter ([`Session::send_as`] waits for the result; implementations must be
    /// time-limited): `Ok` is the content to send, `Err` is the reason for interception (shown to
    /// the user).
    fn filter_outgoing(
        &self,
        _session: &Arc<Session>,
        _key: &ConversationKey,
        content: &str,
        _kind: OutgoingKind,
    ) -> HookFuture<Result<String, String>> {
        let content = content.to_owned();
        Box::pin(async move { Ok(content) })
    }
}
