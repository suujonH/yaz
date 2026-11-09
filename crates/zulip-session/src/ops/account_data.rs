//! 只读快照，以及会话列表、联系人、草稿、设置、通知等账号数据的操作。

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use zulip_api::Form;
use zulip_core::model::{
    Channel, FriendGroup, PinItem, PinScope, PresenceState, RealmInfo, RemarkTarget, Reminder,
    SavedSnippet, ScheduledMessage, TopicInfo, UserSettings, ZulipUser,
};
use zulip_core::permission::{PermissionEvaluator, UserGroup};
use zulip_core::push::{DndLevel, TopicVisibility};
use zulip_core::{ConversationFolder, ConversationKey};
use zulip_store::ContactRepo;

use crate::error::{Result, SessionError};
use crate::event::SessionEvent;
use crate::hooks::HookEvent;
use crate::pages::{self, ChannelSection, ConversationRow, FriendSection, PinnedTopicRow};
use crate::router::apply_setting;
use crate::session::Session;
use crate::state::ScrollPosition;
use crate::state::misc::{Draft, SystemNotice, draft_key, draft_to_dto};

fn now_seconds() -> i64 {
    chrono::Utc::now().timestamp()
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 接口参数：字符串按原文发送，其他值按 JSON 发送。
pub(crate) fn form_of(args: &[(&str, Value)]) -> Form {
    args.iter()
        .fold(Form::new(), |form, (name, value)| match value {
            Value::String(text) => form.add(name, text.as_str()),
            other => form.add_json(name, other),
        })
}

impl Session {
    pub fn self_id(&self) -> i64 {
        self.state.read().users.self_id
    }

    pub fn realm(&self) -> RealmInfo {
        self.state.read().realm.clone()
    }

    pub fn permissions(&self) -> Arc<PermissionEvaluator> {
        self.state.read().permissions.clone()
    }

    pub fn settings(&self) -> UserSettings {
        self.state.read().settings.clone()
    }

    /// 更换自己的头像（PNG 字节，来自 `AvatarImage::render_png`）。
    pub async fn upload_avatar(&self, png: Vec<u8>) -> Result<()> {
        self.client
            .upload_avatar(png, "avatar.png", "image/png")
            .await?;
        Ok(())
    }

    /// 时间是否用 24 小时制。
    pub fn use_24h(&self) -> bool {
        self.state.read().settings.twenty_four_hour_time
    }

    pub fn alert_words(&self) -> Vec<String> {
        self.state.read().alert_words.clone()
    }

    pub fn user(&self, id: i64) -> Option<ZulipUser> {
        self.state.read().users.get(id).cloned()
    }

    pub fn users(&self) -> Vec<ZulipUser> {
        self.state.read().users.all().cloned().collect()
    }

    /// 备注 → 昵称 → “未知用户”。
    pub fn display_name(&self, id: i64) -> String {
        self.state.read().display_name(id)
    }

    pub fn name_with_original(&self, id: i64) -> String {
        self.state.read().name_with_original(id)
    }

    /// 本地备注。写入用 [`rename`](Self::rename)。
    pub fn remark(&self, target: &RemarkTarget) -> Option<String> {
        self.state.read().remarks.get(target).map(str::to_owned)
    }

    pub fn presence(&self, id: i64) -> PresenceState {
        self.state.read().users.presence(id, now_seconds())
    }

    pub fn status_text(&self, id: i64) -> Option<String> {
        self.state.read().users.status_text(id).map(str::to_owned)
    }

    /// 频道权限设置可以选择的系统组：组 id 和中文名。
    pub fn channel_permission_choices(&self, setting: &str) -> Vec<(i64, &'static str)> {
        self.state.read().channel_permission_choices(setting)
    }

    pub fn user_groups(&self) -> Vec<UserGroup> {
        let mut groups: Vec<UserGroup> = self.state.read().users.groups.values().cloned().collect();
        groups.sort_by_key(|g| g.id);
        groups
    }

    pub fn is_user_muted(&self, id: i64) -> bool {
        self.state.read().users.is_muted(id)
    }

    pub fn muted_users(&self) -> Vec<i64> {
        self.state.read().users.muted_ids()
    }

    pub fn notification_bot_id(&self) -> Option<i64> {
        self.state.read().users.notification_bot_id
    }

    pub fn channel(&self, id: i64) -> Option<Channel> {
        self.state.read().channels.get(id).cloned()
    }

    pub fn channels(&self) -> Vec<Channel> {
        self.state.read().channels.all().cloned().collect()
    }

    pub fn subscribed_channels(&self) -> Vec<Channel> {
        self.state.read().channels.subscribed()
    }

    pub fn channel_display_name(&self, id: i64) -> String {
        self.state.read().channel_display_name(id)
    }

    /// 已加载的话题；`None` 时用 `load_topics` 加载。
    pub fn topics(&self, stream_id: i64) -> Option<Vec<TopicInfo>> {
        self.state
            .read()
            .channels
            .topics(stream_id)
            .map(<[TopicInfo]>::to_vec)
    }

    pub fn topic_visibility(&self, stream_id: i64, topic: &str) -> TopicVisibility {
        self.state.read().channels.visibility(stream_id, topic)
    }

    pub fn title(&self, key: &ConversationKey) -> String {
        self.state.read().title(key)
    }

    pub fn display_title(&self, key: &ConversationKey) -> String {
        self.state.read().display_title(key, false)
    }

    pub fn topic_name(&self, key: &ConversationKey) -> String {
        self.state.read().topic_name(key)
    }

    pub fn topic_title_with_original(&self, key: &ConversationKey) -> String {
        self.state.read().topic_title_with_original(key)
    }

    pub fn unread_count(&self, key: &ConversationKey) -> usize {
        self.state.read().unread.count(key)
    }

    pub fn total_unread_for_badge(&self) -> usize {
        self.state.read().total_unread_for_badge()
    }

    /// 免打扰会话中还有 @ 或提醒词未读的会话。
    pub fn unread_muted_alert_keys(&self) -> Vec<ConversationKey> {
        self.state.read().unread_muted_alert_keys()
    }

    pub fn message_rows(&self) -> Vec<ConversationRow> {
        pages::message_rows(&self.state.read())
    }

    pub fn folder_rows(&self, folder: ConversationFolder) -> Vec<ConversationRow> {
        pages::folder_rows(&self.state.read(), folder)
    }

    pub fn friend_sections(&self) -> Vec<FriendSection> {
        pages::friend_sections(&self.state.read(), now_seconds())
    }

    pub fn channel_sections(&self) -> Vec<ChannelSection> {
        pages::channel_sections(&self.state.read())
    }

    pub fn pinned_topic_rows(&self) -> Vec<PinnedTopicRow> {
        pages::pinned_topic_rows(&self.state.read())
    }

    /// 用户所在的好友分组。
    pub fn friend_group_of(&self, user_id: i64) -> Option<i64> {
        self.state.read().contacts.group_of(user_id)
    }

    pub fn friend_groups(&self) -> Vec<FriendGroup> {
        self.state.read().contacts.groups().to_vec()
    }

    pub fn scroll_position(&self, key: &ConversationKey) -> Option<ScrollPosition> {
        self.state.read().view.scroll(key)
    }

    pub fn is_conversation_open(&self, key: &ConversationKey) -> bool {
        self.state.read().messages.is_open(key)
    }

    pub fn conversation_level(&self, key: &ConversationKey) -> DndLevel {
        self.state.read().conversations.level(key)
    }

    pub fn set_conversation_level(&self, key: &ConversationKey, level: DndLevel) {
        if self.state.write().conversations.set_level(key, level) {
            self.emit(SessionEvent::ConversationChanged(key.clone()));
            self.emit(SessionEvent::ConversationsChanged);
        }
    }

    /// 置顶：消息页的会话和会话夹，联系人页的好友和话题。
    pub fn is_pinned(&self, scope: PinScope, item: &PinItem) -> bool {
        self.state.read().pins.get(scope, item).is_some()
    }

    pub fn set_pinned(&self, scope: PinScope, item: &PinItem, pinned: bool) {
        if !self
            .state
            .write()
            .pins
            .set(scope, item, pinned, now_millis())
        {
            return;
        }
        match scope {
            PinScope::Messages => {
                if let PinItem::Conversation(key) = item {
                    self.emit(SessionEvent::ConversationChanged(key.clone()));
                }
                self.emit(SessionEvent::ConversationsChanged);
            }
            PinScope::Contacts => self.emit(SessionEvent::ContactsChanged),
        }
    }

    /// 消息列表取决于本机设置（收起已解决话题、话题标题模板）：这些设置变化后让界面重读。
    pub fn refresh_conversations(&self) {
        self.emit(SessionEvent::ConversationsChanged);
    }

    pub async fn add_friend_group(&self, name: &str) -> Result<FriendGroup> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(SessionError::invalid("名称不能为空"));
        }
        let account_id = self.account_id;
        let group = self
            .db
            .call(move |db| ContactRepo::new(db, account_id).add_group(&name))
            .await?;
        self.state.write().contacts.push_group(group.clone());
        self.emit(SessionEvent::ContactsChanged);
        Ok(group)
    }

    pub fn move_friends(&self, user_ids: &[i64], group: Option<i64>) -> Result<()> {
        self.state.write().contacts.move_users(user_ids, group)?;
        self.emit(SessionEvent::ContactsChanged);
        Ok(())
    }

    pub fn draft(&self, key: &ConversationKey) -> Option<Draft> {
        self.state.read().drafts.get(key).cloned()
    }

    pub fn drafts(&self) -> Vec<(ConversationKey, Draft)> {
        let state = self.state.read();
        state
            .drafts
            .iter()
            .map(|(k, d)| (k.clone(), d.clone()))
            .collect()
    }

    /// 保存草稿，空内容删除。显示值立即更新；同步模式下同一会话的保存串行执行，只发送最新的值。
    pub async fn save_draft(&self, key: &ConversationKey, content: &str) -> Result<()> {
        let at = now_seconds();
        let (sync, changed) = {
            let mut state = self.state.write();
            let changed = state.drafts.get(key).map_or("", |d| d.content.as_str()) != content;
            state.drafts.set_visible(key, content, at);
            if !state.settings.enable_drafts_synchronization {
                let conv = key.to_string();
                if content.is_empty() {
                    state.drafts.repo.delete(&conv);
                } else {
                    state.drafts.repo.set(&conv, content, at);
                }
                (false, changed)
            } else if state.drafts.saving.contains(key) {
                state.drafts.queued.insert(key.clone(), content.to_owned());
                drop(state);
                self.emit(SessionEvent::DraftChanged(key.clone()));
                return Ok(());
            } else {
                state.drafts.saving.insert(key.clone());
                (true, changed)
            }
        };
        self.emit(SessionEvent::DraftChanged(key.clone()));
        if !sync {
            if changed {
                self.notify_hooks(HookEvent::DraftChanged(key.clone()));
            }
            return Ok(());
        }
        let mut next = content.to_owned();
        loop {
            let result = self.push_draft(key, &next).await;
            let mut state = self.state.write();
            if result.is_ok()
                && let Some(queued) = state.drafts.queued.remove(key)
            {
                next = queued;
                continue;
            }
            state.drafts.queued.remove(key);
            state.drafts.saving.remove(key);
            return result;
        }
    }

    async fn push_draft(&self, key: &ConversationKey, content: &str) -> Result<()> {
        let (server_id, dto) = {
            let state = self.state.read();
            let id = state.drafts.get(key).and_then(|d| d.server_id);
            let topic = state.topic_name(key);
            (
                id,
                draft_to_dto(key, &topic, content, state.users.self_id, id, now_seconds()),
            )
        };
        if content.is_empty() {
            let existing = match server_id {
                Some(id) => Some(id),
                None => self.find_server_draft(key).await?,
            };
            if let Some(id) = existing {
                self.client.delete_draft(id).await?;
            }
            return Ok(());
        }
        match server_id {
            Some(id) => self.client.edit_draft(id, &dto).await?,
            None => {
                let id = self.client.create_draft(&dto).await?;
                self.state.write().drafts.set_server_id(key, Some(id));
            }
        }
        Ok(())
    }

    // 本地删除后显示值已经没有 id：保存竞争时到服务器上查
    async fn find_server_draft(&self, key: &ConversationKey) -> Result<Option<i64>> {
        let self_id = self.self_id();
        let drafts = self.client.get_drafts().await?;
        Ok(drafts
            .iter()
            .find(|d| draft_key(d, self_id).as_ref() == Some(key))
            .and_then(|d| d.id))
    }

    /// 切换草稿同步：关闭时把草稿留在本机；开启时上传本机草稿并以服务器为准。
    pub(crate) async fn switch_draft_sync(&self, enabled: bool) -> Result<()> {
        if !enabled {
            let state = self.state.read();
            for (key, draft) in state.drafts.iter() {
                state
                    .drafts
                    .repo
                    .set(&key.to_string(), &draft.content, draft.updated_at);
            }
            return Ok(());
        }
        let local: Vec<(ConversationKey, Draft)> = {
            let state = self.state.read();
            state
                .drafts
                .iter()
                .map(|(k, d)| (k.clone(), d.clone()))
                .collect()
        };
        for (key, draft) in &local {
            let dto = {
                let state = self.state.read();
                draft_to_dto(
                    key,
                    &state.topic_name(key),
                    &draft.content,
                    state.users.self_id,
                    None,
                    now_seconds(),
                )
            };
            self.client.create_draft(&dto).await?;
        }
        let server = self.client.get_drafts().await?;
        let keys: Vec<ConversationKey> = {
            let mut state = self.state.write();
            let self_id = state.users.self_id;
            let mut entries: HashMap<ConversationKey, Draft> = HashMap::new();
            for dto in &server {
                if let Some(key) = draft_key(dto, self_id) {
                    entries.insert(
                        key,
                        Draft {
                            content: dto.content.clone(),
                            server_id: dto.id,
                            updated_at: dto.timestamp.unwrap_or_default(),
                        },
                    );
                }
            }
            let keys = entries.keys().cloned().collect();
            state.drafts.replace_all(entries);
            state.drafts.repo.delete_all();
            keys
        };
        for key in keys {
            self.emit(SessionEvent::DraftChanged(key));
        }
        Ok(())
    }

    /// 修改 Zulip 设置：先改本地，失败时回滚。
    pub async fn set_zulip_setting(self: &Arc<Self>, name: &str, value: Value) -> Result<()> {
        let old = {
            let mut state = self.state.write();
            let old = serde_json::to_value(&state.settings)
                .ok()
                .and_then(|v| v.get(name).cloned());
            let Some(old) = old else {
                return Err(SessionError::invalid(format!("未知的设置：{name}")));
            };
            apply_setting(&mut state, name, &value);
            old
        };
        self.emit(SessionEvent::SettingsChanged(name.to_owned()));
        let form = Form::new().add_json(name, &value);
        if let Err(error) = self.client.update_settings(&form).await {
            apply_setting(&mut self.state.write(), name, &old);
            self.emit(SessionEvent::SettingsChanged(name.to_owned()));
            return Err(error.into());
        }
        if name == "enable_drafts_synchronization"
            && let Some(enabled) = value.as_bool()
        {
            self.switch_draft_sync(enabled).await?;
        }
        Ok(())
    }

    pub async fn set_status_text(&self, text: &str) -> Result<()> {
        self.client.set_status(text).await?;
        Ok(())
    }

    pub async fn add_alert_words(&self, words: &[String]) -> Result<()> {
        self.client.add_alert_words(words).await?;
        Ok(())
    }

    pub async fn remove_alert_words(&self, words: &[String]) -> Result<()> {
        self.client.remove_alert_words(words).await?;
        Ok(())
    }

    pub async fn set_user_muted(&self, user_id: i64, muted: bool) -> Result<()> {
        if muted {
            self.client.mute_user(user_id).await?;
        } else {
            self.client.unmute_user(user_id).await?;
        }
        Ok(())
    }

    pub async fn update_user(
        &self,
        user_id: i64,
        name: Option<&str>,
        role: Option<i32>,
    ) -> Result<()> {
        self.client.update_user(user_id, name, role).await?;
        Ok(())
    }

    /// 修改组织设置。`args` 是接口参数名和值。
    pub async fn update_realm(&self, args: &[(&str, Value)]) -> Result<()> {
        self.client.update_realm(&form_of(args)).await?;
        Ok(())
    }

    pub fn system_notices(&self) -> Vec<SystemNotice> {
        self.state.read().notices.notices()
    }

    pub fn delivered_reminders(&self) -> Vec<SystemNotice> {
        self.state.read().notices.delivered_reminders()
    }

    pub fn unread_notice_count(&self) -> usize {
        self.state.read().notices.unread_notice_count()
    }

    pub fn unread_reminder_count(&self) -> usize {
        self.state.read().notices.unread_reminder_count()
    }

    /// 系统通知全部标记已读。
    pub async fn mark_notices_read(self: &Arc<Self>) -> Result<()> {
        let ids = {
            let state = self.state.read();
            state.notices.unread_ids()
        };
        self.state.write().notices.set_read(&ids, true);
        self.emit(SessionEvent::SystemNoticesChanged);
        self.mark_read(&ids).await
    }

    /// 清空系统通知（只在本机）。
    pub fn clear_notices(&self) {
        self.state.write().notices.clear_notices();
        self.emit(SessionEvent::SystemNoticesChanged);
    }

    /// 清空已送达的提醒（只在本机）。
    pub fn clear_delivered_reminders(&self) {
        self.state.write().notices.clear_all_reminders();
        self.emit(SessionEvent::SystemNoticesChanged);
    }

    pub fn pending_reminders(&self) -> Vec<Reminder> {
        self.state.read().queued.reminders.clone()
    }

    pub fn scheduled_messages(&self) -> Vec<ScheduledMessage> {
        self.state.read().queued.scheduled.clone()
    }

    /// 这个会话的定时消息。
    pub fn scheduled_for(&self, key: &ConversationKey) -> Vec<ScheduledMessage> {
        self.state
            .read()
            .queued
            .scheduled
            .iter()
            .filter(|m| m.key == *key)
            .cloned()
            .collect()
    }

    pub fn snippets(&self) -> Vec<SavedSnippet> {
        self.state.read().queued.snippets.clone()
    }

    pub async fn create_snippet(&self, title: &str, content: &str) -> Result<i64> {
        Ok(self.client.create_saved_snippet(title, content).await?)
    }
}
