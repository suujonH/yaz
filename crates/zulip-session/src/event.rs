use zulip_core::{ConversationKey, ListEntry};

/// Connection state to the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Reconnecting,
    /// Login is no longer valid and the event loop has exited.
    AuthFailed,
}

/// Avatar on a notification popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationAvatar {
    /// User avatar URL (relative or absolute).
    Image(String),
    /// Text avatar: the text and the background color.
    Text { text: String, color: String },
}

/// A new message judged to need a notification. The popup and sound are handled by the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRequest {
    pub key: ConversationKey,
    /// Display slot of a notification: a new notification in the same slot replaces the old one.
    /// Do-not-disturb conversations share the Topic Assistant slot.
    pub slot: ListEntry,
    pub message_id: Option<i64>,
    pub title: String,
    pub body: String,
    pub avatar: NotificationAvatar,
    pub show_popup: bool,
    pub play_sound: bool,
    pub is_reminder: bool,
}

/// Session event. After receiving `broadcast::error::RecvError::Lagged` the UI should re-read
/// everything.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    Connection(ConnectionState),
    /// All state was reloaded after the queue became invalid; equivalent to Lagged.
    Resynced,

    UserChanged(i64),
    UsersChanged,
    PresenceChanged(Vec<i64>),
    MutedUsersChanged,

    ChannelChanged(i64),
    ChannelsChanged,
    TopicsChanged(i64),
    UserTopicChanged {
        stream_id: i64,
        topic: String,
    },

    RealmChanged,
    PermissionsChanged,
    SettingsChanged(String),
    AlertWordsChanged,
    EmojiChanged,

    /// A new message entered the timeline.
    MessageAdded {
        key: ConversationKey,
        id: i64,
    },
    /// A local echo was replaced by the server's message.
    MessageConfirmed {
        key: ConversationKey,
        local_id: String,
        id: i64,
    },
    /// Content, reactions, flags, widgets, send state, etc. changed.
    MessageUpdated {
        key: ConversationKey,
        id: i64,
    },
    /// Removed from the timeline (moved, local echo canceled, system-notice message deleted).
    MessageRemoved {
        key: ConversationKey,
        id: i64,
    },
    /// Paged loading or wholesale invalidation; this timeline needs to be re-read.
    TimelineChanged(ConversationKey),
    StarredChanged {
        id: i64,
        starred: bool,
    },

    UnreadChanged(Vec<ConversationKey>),
    /// The members or order of the conversation list changed.
    ConversationsChanged,
    /// A single conversation's preview, title, pin, do-not-disturb, etc. changed.
    ConversationChanged(ConversationKey),
    ConversationRekeyed {
        old: ConversationKey,
        new: ConversationKey,
    },

    DraftChanged(ConversationKey),
    ContactsChanged,
    TypingChanged(ConversationKey),
    SystemNoticesChanged,
    RemindersChanged,
    ScheduledMessagesChanged,
    SnippetsChanged,
    AttachmentsChanged,

    /// A user-triggered operation succeeded but the user needs to be told (e.g. a message sent after
    /// @all was stripped).
    Toast(String),
    Notification(Box<NotificationRequest>),
}
