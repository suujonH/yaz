use zulip_core::{ConversationKey, ListEntry};

/// 与服务器的连接状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Reconnecting,
    /// 登录已失效，事件循环已退出。
    AuthFailed,
}

/// 通知弹窗上的头像。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationAvatar {
    /// 用户头像地址（相对或绝对）。
    Image(String),
    /// 文字头像：文字和底色。
    Text { text: String, color: String },
}

/// 判定为需要提醒的新消息。弹窗和声音由 app 处理。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRequest {
    pub key: ConversationKey,
    /// 通知的显示位置：同一位置的新通知替换旧的。免打扰会话共用群助手。
    pub slot: ListEntry,
    pub message_id: Option<i64>,
    pub title: String,
    pub body: String,
    pub avatar: NotificationAvatar,
    pub show_popup: bool,
    pub play_sound: bool,
    pub is_reminder: bool,
}

/// 会话事件。收到 `broadcast::error::RecvError::Lagged` 后界面应全量重读。
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    Connection(ConnectionState),
    /// 队列失效后重新加载了全部状态，等同于 Lagged。
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

    /// 有新的消息进入时间线。
    MessageAdded {
        key: ConversationKey,
        id: i64,
    },
    /// 本地回显被服务器的消息替换。
    MessageConfirmed {
        key: ConversationKey,
        local_id: String,
        id: i64,
    },
    /// 内容、回应、flags、小部件、发送状态等变化。
    MessageUpdated {
        key: ConversationKey,
        id: i64,
    },
    /// 从时间线移除（移动、本地回显取消、通知类消息被删除）。
    MessageRemoved {
        key: ConversationKey,
        id: i64,
    },
    /// 分页加载或整体失效，需要重读这个时间线。
    TimelineChanged(ConversationKey),
    StarredChanged {
        id: i64,
        starred: bool,
    },

    UnreadChanged(Vec<ConversationKey>),
    /// 会话列表的成员或顺序变化。
    ConversationsChanged,
    /// 单个会话的预览、标题、置顶、免打扰等变化。
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

    /// 用户触发的操作成功了，但需要告诉用户（如 @全体成员 被去掉后发送）。
    Toast(String),
    Notification(Box<NotificationRequest>),
}
