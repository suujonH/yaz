//! 会话层对上层（插件宿主）的钩子。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use zulip_core::ConversationKey;
use zulip_core::model::{ChatMessage, Reaction};

use crate::event::ConnectionState;
use crate::session::Session;

pub type HookFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// 发送内容的来源。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutgoingKind {
    #[default]
    Text,
    File,
    Forward,
}

/// 通知型事件，不能拦截。`key` 为 `None` 表示消息不在缓存中且事件没有带会话（私聊）。
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
    /// `all`：全部消息（此时 `ids` 可能为空）。
    ReadChanged {
        ids: Vec<i64>,
        read: bool,
        all: bool,
    },
    /// 话题改名或移动。`whole`：整个话题，否则只是其中一部分消息。
    TopicMoved {
        old_stream: i64,
        old_topic: String,
        new_stream: i64,
        new_topic: String,
        ids: Vec<i64>,
        whole: bool,
    },
    /// `op`：服务器事件的操作（create、update、add、remove 等）。
    ChannelsChanged {
        stream_ids: Vec<i64>,
        op: String,
    },
    UserChanged(i64),
    PresenceChanged(Vec<i64>),
    /// 用户切换到了这个会话。
    ConversationOpened(ConversationKey),
    Connection(ConnectionState),
    /// 服务器的 typing 事件。`typing` 为 false 表示对方停止输入；对方异常离开时不一定有这个事件。
    TypingChanged {
        key: ConversationKey,
        user_id: i64,
        typing: bool,
    },
    ScheduledChanged,
    RemindersChanged,
    /// 草稿内容变化：本机保存、删除（关闭同步时），或服务器同步的变化。
    DraftChanged(ConversationKey),
    UserTopicChanged {
        stream_id: i64,
        topic: String,
    },
    MutedUsersChanged,
    /// `properties`：变化的组织设置名。
    RealmChanged {
        properties: Vec<String>,
    },
}

/// 通过 [`Hub::set_hooks`](crate::Hub::set_hooks) 安装，对之后登录的账号生效。
pub trait SessionHooks: Send + Sync {
    /// 账号登录完成。
    fn session_started(&self, session: &Arc<Session>);

    /// 账号即将结束：等待完成（最多 2 秒）后才关闭会话。
    fn session_ending(&self, session: &Arc<Session>) -> HookFuture<()>;

    /// 别人发来的新消息。返回 true 时拦截：不显示、不提醒、标记已读，本次运行内加载历史时也不显示。
    /// 事件循环等待结果，实现必须限时。
    fn filter_incoming(&self, session: &Arc<Session>, message: &ChatMessage) -> HookFuture<bool>;

    /// 新消息已进入状态（含自己发送的消息，不含被拦截的）。
    fn on_message(&self, session: &Arc<Session>, message: &ChatMessage);

    /// 状态已更新后调用，不能阻塞。
    fn on_event(&self, _session: &Arc<Session>, _event: HookEvent) {}

    /// 发送前过滤（[`Session::send_as`] 等待结果，实现必须限时）：`Ok` 为要发送的内容，
    /// `Err` 为拦截的原因（显示给用户）。
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
