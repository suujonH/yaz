//! Timeline: load, locate, open and close conversations.

use std::sync::Arc;

use zulip_api::{MessageDto, MessageQuery};
use zulip_core::ConversationKey;
use zulip_core::links;
use zulip_core::model::{ChatMessage, MentionKind};

use crate::error::Result;
use crate::event::SessionEvent;
use crate::hooks::HookEvent;
use crate::session::Session;
use crate::state::ScrollPosition;
use crate::state::messages::{ENOUGH_CACHED, PageKind};
use crate::state::unread::UnreadSnapshot;

const PAGE_SIZE: u32 = 50;
const AROUND_BEFORE: u32 = 20;
const AROUND_AFTER: u32 = 50;

/// Timeline snapshot. Messages are shared immutable data, so cloning is cheap.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineSnapshot {
    pub messages: Vec<Arc<ChatMessage>>,
    pub found_oldest: bool,
    pub found_newest: bool,
    pub version: u64,
}

/// Information when opening a conversation: unread positioning and scroll restoration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedConversation {
    pub unread: UnreadSnapshot,
    pub first_unread_id: Option<i64>,
    /// Unread messages that mention me, newest first (the "@me" banner jumps through them one by one).
    pub mention_queue: Vec<(i64, MentionKind)>,
    pub scroll: Option<ScrollPosition>,
}

impl OpenedConversation {
    fn new(unread: UnreadSnapshot, scroll: Option<ScrollPosition>) -> Self {
        Self {
            first_unread_id: unread.ids.first().copied(),
            mention_queue: unread.mentions.iter().rev().copied().collect(),
            unread,
            scroll,
        }
    }
}

/// Result of locating a message: its conversation and message id; the message is already loaded
/// into the timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedMessage {
    pub key: ConversationKey,
    pub message_id: i64,
}

impl Session {
    pub fn timeline(&self, key: &ConversationKey) -> TimelineSnapshot {
        let state = self.state.read();
        state
            .messages
            .timeline(key)
            .map(|t| TimelineSnapshot {
                messages: t
                    .messages
                    .iter()
                    .filter(|m| !state.messages.is_hidden(m))
                    .cloned()
                    .collect(),
                found_oldest: t.found_oldest,
                found_newest: t.found_newest,
                version: t.version,
            })
            .unwrap_or_default()
    }

    pub fn message(&self, id: i64) -> Option<Arc<ChatMessage>> {
        self.state.read().messages.get(id).cloned()
    }

    /// A window opens a conversation: it is put in the list, not evicted from the cache, and the
    /// unread positioning info is returned. Several windows can open the same conversation at once.
    pub fn open_conversation(&self, key: &ConversationKey) -> OpenedConversation {
        let mut state = self.state.write();
        state.set_open(key, true);
        let title = state.topic_name(key);
        let listed = state.conversations.ensure_in_list(key, &title);
        let unread = state.unread.snapshot(key);
        let scroll = state.view.scroll(key);
        drop(state);
        if listed {
            self.emit(SessionEvent::ConversationsChanged);
        }
        self.set_current_conversation(key);
        OpenedConversation::new(unread, scroll)
    }

    /// The same positioning info as [`open_conversation`](Self::open_conversation) without its
    /// effects (not marked open, not added to the list, not the current conversation, no hook):
    /// used to render a window in advance before it is shown.
    pub fn peek_conversation(&self, key: &ConversationKey) -> OpenedConversation {
        let state = self.state.read();
        OpenedConversation::new(state.unread.snapshot(key), state.view.scroll(key))
    }

    pub fn close_conversation(&self, key: &ConversationKey) {
        let mut state = self.state.write();
        state.set_open(key, false);
        let closed = !state.messages.is_open(key);
        drop(state);
        let mut current = self.current.lock();
        if closed && current.as_ref() == Some(key) {
            *current = None;
        }
    }

    /// The conversation the user switched to last (opened, or its window came back to the
    /// foreground); `None` when closed.
    pub fn current_conversation(&self) -> Option<ConversationKey> {
        self.current.lock().clone()
    }

    fn set_current_conversation(&self, key: &ConversationKey) {
        let mut current = self.current.lock();
        if current.as_ref() != Some(key) {
            *current = Some(key.clone());
            drop(current);
            self.notify_hooks(HookEvent::ConversationOpened(key.clone()));
        }
    }

    /// UI report: this conversation is in the foreground window and showing the latest message. New
    /// messages are then automatically read and do not notify.
    pub fn set_actively_viewing(&self, key: &ConversationKey, active: bool) {
        self.state.write().view.set_actively_viewing(key, active);
        if active {
            self.set_current_conversation(key);
        }
    }

    pub fn save_scroll_position(&self, key: &ConversationKey, position: Option<ScrollPosition>) {
        self.state.write().view.save_scroll(key, position);
    }

    async fn load_page(
        &self,
        key: &ConversationKey,
        anchor: String,
        before: u32,
        after: u32,
        include_anchor: bool,
        kind: PageKind,
    ) -> Result<()> {
        let narrow = self.narrow_of(key);
        let mut query = MessageQuery::new(narrow, anchor, before, after);
        query.include_anchor = include_anchor;
        let result = self.client.get_messages(&query).await?;
        {
            let mut state = self.state.write();
            let page: Vec<ChatMessage> = result
                .messages
                .iter()
                .map(|dto| state.prepare_message(dto))
                .collect();
            state
                .messages
                .merge_page(key, page, kind, result.found_oldest, result.found_newest);
        }
        self.emit(SessionEvent::TimelineChanged(key.clone()));
        Ok(())
    }

    /// Loads the latest page. No request when the cache is already sufficient; concurrent calls for
    /// the same conversation issue only one request.
    pub async fn load_newest(&self, key: &ConversationKey) -> Result<()> {
        let gate = self.gate(format!("timeline:{key}"));
        let _guard = gate.lock().await;
        let enough = self
            .state
            .read()
            .messages
            .timeline(key)
            .is_some_and(|t| t.has_enough_cached());
        if enough {
            return Ok(());
        }
        self.load_page(
            key,
            "newest".to_owned(),
            PAGE_SIZE.max(ENOUGH_CACHED as u32),
            0,
            true,
            PageKind::Newest,
        )
        .await
    }

    /// Pages upward. Does nothing when the earliest is already reached.
    pub async fn load_older(&self, key: &ConversationKey) -> Result<()> {
        let gate = self.gate(format!("timeline:{key}"));
        let _guard = gate.lock().await;
        let anchor = {
            let state = self.state.read();
            let Some(timeline) = state.messages.timeline(key) else {
                return Ok(());
            };
            if timeline.found_oldest {
                return Ok(());
            }
            timeline.messages.iter().map(|m| m.id).find(|id| *id > 0)
        };
        let Some(anchor) = anchor else {
            return Ok(());
        };
        self.load_page(
            key,
            anchor.to_string(),
            PAGE_SIZE,
            0,
            false,
            PageKind::Older,
        )
        .await
    }

    /// Pages downward. Does nothing when the latest is already reached.
    pub async fn load_newer(&self, key: &ConversationKey) -> Result<()> {
        let gate = self.gate(format!("timeline:{key}"));
        let _guard = gate.lock().await;
        let anchor = {
            let state = self.state.read();
            let Some(timeline) = state.messages.timeline(key) else {
                return Ok(());
            };
            if timeline.found_newest {
                return Ok(());
            }
            timeline
                .messages
                .iter()
                .rev()
                .map(|m| m.id)
                .find(|id| *id > 0)
        };
        let Some(anchor) = anchor else {
            return Ok(());
        };
        self.load_page(
            key,
            anchor.to_string(),
            0,
            PAGE_SIZE,
            false,
            PageKind::Newer,
        )
        .await
    }

    /// Locates a message: search, favorites, reminders, links, unread and @ jumps all go through here.
    /// When `key` is unknown, first queries the conversation the message is in. No paging request
    /// when the message is already in the window.
    pub async fn locate_message(
        &self,
        key: Option<&ConversationKey>,
        message_id: i64,
    ) -> Result<LocatedMessage> {
        let key = match key {
            Some(key) => key.clone(),
            None => match self.message(message_id) {
                Some(message) => message.key.clone(),
                None => {
                    let dto = self.client.get_message(message_id, true).await?;
                    self.state.read().prepare_message(&dto).key
                }
            },
        };
        let cached = self.message(message_id).is_some_and(|m| m.key == key);
        if !cached {
            let gate = self.gate(format!("timeline:{key}"));
            let _guard = gate.lock().await;
            self.load_page(
                &key,
                message_id.to_string(),
                AROUND_BEFORE,
                AROUND_AFTER,
                true,
                PageKind::Around,
            )
            .await?;
        }
        Ok(LocatedMessage { key, message_id })
    }

    /// Opens a message link of this organization; `Ok(None)` when it is not a message link.
    pub async fn locate_link(&self, url: &str) -> Result<Option<LocatedMessage>> {
        let realm = self.client.realm_url().to_string();
        let self_id = self.state.read().users.self_id;
        let Some(link) = links::parse(url, realm.trim_end_matches('/')) else {
            return Ok(None);
        };
        let Some(id) = link.near_message_id else {
            return Ok(None);
        };
        let key = match (link.stream_id, link.topic, link.dm_user_ids) {
            (Some(stream), Some(topic), _) => Some(ConversationKey::topic(stream, topic)),
            (_, _, Some(ids)) => Some(ConversationKey::dm(ids, self_id)),
            _ => None,
        };
        self.locate_message(key.as_ref(), id).await.map(Some)
    }

    /// Converts a page of message DTOs to the domain model, without writing to the cache.
    pub(crate) fn prepare_all(&self, dtos: &[MessageDto]) -> Vec<ChatMessage> {
        let state = self.state.read();
        dtos.iter().map(|dto| state.prepare_message(dto)).collect()
    }
}
