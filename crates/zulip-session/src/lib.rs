//! 账号会话层：多账号管理、单账号的状态和业务操作，没有 UI 依赖。
//!
//! [`Hub`] 管理账号；[`Session`] 是一个登录的账号。读接口同步且便宜（快照或克隆），
//! 操作是 async 方法。状态变化通过 [`Session::subscribe`] 的 [`SessionEvent`] 通知，
//! 收到 `Lagged` 或 [`SessionEvent::Resynced`] 时界面全量重读。

pub mod avatar_image;
pub mod compose;
pub mod convert;
pub mod error;
pub mod event;
pub mod hooks;
pub mod hub;
pub(crate) mod image_cache;
pub mod login;
pub(crate) mod notify;
pub mod ops;
pub mod pages;
pub mod queries;
pub(crate) mod router;
pub mod session;
pub(crate) mod state;
pub mod topic;

pub use avatar_image::{AvatarImage, AvatarImageError};
pub use error::{Result, SessionError, describe_login_error};
pub use event::{ConnectionState, NotificationAvatar, NotificationRequest, SessionEvent};
pub use hooks::{HookEvent, HookFuture, OutgoingKind, SessionHooks};
pub use hub::{AccountEntry, AccountStatus, Hub, HubEvent, PasswordLogin};
pub use ops::files::{OpenAction, open_action_for};
pub use ops::messages::{MessageActions, WidgetAction};
pub use ops::timeline::{LocatedMessage, OpenedConversation, TimelineSnapshot};
pub use ops::topics::{DeleteTarget, RenameTarget, TopicDestination};
pub use pages::{
    ChannelSection, ConversationAvatar, ConversationRow, FriendItem, FriendSection,
    FriendSectionKind, PinnedTopicRow, badge_text,
};
pub use queries::{
    ChannelHit, ContactHit, ContentFilter, DateGroup, FileEntry, FileSort, ItemPreview,
    MessageGroup, MessageItem, MessagePager, MessageReference, MessageReferences, MessageSearch,
    QueryScope, file_count_text, group_by_date, sort_files,
};
pub use session::{MIN_FEATURE_LEVEL, Session};
pub use state::misc::{Draft, SystemNotice};
pub use state::unread::UnreadSnapshot;
pub use state::{ScrollPosition, folder_title};
