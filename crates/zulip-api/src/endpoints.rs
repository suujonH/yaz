use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};
use zulip_core::model::{Attachment, AttachmentList, MessageRevision};

use crate::client::{ZulipClient, decode, take_field};
use crate::dto::*;
use crate::error::ApiError;
use crate::form::Form;

const REGISTER_TIMEOUT: Duration = Duration::from_secs(60);
const DELETE_QUEUE_TIMEOUT: Duration = Duration::from_secs(3);

// REST 读取默认把空话题替换为 realm_empty_topic_display_name，而事件队列返回空字符串；
// 读取消息和话题时都传 allow_empty_topic_name=true，保证同一会话的键一致。

fn with_recipient(
    form: Form,
    channel_type: &str,
    stream_id: Option<i64>,
    topic: Option<&str>,
    user_ids: &[i64],
) -> Form {
    match stream_id {
        Some(id) => form
            .add("type", channel_type)
            .add("to", id)
            .add("topic", topic.unwrap_or_default()),
        None => form.add("type", "direct").add_json("to", user_ids),
    }
}

impl ZulipClient {
    // ---- 登录前 ----

    pub async fn server_settings(&self) -> Result<ServerSettingsDto, ApiError> {
        self.call(Method::GET, "api/v1/server_settings", None).await
    }

    pub async fn fetch_api_key(
        &self,
        username: &str,
        password: &str,
    ) -> Result<ApiKeyResultDto, ApiError> {
        let form = Form::new()
            .add("username", username)
            .add("password", password);
        self.call(Method::POST, "api/v1/fetch_api_key", Some(&form))
            .await
    }

    // ---- 事件队列 ----

    /// 注册事件队列并取得初始状态。不传 `event_types`，接收全部事件。
    pub async fn register(&self) -> Result<RegisterResult, ApiError> {
        let form = Form::new()
            .add("apply_markdown", true)
            .add("client_gravatar", false)
            .add("slim_presence", true)
            .add("include_subscribers", false)
            .add_json(
                "client_capabilities",
                &json!({
                    "notification_settings_null": true,
                    "bulk_message_deletion": true,
                    "user_avatar_url_field_optional": false,
                    "stream_typing_notifications": true,
                    "user_settings_object": true,
                    "linkifier_url_template": true,
                    "include_deactivated_groups": false,
                    "archived_channels": true,
                    "empty_topic_name": true,
                    "simplified_presence_events": true,
                }),
            );
        let root = self
            .request(
                Method::POST,
                "api/v1/register",
                Some(&form),
                Some(REGISTER_TIMEOUT),
            )
            .await?;
        decode(root)
    }

    /// 长轮询取事件，返回原始事件对象。`timeout` 应大于服务器的长轮询超时。
    pub async fn get_events(
        &self,
        queue_id: &str,
        last_event_id: i64,
        timeout: Duration,
    ) -> Result<Vec<Value>, ApiError> {
        let form = Form::new()
            .add("queue_id", queue_id)
            .add("last_event_id", last_event_id);
        let root = self
            .request(Method::GET, "api/v1/events", Some(&form), Some(timeout))
            .await?;
        Ok(match root.get("events") {
            Some(Value::Array(events)) => events.clone(),
            _ => Vec::new(),
        })
    }

    pub async fn delete_queue(&self, queue_id: &str) -> Result<(), ApiError> {
        let form = Form::new().add("queue_id", queue_id);
        self.request(
            Method::DELETE,
            "api/v1/events",
            Some(&form),
            Some(DELETE_QUEUE_TIMEOUT),
        )
        .await?;
        Ok(())
    }

    // ---- 消息 ----

    pub async fn get_messages(&self, query: &MessageQuery) -> Result<GetMessagesResult, ApiError> {
        let mut form = Form::new()
            .add("anchor", &query.anchor)
            .add("num_before", query.num_before)
            .add("num_after", query.num_after)
            .add_json("narrow", &query.narrow)
            .add("include_anchor", query.include_anchor)
            .add("apply_markdown", query.apply_markdown)
            .add("client_gravatar", false)
            .add("allow_empty_topic_name", true);
        if let Some(date) = &query.anchor_date {
            form = form.add("anchor_date", date);
        }
        self.call(Method::GET, "api/v1/messages", Some(&form)).await
    }

    /// 取单条消息。`apply_markdown` 为 false 时 `content` 是 Markdown 原文。
    pub async fn get_message(&self, id: i64, apply_markdown: bool) -> Result<MessageDto, ApiError> {
        let form = Form::new()
            .add("apply_markdown", apply_markdown)
            .add("allow_empty_topic_name", true);
        self.call_field(
            Method::GET,
            &format!("api/v1/messages/{id}"),
            Some(&form),
            "message",
        )
        .await
    }

    /// 按服务器的规则把 Markdown 渲染为消息的 HTML（不发送）。
    pub async fn render_message(&self, content: &str) -> Result<String, ApiError> {
        let form = Form::new().add("content", content);
        self.call_field(
            Method::POST,
            "api/v1/messages/render",
            Some(&form),
            "rendered",
        )
        .await
    }

    pub async fn send_message(
        &self,
        message: &OutgoingMessage,
    ) -> Result<SendMessageResult, ApiError> {
        let mut form = with_recipient(
            Form::new(),
            "channel",
            message.stream_id,
            message.topic.as_deref(),
            &message.user_ids,
        )
        .add("content", &message.content)
        .add("read_by_sender", true);
        if let (Some(queue_id), Some(local_id)) = (&message.queue_id, &message.local_id) {
            form = form.add("queue_id", queue_id).add("local_id", local_id);
        }
        self.call(Method::POST, "api/v1/messages", Some(&form))
            .await
    }

    /// 修改消息标志。
    pub async fn update_flags(
        &self,
        ids: &[i64],
        op: FlagOp,
        flag: MessageFlag,
    ) -> Result<(), ApiError> {
        let form = Form::new()
            .add_json("messages", ids)
            .add("op", op.as_str())
            .add("flag", flag.as_str());
        self.call::<Value>(Method::POST, "api/v1/messages/flags", Some(&form))
            .await?;
        Ok(())
    }

    pub async fn update_flags_for_narrow(
        &self,
        update: &FlagsNarrowUpdate,
    ) -> Result<FlagsNarrowResult, ApiError> {
        let form = Form::new()
            .add_json("narrow", &update.narrow)
            .add("anchor", &update.anchor)
            .add("include_anchor", update.include_anchor)
            .add("num_before", update.num_before)
            .add("num_after", update.num_after)
            .add("op", update.op.as_str())
            .add("flag", update.flag.as_str());
        self.call(Method::POST, "api/v1/messages/flags/narrow", Some(&form))
            .await
    }

    /// 添加表情回应。`reaction_type` 为 `unicode_emoji` / `realm_emoji` / `zulip_extra_emoji`。
    pub async fn add_reaction(
        &self,
        message_id: i64,
        emoji_name: &str,
        emoji_code: &str,
        reaction_type: &str,
    ) -> Result<(), ApiError> {
        let form = Form::new()
            .add("emoji_name", emoji_name)
            .add("emoji_code", emoji_code)
            .add("reaction_type", reaction_type);
        self.call::<Value>(
            Method::POST,
            &format!("api/v1/messages/{message_id}/reactions"),
            Some(&form),
        )
        .await?;
        Ok(())
    }

    pub async fn remove_reaction(
        &self,
        message_id: i64,
        emoji_name: &str,
        emoji_code: &str,
        reaction_type: &str,
    ) -> Result<(), ApiError> {
        let form = Form::new()
            .add("emoji_name", emoji_name)
            .add("emoji_code", emoji_code)
            .add("reaction_type", reaction_type);
        self.call::<Value>(
            Method::DELETE,
            &format!("api/v1/messages/{message_id}/reactions"),
            Some(&form),
        )
        .await?;
        Ok(())
    }

    /// 给消息添加子消息（投票、勾选任务等小部件操作）。接口没有收入 API 文档，网页端的小部件使用它；
    /// 服务器按小部件类型校验 `content`，结果通过 submessage 事件下发。
    pub async fn send_submessage(
        &self,
        message_id: i64,
        msg_type: &str,
        content: &str,
    ) -> Result<(), ApiError> {
        let form = Form::new()
            .add("message_id", message_id)
            .add("msg_type", msg_type)
            .add("content", content);
        self.call::<Value>(Method::POST, "api/v1/submessage", Some(&form))
            .await?;
        Ok(())
    }

    /// 移动消息、修改话题名或编辑内容（`PATCH /messages/{id}`）。
    /// 超出移动时限时返回 code 为 `MOVE_MESSAGES_TIME_LIMIT_EXCEEDED` 的 [`ApiError::Api`]（一条都没有移动）。
    pub async fn update_message(
        &self,
        message_id: i64,
        update: &MessageUpdate,
    ) -> Result<(), ApiError> {
        let mut form = Form::new();
        if let Some(topic) = &update.topic {
            form = form.add("topic", topic);
        }
        if let Some(stream_id) = update.stream_id {
            form = form.add("stream_id", stream_id);
        }
        if let Some(mode) = &update.propagate_mode {
            form = form.add("propagate_mode", mode);
        }
        if let Some(content) = &update.content {
            form = form.add("content", content);
        }
        self.call::<Value>(
            Method::PATCH,
            &format!("api/v1/messages/{message_id}"),
            Some(&form),
        )
        .await?;
        Ok(())
    }

    /// 删除消息（`DELETE /messages/{id}`，需要组织允许或管理员权限）。
    pub async fn delete_message(&self, message_id: i64) -> Result<(), ApiError> {
        self.call::<Value>(
            Method::DELETE,
            &format!("api/v1/messages/{message_id}"),
            None,
        )
        .await?;
        Ok(())
    }

    /// 消息的编辑历史，第一项为原始版本。
    pub async fn get_message_history(
        &self,
        message_id: i64,
    ) -> Result<Vec<MessageRevision>, ApiError> {
        let form = Form::new().add("allow_empty_topic_name", true);
        self.call_field(
            Method::GET,
            &format!("api/v1/messages/{message_id}/history"),
            Some(&form),
            "message_history",
        )
        .await
    }

    /// 已读这条消息、且允许查看已读状态的用户 id（关闭了已读回执的用户不会出现）。
    pub async fn get_read_receipts(&self, message_id: i64) -> Result<Vec<i64>, ApiError> {
        self.call_field(
            Method::GET,
            &format!("api/v1/messages/{message_id}/read_receipts"),
            None,
            "user_ids",
        )
        .await
    }

    /// 正在输入通知。`op` 为 `start` / `stop`；频道话题传 `stream_id` 与 `topic`，
    /// 私聊传 `user_ids`（收件人，不含自己）。
    pub async fn send_typing(
        &self,
        op: &str,
        stream_id: Option<i64>,
        topic: Option<&str>,
        user_ids: &[i64],
    ) -> Result<(), ApiError> {
        let form = Form::new().add("op", op);
        let form = match stream_id {
            Some(id) => form
                .add("type", "stream")
                .add("stream_id", id)
                .add("topic", topic.unwrap_or_default()),
            None => form.add("type", "direct").add_json("to", user_ids),
        };
        self.call::<Value>(Method::POST, "api/v1/typing", Some(&form))
            .await?;
        Ok(())
    }

    // ---- 频道与话题 ----

    pub async fn get_subscribers(&self, stream_id: i64) -> Result<Vec<i64>, ApiError> {
        self.call_field(
            Method::GET,
            &format!("api/v1/streams/{stream_id}/members"),
            None,
            "subscribers",
        )
        .await
    }

    /// 频道的话题列表（按最新消息 id 降序）。
    pub async fn get_topics(&self, stream_id: i64) -> Result<Vec<TopicDto>, ApiError> {
        let form = Form::new().add("allow_empty_topic_name", true);
        self.call_field(
            Method::GET,
            &format!("api/v1/users/me/{stream_id}/topics"),
            Some(&form),
            "topics",
        )
        .await
    }

    pub async fn set_topic_visibility(
        &self,
        stream_id: i64,
        topic: &str,
        policy: i32,
    ) -> Result<(), ApiError> {
        let form = Form::new()
            .add("stream_id", stream_id)
            .add("topic", topic)
            .add("visibility_policy", policy);
        self.call::<Value>(Method::POST, "api/v1/user_topics", Some(&form))
            .await?;
        Ok(())
    }

    pub async fn update_subscription_properties(
        &self,
        changes: &[SubscriptionPropertyChange],
    ) -> Result<(), ApiError> {
        let data: Vec<Value> = changes
            .iter()
            .map(|c| json!({ "stream_id": c.stream_id, "property": c.property, "value": c.value }))
            .collect();
        let form = Form::new().add_json("subscription_data", &data);
        self.call::<Value>(
            Method::POST,
            "api/v1/users/me/subscriptions/properties",
            Some(&form),
        )
        .await?;
        Ok(())
    }

    /// 删除话题中的消息（一次可能只删除一批），返回是否已全部删除。
    pub async fn delete_topic(&self, stream_id: i64, topic: &str) -> Result<bool, ApiError> {
        let form = Form::new().add("topic_name", topic);
        let root: Value = self
            .call(
                Method::POST,
                &format!("api/v1/streams/{stream_id}/delete_topic"),
                Some(&form),
            )
            .await?;
        Ok(root
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(true))
    }

    /// 订阅频道；`principals` 不为 `None` 时为这些用户订阅。
    pub async fn subscribe<S: AsRef<str>>(
        &self,
        channel_names: &[S],
        principals: Option<&[i64]>,
    ) -> Result<(), ApiError> {
        let subscriptions: Vec<Value> = channel_names
            .iter()
            .map(|n| json!({ "name": n.as_ref() }))
            .collect();
        let mut form = Form::new().add_json("subscriptions", &subscriptions);
        if let Some(principals) = principals {
            form = form.add_json("principals", principals);
        }
        self.call::<Value>(Method::POST, "api/v1/users/me/subscriptions", Some(&form))
            .await?;
        Ok(())
    }

    /// 退订频道；`principals` 不为 `None` 时为这些用户退订。
    pub async fn unsubscribe<S: AsRef<str>>(
        &self,
        channel_names: &[S],
        principals: Option<&[i64]>,
    ) -> Result<(), ApiError> {
        let names: Vec<&str> = channel_names.iter().map(AsRef::as_ref).collect();
        let mut form = Form::new().add_json("subscriptions", &names);
        if let Some(principals) = principals {
            form = form.add_json("principals", principals);
        }
        self.call::<Value>(Method::DELETE, "api/v1/users/me/subscriptions", Some(&form))
            .await?;
        Ok(())
    }

    /// 创建频道（Zulip 11 起的 `POST /channels/create`），返回频道 id。
    pub async fn create_channel(
        &self,
        name: &str,
        description: &str,
        is_private: bool,
        subscribers: &[i64],
    ) -> Result<i64, ApiError> {
        let form = Form::new()
            .add("name", name)
            .add("description", description)
            .add("invite_only", is_private)
            .add_json("subscribers", subscribers);
        self.call_field(Method::POST, "api/v1/channels/create", Some(&form), "id")
            .await
    }

    /// 修改频道（`PATCH /streams/{id}`）：`new_name`、`description`、`is_private`、
    /// `history_public_to_subscribers`，以及 `{"new": …, "old": …}` 形式的各个 `can_*_group`。
    pub async fn update_channel(&self, stream_id: i64, args: &Form) -> Result<(), ApiError> {
        self.call::<Value>(
            Method::PATCH,
            &format!("api/v1/streams/{stream_id}"),
            Some(args),
        )
        .await?;
        Ok(())
    }

    // ---- 定时消息、提醒、常用语、邀请 ----

    /// 创建定时消息（`queue_id`、`local_id` 不使用），返回定时消息 id。
    pub async fn create_scheduled_message(
        &self,
        message: &OutgoingMessage,
        deliver_at: i64,
    ) -> Result<i64, ApiError> {
        // 与立即发送一致：服务器按客户端名称猜测时不认识本客户端，送达后会成为自己的未读消息
        let form = with_recipient(
            Form::new(),
            "stream",
            message.stream_id,
            message.topic.as_deref(),
            &message.user_ids,
        )
        .add("content", &message.content)
        .add("scheduled_delivery_timestamp", deliver_at)
        .add("read_by_sender", true);
        self.call_field(
            Method::POST,
            "api/v1/scheduled_messages",
            Some(&form),
            "scheduled_message_id",
        )
        .await
    }

    pub async fn delete_scheduled_message(&self, id: i64) -> Result<(), ApiError> {
        self.call::<Value>(
            Method::DELETE,
            &format!("api/v1/scheduled_messages/{id}"),
            None,
        )
        .await?;
        Ok(())
    }

    /// 在指定时间提醒自己这条消息（服务器到时发一条私信给自己），返回提醒 id。
    pub async fn create_reminder(&self, message_id: i64, deliver_at: i64) -> Result<i64, ApiError> {
        let form = Form::new()
            .add("message_id", message_id)
            .add("scheduled_delivery_timestamp", deliver_at);
        self.call_field(Method::POST, "api/v1/reminders", Some(&form), "reminder_id")
            .await
    }

    pub async fn delete_reminder(&self, id: i64) -> Result<(), ApiError> {
        self.call::<Value>(Method::DELETE, &format!("api/v1/reminders/{id}"), None)
            .await?;
        Ok(())
    }

    /// 新建常用语，返回 id。
    pub async fn create_saved_snippet(&self, title: &str, content: &str) -> Result<i64, ApiError> {
        let form = Form::new().add("title", title).add("content", content);
        self.call_field(
            Method::POST,
            "api/v1/saved_snippets",
            Some(&form),
            "saved_snippet_id",
        )
        .await
    }

    pub async fn delete_saved_snippet(&self, id: i64) -> Result<(), ApiError> {
        self.call::<Value>(Method::DELETE, &format!("api/v1/saved_snippets/{id}"), None)
            .await?;
        Ok(())
    }

    /// 邀请新成员加入组织（角色为成员）。`emails` 用逗号或换行分隔；
    /// 有邮箱无效时服务器不发送任何邀请，返回 code 为 `INVITATION_FAILED` 的 [`ApiError::Api`]。
    pub async fn send_invites(&self, emails: &str, stream_ids: &[i64]) -> Result<(), ApiError> {
        let form = Form::new()
            .add("invitee_emails", emails)
            .add_json("stream_ids", stream_ids)
            .add("invite_as", 400)
            .add("include_realm_default_subscriptions", false);
        self.call::<Value>(Method::POST, "api/v1/invites", Some(&form))
            .await?;
        Ok(())
    }

    // ---- 用户 ----

    /// 修改用户（`PATCH /users/{id}`，管理员）：名称、角色（100 所有者 … 600 访客），`None` 表示不修改。
    pub async fn update_user(
        &self,
        user_id: i64,
        full_name: Option<&str>,
        role: Option<i32>,
    ) -> Result<(), ApiError> {
        let mut form = Form::new();
        if let Some(name) = full_name {
            form = form.add("full_name", name);
        }
        if let Some(role) = role {
            form = form.add("role", role);
        }
        self.call::<Value>(
            Method::PATCH,
            &format!("api/v1/users/{user_id}"),
            Some(&form),
        )
        .await?;
        Ok(())
    }

    /// 屏蔽（静音）用户。
    pub async fn mute_user(&self, user_id: i64) -> Result<(), ApiError> {
        self.call::<Value>(
            Method::POST,
            &format!("api/v1/users/me/muted_users/{user_id}"),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn unmute_user(&self, user_id: i64) -> Result<(), ApiError> {
        self.call::<Value>(
            Method::DELETE,
            &format!("api/v1/users/me/muted_users/{user_id}"),
            None,
        )
        .await?;
        Ok(())
    }

    // ---- 上传的文件 ----

    /// 自己上传的文件，以及已使用的存储空间。
    pub async fn get_attachments(&self) -> Result<AttachmentList, ApiError> {
        let root: Value = self.call(Method::GET, "api/v1/attachments", None).await?;
        let upload_space_used = root
            .get("upload_space_used")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let attachments: Vec<AttachmentDto> = take_field(root, "attachments")?;
        Ok(AttachmentList {
            attachments: attachments.into_iter().map(Attachment::from).collect(),
            upload_space_used,
        })
    }

    /// 从服务器删除上传的文件；引用它的消息中的链接随之失效。
    pub async fn delete_attachment(&self, id: i64) -> Result<(), ApiError> {
        self.call::<Value>(Method::DELETE, &format!("api/v1/attachments/{id}"), None)
            .await?;
        Ok(())
    }

    // ---- 草稿 ----

    pub async fn get_drafts(&self) -> Result<Vec<DraftDto>, ApiError> {
        self.call_field(Method::GET, "api/v1/drafts", None, "drafts")
            .await
    }

    /// 创建草稿，返回服务器分配的 id。
    pub async fn create_draft(&self, draft: &DraftDto) -> Result<i64, ApiError> {
        let form = Form::new().add_json("drafts", &[draft_payload(draft)]);
        let ids: Vec<i64> = self
            .call_field(Method::POST, "api/v1/drafts", Some(&form), "ids")
            .await?;
        ids.first()
            .copied()
            .ok_or_else(|| ApiError::Decode("响应中没有草稿 id".to_owned()))
    }

    pub async fn edit_draft(&self, id: i64, draft: &DraftDto) -> Result<(), ApiError> {
        let form = Form::new().add_json("draft", &draft_payload(draft));
        self.call::<Value>(Method::PATCH, &format!("api/v1/drafts/{id}"), Some(&form))
            .await?;
        Ok(())
    }

    pub async fn delete_draft(&self, id: i64) -> Result<(), ApiError> {
        self.call::<Value>(Method::DELETE, &format!("api/v1/drafts/{id}"), None)
            .await?;
        Ok(())
    }

    // ---- 在线状态与设置 ----

    pub async fn update_presence(
        &self,
        status: &str,
        last_update_id: i64,
        new_user_input: bool,
    ) -> Result<PresenceUpdateResult, ApiError> {
        let form = Form::new()
            .add("status", status)
            .add("last_update_id", last_update_id)
            .add("new_user_input", new_user_input)
            .add("slim_presence", true);
        self.call(Method::POST, "api/v1/users/me/presence", Some(&form))
            .await
    }

    pub async fn update_settings(&self, args: &Form) -> Result<(), ApiError> {
        self.call::<Value>(Method::PATCH, "api/v1/settings", Some(args))
            .await?;
        Ok(())
    }

    /// 设置个性签名；空字符串表示清除。
    pub async fn set_status(&self, text: &str) -> Result<(), ApiError> {
        let form = Form::new().add("status_text", text);
        self.call::<Value>(Method::POST, "api/v1/users/me/status", Some(&form))
            .await?;
        Ok(())
    }

    /// 添加提醒词，返回服务器上的完整列表。
    pub async fn add_alert_words<S: AsRef<str>>(
        &self,
        words: &[S],
    ) -> Result<Vec<String>, ApiError> {
        self.alert_words(Method::POST, words).await
    }

    /// 删除提醒词，返回服务器上的完整列表。
    pub async fn remove_alert_words<S: AsRef<str>>(
        &self,
        words: &[S],
    ) -> Result<Vec<String>, ApiError> {
        self.alert_words(Method::DELETE, words).await
    }

    async fn alert_words<S: AsRef<str>>(
        &self,
        method: Method,
        words: &[S],
    ) -> Result<Vec<String>, ApiError> {
        let words: Vec<&str> = words.iter().map(AsRef::as_ref).collect();
        let form = Form::new().add_json("alert_words", &words);
        self.call_field(
            method,
            "api/v1/users/me/alert_words",
            Some(&form),
            "alert_words",
        )
        .await
    }

    /// 修改组织设置（仅管理员）。
    pub async fn update_realm(&self, args: &Form) -> Result<(), ApiError> {
        self.call::<Value>(Method::PATCH, "api/v1/realm", Some(args))
            .await?;
        Ok(())
    }
}

fn draft_payload(draft: &DraftDto) -> Value {
    let mut payload = json!({
        "type": draft.kind,
        "to": draft.to,
        "topic": draft.topic,
        "content": draft.content,
    });
    if let Some(timestamp) = draft.timestamp {
        payload["timestamp"] = json!(timestamp);
    }
    payload
}
