//! Server state. All data is seen from the logged-in account (me): flags exist in one copy, and me
//! is subscribed to every channel.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Map, Value, json};

use crate::markdown::{self, escape};
use crate::{BOT_ID, ME_ID};

pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub is_bot: bool,
    pub role: i32,
    pub is_active: bool,
    pub avatar_version: i64,
}

pub struct Chan {
    pub id: i64,
    pub name: String,
    pub description: String,
    /// Server-rendered announcement, used for HTML the fake server's Markdown does not support;
    /// when `None`, rendered from `description`.
    pub rendered_description: Option<String>,
    pub invite_only: bool,
    pub color: String,
    pub subscribed: bool,
    pub is_muted: bool,
    pub properties: Map<String, Value>,
    pub date_created: i64,
}

pub struct Msg {
    pub id: i64,
    pub sender_id: i64,
    pub stream_id: Option<i64>,
    pub topic: String,
    /// All participants of a DM (ascending, including me).
    pub dm_users: Vec<i64>,
    pub raw: String,
    pub html: String,
    pub timestamp: i64,
    pub flags: BTreeSet<String>,
    pub reactions: Vec<Value>,
    pub submessages: Vec<Value>,
    pub last_edit: Option<i64>,
    pub history: Vec<Value>,
}

#[derive(Default)]
pub struct Queue {
    pub next_id: i64,
    pub events: Vec<Value>,
}

pub struct Upload {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub name: String,
    pub attachment_id: i64,
    pub created_at: i64,
    /// Messages that reference it but can no longer be fetched.
    pub dangling_refs: Vec<i64>,
}

/// Send target.
#[derive(Clone, Debug)]
pub enum Target {
    Stream(i64, String),
    Dm(Vec<i64>),
}

#[derive(Default)]
pub struct State {
    pub users: Vec<User>,
    pub channels: Vec<Chan>,
    pub messages: BTreeMap<i64, Msg>,
    pub queues: HashMap<String, Queue>,
    pub uploads: BTreeMap<String, Upload>,
    pub drafts: BTreeMap<i64, Value>,
    pub scheduled: BTreeMap<i64, Value>,
    pub reminders: BTreeMap<i64, Value>,
    pub snippets: BTreeMap<i64, Value>,
    pub user_topics: Vec<Value>,
    pub muted_users: Vec<Value>,
    pub settings: Map<String, Value>,
    pub alert_words: Vec<String>,
    pub presences: Map<String, Value>,
    pub user_status: Map<String, Value>,
    pub realm: Map<String, Value>,
    /// Messages beyond the move time limit may be moved only after this id; `None` means no limit.
    pub move_limit_first_id: Option<i64>,
    /// Return a server error when fetching a single message, simulating a partial failure to load
    /// references on the resources page.
    pub fail_message_fetch: bool,
    /// Always return a server error when fetching these messages, simulating that only some
    /// references to the same file can be loaded.
    pub failing_messages: std::collections::HashSet<i64>,
    pub next_id: i64,
    pub presence_last_update_id: i64,
}

impl State {
    pub fn alloc_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn user(&self, id: i64) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    pub fn channel(&self, id: i64) -> Option<&Chan> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn avatar_url(&self, id: i64) -> String {
        let version = self.user(id).map_or(1, |u| u.avatar_version);
        format!("/avatar/{id}.png?version={version}")
    }

    pub fn emit(&mut self, event: Value) {
        for queue in self.queues.values_mut() {
            push_to_queue(queue, event.clone());
        }
    }

    pub fn render(&self, content: &str) -> String {
        markdown::render(content, &|name| {
            self.users.iter().find(|u| u.name == name).map(|u| u.id)
        })
    }

    /// Write a message and push an event. `local` is the sender's `(queue_id, local_id)`.
    pub fn send(
        &mut self,
        sender_id: i64,
        target: Target,
        content: &str,
        timestamp: i64,
        local: Option<(String, String)>,
    ) -> i64 {
        let id = self.insert(sender_id, target, content, timestamp);
        let message = self.message_json(&self.messages[&id]);
        let flags: Vec<String> = self.messages[&id].flags.iter().cloned().collect();
        for (queue_id, queue) in &mut self.queues {
            let mut event = json!({"type": "message", "message": message, "flags": flags});
            if let Some((q, local_id)) = &local
                && q == queue_id
            {
                event["local_message_id"] = json!(local_id);
            }
            push_to_queue(queue, event);
        }
        if let Some(widget) = self.messages[&id].submessages.first().cloned() {
            let mut event = widget;
            event["type"] = json!("submessage");
            event["submessage_id"] = event["id"].clone();
            self.emit(event);
        }
        id
    }

    /// Write a message without pushing events (for seed data).
    pub fn insert(&mut self, sender_id: i64, target: Target, content: &str, timestamp: i64) -> i64 {
        let id = self.alloc_id();
        let html = self.render(content);
        let mut flags = BTreeSet::new();
        if sender_id == ME_ID {
            flags.insert("read".to_owned());
        }
        if html.contains(&format!("data-user-id=\"{ME_ID}\"")) && !html.contains("silent") {
            flags.insert("mentioned".to_owned());
        }
        if html.contains("channel-wildcard-mention") {
            flags.insert("stream_wildcard_mentioned".to_owned());
        }
        if html.contains("topic-mention") {
            flags.insert("topic_wildcard_mentioned".to_owned());
        }
        if self
            .alert_words
            .iter()
            .any(|w| content.to_lowercase().contains(&w.to_lowercase()))
        {
            flags.insert("has_alert_word".to_owned());
        }
        let (stream_id, topic, dm_users) = match target {
            Target::Stream(stream, topic) => (Some(stream), topic, Vec::new()),
            Target::Dm(users) => {
                let mut all: Vec<i64> = users;
                all.push(sender_id);
                all.push(ME_ID);
                all.sort_unstable();
                all.dedup();
                (None, String::new(), all)
            }
        };
        let submessages = poll_definition(content)
            .map(|def| {
                vec![json!({
                    "id": id * 1000,
                    "message_id": id,
                    "sender_id": sender_id,
                    "msg_type": "widget",
                    "content": def.to_string(),
                })]
            })
            .unwrap_or_default();
        self.messages.insert(
            id,
            Msg {
                id,
                sender_id,
                stream_id,
                topic,
                dm_users,
                raw: content.to_owned(),
                html,
                timestamp,
                flags,
                reactions: Vec::new(),
                submessages,
                last_edit: None,
                history: Vec::new(),
            },
        );
        id
    }

    pub fn message_json(&self, m: &Msg) -> Value {
        let sender = self.user(m.sender_id);
        let recipient = match m.stream_id {
            Some(id) => json!(self.channel(id).map_or("", |c| c.name.as_str())),
            None => Value::Array(
                m.dm_users
                    .iter()
                    .filter_map(|id| self.user(*id))
                    .map(|u| json!({"id": u.id, "email": u.email, "full_name": u.name}))
                    .collect(),
            ),
        };
        let mut value = json!({
            "id": m.id,
            "sender_id": m.sender_id,
            "sender_full_name": sender.map_or("", |u| u.name.as_str()),
            "sender_email": sender.map_or("", |u| u.email.as_str()),
            "avatar_url": self.avatar_url(m.sender_id),
            "content": m.html,
            "timestamp": m.timestamp,
            "type": if m.stream_id.is_some() { "stream" } else { "private" },
            "subject": m.topic,
            "display_recipient": recipient,
            "flags": m.flags,
            "reactions": m.reactions,
            "submessages": m.submessages,
            "is_me_message": m.raw.starts_with("/me "),
        });
        if let Some(id) = m.stream_id {
            value["stream_id"] = json!(id);
        }
        if let Some(edit) = m.last_edit {
            value["last_edit_timestamp"] = json!(edit);
        }
        value
    }

    pub fn user_json(&self, u: &User) -> Value {
        json!({
            "user_id": u.id,
            "email": u.email,
            "full_name": u.name,
            "avatar_url": self.avatar_url(u.id),
            "is_bot": u.is_bot,
            "bot_type": if u.is_bot { json!(1) } else { Value::Null },
            "role": u.role,
            "is_active": u.is_active,
            "timezone": "Asia/Shanghai",
        })
    }

    pub fn stream_json(&self, c: &Chan) -> Value {
        let mut value = json!({
            "stream_id": c.id,
            "name": c.name,
            "description": c.description,
            "rendered_description": c
                .rendered_description
                .clone()
                .unwrap_or_else(|| self.render(&c.description)),
            "invite_only": c.invite_only,
            "is_web_public": false,
            "is_archived": false,
            "history_public_to_subscribers": true,
            "subscriber_count": self.users.iter().filter(|u| !u.is_bot && u.is_active).count(),
            "creator_id": ME_ID,
            "date_created": c.date_created,
            "topics_policy": "inherit",
            "can_administer_channel_group": ADMINS_GROUP,
            "can_add_subscribers_group": ADMINS_GROUP,
            "can_remove_subscribers_group": ADMINS_GROUP,
            "can_send_message_group": EVERYONE_GROUP,
            "can_move_messages_out_of_channel_group": ADMINS_GROUP,
            "can_move_messages_within_channel_group": ADMINS_GROUP,
            "can_resolve_topics_group": ADMINS_GROUP,
            "can_create_topic_group": EVERYONE_GROUP,
            "can_delete_any_message_group": ADMINS_GROUP,
            "can_delete_own_message_group": EVERYONE_GROUP,
            "can_subscribe_group": ADMINS_GROUP,
        });
        if let Some(object) = value.as_object_mut() {
            object.extend(c.properties.clone());
        }
        value
    }

    pub fn subscription_json(&self, c: &Chan) -> Value {
        let mut value = self.stream_json(c);
        value["color"] = json!(c.color);
        value["is_muted"] = json!(c.is_muted);
        for property in [
            "desktop_notifications",
            "audible_notifications",
            "push_notifications",
            "email_notifications",
            "wildcard_mentions_notify",
        ] {
            value[property] = c.properties.get(property).cloned().unwrap_or(Value::Null);
        }
        value
    }

    pub fn unread_json(&self) -> Value {
        let mut pms: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        let mut huddles: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        let mut streams: BTreeMap<(i64, String), Vec<i64>> = BTreeMap::new();
        let mut mentions = Vec::new();
        let mut count = 0;
        for m in self.messages.values().filter(|m| !m.flags.contains("read")) {
            count += 1;
            if m.flags.contains("mentioned")
                || m.flags.contains("stream_wildcard_mentioned")
                || m.flags.contains("topic_wildcard_mentioned")
            {
                mentions.push(m.id);
            }
            if let Some(stream) = m.stream_id {
                streams
                    .entry((stream, m.topic.clone()))
                    .or_default()
                    .push(m.id);
            } else if m.dm_users.len() > 2 {
                let ids: Vec<String> = m.dm_users.iter().map(i64::to_string).collect();
                huddles.entry(ids.join(",")).or_default().push(m.id);
            } else {
                let other = m
                    .dm_users
                    .iter()
                    .copied()
                    .find(|id| *id != ME_ID)
                    .unwrap_or(ME_ID);
                pms.entry(other).or_default().push(m.id);
            }
        }
        json!({
            "count": count,
            "pms": pms.into_iter()
                .map(|(id, ids)| json!({"other_user_id": id, "unread_message_ids": ids}))
                .collect::<Vec<_>>(),
            "streams": streams.into_iter()
                .map(|((s, t), ids)| json!({"stream_id": s, "topic": t, "unread_message_ids": ids}))
                .collect::<Vec<_>>(),
            "huddles": huddles.into_iter()
                .map(|(s, ids)| json!({"user_ids_string": s, "unread_message_ids": ids}))
                .collect::<Vec<_>>(),
            "mentions": mentions,
            "old_unreads_missing": false,
        })
    }

    pub fn register_json(&mut self, realm_url: &str) -> Value {
        let queue_id = format!("q{}", self.alloc_id());
        self.queues.insert(
            queue_id.clone(),
            Queue {
                next_id: 0,
                events: Vec::new(),
            },
        );
        let everyone: Vec<i64> = self.users.iter().map(|u| u.id).collect();
        let starred: Vec<i64> = self
            .messages
            .values()
            .filter(|m| m.flags.contains("starred"))
            .map(|m| m.id)
            .collect();
        let mut recent: BTreeMap<Vec<i64>, i64> = BTreeMap::new();
        for m in self.messages.values().filter(|m| m.stream_id.is_none()) {
            let others: Vec<i64> = m
                .dm_users
                .iter()
                .copied()
                .filter(|id| *id != ME_ID)
                .collect();
            recent.insert(others, m.id);
        }
        let realm_users: Vec<Value> = self
            .users
            .iter()
            .filter(|u| u.id != BOT_ID && u.is_active)
            .map(|u| self.user_json(u))
            .collect();
        let bots: Vec<Value> = self
            .user(BOT_ID)
            .map(|u| self.user_json(u))
            .into_iter()
            .collect();
        let inactive: Vec<Value> = self
            .users
            .iter()
            .filter(|u| !u.is_active)
            .map(|u| self.user_json(u))
            .collect();
        let me = self.user(ME_ID);
        let mut result = json!({
            "queue_id": queue_id,
            "last_event_id": -1,
            "zulip_feature_level": 445,
            "zulip_version": "11.0-test",
            "event_queue_longpoll_timeout_seconds": 90,
            "user_id": ME_ID,
            "email": me.map_or("", |u| u.email.as_str()),
            "full_name": me.map_or("", |u| u.name.as_str()),
            "avatar_url": self.avatar_url(ME_ID),
            "is_admin": true,
            "realm_users": realm_users,
            "realm_non_active_users": inactive,
            "cross_realm_bots": bots,
            "subscriptions": self.channels.iter().filter(|c| c.subscribed)
                .map(|c| self.subscription_json(c)).collect::<Vec<_>>(),
            "unsubscribed": self.channels.iter().filter(|c| !c.subscribed)
                .map(|c| self.subscription_json(c)).collect::<Vec<_>>(),
            "never_subscribed": [],
            "streams": self.channels.iter().map(|c| self.stream_json(c)).collect::<Vec<_>>(),
            "recent_private_conversations": recent.into_iter()
                .map(|(ids, max)| json!({"user_ids": ids, "max_message_id": max}))
                .collect::<Vec<_>>(),
            "unread_msgs": self.unread_json(),
            "user_topics": self.user_topics,
            "drafts": self.drafts.values().collect::<Vec<_>>(),
            "starred_messages": starred,
            "muted_users": self.muted_users,
            "presences": self.presences,
            "presence_last_update_id": self.presence_last_update_id,
            "user_status": self.user_status,
            "user_settings": self.settings,
            "alert_words": self.alert_words,
            "scheduled_messages": self.scheduled.values().collect::<Vec<_>>(),
            "saved_snippets": self.snippets.values().collect::<Vec<_>>(),
            "reminders": self.reminders.values().collect::<Vec<_>>(),
            "realm_user_groups": [
                {"id": EVERYONE_GROUP, "name": "role:everyone", "description": "", "members": everyone,
                 "direct_subgroup_ids": [], "is_system_group": true, "deactivated": false},
                {"id": MEMBERS_GROUP, "name": "role:members", "description": "", "members": everyone,
                 "direct_subgroup_ids": [], "is_system_group": true, "deactivated": false},
                {"id": ADMINS_GROUP, "name": "role:administrators", "description": "", "members": [ME_ID],
                 "direct_subgroup_ids": [], "is_system_group": true, "deactivated": false},
            ],
            "realm_emoji": {
                "1": {"id": "1", "name": "喵喵", "source_url": "/avatar/1.png", "still_url": null, "deactivated": false}
            },
            "custom_profile_fields": [
                {"id": 1, "type": 1, "name": "部门", "hint": "", "order": 1, "display_in_profile_summary": true, "field_data": ""}
            ],
            "server_emoji_data_url": format!("{realm_url}/static/emoji.json"),
            "max_message_length": 10000,
            "max_topic_length": 60,
            "max_file_upload_size_mib": 25,
            "realm_upload_quota_mib": null,
            "realm_empty_topic_display_name": "一般聊天",
            "realm_name": "测试组织",
            "realm_icon_url": "/avatar/1.png",
            "realm_allow_message_editing": true,
            "realm_push_notifications_enabled": false,
            "realm_message_content_edit_limit_seconds": null,
            "realm_message_content_delete_limit_seconds": null,
            "realm_enable_read_receipts": true,
            "realm_message_edit_history_visibility_policy": "all",
            "realm_topics_policy": "allow_empty_topic",
            "server_typing_started_wait_period_milliseconds": 10000,
            "server_typing_stopped_wait_period_milliseconds": 5000,
            "server_typing_started_expiry_period_milliseconds": 15000,
            "server_presence_ping_interval_seconds": 60,
            "server_presence_offline_threshold_seconds": 140,
            "server_supported_permission_settings": {
                "realm": {
                    "can_create_public_channel_group": {"allow_everyone_group": false},
                    "can_create_private_channel_group": {"allow_everyone_group": false},
                    "can_invite_users_group": {"allow_everyone_group": false},
                    "can_mention_many_users_group": {"allow_everyone_group": true},
                    "can_move_messages_between_topics_group": {"allow_everyone_group": true},
                    "can_move_messages_between_channels_group": {"allow_everyone_group": false},
                    "can_resolve_topics_group": {"allow_everyone_group": true},
                    "can_delete_any_message_group": {"allow_everyone_group": false},
                    "can_delete_own_message_group": {"allow_everyone_group": true},
                },
                "stream": {
                    "can_send_message_group": {"allow_everyone_group": true},
                    "can_create_topic_group": {"allow_everyone_group": true},
                },
                "group": {},
            },
        });
        if let Some(object) = result.as_object_mut() {
            object.extend(self.realm.clone());
        }
        result
    }
}

pub fn push_to_queue(queue: &mut Queue, mut event: Value) {
    event["id"] = json!(queue.next_id);
    queue.next_id += 1;
    queue.events.push(event);
}

pub const EVERYONE_GROUP: i64 = 1;
pub const MEMBERS_GROUP: i64 = 2;
pub const ADMINS_GROUP: i64 = 3;

/// `/poll question` + one option per line.
fn poll_definition(content: &str) -> Option<Value> {
    let mut lines = content.lines();
    let question = lines.next()?.strip_prefix("/poll ")?;
    let options: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
    Some(json!({
        "widget_type": "poll",
        "extra_data": {"question": question, "options": options},
    }))
}

/// Highlighted version of a search result: marks only the first hit in the text part.
pub fn highlight(text: &str, query: &str) -> String {
    let query = escape(query);
    if query.is_empty() {
        return text.to_owned();
    }
    text.replacen(
        &query,
        &format!("<span class=\"highlight\">{query}</span>"),
        1,
    )
}
