//! 领域模型。字段公开，由上层从接口数据构造；收到事件时用 `apply_*` 就地更新。

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

/// 公告频道等 id 字段：-1（未设置）转为 `None`。
fn positive_id(value: &serde_json::Value) -> Option<i64> {
    value.as_i64().filter(|&id| id > 0)
}
