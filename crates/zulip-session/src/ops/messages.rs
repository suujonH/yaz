//! 单条消息的操作：编辑、回应、星标、小部件、提醒、链接、权限。

use zulip_api::{FlagOp, MessageFlag, MessageUpdate};
use zulip_core::ConversationKey;
use zulip_core::emoji::EmojiEntry;
use zulip_core::links;
use zulip_core::model::{ChatMessage, MessageRevision};
use zulip_core::widget::{self, Widget};
use zulip_store::MessageVersionRepo;

use super::read_state::FLAG_BATCH;
use crate::error::{Result, SessionError};
use crate::session::Session;
use crate::state::State;

/// 频道订阅者超过这个数时，@全体成员 需要权限。
const WILDCARD_FREE_LIMIT: usize = 15;

/// 一条消息上可以执行的操作。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageActions {
    pub can_edit: bool,
    pub can_delete: bool,
    pub can_show_edit_history: bool,
    pub can_show_read_receipts: bool,
}

/// 小部件操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WidgetAction {
    Vote { option_key: String, vote: bool },
    AddPollOption { option: String },
    AddTodoTask { task: String, desc: String },
    StrikeTodoTask { task_key: String },
}

impl Session {
    pub async fn create_reminder(&self, message_id: i64, deliver_at: i64) -> Result<i64> {
        Ok(self.client.create_reminder(message_id, deliver_at).await?)
    }

    /// 消息的 Markdown 原文。
    pub async fn raw_content(&self, message_id: i64) -> Result<String> {
        Ok(self.client.get_message(message_id, false).await?.content)
    }

    pub async fn render_preview(&self, content: &str) -> Result<String> {
        Ok(self.client.render_message(content).await?)
    }

    pub async fn edit_message(&self, message_id: i64, content: &str) -> Result<()> {
        let update = MessageUpdate {
            content: Some(content.to_owned()),
            ..MessageUpdate::default()
        };
        self.client.update_message(message_id, &update).await?;
        Ok(())
    }

    pub async fn message_history(&self, message_id: i64) -> Result<Vec<MessageRevision>> {
        Ok(self.client.get_message_history(message_id).await?)
    }

    /// 本机保存的编辑前的各个版本（服务器渲染的 HTML），从最早到最近。消息已被删除时也能取到。
    pub async fn previous_contents(&self, message_id: i64) -> Result<Vec<String>> {
        let account_id = self.account_id;
        Ok(self
            .db
            .call(move |db| MessageVersionRepo::new(db, account_id).load(message_id))
            .await?)
    }

    pub async fn read_receipts(&self, message_id: i64) -> Result<Vec<i64>> {
        Ok(self.client.get_read_receipts(message_id).await?)
    }

    /// 这个会话是否显示“已读/未读”：只有一对一私聊，且组织开启了已读回执。
    pub fn shows_peer_read(&self, key: &ConversationKey) -> bool {
        matches!(key, ConversationKey::Dm(d) if !d.is_group_dm() && !d.is_self_dm())
            && self.state.read().realm.enable_read_receipts
    }

    /// 一对一私聊中对方是否已读我的最后一条消息。不适用时为 `None`。
    pub async fn peer_has_read(&self, key: &ConversationKey) -> Result<Option<bool>> {
        if !self.shows_peer_read(key) {
            return Ok(None);
        }
        let ConversationKey::Dm(d) = key else {
            return Ok(None);
        };
        let last_own = {
            let state = self.state.read();
            let self_id = state.users.self_id;
            state.messages.timeline(key).and_then(|t| {
                t.messages
                    .iter()
                    .rev()
                    .find(|m| m.id > 0 && m.sender_id == self_id)
                    .map(|m| m.id)
            })
        };
        let Some(id) = last_own else {
            return Ok(None);
        };
        let readers = self.client.get_read_receipts(id).await?;
        Ok(Some(d.other_user_ids().iter().any(|u| readers.contains(u))))
    }

    /// 切换自己的表情回应。结果由事件更新，不乐观修改。
    pub async fn toggle_reaction(&self, message_id: i64, emoji: &EmojiEntry) -> Result<()> {
        self.toggle_reaction_of(
            message_id,
            &emoji.name,
            &emoji.code,
            emoji.kind.reaction_type(),
        )
        .await
    }

    /// 自己已经贴过就撤回，否则贴上。`kind` 是服务器的 reaction_type。
    pub async fn toggle_reaction_of(
        &self,
        message_id: i64,
        name: &str,
        code: &str,
        kind: &str,
    ) -> Result<()> {
        let mine = {
            let state = self.state.read();
            let self_id = state.users.self_id;
            state.messages.get(message_id).is_some_and(|m| {
                m.reactions.iter().any(|r| {
                    r.user_id == self_id && r.emoji_code == code && r.reaction_type == kind
                })
            })
        };
        if mine {
            self.client
                .remove_reaction(message_id, name, code, kind)
                .await?;
        } else {
            self.client
                .add_reaction(message_id, name, code, kind)
                .await?;
            self.add_recent_emoji(name);
        }
        Ok(())
    }

    pub async fn set_starred(&self, ids: &[i64], starred: bool) -> Result<()> {
        let op = if starred { FlagOp::Add } else { FlagOp::Remove };
        for batch in ids.chunks(FLAG_BATCH) {
            self.client
                .update_flags(batch, op, MessageFlag::Starred)
                .await?;
        }
        Ok(())
    }

    /// 投票、待办等小部件的操作。
    pub async fn widget_action(&self, message_id: i64, action: WidgetAction) -> Result<()> {
        let content = {
            let state = self.state.read();
            let self_id = state.users.self_id;
            let message = state
                .messages
                .get(message_id)
                .ok_or_else(|| SessionError::invalid("消息不存在"))?;
            let next = widget::next_own_index(&message.submessages, self_id);
            match (
                &action,
                widget::parse(&message.submessages, message.sender_id),
            ) {
                (WidgetAction::Vote { option_key, vote }, _) => {
                    widget::vote_event(option_key, *vote)
                }
                (WidgetAction::AddPollOption { option }, Some(Widget::Poll(poll))) => {
                    if poll.options.iter().any(|o| &o.text == option) {
                        return Err(SessionError::invalid("选项已存在"));
                    }
                    widget::new_option_event(next, option)
                }
                (WidgetAction::AddTodoTask { task, desc }, Some(Widget::Todo(todo))) => {
                    if todo.tasks.iter().any(|t| &t.text == task) {
                        return Err(SessionError::invalid("任务已存在"));
                    }
                    widget::new_task_event(next, task, desc)
                }
                (WidgetAction::StrikeTodoTask { task_key }, _) => widget::strike_event(task_key),
                _ => return Err(SessionError::invalid("这条消息不支持这个操作")),
            }
        };
        self.client
            .send_submessage(message_id, "widget", &content)
            .await?;
        Ok(())
    }

    /// 消息链接。
    pub fn message_link(&self, key: &ConversationKey, message_id: i64) -> String {
        let state = self.state.read();
        let realm = state.realm.url.clone();
        let (channel, topic) = match key {
            ConversationKey::Topic(t) => (
                state
                    .channels
                    .get(t.stream_id)
                    .map(|c| c.name.clone())
                    .unwrap_or_default(),
                state.topic_name(key),
            ),
            _ => (String::new(), String::new()),
        };
        links::message_link(
            &realm,
            key,
            state.users.self_id,
            message_id,
            &channel,
            &topic,
        )
    }

    /// 回复引用的 Markdown。
    pub async fn quote_message(&self, message_id: i64) -> Result<String> {
        let raw = self.raw_content(message_id).await?;
        let message = self
            .message(message_id)
            .ok_or_else(|| SessionError::invalid("消息不存在"))?;
        let link = self.message_link(&message.key, message_id);
        Ok(crate::compose::quote_markdown(
            &message.sender_name,
            message.sender_id,
            &link,
            &raw,
        ))
    }

    pub fn message_actions(&self, message_id: i64) -> MessageActions {
        let state = self.state.read();
        let Some(m) = state.messages.get(message_id) else {
            return MessageActions::default();
        };
        actions_of(&state, m, chrono::Utc::now().timestamp())
    }

    /// 这个会话中是否可以 @全体成员。人少或私聊总是可以。
    pub fn is_wildcard_allowed(&self, key: &ConversationKey) -> bool {
        let state = self.state.read();
        let ConversationKey::Topic(t) = key else {
            return true;
        };
        let count = state
            .channels
            .get(t.stream_id)
            .map_or(0, |c| c.subscriber_count.max(0) as usize);
        count <= WILDCARD_FREE_LIMIT || state.permissions.can_mention_everyone()
    }

    /// 聊天标题：话题带订阅人数，群聊带总人数。
    pub fn chat_title(&self, key: &ConversationKey) -> String {
        let state = self.state.read();
        let title = state.display_title(key, false);
        match key {
            ConversationKey::Topic(t) => match state.channels.get(t.stream_id) {
                Some(c) if c.subscriber_count > 0 => format!("{title} ({})", c.subscriber_count),
                _ => title,
            },
            ConversationKey::Dm(d) if d.is_group_dm() => {
                format!("{title} ({})", d.other_user_ids().len() + 1)
            }
            _ => title,
        }
    }

    /// 删除一条消息。统一入口见 `Session::delete`。
    pub(crate) async fn delete_message(&self, message_id: i64) -> Result<()> {
        self.client.delete_message(message_id).await?;
        Ok(())
    }
}

pub(crate) fn actions_of(state: &State, m: &ChatMessage, now: i64) -> MessageActions {
    if m.id <= 0 || m.is_deleted() {
        return MessageActions::default();
    }
    let self_id = state.users.self_id;
    let has_widget = !m.submessages.is_empty();
    let channel = m.stream_id.and_then(|id| state.channels.get(id));
    let my_bot = state
        .users
        .get(m.sender_id)
        .is_some_and(|u| u.is_bot && u.bot_owner_id == Some(self_id));
    MessageActions {
        can_edit: !has_widget
            && state
                .permissions
                .can_edit_message(m.sender_id, m.timestamp, now),
        can_delete: state.permissions.can_delete_message(
            channel.map(|c| &c.permissions),
            channel.is_some_and(|c| c.is_archived),
            m.sender_id == self_id || my_bot,
            m.timestamp,
            now,
        ),
        can_show_edit_history: m.last_edit_timestamp.is_some()
            && state.realm.edit_history_visibility_policy != "none",
        can_show_read_receipts: state.realm.enable_read_receipts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::state;

    #[test]
    fn actions_follow_ownership_and_widgets() {
        let s = state();
        let key = ConversationKey::topic(10, "t");
        let mut own = ChatMessage::new(5, key.clone(), 1, 100);
        own.stream_id = Some(10);
        let actions = actions_of(&s, &own, 100);
        assert!(actions.can_edit);
        own.submessages
            .push(zulip_core::model::Submessage::default());
        assert!(!actions_of(&s, &own, 100).can_edit);

        let other = ChatMessage::new(6, key.clone(), 2, 100);
        assert!(!actions_of(&s, &other, 100).can_edit);

        let pending = ChatMessage::new(-1, key.clone(), 1, 100);
        assert_eq!(actions_of(&s, &pending, 100), MessageActions::default());

        let mut edited = ChatMessage::new(7, key, 2, 100);
        edited.last_edit_timestamp = Some(101);
        assert!(actions_of(&s, &edited, 100).can_show_edit_history);
    }
}
