//! 时间线：加载、定位、打开和关闭会话。

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

/// 时间线快照。消息是共享的不可变数据，克隆很便宜。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineSnapshot {
    pub messages: Vec<Arc<ChatMessage>>,
    pub found_oldest: bool,
    pub found_newest: bool,
    pub version: u64,
}

/// 打开会话时的信息：未读定位和滚动恢复。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedConversation {
    pub unread: UnreadSnapshot,
    pub first_unread_id: Option<i64>,
    /// 提及我的未读，最新的在前（“@我”横幅逐条跳转）。
    pub mention_queue: Vec<(i64, MentionKind)>,
    pub scroll: Option<ScrollPosition>,
}

/// 消息定位的结果：所在会话和消息 id，消息已加载到时间线。
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

    /// 窗口打开会话：放入列表、不被缓存淘汰，返回未读定位信息。多个窗口可以同时打开同一会话。
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
        OpenedConversation {
            first_unread_id: unread.ids.first().copied(),
            mention_queue: unread.mentions.iter().rev().copied().collect(),
            unread,
            scroll,
        }
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

    /// 用户最后切换到的会话（打开，或它所在的窗口回到前台）；已关闭时为 `None`。
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

    /// 界面报告：这个会话在前台窗口中并且看着最新消息。此时新消息自动已读、不提醒。
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

    /// 加载最新一页。缓存已足够时不发请求；同一会话的并发调用只请求一次。
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

    /// 向上翻页。已到最早时什么也不做。
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

    /// 向下翻页。已到最新时什么也不做。
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

    /// 定位到消息：搜索、收藏、提醒、链接、未读和 @ 跳转都走这里。
    /// `key` 未知时先查询消息所在的会话。消息已在窗口中时不发分页请求。
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

    /// 打开本组织的消息链接；不是消息链接时为 `Ok(None)`。
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

    /// 一页消息 DTO 转为领域模型，不写入缓存。
    pub(crate) fn prepare_all(&self, dtos: &[MessageDto]) -> Vec<ChatMessage> {
        let state = self.state.read();
        dtos.iter().map(|dto| state.prepare_message(dto)).collect()
    }
}
