//! Event chain: calls the plugins that receive an event in priority order (filter / on_message
//! etc.) and handles timeouts uniformly.

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

/// Maximum time the event chain and panels wait.
pub const CHAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Time window in which content returned by [`PluginHost::filter_outgoing`] can be sent without
/// being filtered again.
pub const PREFILTER_LIMIT: Duration = Duration::from_secs(30);

/// The pre-send filter did not pass; `to_string()` can be shown to the user directly.
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

// Timeout bookkeeping for event chains.
#[derive(Default)]
pub(crate) struct ChainState {
    // The plugin currently being called on the plugin thread (used to locate it on timeout).
    current: Mutex<Option<Arc<NativePlugin>>>,
    current_call: AtomicU64,
    // Several event chains queued behind the same slow call count as only one timeout.
    charged_call: AtomicU64,
    // Event chains that have timed out but are still running on the plugin thread: new events are
    // treated as IGNORE meanwhile, to avoid piling up.
    stuck: Mutex<Option<u64>>,
    next_chain: AtomicU64,
}

// Event chains already delivered to the plugin thread.
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
    // On the plugin thread: calls an exported function and records the plugin being executed. The
    // timeout counter is reset only when the call returns within the timeout: a plugin that times
    // out every time but eventually returns is still marked unresponsive.
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

    /// Pre-filters a text message so the result is known before sending (e.g. to keep the composer
    /// content when it is blocked). [`Session::send_as`] filters by itself; sending the returned
    /// content within [`PREFILTER_LIMIT`] afterwards is not filtered again.
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

    /// Pre-send filter: returns the (possibly rewritten) content when it passes, and the original
    /// content on timeout. Each plugin sees the content rewritten by the previous plugins.
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
                    // Monitor-only plugins cannot rewrite the content.
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

    /// Event chain: calls the plugins that receive the event in priority order (ties broken by id),
    /// stopping when one returns BLOCK; plugins with priority 10000 are monitor-only, and a BLOCK
    /// from them is treated as IGNORE. Waits at most 2 seconds; after a timeout the remaining
    /// plugins are not called, and the timeout count of the plugin being executed is incremented by
    /// 1.
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

    // Delivered synchronously to the plugin thread; events run in call order. Falls back to state
    // when there is no plugin to call.
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

    // Event chain that does not wait for the result (notification-type events).
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
