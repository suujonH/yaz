//! Sending messages: local echo, pre-send filtering, resend, scheduled send.

use std::sync::Arc;
use std::time::Duration;

use zulip_api::{ApiError, OutgoingMessage};
use zulip_core::ConversationKey;
use zulip_core::model::{ChatMessage, MessageState};
use zulip_core::text::mention;

use crate::error::{Result, SessionError};
use crate::event::SessionEvent;
use crate::hooks::OutgoingKind;
use crate::session::Session;

const ECHO_FALLBACK: Duration = Duration::from_secs(10);

fn outgoing(key: &ConversationKey, topic: &str, content: &str, self_id: i64) -> OutgoingMessage {
    match key {
        ConversationKey::Topic(t) => OutgoingMessage {
            stream_id: Some(t.stream_id),
            topic: Some(topic.to_owned()),
            user_ids: Vec::new(),
            content: content.to_owned(),
            queue_id: None,
            local_id: None,
        },
        ConversationKey::Dm(d) => OutgoingMessage {
            stream_id: None,
            topic: None,
            user_ids: if d.is_self_dm() {
                vec![self_id]
            } else {
                d.other_user_ids().to_vec()
            },
            content: content.to_owned(),
            queue_id: None,
            local_id: None,
        },
    }
}

fn wildcard_notice(error: &ApiError) -> Option<(bool, &'static str)> {
    match error.code() {
        Some("STREAM_WILDCARD_MENTION_NOT_ALLOWED") => {
            Some((false, "没有权限使用 @全体成员，已去掉后发送"))
        }
        Some("TOPIC_WILDCARD_MENTION_NOT_ALLOWED") => {
            Some((true, "没有权限使用 @话题参与者，已去掉后发送"))
        }
        _ => None,
    }
}

impl Session {
    /// Sends a message: a local echo appears immediately (`MessageAdded`, negative id) and the
    /// server's message id is returned. On failure the echo is marked failed and an error is
    /// returned; resend with [`retry_send`](Self::retry_send).
    pub async fn send_message(
        self: &Arc<Self>,
        key: &ConversationKey,
        content: &str,
    ) -> Result<i64> {
        self.send_as(key, content, OutgoingKind::Text).await
    }

    /// Unified exit for all sending; first goes through the hook's pre-send filter (when
    /// intercepted, the error content can be shown to the user directly). The rest is the same as
    /// [`send_message`](Self::send_message).
    pub async fn send_as(
        self: &Arc<Self>,
        key: &ConversationKey,
        content: &str,
        kind: OutgoingKind,
    ) -> Result<i64> {
        let filtered = match &self.hooks {
            Some(hooks) => Some(
                hooks
                    .filter_outgoing(self, key, content, kind)
                    .await
                    .map_err(SessionError::invalid)?,
            ),
            None => None,
        };
        let content = filtered.as_deref().unwrap_or(content).trim_end();
        if content.trim().is_empty() {
            return Err(SessionError::invalid("不能发送空消息"));
        }
        let (id, local_id) = {
            let mut state = self.state.write();
            let max = state.realm.max_message_length;
            if max > 0 && content.chars().count() > max as usize {
                return Err(SessionError::invalid("消息过长"));
            }
            let self_id = state.users.self_id;
            let mut message =
                ChatMessage::new(0, key.clone(), self_id, chrono::Utc::now().timestamp());
            if let Some(me) = state.users.get(self_id) {
                message.sender_name.clone_from(&me.full_name);
                message.sender_email.clone_from(&me.email);
                message.avatar_url.clone_from(&me.avatar_url);
            }
            if let ConversationKey::Topic(t) = key {
                message.stream_id = Some(t.stream_id);
                message.topic = state.topic_name(key);
            }
            state.messages.add_pending(message, content)
        };
        self.emit(SessionEvent::MessageAdded {
            key: key.clone(),
            id,
        });
        self.post_pending(&local_id).await
    }

    /// Resends a failed message.
    pub async fn retry_send(self: &Arc<Self>, local_id: &str) -> Result<i64> {
        let changed = {
            let mut state = self.state.write();
            let failed = state
                .messages
                .find_pending(local_id)
                .is_some_and(|m| matches!(m.state, MessageState::Failed(_)));
            if !failed {
                return Err(SessionError::invalid("这条消息已不能重发"));
            }
            state.messages.set_pending_failed(local_id, false)
        };
        if let Some((key, id)) = changed {
            self.emit(SessionEvent::MessageUpdated { key, id });
        }
        self.post_pending(local_id).await
    }

    /// Gives up on a message that failed to send.
    pub fn discard_failed(&self, local_id: &str) {
        let removed = {
            let mut state = self.state.write();
            let failed = state
                .messages
                .find_pending(local_id)
                .is_some_and(|m| matches!(m.state, MessageState::Failed(_)));
            if failed {
                state.messages.remove_pending(local_id)
            } else {
                None
            }
        };
        if let Some((key, id)) = removed {
            self.emit(SessionEvent::MessageRemoved { key, id });
        }
    }

    async fn post_pending(self: &Arc<Self>, local_id: &str) -> Result<i64> {
        let (key, content, mut message) = {
            let state = self.state.read();
            let Some(pending) = state.messages.find_pending(local_id) else {
                return Err(SessionError::invalid("这条消息已不能重发"));
            };
            let Some(echo) = pending.local_echo() else {
                return Err(SessionError::invalid("这条消息已不能重发"));
            };
            let message = outgoing(
                &pending.key,
                &pending.topic,
                &echo.raw_content,
                state.users.self_id,
            );
            (pending.key.clone(), echo.raw_content.clone(), message)
        };
        let queue_id = self.queue.lock().queue_id.clone();
        message.queue_id = Some(queue_id);
        message.local_id = Some(local_id.to_owned());
        let mut result = self.client.send_message(&message).await;
        if let Err(error) = &result
            && let Some((topic_only, notice)) = wildcard_notice(error)
        {
            message.content = mention::remove_wildcards(&content, topic_only);
            result = self.client.send_message(&message).await;
            if result.is_ok() {
                self.emit(SessionEvent::Toast(notice.to_owned()));
            }
        }
        match result {
            Ok(sent) => {
                self.schedule_echo_fallback(local_id.to_owned(), sent.id);
                if self.draft(&key).is_some()
                    && let Err(error) = self.save_draft(&key, "").await
                {
                    log::warn!("删除已发送的草稿失败：{error}");
                }
                Ok(sent.id)
            }
            Err(error) => {
                let changed = self
                    .state
                    .write()
                    .messages
                    .set_pending_failed(local_id, true);
                if let Some((key, id)) = changed {
                    self.emit(SessionEvent::MessageUpdated { key, id });
                }
                Err(error.into())
            }
        }
    }

    /// The event queue may become invalid after sending, losing the echo: if there is still no echo
    /// after a while, fetch it proactively.
    fn schedule_echo_fallback(self: &Arc<Self>, local_id: String, message_id: i64) {
        let this = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = this.cancel.cancelled() => return,
                () = tokio::time::sleep(ECHO_FALLBACK) => {}
            }
            if this.state.read().messages.find_pending(&local_id).is_none() {
                return;
            }
            match this.client.get_message(message_id, true).await {
                Ok(dto) => this.accept_fetched(&dto, &local_id),
                Err(error) => log::warn!("取回显消息失败：{error}"),
            }
        });
    }

    pub async fn schedule_message(
        &self,
        key: &ConversationKey,
        content: &str,
        deliver_at: i64,
    ) -> Result<i64> {
        let message = {
            let state = self.state.read();
            outgoing(key, &state.topic_name(key), content, state.users.self_id)
        };
        Ok(self
            .client
            .create_scheduled_message(&message, deliver_at)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_targets() {
        let topic = outgoing(&ConversationKey::topic(3, "t"), "T", "x", 1);
        assert_eq!(
            (topic.stream_id, topic.topic.as_deref()),
            (Some(3), Some("T"))
        );
        let own = outgoing(&ConversationKey::dm([1], 1), "", "x", 1);
        assert_eq!(own.user_ids, [1]);
        let dm = outgoing(&ConversationKey::dm([2, 3], 1), "", "x", 1);
        assert_eq!(dm.user_ids, [2, 3]);
    }
}
