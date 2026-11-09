//! 删除、重命名的统一入口，以及话题和频道的操作。

use std::sync::Arc;

use serde_json::Value;
use zulip_api::{ApiError, Form, MessageQuery, MessageUpdate, SubscriptionPropertyChange};
use zulip_core::ConversationKey;
use zulip_core::model::{Channel, PinItem, PinScope, RemarkTarget};
use zulip_core::push::TopicVisibility;

use crate::error::{Result, SessionError};
use crate::event::SessionEvent;
use crate::ops::account_data::form_of;
use crate::session::Session;
use crate::topic::{move_targets, strip_resolved, toggle_resolved_name};

/// 可以删除的对象。全部通过 [`Session::delete`]，结果都是 `Result<()>`，界面走同一条
/// 确认 → 执行 → 失败提示 → 刷新（事件）的流程。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeleteTarget {
    Message(i64),
    /// 删除话题及其全部消息。
    Topic(ConversationKey),
    /// 从消息列表移除，不影响服务器上的消息。
    ConversationEntry(ConversationKey),
    Draft(ConversationKey),
    Attachment(i64),
    ScheduledMessage(i64),
    Reminder(i64),
    /// 清除一条已送达的提醒（只在本机）。
    DeliveredReminder(i64),
    Snippet(i64),
    FriendGroup(i64),
    /// 清除下载记录（不删除文件）。
    DownloadRecord(String),
}

/// 可以重命名的对象。全部通过 [`Session::rename`]。备注类传空白表示清除。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameTarget {
    Topic(ConversationKey),
    Channel(i64),
    FriendGroup(i64),
    Remark(RemarkTarget),
}

/// 话题移动的目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicDestination {
    pub stream_id: i64,
    pub topic: String,
}

fn invalid_empty() -> SessionError {
    SessionError::invalid("名称不能为空")
}

/// 移动时限错误 → 明确的结果：全部超限时为提示文字，部分超限时可以确认后只移动允许的部分。
fn map_move_error(error: ApiError) -> SessionError {
    if error.code() != Some("MOVE_MESSAGES_TIME_LIMIT_EXCEEDED") {
        return error.into();
    }
    let body = match &error {
        ApiError::Api { body, .. } => body.as_deref().cloned(),
        _ => None,
    }
    .unwrap_or(Value::Null);
    let number = |name: &str| body.get(name).and_then(Value::as_i64);
    match (
        number("total_messages_allowed_to_move").unwrap_or(0),
        number("first_message_id_allowed_to_move"),
    ) {
        (allowed, Some(first_allowed_id)) if allowed > 0 => SessionError::TopicMoveLimited {
            allowed,
            total: number("total_messages_in_topic").unwrap_or(allowed),
            first_allowed_id,
        },
        _ => SessionError::invalid("所有消息都已超出可移动的时间限制"),
    }
}

impl Session {
    pub async fn delete(self: &Arc<Self>, target: DeleteTarget) -> Result<()> {
        match target {
            DeleteTarget::Message(id) => self.delete_message(id).await,
            DeleteTarget::Topic(key) => self.delete_topic(&key).await,
            DeleteTarget::ConversationEntry(key) => {
                let changed = self.state.write().conversations.remove_from_list(&key);
                if changed {
                    self.emit(SessionEvent::ConversationsChanged);
                }
                Ok(())
            }
            DeleteTarget::Draft(key) => self.save_draft(&key, "").await,
            DeleteTarget::Attachment(id) => {
                self.client.delete_attachment(id).await?;
                Ok(())
            }
            DeleteTarget::ScheduledMessage(id) => {
                self.client.delete_scheduled_message(id).await?;
                Ok(())
            }
            DeleteTarget::Reminder(id) => {
                self.client.delete_reminder(id).await?;
                Ok(())
            }
            DeleteTarget::DeliveredReminder(id) => {
                self.state.write().notices.clear_reminder(id);
                self.emit(SessionEvent::SystemNoticesChanged);
                Ok(())
            }
            DeleteTarget::Snippet(id) => {
                self.client.delete_saved_snippet(id).await?;
                Ok(())
            }
            DeleteTarget::FriendGroup(id) => {
                self.state.write().contacts.delete_group(id);
                self.emit(SessionEvent::ContactsChanged);
                Ok(())
            }
            DeleteTarget::DownloadRecord(path) => {
                self.forget_download(&path);
                Ok(())
            }
        }
    }

    pub async fn rename(&self, target: RenameTarget, name: &str) -> Result<()> {
        match target {
            RenameTarget::Topic(key) => {
                let name = name.trim();
                if name.is_empty() {
                    return Err(invalid_empty());
                }
                let ConversationKey::Topic(t) = &key else {
                    return Err(SessionError::invalid("只有话题可以改名"));
                };
                let destination = TopicDestination {
                    stream_id: t.stream_id,
                    topic: name.to_owned(),
                };
                self.move_topic(&key, &destination).await
            }
            RenameTarget::Channel(id) => {
                let name = name.trim();
                if name.is_empty() {
                    return Err(invalid_empty());
                }
                self.client
                    .update_channel(id, &Form::new().add("new_name", name))
                    .await?;
                Ok(())
            }
            RenameTarget::FriendGroup(id) => {
                let name = name.trim();
                if name.is_empty() {
                    return Err(invalid_empty());
                }
                self.state.write().contacts.rename_group(id, name);
                self.emit(SessionEvent::ContactsChanged);
                Ok(())
            }
            RenameTarget::Remark(target) => {
                if self.state.write().remarks.set(&target, Some(name)) {
                    match target.canonical() {
                        RemarkTarget::User(id) => {
                            self.emit(SessionEvent::UserChanged(id));
                            self.emit(SessionEvent::ContactsChanged);
                        }
                        RemarkTarget::Channel(id) => {
                            self.emit(SessionEvent::ChannelChanged(id));
                            self.emit(SessionEvent::ContactsChanged);
                        }
                        RemarkTarget::Conversation(key) => {
                            self.emit(SessionEvent::ConversationChanged(key));
                        }
                    }
                }
                Ok(())
            }
        }
    }

    async fn latest_id_in(&self, key: &ConversationKey) -> Result<i64> {
        let narrow = self.narrow_of(key);
        let result = self
            .client
            .get_messages(&MessageQuery::new(narrow, "newest", 1, 0))
            .await?;
        result
            .messages
            .last()
            .map(|m| m.id)
            .ok_or_else(|| SessionError::invalid("话题中没有消息"))
    }

    /// 移动或改名整个话题。部分消息超出时限时返回 [`SessionError::TopicMoveLimited`]，
    /// 确认后调用 [`move_topic_from`](Self::move_topic_from)。
    pub async fn move_topic(&self, key: &ConversationKey, to: &TopicDestination) -> Result<()> {
        let id = self.latest_id_in(key).await?;
        self.update_topic(key, id, "change_all", to).await
    }

    /// 只移动 `first_id` 及之后的消息。
    pub async fn move_topic_from(
        &self,
        key: &ConversationKey,
        first_id: i64,
        to: &TopicDestination,
    ) -> Result<()> {
        self.update_topic(key, first_id, "change_later", to).await
    }

    async fn update_topic(
        &self,
        key: &ConversationKey,
        id: i64,
        mode: &str,
        to: &TopicDestination,
    ) -> Result<()> {
        let ConversationKey::Topic(t) = key else {
            return Err(SessionError::invalid("只有话题可以移动"));
        };
        let update = MessageUpdate {
            topic: Some(to.topic.clone()),
            stream_id: (to.stream_id != t.stream_id).then_some(to.stream_id),
            propagate_mode: Some(mode.to_owned()),
            content: None,
        };
        self.client
            .update_message(id, &update)
            .await
            .map_err(map_move_error)?;
        Ok(())
    }

    /// 切换话题的已解决状态。
    pub async fn toggle_topic_resolved(&self, key: &ConversationKey) -> Result<()> {
        let ConversationKey::Topic(t) = key else {
            return Err(SessionError::invalid("只有话题可以标记解决"));
        };
        let name = self.state.read().topic_name(key);
        if strip_resolved(&name).is_empty() {
            return Err(SessionError::invalid("这个话题不能标记解决"));
        }
        let destination = TopicDestination {
            stream_id: t.stream_id,
            topic: toggle_resolved_name(&name),
        };
        self.move_topic(key, &destination).await
    }

    async fn delete_topic(self: &Arc<Self>, key: &ConversationKey) -> Result<()> {
        let ConversationKey::Topic(t) = key else {
            return Err(SessionError::invalid("只有话题可以删除"));
        };
        let name = self.state.read().topic_name(key);
        // 消息很多时服务器分批删除，返回未完成时继续
        while !self.client.delete_topic(t.stream_id, &name).await? {}
        {
            let mut state = self.state.write();
            let item = PinItem::Conversation(key.clone());
            state.pins.set(PinScope::Messages, &item, false, 0);
            state.pins.set(PinScope::Contacts, &item, false, 0);
            state.conversations.remove_from_list(key);
        }
        self.emit(SessionEvent::ContactsChanged);
        self.emit(SessionEvent::ConversationsChanged);
        Ok(())
    }

    pub fn topic_move_targets(&self, from_stream: i64) -> Vec<Channel> {
        let state = self.state.read();
        move_targets(
            &state.channels.subscribed(),
            from_stream,
            &state.permissions,
            &state.realm,
        )
    }

    pub async fn set_topic_visibility(
        &self,
        key: &ConversationKey,
        visibility: TopicVisibility,
    ) -> Result<()> {
        let ConversationKey::Topic(t) = key else {
            return Err(SessionError::invalid("只有话题可以设置关注"));
        };
        let name = self.state.read().topic_name(key);
        self.client
            .set_topic_visibility(t.stream_id, &name, visibility as i32)
            .await?;
        Ok(())
    }

    /// 加载频道的话题列表。
    pub async fn load_topics(&self, stream_id: i64) -> Result<()> {
        let topics = self.client.get_topics(stream_id).await?;
        self.state.write().channels.set_topics(stream_id, &topics);
        self.emit(SessionEvent::TopicsChanged(stream_id));
        Ok(())
    }

    pub async fn load_subscribers(&self, stream_id: i64) -> Result<Vec<i64>> {
        let ids = self.client.get_subscribers(stream_id).await?;
        self.state
            .write()
            .channels
            .set_subscribers(stream_id, ids.clone());
        Ok(ids)
    }

    pub async fn subscribe_channel(&self, names: &[String]) -> Result<()> {
        self.client.subscribe(names, None).await?;
        Ok(())
    }

    pub async fn unsubscribe_channel(&self, names: &[String]) -> Result<()> {
        self.client.unsubscribe(names, None).await?;
        Ok(())
    }

    pub async fn add_subscribers(&self, channel_name: &str, user_ids: &[i64]) -> Result<()> {
        self.client
            .subscribe(&[channel_name.to_owned()], Some(user_ids))
            .await?;
        Ok(())
    }

    pub async fn remove_subscribers(&self, channel_name: &str, user_ids: &[i64]) -> Result<()> {
        self.client
            .unsubscribe(&[channel_name.to_owned()], Some(user_ids))
            .await?;
        Ok(())
    }

    pub async fn create_channel(
        &self,
        name: &str,
        description: &str,
        private: bool,
        subscribers: &[i64],
    ) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            return Err(invalid_empty());
        }
        Ok(self
            .client
            .create_channel(name, description, private, subscribers)
            .await?)
    }

    /// 修改频道设置。`args` 是接口参数名和值，权限是 `{"new": …, "old": …}`：
    /// 服务器上的值已不是 `old` 时不修改，返回明确的提示。
    pub async fn update_channel(&self, stream_id: i64, args: &[(&str, Value)]) -> Result<()> {
        self.client
            .update_channel(stream_id, &form_of(args))
            .await
            .map_err(|error| {
                if error.code() == Some("EXPECTATION_MISMATCH") {
                    SessionError::invalid("设置已被他人修改，请重新打开")
                } else {
                    error.into()
                }
            })?;
        Ok(())
    }

    /// 修改频道的个人设置（颜色、静音、通知）。
    pub async fn set_subscription_property(
        &self,
        stream_id: i64,
        property: &str,
        value: Value,
    ) -> Result<()> {
        let change = SubscriptionPropertyChange {
            stream_id,
            property: property.to_owned(),
            value,
        };
        self.client
            .update_subscription_properties(std::slice::from_ref(&change))
            .await?;
        Ok(())
    }

    pub async fn send_invites(&self, emails: &str, stream_ids: &[i64]) -> Result<()> {
        let parsed = crate::compose::parse_emails(emails);
        if parsed.is_empty() {
            return Err(SessionError::invalid("请输入邮箱"));
        }
        self.client
            .send_invites(&parsed.join(","), stream_ids)
            .await?;
        Ok(())
    }
}
