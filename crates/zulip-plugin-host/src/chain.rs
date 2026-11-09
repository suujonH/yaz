//! 事件链：按优先级依次调用接收事件的插件（filter / on_message 等），统一处理超时。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use zulip_core::ConversationKey;
use zulip_core::html::to_plain_text;
use zulip_core::model::ChatMessage;
use zulip_plugin_sdk::abi;
use zulip_session::{ConnectionState, HookEvent, HookFuture, OutgoingKind, Session};

use crate::event::PluginEvent;
use crate::host::PluginHost;
use crate::host_api::HOST_CATEGORY;
use crate::manifest::{
    FILTER_INCOMING, FILTER_OUTGOING, MONITOR_PRIORITY, ON_CHANNEL_CHANGED, ON_CONNECTION_CHANGED,
    ON_CONVERSATION_OPENED, ON_DRAFT_CHANGED, ON_MESSAGE, ON_MESSAGE_DELETED, ON_MESSAGE_EDITED,
    ON_MUTED_USERS_CHANGED, ON_PRESENCE_CHANGED, ON_REACTION_CHANGED, ON_READ_STATE_CHANGED,
    ON_REALM_CHANGED, ON_REMINDERS_CHANGED, ON_SCHEDULED_CHANGED, ON_TOPIC_MOVED,
    ON_TYPING_CHANGED, ON_USER_CHANGED, ON_USER_TOPIC_CHANGED,
};
use crate::marshal::{Conversation, MessageSnapshot, conversation_json};
use crate::plugin::NativePlugin;
use crate::thread;

/// 事件链和面板最多等待的时间。
pub const CHAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// [`PluginHost::filter_outgoing`] 的结果可以免过滤发送的时限。
pub const PREFILTER_LIMIT: Duration = Duration::from_secs(30);

/// 发送前过滤没有通过；`to_string()` 可以直接显示给用户。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutgoingRejected {
    #[error("消息已被插件拦截")]
    Blocked,
    #[error("消息已被插件清空")]
    Emptied,
    #[error("消息过长")]
    TooLong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChainResult {
    Ignore,
    Block,
    TimedOut,
}

// 事件链的超时记账。
#[derive(Default)]
pub(crate) struct ChainState {
    // 插件线程上正在调用的插件（超时时用于定位）。
    current: Mutex<Option<Arc<NativePlugin>>>,
    current_call: AtomicU64,
    // 排在同一个慢调用后面的多条事件链只计一次超时。
    charged_call: AtomicU64,
    // 已经超时、仍在插件线程上执行的事件链：期间新的事件直接按 IGNORE 处理，避免越积越多。
    stuck: Mutex<Option<u64>>,
    next_chain: AtomicU64,
}

// 已投递到插件线程的事件链。
struct StartedChain<S> {
    reply: oneshot::Receiver<(ChainResult, S)>,
    cancel: Arc<AtomicBool>,
}

pub(crate) struct Prefiltered {
    account_id: i64,
    key: ConversationKey,
    content: String,
    at: Instant,
}

impl PluginHost {
    // 插件线程上：调用一个导出函数，记录正在执行的插件。
    // 只有在超时时间内返回才清零超时计数：每次都超时但最终返回的插件仍会被标记为无响应。
    pub(crate) fn invoke<R>(&self, plugin: &Arc<NativePlugin>, call: impl FnOnce() -> R) -> R {
        self.chain.current_call.fetch_add(1, Ordering::AcqRel);
        *self.chain.current.lock() = Some(plugin.clone());
        let start = Instant::now();
        let result = call();
        if start.elapsed() < CHAIN_TIMEOUT {
            plugin.on_returned();
        }
        *self.chain.current.lock() = None;
        result
    }

    /// 提前过滤文字消息，用于在发送前知道结果（比如被拦截时保留输入框内容）。
    /// [`Session::send_as`] 本身会过滤；随后 [`PREFILTER_LIMIT`] 内发送返回的内容时不再过滤。
    pub async fn filter_outgoing(
        &self,
        session: &Arc<Session>,
        key: &ConversationKey,
        content: &str,
    ) -> Result<String, OutgoingRejected> {
        let filtered = self
            .filter_outgoing_as(session, key, content, OutgoingKind::Text)
            .await?;
        let mut prefiltered = self.prefiltered.lock();
        prefiltered.retain(|p| p.at.elapsed() < PREFILTER_LIMIT);
        prefiltered.push(Prefiltered {
            account_id: session.account_id(),
            key: key.clone(),
            content: filtered.clone(),
            at: Instant::now(),
        });
        Ok(filtered)
    }

    fn take_prefiltered(&self, account_id: i64, key: &ConversationKey, content: &str) -> bool {
        let mut prefiltered = self.prefiltered.lock();
        let found = prefiltered.iter().position(|p| {
            p.account_id == account_id
                && p.key == *key
                && p.content == content
                && p.at.elapsed() < PREFILTER_LIMIT
        });
        found.map(|index| prefiltered.remove(index)).is_some()
    }

    /// 发送前过滤：通过时返回（可能被改写的）内容，超时时返回原内容。
    /// 每个插件看到的是前面的插件改写后的内容。
    pub async fn filter_outgoing_as(
        &self,
        session: &Arc<Session>,
        key: &ConversationKey,
        content: &str,
        kind: OutgoingKind,
    ) -> Result<String, OutgoingRejected> {
        let candidates = self.has_handlers(FILTER_OUTGOING);
        let conv = Conversation::from_key(key, session);
        let (false, Some(conv)) = (!candidates, conv) else {
            return Ok(content.to_owned());
        };
        let account_id = session.account_id();
        let (result, state) = self
            .run_chain(
                FILTER_OUTGOING,
                session,
                content.to_owned(),
                move |plugin, current| {
                    let (result, changed) = thread::in_outgoing_filter(|| {
                        plugin.call_filter_outgoing(account_id, &conv, current, kind)
                    });
                    // 只监控的插件不能改写内容。
                    if let Some(changed) = changed
                        && plugin.priority(FILTER_OUTGOING) != MONITOR_PRIORITY
                    {
                        *current = changed;
                    }
                    result
                },
            )
            .await;
        match (result, state) {
            (ChainResult::Block, _) => Err(OutgoingRejected::Blocked),
            (ChainResult::Ignore, Some(current)) if current != content => {
                if current.trim().is_empty() {
                    return Err(OutgoingRejected::Emptied);
                }
                let limit = session.realm().max_message_length;
                if limit > 0 && current.chars().count() > limit as usize {
                    return Err(OutgoingRejected::TooLong);
                }
                Ok(current)
            }
            _ => Ok(content.to_owned()),
        }
    }

    fn has_handlers(&self, event: &str) -> bool {
        self.plugins.read().iter().any(|p| p.handles(event))
    }

    /// 事件链：按优先级（相同时按 id）依次调用接收该事件的插件，返回 BLOCK 时停止；
    /// 优先级 10000 的插件只能监控，它返回 BLOCK 视为 IGNORE。最多等待 2 秒，
    /// 超时后剩余的插件不再调用，正在执行的插件超时计数加 1。
    async fn run_chain<S: Send + 'static>(
        &self,
        event: &'static str,
        session: &Arc<Session>,
        state: S,
        invoke: impl FnMut(&Arc<NativePlugin>, &mut S) -> i32 + Send + 'static,
    ) -> (ChainResult, Option<S>) {
        match self.start_chain(event, session, state, invoke) {
            Ok(started) => self.finish_chain(event, started).await,
            Err(state) => (ChainResult::Ignore, Some(state)),
        }
    }

    // 同步投递到插件线程，事件按调用顺序执行。没有要调用的插件时退回 state。
    fn start_chain<S: Send + 'static>(
        &self,
        event: &'static str,
        session: &Arc<Session>,
        mut state: S,
        mut invoke: impl FnMut(&Arc<NativePlugin>, &mut S) -> i32 + Send + 'static,
    ) -> Result<StartedChain<S>, S> {
        let mut candidates: Vec<_> = self
            .plugins()
            .into_iter()
            .filter(|p| p.handles(event))
            .collect();
        candidates.sort_by_key(|p| (p.priority(event), p.id()));
        let Some(chain_host) = self.weak.upgrade() else {
            return Err(state);
        };
        if candidates.is_empty() || self.chain.stuck.lock().is_some() {
            return Err(state);
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let reply = self.thread.run(Some(session.clone()), move || {
            for plugin in &candidates {
                if cancelled.load(Ordering::Acquire) {
                    break;
                }
                if !plugin.is_enabled() || plugin.is_unresponsive() {
                    continue;
                }
                let result = chain_host.invoke(plugin, || invoke(plugin, &mut state));
                if result == abi::EVENT_BLOCK && plugin.priority(event) != MONITOR_PRIORITY {
                    return (ChainResult::Block, state);
                }
            }
            (ChainResult::Ignore, state)
        });
        Ok(StartedChain { reply, cancel })
    }

    async fn finish_chain<S: Send + 'static>(
        &self,
        event: &'static str,
        started: StartedChain<S>,
    ) -> (ChainResult, Option<S>) {
        let StartedChain {
            reply: mut rx,
            cancel,
        } = started;
        match tokio::time::timeout(CHAIN_TIMEOUT, &mut rx).await {
            Ok(Ok((result, state))) => return (result, Some(state)),
            Ok(Err(_)) => return (ChainResult::Ignore, None),
            Err(_) => {}
        }

        cancel.store(true, Ordering::Release);
        self.charge_timeout(event);
        let Some(host) = self.weak.upgrade() else {
            return (ChainResult::TimedOut, None);
        };
        let chain_id = self.chain.next_chain.fetch_add(1, Ordering::AcqRel) + 1;
        *self.chain.stuck.lock() = Some(chain_id);
        self.runtime.spawn(async move {
            let _ = rx.await;
            let mut stuck = host.chain.stuck.lock();
            if *stuck == Some(chain_id) {
                *stuck = None;
            }
        });
        self.emit(PluginEvent::PluginsChanged);
        (ChainResult::TimedOut, None)
    }

    // 不等待结果的事件链（通知型事件）。
    fn spawn_chain<S: Send + 'static>(
        &self,
        event: &'static str,
        session: &Arc<Session>,
        state: S,
        invoke: impl FnMut(&Arc<NativePlugin>, &mut S) -> i32 + Send + 'static,
    ) {
        let (Some(host), Ok(started)) = (
            self.weak.upgrade(),
            self.start_chain(event, session, state, invoke),
        ) else {
            return;
        };
        self.runtime.spawn(async move {
            host.finish_chain(event, started).await;
        });
    }

    fn notify(&self, event: &'static str, session: &Arc<Session>, mut payload: Value) {
        payload["account"] = json!(session.account_id());
        let json = payload.to_string();
        self.spawn_chain(event, session, (), move |plugin, ()| {
            plugin.call_notification(event, &json);
            abi::EVENT_IGNORE
        });
    }

    fn charge_timeout(&self, event: &str) {
        let call = self.chain.current_call.load(Ordering::Acquire);
        let stuck = self.chain.current.lock().clone();
        log::warn!(
            "插件事件 {event} 超时：{}",
            stuck.as_ref().map_or("未知".to_owned(), |p| p.id())
        );
        let Some(plugin) = stuck else {
            return;
        };
        if self.chain.charged_call.swap(call, Ordering::AcqRel) == call {
            return;
        }
        plugin.on_timeout();
        let suffix = if plugin.is_unresponsive() {
            "，连续多次超时，本次运行内不再派发事件"
        } else {
            ""
        };
        plugin.add_log(
            abi::LOG_WARNING,
            HOST_CATEGORY,
            &format!(
                "{event} 超过 {} 秒没有返回{suffix}",
                CHAIN_TIMEOUT.as_secs()
            ),
        );
    }
}

impl PluginHost {
    pub(crate) fn dispatch_filter_incoming(
        &self,
        session: &Arc<Session>,
        message: &ChatMessage,
    ) -> HookFuture<bool> {
        let host = self.weak.upgrade();
        let snapshot = MessageSnapshot::new(message, session, false);
        let (Some(host), Some(snapshot), true) =
            (host, snapshot, self.has_handlers(FILTER_INCOMING))
        else {
            return Box::pin(async { false });
        };
        let session = session.clone();
        Box::pin(async move {
            let (result, _) = host
                .run_chain(FILTER_INCOMING, &session, (), move |plugin, ()| {
                    plugin.call_filter_incoming(&snapshot)
                })
                .await;
            result == ChainResult::Block
        })
    }

    pub(crate) fn dispatch_on_message(&self, session: &Arc<Session>, message: &ChatMessage) {
        let host = self.weak.upgrade();
        let snapshot = MessageSnapshot::new(message, session, false);
        let (Some(host), Some(snapshot), true) = (host, snapshot, self.has_handlers(ON_MESSAGE))
        else {
            return;
        };
        host.spawn_chain(ON_MESSAGE, session, (), move |plugin, ()| {
            plugin.call_on_message(&snapshot)
        });
    }

    pub(crate) fn dispatch_event(&self, session: &Arc<Session>, event: HookEvent) {
        let name = hook_event_name(&event);
        if self.has_handlers(name) {
            self.notify(name, session, hook_event_payload(event, session));
        }
    }

    pub(crate) fn dispatch_filter_outgoing(
        &self,
        session: &Arc<Session>,
        key: &ConversationKey,
        content: &str,
        kind: OutgoingKind,
    ) -> HookFuture<Result<String, String>> {
        let Some(host) = self.weak.upgrade() else {
            let content = content.to_owned();
            return Box::pin(async move { Ok(content) });
        };
        let session = session.clone();
        let key = key.clone();
        let content = content.to_owned();
        if kind == OutgoingKind::Text && host.take_prefiltered(session.account_id(), &key, &content)
        {
            return Box::pin(async move { Ok(content) });
        }
        Box::pin(async move {
            host.filter_outgoing_as(&session, &key, &content, kind)
                .await
                .map_err(|rejected| rejected.to_string())
        })
    }
}

fn hook_event_name(event: &HookEvent) -> &'static str {
    match event {
        HookEvent::MessageEdited { .. } => ON_MESSAGE_EDITED,
        HookEvent::MessagesDeleted { .. } => ON_MESSAGE_DELETED,
        HookEvent::ReactionChanged { .. } => ON_REACTION_CHANGED,
        HookEvent::ReadChanged { .. } => ON_READ_STATE_CHANGED,
        HookEvent::TopicMoved { .. } => ON_TOPIC_MOVED,
        HookEvent::ChannelsChanged { .. } => ON_CHANNEL_CHANGED,
        HookEvent::UserChanged(_) => ON_USER_CHANGED,
        HookEvent::PresenceChanged(_) => ON_PRESENCE_CHANGED,
        HookEvent::ConversationOpened(_) => ON_CONVERSATION_OPENED,
        HookEvent::Connection(_) => ON_CONNECTION_CHANGED,
        HookEvent::TypingChanged { .. } => ON_TYPING_CHANGED,
        HookEvent::ScheduledChanged => ON_SCHEDULED_CHANGED,
        HookEvent::RemindersChanged => ON_REMINDERS_CHANGED,
        HookEvent::DraftChanged(_) => ON_DRAFT_CHANGED,
        HookEvent::UserTopicChanged { .. } => ON_USER_TOPIC_CHANGED,
        HookEvent::MutedUsersChanged => ON_MUTED_USERS_CHANGED,
        HookEvent::RealmChanged { .. } => ON_REALM_CHANGED,
    }
}

fn hook_event_payload(event: HookEvent, session: &Session) -> Value {
    let channel = |stream_id: i64, topic: String| {
        Conversation::Channel {
            stream_id,
            stream_name: session
                .channel(stream_id)
                .map(|c| c.name)
                .unwrap_or_default(),
            topic,
        }
        .to_json()
    };
    match event {
        HookEvent::MessageEdited {
            key,
            id,
            edit_timestamp,
            html,
        } => json!({
            "message_id": id,
            "conv": conversation_json(key.as_ref(), session),
            "edit_timestamp": edit_timestamp,
            "content_text": to_plain_text(&html),
            "content_html": html,
        }),
        HookEvent::MessagesDeleted { key, ids } => json!({
            "message_ids": ids,
            "conv": conversation_json(key.as_ref(), session),
        }),
        HookEvent::ReactionChanged {
            key,
            id,
            reaction,
            added,
        } => json!({
            "message_id": id,
            "conv": conversation_json(key.as_ref(), session),
            "added": added,
            "reaction": reaction,
        }),
        HookEvent::ReadChanged { ids, read, all } => {
            json!({"message_ids": ids, "read": read, "all": all})
        }
        HookEvent::TopicMoved {
            old_stream,
            old_topic,
            new_stream,
            new_topic,
            ids,
            whole,
        } => json!({
            "old": channel(old_stream, old_topic),
            "new": channel(new_stream, new_topic),
            "message_ids": ids,
            "whole": whole,
        }),
        HookEvent::ChannelsChanged { stream_ids, op } => {
            json!({"streams": stream_ids, "op": op})
        }
        HookEvent::UserChanged(id) => json!({"user": id}),
        HookEvent::PresenceChanged(ids) => json!({"users": ids}),
        HookEvent::ConversationOpened(key) => {
            json!({"conv": conversation_json(Some(&key), session)})
        }
        HookEvent::Connection(state) => json!({"state": match state {
            ConnectionState::Connected => "connected",
            ConnectionState::Reconnecting => "reconnecting",
            ConnectionState::AuthFailed => "auth_failed",
        }}),
        HookEvent::TypingChanged {
            key,
            user_id,
            typing,
        } => json!({
            "conv": conversation_json(Some(&key), session),
            "user": user_id,
            "typing": typing,
        }),
        HookEvent::ScheduledChanged | HookEvent::RemindersChanged => json!({}),
        HookEvent::DraftChanged(key) => {
            json!({"conv": conversation_json(Some(&key), session)})
        }
        HookEvent::UserTopicChanged { stream_id, topic } => json!({
            "visibility": session.topic_visibility(stream_id, &topic) as i32,
            "conv": channel(stream_id, topic),
        }),
        HookEvent::MutedUsersChanged => json!({"users": session.muted_users()}),
        HookEvent::RealmChanged { properties } => json!({"properties": properties}),
    }
}
