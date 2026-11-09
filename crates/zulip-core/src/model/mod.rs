//! Domain model. Fields are public and constructed by upper layers from API data; on events they are updated in place with `apply_*`.

mod channel;
mod contact;
mod local;
mod message;
mod queued;
mod realm;
mod settings;
mod status;
mod user;

pub use channel::{Channel, Subscription, TopicInfo};
pub use contact::FriendGroup;
pub use local::{PinItem, PinScope, RemarkTarget};
pub use message::{ChatMessage, LocalEcho, MessageFlags, MessageState, Reaction, Submessage};
pub use queued::{
    Attachment, AttachmentList, MessageRevision, Reminder, SavedSnippet, ScheduledMessage,
    UploadedFile,
};
pub use realm::{CustomProfileField, RealmInfo};
pub use settings::UserSettings;
pub use status::{ConversationNotice, MentionKind, PresenceState};
pub use user::{ProfileValue, ZulipUser};

/// id fields such as the announcement channel: -1 (unset) becomes `None`.
fn positive_id(value: &serde_json::Value) -> Option<i64> {
    value.as_i64().filter(|&id| id > 0)
}
