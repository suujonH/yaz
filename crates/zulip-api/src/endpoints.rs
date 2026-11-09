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

// REST reads replace an empty topic with realm_empty_topic_display_name by default, while the event
// queue returns an empty string; reading messages and topics both pass allow_empty_topic_name=true
// so keys of the same conversation match.

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
    // ---- Before login ----

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

    // ---- Event queue ----

    /// Register an event queue and get the initial state. Without `event_types`, receives all
    /// events.
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

    /// Long-poll for events, returning raw event objects. `timeout` should be greater than the
    /// server's long-poll timeout.
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

    // ---- Messages ----

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

    /// Fetch a single message. When `apply_markdown` is false, `content` is the Markdown source.
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

    /// Render Markdown to the message HTML using the server's rules (does not send).
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

    /// Modify message flags.
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

    /// Add a reaction. `reaction_type` is `unicode_emoji` / `realm_emoji` / `zulip_extra_emoji`.
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

    /// Add a submessage to a message (widget operations such as polls and to-do checklists). The
    /// endpoint is not in the API docs; the web client's widgets use it; the server validates
    /// `content` by widget type, and the result is delivered through the submessage event.
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

    /// Move messages, change the topic name, or edit content (`PATCH /messages/{id}`). Returns
    /// [`ApiError::Api`] with code `MOVE_MESSAGES_TIME_LIMIT_EXCEEDED` when the move time limit is
    /// exceeded (none were moved).
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

    /// Delete a message (`DELETE /messages/{id}`; requires the organization to allow it or
    /// administrator permission).
    pub async fn delete_message(&self, message_id: i64) -> Result<(), ApiError> {
        self.call::<Value>(
            Method::DELETE,
            &format!("api/v1/messages/{message_id}"),
            None,
        )
        .await?;
        Ok(())
    }

    /// Edit history of a message; the first item is the original version.
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

    /// IDs of users who have read this message and allow their read status to be seen (users with
    /// read receipts off do not appear).
    pub async fn get_read_receipts(&self, message_id: i64) -> Result<Vec<i64>, ApiError> {
        self.call_field(
            Method::GET,
            &format!("api/v1/messages/{message_id}/read_receipts"),
            None,
            "user_ids",
        )
        .await
    }

    /// Typing notification. `op` is `start` / `stop`; for a channel topic pass `stream_id` and
    /// `topic`, for a DM pass `user_ids` (recipients, excluding self).
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

    // ---- Channels and topics ----

    pub async fn get_subscribers(&self, stream_id: i64) -> Result<Vec<i64>, ApiError> {
        self.call_field(
            Method::GET,
            &format!("api/v1/streams/{stream_id}/members"),
            None,
            "subscribers",
        )
        .await
    }

    /// Topic list of a channel (descending by latest message id).
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

    /// Delete messages in a topic (one call may delete only a batch); returns whether all are
    /// deleted.
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

    /// Subscribe to a channel; when `principals` is not `None`, subscribe those users.
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

    /// Unsubscribe from a channel; when `principals` is not `None`, unsubscribe those users.
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

    /// Create a channel (`POST /channels/create` since Zulip 11); returns the channel id.
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

    /// Modify a channel (`PATCH /streams/{id}`): `new_name`, `description`, `is_private`,
    ///
    /// `history_public_to_subscribers`, and each `can_*_group` in the form `{"new": ..., "old":
    /// ...}`.
    pub async fn update_channel(&self, stream_id: i64, args: &Form) -> Result<(), ApiError> {
        self.call::<Value>(
            Method::PATCH,
            &format!("api/v1/streams/{stream_id}"),
            Some(args),
        )
        .await?;
        Ok(())
    }

    // ---- Scheduled messages, reminders, canned replies, invitations ----

    /// Create a scheduled message (`queue_id` and `local_id` are not used); returns the scheduled
    /// message id.
    pub async fn create_scheduled_message(
        &self,
        message: &OutgoingMessage,
        deliver_at: i64,
    ) -> Result<i64, ApiError> {
        // Same as immediate send: when the server guesses by client name it does not recognize this
        // client, and after delivery the message becomes your own unread message
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

    /// Remind yourself of this message at a given time (the server sends yourself a DM when due);
    /// returns the reminder id.
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

    /// Create a canned reply; returns the id.
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

    /// Invite new members to the organization (role member). `emails` is separated by commas or
    /// newlines; if any email is invalid the server sends no invitations and returns
    /// [`ApiError::Api`] with code `INVITATION_FAILED`.
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

    // ---- Users ----

    /// Modify a user (`PATCH /users/{id}`, administrator): name, role (100 owner ... 600 guest);
    /// `None` means no change.
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

    /// Mute a user.
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

    // ---- Uploaded files ----

    /// Files you uploaded, and the storage space used.
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

    /// Delete an uploaded file from the server; links to it in messages that reference it stop
    /// working.
    pub async fn delete_attachment(&self, id: i64) -> Result<(), ApiError> {
        self.call::<Value>(Method::DELETE, &format!("api/v1/attachments/{id}"), None)
            .await?;
        Ok(())
    }

    // ---- Drafts ----

    pub async fn get_drafts(&self) -> Result<Vec<DraftDto>, ApiError> {
        self.call_field(Method::GET, "api/v1/drafts", None, "drafts")
            .await
    }

    /// Create a draft; returns the id assigned by the server.
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

    // ---- Presence and settings ----

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

    /// Set the status text; an empty string clears it.
    pub async fn set_status(&self, text: &str) -> Result<(), ApiError> {
        let form = Form::new().add("status_text", text);
        self.call::<Value>(Method::POST, "api/v1/users/me/status", Some(&form))
            .await?;
        Ok(())
    }

    /// Add an alert word; returns the full list on the server.
    pub async fn add_alert_words<S: AsRef<str>>(
        &self,
        words: &[S],
    ) -> Result<Vec<String>, ApiError> {
        self.alert_words(Method::POST, words).await
    }

    /// Remove an alert word; returns the full list on the server.
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

    /// Modify organization settings (administrators only).
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
