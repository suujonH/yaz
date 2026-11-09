//! Zulip events -> state transitions. Modifies [`State`] in a purely functional style, producing
//! events to broadcast and follow-up actions to execute asynchronously.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::Value;
use zulip_api::{MessageDto, PresenceDto, RealmEmojiDto, UserDto};
use zulip_core::ConversationKey;
use zulip_core::html;
use zulip_core::model::{ChatMessage, MentionKind, MessageFlags, Reaction, Submessage};
use zulip_core::notice::SystemNoticeKind;
use zulip_core::permission::GroupSettingValue;

use crate::convert::user_from_dto;
use crate::event::SessionEvent;
use crate::hooks::HookEvent;
use crate::notify;
use crate::state::conversations::Latest;
use crate::state::messages::ContentEdit;
use crate::state::{State, realm_emoji_from};

/// Actions that must be executed asynchronously.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Fetch preview messages for conversations in the backfill queue.
    Backfill,
    /// The conversation being viewed received a new message; mark it read.
    MarkRead(Vec<i64>),
    /// A message became unread again but the event has no details; fetch the message.
    FetchUnread(i64),
    /// Another client toggled draft sync.
    SwitchDraftSync(bool),
    ExpireTyping(ConversationKey, Duration),
    /// Favorite or unfavorite: save or delete the local copies of the images and files in the message.
    FavoriteCopies {
        ids: Vec<i64>,
        starred: bool,
    },
}

#[derive(Default, Debug)]
pub struct Applied {
    pub events: Vec<SessionEvent>,
    pub effects: Vec<Effect>,
    pub notices: Vec<HookEvent>,
}

impl Applied {
    fn emit(&mut self, event: SessionEvent) {
        if !self.events.contains(&event) {
            self.events.push(event);
        }
    }
}

fn str_of<'a>(event: &'a Value, name: &str) -> Option<&'a str> {
    event.get(name).and_then(Value::as_str)
}

fn i64_of(event: &Value, name: &str) -> Option<i64> {
    event.get(name).and_then(Value::as_i64)
}

fn ids_of(event: &Value, name: &str) -> Vec<i64> {
    event
        .get(name)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

fn parse<T: serde::de::DeserializeOwned + Default>(value: Option<&Value>) -> T {
    value
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Applies one event. Unknown event types are ignored.
pub fn apply_event(state: &mut State, event: &Value) -> Applied {
    let mut out = Applied::default();
    match str_of(event, "type").unwrap_or_default() {
        "message" => on_message(state, event, &mut out),
        "update_message" => on_update_message(state, event, &mut out),
        "delete_message" => on_delete_message(state, event, &mut out),
        "update_message_flags" => on_flags(state, event, &mut out),
        "reaction" => on_reaction(state, event, &mut out),
        "submessage" => on_submessage(state, event, &mut out),
        "subscription" => {
            let ids = state.channels.apply_subscription_event(event);
            on_channels(ids, event, &mut out);
        }
        "stream" => {
            let ids = state.channels.apply_stream_event(event);
            on_channels(ids, event, &mut out);
        }
        "realm_user" => on_realm_user(state, event, &mut out),
        "user_group" => {
            state.users.apply_group_event(event);
            state.rebuild_permissions();
            out.emit(SessionEvent::PermissionsChanged);
        }
        "realm" => on_realm(state, event, &mut out),
        "user_topic" => {
            if let Some((stream_id, topic)) = state.channels.apply_user_topic(event) {
                out.notices.push(HookEvent::UserTopicChanged {
                    stream_id,
                    topic: topic.clone(),
                });
                out.emit(SessionEvent::UserTopicChanged { stream_id, topic });
            }
        }
        "user_settings" => on_user_settings(state, event, &mut out),
        "drafts" if state.settings.enable_drafts_synchronization => {
            for key in state.drafts.apply_event(event, state.users.self_id) {
                out.notices.push(HookEvent::DraftChanged(key.clone()));
                out.emit(SessionEvent::DraftChanged(key));
            }
        }
        "presence" => {
            let presences: std::collections::HashMap<String, PresenceDto> =
                parse(event.get("presences"));
            let presences = presences
                .into_iter()
                .filter_map(|(id, p)| Some((id.parse().ok()?, p)))
                .collect();
            let changed = state.users.apply_presences(&presences);
            if !changed.is_empty() {
                out.notices
                    .push(HookEvent::PresenceChanged(changed.clone()));
                out.emit(SessionEvent::PresenceChanged(changed));
            }
        }
        "user_status" => {
            if let Some(id) = i64_of(event, "user_id") {
                state.users.set_status(id, str_of(event, "status_text"));
                out.notices.push(HookEvent::UserChanged(id));
                out.emit(SessionEvent::UserChanged(id));
            }
        }
        "muted_users" => {
            let muted: Vec<zulip_api::MutedUserDto> = parse(event.get("muted_users"));
            state.users.set_muted(muted.iter().map(|m| m.id));
            out.notices.push(HookEvent::MutedUsersChanged);
            out.emit(SessionEvent::MutedUsersChanged);
        }
        "realm_emoji" => {
            let emoji: std::collections::HashMap<String, RealmEmojiDto> =
                parse(event.get("realm_emoji"));
            state.realm_emoji = realm_emoji_from(emoji.values());
            state.rebuild_emoji();
            out.emit(SessionEvent::EmojiChanged);
        }
        "custom_profile_fields" => {
            state.realm.profile_fields = parse(event.get("fields"));
            out.notices.push(HookEvent::RealmChanged {
                properties: vec!["custom_profile_fields".to_owned()],
            });
            out.emit(SessionEvent::RealmChanged);
        }
        "alert_words" => {
            state.alert_words = parse(event.get("alert_words"));
            out.emit(SessionEvent::AlertWordsChanged);
        }
        "typing" => on_typing(state, event, &mut out),
        "scheduled_messages" => {
            state
                .queued
                .apply_scheduled_event(event, state.users.self_id);
            out.notices.push(HookEvent::ScheduledChanged);
            out.emit(SessionEvent::ScheduledMessagesChanged);
        }
        "saved_snippets" => {
            state.queued.apply_snippets_event(event);
            out.emit(SessionEvent::SnippetsChanged);
        }
        "reminders" => {
            state.queued.apply_reminders_event(event);
            out.notices.push(HookEvent::RemindersChanged);
            out.emit(SessionEvent::RemindersChanged);
        }
        "attachment" => out.emit(SessionEvent::AttachmentsChanged),
        _ => {}
    }
    out
}

fn on_channels(ids: Vec<i64>, event: &Value, out: &mut Applied) {
    for id in &ids {
        out.emit(SessionEvent::ChannelChanged(*id));
    }
    out.emit(SessionEvent::ChannelsChanged);
    out.notices.push(HookEvent::ChannelsChanged {
        stream_ids: ids,
        op: str_of(event, "op").unwrap_or_default().to_owned(),
    });
}

/// The message inside a message event: flags are on the outer layer of the event.
pub(crate) fn message_of_event(state: &State, event: &Value) -> Option<ChatMessage> {
    let dto: MessageDto = serde_json::from_value(event.get("message")?.clone()).ok()?;
    let mut message = state.prepare_message(&dto);
    if let Some(flags) = event.get("flags") {
        let flags: Vec<String> = parse(Some(flags));
        message.flags = MessageFlags::from_names(&flags);
    }
    Some(message)
}

/// A new message enters the various states. Idempotent when the message already exists.
pub fn accept_message(
    state: &mut State,
    m: ChatMessage,
    local_id: Option<&str>,
    out: &mut Applied,
) {
    let key = m.key.clone();
    let id = m.id;
    if matches!(
        m.notice,
        SystemNoticeKind::Notice | SystemNoticeKind::Reminder
    ) {
        state.messages.on_new_message(m.clone(), None);
        if state.notices.add(&m) {
            out.emit(SessionEvent::SystemNoticesChanged);
            if m.notice == SystemNoticeKind::Reminder && !m.is_read() {
                out.emit(SessionEvent::Notification(Box::new(notify::on_reminder(
                    state, &m,
                ))));
            }
        }
        return;
    }

    let notification = notify::on_new_message(state, &m);
    let (added, replaced) = state.messages.on_new_message(m.clone(), local_id);
    if let (Some(_), Some(local)) = (replaced, local_id) {
        out.emit(SessionEvent::MessageConfirmed {
            key: key.clone(),
            local_id: local.to_owned(),
            id,
        });
    } else if added {
        out.emit(SessionEvent::MessageAdded {
            key: key.clone(),
            id,
        });
    }
    if let Some(stream) = m.stream_id
        && state.channels.on_new_message(stream, &m.topic, id)
    {
        out.emit(SessionEvent::TopicsChanged(stream));
    }
    if state.messages.is_hidden(&m) {
        return;
    }
    if state.unread.add_message(&m) {
        out.emit(SessionEvent::UnreadChanged(vec![key.clone()]));
    }
    if !state.is_bot_notice(m.sender_id, &key) {
        let latest = latest_of(&m);
        if state.conversations.apply_latest(&key, &latest) {
            out.emit(SessionEvent::ConversationChanged(key.clone()));
            out.emit(SessionEvent::ConversationsChanged);
        }
    }
    if let Some(request) = notification {
        out.emit(SessionEvent::Notification(Box::new(request)));
    }
    if !m.is_read() && state.view.is_actively_viewing(&key) {
        out.effects.push(Effect::MarkRead(vec![id]));
    }
}

pub fn latest_of(m: &ChatMessage) -> Latest {
    Latest {
        id: m.id,
        time: m.timestamp,
        sender_id: m.sender_id,
        preview: html::to_preview(&m.content_html),
        title: m.topic.clone(),
    }
}

fn on_message(state: &mut State, event: &Value, out: &mut Applied) {
    let Some(message) = message_of_event(state, event) else {
        log::warn!("无法解析消息事件");
        return;
    };
    accept_message(state, message, str_of(event, "local_message_id"), out);
}

fn on_update_message(state: &mut State, event: &Value, out: &mut Applied) {
    if let Some(id) = i64_of(event, "message_id")
        && let Some(html) = str_of(event, "rendered_content")
    {
        apply_content_edit(state, id, html, event, out);
    }

    if event.get("subject").is_some() || event.get("new_stream_id").is_some() {
        on_move(state, event, out);
    }
}

fn apply_content_edit(state: &mut State, id: i64, html: &str, event: &Value, out: &mut Applied) {
    let edit_timestamp = i64_of(event, "edit_timestamp");
    let rendering_only = event
        .get("rendering_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let flags: Option<Vec<String>> = event
        .get("flags")
        .and_then(|f| serde_json::from_value(f.clone()).ok());
    let edit = ContentEdit {
        id,
        html: Some(html),
        flags: flags.as_deref(),
        edit_timestamp: if rendering_only { None } else { edit_timestamp },
    };
    let applied = state.messages.apply_edit(&edit);
    if !rendering_only {
        let key = applied
            .as_ref()
            .map(|(key, _)| key.clone())
            .or_else(|| key_of_stream_event(event));
        out.notices.push(HookEvent::MessageEdited {
            key,
            id,
            edit_timestamp,
            html: html.to_owned(),
        });
    }
    // An edited message is not necessarily in the cache: the old content comes from the event
    let previous = applied
        .as_ref()
        .and_then(|(_, previous)| previous.clone())
        .or_else(|| str_of(event, "orig_rendered_content").map(str::to_owned));
    if !rendering_only && let (Some(previous), Some(at)) = (previous, edit_timestamp) {
        state.versions.add(id, at, &previous);
    }
    if let Some((key, _)) = &applied {
        out.emit(SessionEvent::MessageUpdated {
            key: key.clone(),
            id,
        });
        if let Some(message) = state.messages.get(id).cloned()
            && state.unread.on_edited(&message)
        {
            out.emit(SessionEvent::UnreadChanged(vec![key.clone()]));
        }
    }
    if let Some(key) = state.conversations.on_edited(id, html) {
        out.emit(SessionEvent::ConversationChanged(key));
    }
}

// The conversation in a channel message event; DM events carry no conversation.
fn key_of_stream_event(event: &Value) -> Option<ConversationKey> {
    let topic = str_of(event, "orig_subject")
        .or_else(|| str_of(event, "subject"))
        .or_else(|| str_of(event, "topic"))?;
    Some(ConversationKey::topic(i64_of(event, "stream_id")?, topic))
}

fn on_move(state: &mut State, event: &Value, out: &mut Applied) {
    let Some(old_stream) = i64_of(event, "stream_id") else {
        return;
    };
    let old_topic = str_of(event, "orig_subject")
        .or_else(|| str_of(event, "subject"))
        .unwrap_or_default();
    let new_topic = str_of(event, "subject").unwrap_or(old_topic);
    let new_stream = i64_of(event, "new_stream_id").unwrap_or(old_stream);
    let ids = ids_of(event, "message_ids");
    let change_all = str_of(event, "propagate_mode") == Some("change_all");
    let old_key = ConversationKey::topic(old_stream, old_topic);
    let new_key = ConversationKey::topic(new_stream, new_topic);
    out.notices.push(HookEvent::TopicMoved {
        old_stream,
        old_topic: old_topic.to_owned(),
        new_stream,
        new_topic: new_topic.to_owned(),
        ids: ids.clone(),
        whole: change_all,
    });

    let latest_moved = ids
        .iter()
        .max()
        .and_then(|id| state.messages.get(*id).cloned());
    for key in state.messages.apply_move(&ids, &new_key) {
        out.emit(SessionEvent::TimelineChanged(key));
    }
    let unread_changed = state.unread.apply_move(&ids, &new_key);
    if !unread_changed.is_empty() {
        out.emit(SessionEvent::UnreadChanged(unread_changed));
    }
    for stream in [old_stream, new_stream] {
        if state.channels.invalidate_topics(stream) {
            out.emit(SessionEvent::TopicsChanged(stream));
        }
    }

    if old_key == new_key {
        state.conversations.set_title(&new_key, new_topic);
        out.emit(SessionEvent::ConversationChanged(new_key));
        return;
    }
    let listed = state.conversations.get(&old_key).is_some();
    if change_all && state.rekey_conversation(&old_key, &new_key, new_topic) {
        out.emit(SessionEvent::ContactsChanged);
    }
    if change_all && listed {
        out.emit(SessionEvent::ConversationRekeyed {
            old: old_key,
            new: new_key,
        });
    } else {
        // Partial move: when the old conversation's last message is moved away, fetch again; the new
        // conversation uses the newest moved message as its preview
        state.conversations.on_deleted(&ids);
        match latest_moved {
            Some(moved) => {
                let mut latest = latest_of(&moved);
                new_topic.clone_into(&mut latest.title);
                state.conversations.apply_latest(&new_key, &latest);
            }
            None => {
                let max = ids.iter().max().copied().unwrap_or_default();
                state
                    .conversations
                    .bootstrap_known(&new_key, max, new_topic, false);
            }
        }
    }
    out.emit(SessionEvent::ConversationsChanged);
    out.effects.push(Effect::Backfill);
}

fn on_delete_message(state: &mut State, event: &Value, out: &mut Applied) {
    let mut ids = ids_of(event, "message_ids");
    ids.extend(i64_of(event, "message_id"));
    let mut deleted_key = key_of_stream_event(event);
    for id in &ids {
        let deleted = state.messages.on_deleted(*id);
        if let Some((key, _)) = &deleted {
            deleted_key.get_or_insert_with(|| key.clone());
        }
        match deleted {
            Some((key, true)) => out.emit(SessionEvent::MessageUpdated { key, id: *id }),
            Some((key, false)) => out.emit(SessionEvent::MessageRemoved { key, id: *id }),
            None => {}
        }
    }
    out.notices.push(HookEvent::MessagesDeleted {
        key: deleted_key,
        ids: ids.clone(),
    });
    let changed = state.unread.remove_ids(&ids);
    if !changed.is_empty() {
        out.emit(SessionEvent::UnreadChanged(changed));
    }
    if state.notices.remove(&ids) {
        out.emit(SessionEvent::SystemNoticesChanged);
    }
    state.conversations.on_deleted(&ids);
    if let Some(stream) = i64_of(event, "stream_id")
        && state.channels.invalidate_topics(stream)
    {
        out.emit(SessionEvent::TopicsChanged(stream));
    }
    out.effects.push(Effect::Backfill);
}

fn on_flags(state: &mut State, event: &Value, out: &mut Applied) {
    let flag = str_of(event, "flag").unwrap_or_default();
    let add = str_of(event, "op") == Some("add");
    let all = event.get("all").and_then(Value::as_bool).unwrap_or(false);
    let ids = ids_of(event, "messages");

    let changed = if all && add {
        state.messages.apply_flag_all(flag)
    } else {
        state.messages.apply_flag(&ids, flag, add)
    };
    for (id, key) in changed {
        out.emit(SessionEvent::MessageUpdated { key, id });
    }

    if flag == "read" {
        out.notices.push(HookEvent::ReadChanged {
            ids: ids.clone(),
            read: add,
            all,
        });
    }
    match flag {
        "read" if add => {
            let keys = if all {
                state.unread.clear()
            } else {
                state.unread.remove_ids(&ids)
            };
            if !keys.is_empty() {
                out.emit(SessionEvent::UnreadChanged(keys));
            }
            let notices_changed = if all {
                state.notices.mark_all_read()
            } else {
                !state.notices.set_read(&ids, true).is_empty()
            };
            if notices_changed {
                out.emit(SessionEvent::SystemNoticesChanged);
            }
        }
        "read" => on_marked_unread(state, event, &ids, out),
        "starred" => {
            for id in &ids {
                out.emit(SessionEvent::StarredChanged {
                    id: *id,
                    starred: add,
                });
            }
            if !ids.is_empty() {
                out.effects.push(Effect::FavoriteCopies {
                    ids: ids.clone(),
                    starred: add,
                });
            }
        }
        _ => {}
    }
}

fn on_marked_unread(state: &mut State, event: &Value, ids: &[i64], out: &mut Applied) {
    if !state.notices.set_read(ids, false).is_empty() {
        out.emit(SessionEvent::SystemNoticesChanged);
    }
    let details = event.get("message_details");
    let mut keys = HashSet::new();
    for id in ids {
        if state.notices.contains(*id) {
            continue;
        }
        if let Some(message) = state.messages.get(*id).cloned() {
            if !state.messages.is_hidden(&message) && state.unread.add_message(&message) {
                keys.insert(message.key.clone());
            }
            continue;
        }
        let detail = details.and_then(|d| d.get(id.to_string()));
        match detail.and_then(|d| key_of_details(d, state.users.self_id)) {
            Some(key) => {
                let mention = detail
                    .and_then(|d| d.get("mentioned"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let kind = if mention {
                    MentionKind::Personal
                } else {
                    MentionKind::None
                };
                if state.unread.add_known(*id, &key, kind) {
                    keys.insert(key);
                }
            }
            None => out.effects.push(Effect::FetchUnread(*id)),
        }
    }
    if !keys.is_empty() {
        out.emit(SessionEvent::UnreadChanged(keys.into_iter().collect()));
    }
}

fn key_of_details(detail: &Value, self_id: i64) -> Option<ConversationKey> {
    match str_of(detail, "type")? {
        "stream" => Some(ConversationKey::topic(
            i64_of(detail, "stream_id")?,
            str_of(detail, "topic")?,
        )),
        _ => {
            let ids = ids_of(detail, "user_ids");
            if ids.is_empty() {
                Some(ConversationKey::dm([self_id], self_id))
            } else {
                Some(ConversationKey::dm(ids, self_id))
            }
        }
    }
}

fn on_reaction(state: &mut State, event: &Value, out: &mut Applied) {
    let Some(id) = i64_of(event, "message_id") else {
        return;
    };
    let reaction = Reaction {
        emoji_name: str_of(event, "emoji_name").unwrap_or_default().to_owned(),
        emoji_code: str_of(event, "emoji_code").unwrap_or_default().to_owned(),
        reaction_type: str_of(event, "reaction_type")
            .unwrap_or_default()
            .to_owned(),
        user_id: i64_of(event, "user_id").unwrap_or_default(),
    };
    let added = str_of(event, "op") == Some("add");
    let key = state.messages.apply_reaction(id, reaction.clone(), added);
    out.notices.push(HookEvent::ReactionChanged {
        key: key.clone(),
        id,
        reaction,
        added,
    });
    if let Some(key) = key {
        out.emit(SessionEvent::MessageUpdated { key, id });
    }
}

fn on_submessage(state: &mut State, event: &Value, out: &mut Applied) {
    let Some(id) = i64_of(event, "message_id") else {
        return;
    };
    let submessage = Submessage {
        id: i64_of(event, "submessage_id").unwrap_or_default(),
        message_id: id,
        sender_id: i64_of(event, "sender_id").unwrap_or_default(),
        msg_type: str_of(event, "msg_type").unwrap_or_default().to_owned(),
        content: str_of(event, "content").unwrap_or_default().to_owned(),
    };
    if let Some(key) = state.messages.apply_submessage(submessage) {
        out.emit(SessionEvent::MessageUpdated { key, id });
    }
}

fn on_realm_user(state: &mut State, event: &Value, out: &mut Applied) {
    let person = event.get("person");
    match str_of(event, "op").unwrap_or_default() {
        "add" => {
            let dto: UserDto = parse(person);
            let id = dto.user_id;
            state.users.upsert(user_from_dto(&dto));
            out.notices.push(HookEvent::UserChanged(id));
            out.emit(SessionEvent::UserChanged(id));
            out.emit(SessionEvent::UsersChanged);
        }
        "update" => {
            if let Some(person) = person
                && let Some(id) = state.users.apply_update(person)
            {
                out.notices.push(HookEvent::UserChanged(id));
                out.emit(SessionEvent::UserChanged(id));
                if person.get("is_active").is_some() {
                    out.emit(SessionEvent::UsersChanged);
                }
                if person.get("role").is_some() && id == state.users.self_id {
                    if let Some(role) = person.get("role").and_then(Value::as_i64) {
                        state.inputs.role = role as i32;
                    }
                    state.rebuild_permissions();
                    out.emit(SessionEvent::PermissionsChanged);
                }
            }
        }
        "remove" => {
            if let Some(id) = person.and_then(|p| i64_of(p, "user_id")) {
                state.users.deactivate(id);
                out.notices.push(HookEvent::UserChanged(id));
                out.emit(SessionEvent::UserChanged(id));
                out.emit(SessionEvent::UsersChanged);
            }
        }
        _ => {}
    }
}

fn apply_realm_setting(state: &mut State, name: &str, value: &Value, out: &mut Applied) {
    state.realm.apply(name, value);
    let mut permissions_changed = matches!(
        name,
        "allow_message_editing"
            | "message_content_edit_limit_seconds"
            | "message_content_delete_limit_seconds"
    );
    if name.starts_with("can_")
        && let Ok(setting) = serde_json::from_value::<GroupSettingValue>(value.clone())
    {
        state
            .inputs
            .realm_group_settings
            .insert(name.to_owned(), setting);
        permissions_changed = true;
    }
    if permissions_changed {
        state.rebuild_permissions();
        out.emit(SessionEvent::PermissionsChanged);
    }
}

fn on_realm(state: &mut State, event: &Value, out: &mut Applied) {
    let mut properties = Vec::new();
    match str_of(event, "op").unwrap_or_default() {
        "update" => {
            if let (Some(name), Some(value)) = (str_of(event, "property"), event.get("value")) {
                apply_realm_setting(state, name, value, out);
                properties.push(name.to_owned());
            }
        }
        "update_dict" => {
            if let Some(data) = event.get("data").and_then(Value::as_object) {
                for (name, value) in data {
                    apply_realm_setting(state, name, value, out);
                    properties.push(name.clone());
                }
            }
        }
        _ => return,
    }
    out.notices.push(HookEvent::RealmChanged { properties });
    out.emit(SessionEvent::RealmChanged);
}

fn on_user_settings(state: &mut State, event: &Value, out: &mut Applied) {
    let (Some(property), Some(value)) = (str_of(event, "property"), event.get("value")) else {
        return;
    };
    let draft_sync = state.settings.enable_drafts_synchronization;
    if apply_setting(state, property, value) {
        out.emit(SessionEvent::SettingsChanged(property.to_owned()));
    }
    if state.settings.enable_drafts_synchronization != draft_sync {
        out.effects.push(Effect::SwitchDraftSync(
            state.settings.enable_drafts_synchronization,
        ));
    }
}

/// Modifies the local value of a Zulip setting. Returns whether it is a known setting.
pub fn apply_setting(state: &mut State, property: &str, value: &Value) -> bool {
    let Ok(mut json) = serde_json::to_value(&state.settings) else {
        return false;
    };
    let Some(object) = json.as_object_mut() else {
        return false;
    };
    if !object.contains_key(property) {
        return false;
    }
    object.insert(property.to_owned(), value.clone());
    match serde_json::from_value(json) {
        Ok(settings) => {
            state.settings = settings;
            true
        }
        Err(_) => false,
    }
}

fn on_typing(state: &mut State, event: &Value, out: &mut Applied) {
    let Some(sender) = event.get("sender").and_then(|s| i64_of(s, "user_id")) else {
        return;
    };
    if sender == state.users.self_id || state.users.is_muted(sender) {
        return;
    }
    let key = if let Some(stream) = i64_of(event, "stream_id") {
        ConversationKey::topic(stream, str_of(event, "topic").unwrap_or_default())
    } else if let Some(recipients) = event.get("recipients").and_then(Value::as_array) {
        ConversationKey::dm(
            recipients.iter().filter_map(|r| i64_of(r, "user_id")),
            state.users.self_id,
        )
    } else {
        return;
    };
    if str_of(event, "op") == Some("start") {
        let expiry = Duration::from_millis(state.realm.typing_started_expiry_ms.max(1000) as u64);
        state.typing.start(&key, sender, expiry);
        out.effects.push(Effect::ExpireTyping(key.clone(), expiry));
        out.notices.push(HookEvent::TypingChanged {
            key: key.clone(),
            user_id: sender,
            typing: true,
        });
        out.emit(SessionEvent::TypingChanged(key));
    } else if state.typing.stop(&key, sender) {
        out.notices.push(HookEvent::TypingChanged {
            key: key.clone(),
            user_id: sender,
            typing: false,
        });
        out.emit(SessionEvent::TypingChanged(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::messages::PageKind;
    use crate::state::test_support::*;
    use serde_json::json;
    use zulip_core::model::PresenceState;
    use zulip_core::model::{ConversationNotice, PinItem, PinScope, RemarkTarget};
    use zulip_core::{ConversationFolder, ListEntry};

    fn message_event(id: i64, sender: i64, topic: &str, flags: Value) -> Value {
        json!({
            "type": "message", "flags": flags,
            "message": {
                "id": id, "sender_id": sender, "sender_full_name": "阿杰", "type": "stream",
                "stream_id": 10, "subject": topic, "content": format!("<p>内容{id}</p>"), "timestamp": id,
            }
        })
    }

    fn dm_event(id: i64, sender: i64, flags: Value) -> Value {
        json!({
            "type": "message", "flags": flags,
            "message": {
                "id": id, "sender_id": sender, "type": "private", "content": format!("<p>私聊{id}</p>"),
                "display_recipient": [{"id": 1}, {"id": sender}], "timestamp": id,
            }
        })
    }

    #[test]
    fn stream_message_updates_everything() {
        let mut s = state();
        let key = ConversationKey::topic(10, "接口");
        s.messages
            .merge_page(&key, vec![], PageKind::Newest, true, true);
        let out = apply_event(&mut s, &message_event(5, 2, "接口", json!([])));
        assert!(out.events.contains(&SessionEvent::MessageAdded {
            key: key.clone(),
            id: 5
        }));
        assert!(
            out.events
                .contains(&SessionEvent::UnreadChanged(vec![key.clone()]))
        );
        assert!(out.events.contains(&SessionEvent::ConversationsChanged));
        assert_eq!(s.unread.count(&key), 1);
        let entry = s.conversations.get(&key).unwrap();
        assert_eq!(
            (entry.last_message_id, entry.last_preview.as_str()),
            (5, "内容5")
        );
        // Channels by default do not pop up or make a sound, only flash the taskbar
        let notification = out.events.iter().find_map(|e| match e {
            SessionEvent::Notification(n) => Some(n),
            _ => None,
        });
        let n = notification.unwrap();
        assert!(!n.show_popup && !n.play_sound);
    }

    #[test]
    fn dm_notifies_and_viewing_marks_read_instead() {
        let mut s = state();
        let key = ConversationKey::dm([2], 1);
        let out = apply_event(&mut s, &dm_event(5, 2, json!([])));
        let notification = out.events.iter().find_map(|e| match e {
            SessionEvent::Notification(n) => Some(n.clone()),
            _ => None,
        });
        let n = notification.unwrap();
        assert!(n.show_popup && n.play_sound);
        assert_eq!((n.title.as_str(), n.body.as_str()), ("阿杰", "私聊5"));
        assert!(out.effects.is_empty());

        s.view.set_actively_viewing(&key, true);
        let out = apply_event(&mut s, &dm_event(6, 2, json!([])));
        assert!(
            !out.events
                .iter()
                .any(|e| matches!(e, SessionEvent::Notification(_)))
        );
        assert_eq!(out.effects, [Effect::MarkRead(vec![6])]);
    }

    #[test]
    fn mention_in_silent_conversation_uses_helper_slot() {
        let mut s = state();
        let key = ConversationKey::topic(10, "t");
        s.conversations
            .set_level(&key, zulip_core::push::DndLevel::Silent);
        let out = apply_event(&mut s, &message_event(5, 2, "t", json!([])));
        assert!(
            !out.events
                .iter()
                .any(|e| matches!(e, SessionEvent::Notification(_)))
        );
        let out = apply_event(&mut s, &message_event(6, 2, "t", json!(["mentioned"])));
        let n = out
            .events
            .iter()
            .find_map(|e| match e {
                SessionEvent::Notification(n) => Some(n.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(n.slot, ListEntry::Folder(ConversationFolder::Helper));
    }

    #[test]
    fn blocked_conversation_never_notifies_and_muted_sender_too() {
        let mut s = state();
        s.conversations.set_level(
            &ConversationKey::dm([2], 1),
            zulip_core::push::DndLevel::Blocked,
        );
        let out = apply_event(&mut s, &dm_event(5, 2, json!(["mentioned"])));
        assert!(
            !out.events
                .iter()
                .any(|e| matches!(e, SessionEvent::Notification(_)))
        );
        apply_event(
            &mut s,
            &json!({"type": "muted_users", "muted_users": [{"id": 3}]}),
        );
        let out = apply_event(&mut s, &dm_event(6, 3, json!([])));
        assert!(
            !out.events
                .iter()
                .any(|e| matches!(e, SessionEvent::Notification(_)))
        );
    }

    #[test]
    fn server_drafts_are_ignored_when_sync_is_off() {
        let mut s = state();
        s.settings.enable_drafts_synchronization = false;
        let out = apply_event(
            &mut s,
            &json!({"type": "drafts", "op": "add", "drafts": [
                {"id": 1, "type": "stream", "to": [10], "topic": "t", "content": "x"}
            ]}),
        );
        assert!(out.events.is_empty());
        assert!(s.drafts.get(&ConversationKey::topic(10, "t")).is_none());
    }

    #[test]
    fn local_echo_confirmed() {
        let mut s = state();
        let key = ConversationKey::topic(10, "接口");
        s.messages
            .merge_page(&key, vec![], PageKind::Newest, true, true);
        let (_, local_id) = s
            .messages
            .add_pending(ChatMessage::new(0, key.clone(), 1, 1), "x");
        let mut event = message_event(7, 1, "接口", json!(["read"]));
        event["local_message_id"] = json!(local_id);
        let out = apply_event(&mut s, &event);
        assert!(out.events.contains(&SessionEvent::MessageConfirmed {
            key: key.clone(),
            local_id: local_id.clone(),
            id: 7,
        }));
        assert!(s.messages.find_pending(&local_id).is_none());
        assert_eq!(s.unread.count(&key), 0);
    }

    #[test]
    fn bot_dm_goes_to_notices_not_unread() {
        let mut s = state();
        let event = json!({
            "type": "message", "flags": [],
            "message": {"id": 5, "sender_id": 9, "type": "private", "content": "<p>欢迎</p>",
                        "display_recipient": [{"id": 1}, {"id": 9}], "timestamp": 1}
        });
        let out = apply_event(&mut s, &event);
        assert!(out.events.contains(&SessionEvent::SystemNoticesChanged));
        assert_eq!(s.notices.notices().len(), 1);
        assert_eq!(s.unread.count(&ConversationKey::dm([9], 1)), 0);
        assert!(s.conversations.get(&ConversationKey::dm([9], 1)).is_none());
    }

    #[test]
    fn delivered_reminder_notifies() {
        let mut s = state();
        let event = json!({
            "type": "message", "flags": [],
            "message": {"id": 5, "sender_id": 9, "type": "private",
                        "content": format!("<p>{}</p>", zulip_core::notice::REMINDER_MARKER),
                        "display_recipient": [{"id": 1}, {"id": 9}], "timestamp": 1}
        });
        let out = apply_event(&mut s, &event);
        let n = out
            .events
            .iter()
            .find_map(|e| match e {
                SessionEvent::Notification(n) => Some(n.clone()),
                _ => None,
            })
            .unwrap();
        assert!(n.is_reminder);
        assert_eq!(n.title, "消息提醒");
        assert_eq!(s.notices.delivered_reminders().len(), 1);
    }

    #[test]
    fn inline_bot_notice_in_topic_is_not_preview() {
        let mut s = state();
        let key = ConversationKey::topic(10, "t");
        apply_event(&mut s, &message_event(5, 2, "t", json!(["read"])));
        apply_event(&mut s, &message_event(6, 9, "t", json!(["read"])));
        assert_eq!(s.conversations.get(&key).unwrap().last_message_id, 5);
    }

    #[test]
    fn plugin_hidden_message_is_not_shown() {
        let mut s = state();
        s.messages.hide(5);
        let event = message_event(5, 2, "t", json!([]));
        apply_event(&mut s, &event);
        assert_eq!(s.unread.count(&ConversationKey::topic(10, "t")), 0);
        assert!(
            s.conversations
                .get(&ConversationKey::topic(10, "t"))
                .is_none()
        );
    }

    #[test]
    fn edit_updates_message_preview_and_saves_version() {
        let db = test_db();
        let mut s = state_with(register_json(), &db);
        let key = ConversationKey::topic(10, "t");
        s.messages
            .merge_page(&key, vec![], PageKind::Newest, true, true);
        apply_event(&mut s, &message_event(5, 2, "t", json!([])));
        let out = apply_event(
            &mut s,
            &json!({
                "type": "update_message", "message_id": 5, "message_ids": [5], "edit_timestamp": 100,
                "rendered_content": "<p>新</p>", "orig_rendered_content": "<p>内容5</p>", "flags": ["mentioned"],
            }),
        );
        assert!(out.events.contains(&SessionEvent::MessageUpdated {
            key: key.clone(),
            id: 5
        }));
        let m = s.messages.get(5).unwrap();
        assert_eq!(m.content_html, "<p>新</p>");
        assert_eq!(m.last_edit_timestamp, Some(100));
        assert_eq!(s.conversations.get(&key).unwrap().last_preview, "新");
        assert_eq!(s.versions.load(5).unwrap(), ["<p>内容5</p>"]);
        assert_eq!(s.unread.notice(&key), ConversationNotice::AtMe);

        // Only re-rendered: does not count as an edit
        apply_event(
            &mut s,
            &json!({
                "type": "update_message", "message_id": 5, "rendering_only": true, "edit_timestamp": 200,
                "rendered_content": "<p>新2</p>", "orig_rendered_content": "<p>新</p>",
            }),
        );
        let m = s.messages.get(5).unwrap();
        assert_eq!(m.last_edit_timestamp, Some(100));
        assert_eq!(s.versions.load(5).unwrap().len(), 1);
    }

    #[test]
    fn move_all_rekeys_conversation_and_pins() {
        let mut s = state();
        let old = ConversationKey::topic(10, "旧");
        let new = ConversationKey::topic(10, "新");
        s.messages
            .merge_page(&old, vec![], PageKind::Newest, true, true);
        apply_event(&mut s, &message_event(5, 2, "旧", json!([])));
        let pin = |key: &ConversationKey| PinItem::Conversation(key.clone());
        s.pins.set(PinScope::Contacts, &pin(&old), true, 1);
        s.pins.set(PinScope::Messages, &pin(&old), true, 1);
        s.remarks
            .set(&RemarkTarget::Conversation(old.clone()), Some("备注"));
        s.view.save_scroll(
            &old,
            Some(crate::state::ScrollPosition {
                message_id: 5,
                offset: 0,
            }),
        );

        let out = apply_event(
            &mut s,
            &json!({
                "type": "update_message", "message_id": 5, "message_ids": [5], "stream_id": 10,
                "orig_subject": "旧", "subject": "新", "propagate_mode": "change_all", "edit_timestamp": 9,
            }),
        );
        assert!(out.events.contains(&SessionEvent::ConversationRekeyed {
            old: old.clone(),
            new: new.clone()
        }));
        assert!(s.conversations.get(&old).is_none());
        assert_eq!(s.conversations.get(&new).unwrap().title, "新");
        assert_eq!(
            s.remarks.get(&RemarkTarget::Conversation(new.clone())),
            Some("备注")
        );
        assert_eq!(s.pins.items(PinScope::Contacts), [&pin(&new)]);
        assert_eq!(s.pins.items(PinScope::Messages), [&pin(&new)]);
        assert_eq!(s.unread.ids(&new), [5]);
        assert!(s.view.scroll(&new).is_some());
    }

    #[test]
    fn partial_move_keeps_old_conversation_and_refetches_preview() {
        let mut s = state();
        let old = ConversationKey::topic(10, "旧");
        let new = ConversationKey::topic(11, "旧");
        s.messages
            .merge_page(&old, vec![], PageKind::Newest, true, true);
        apply_event(&mut s, &message_event(5, 2, "旧", json!([])));
        apply_event(&mut s, &message_event(6, 2, "旧", json!([])));
        let out = apply_event(
            &mut s,
            &json!({
                "type": "update_message", "message_id": 6, "message_ids": [6], "stream_id": 10,
                "new_stream_id": 11, "orig_subject": "旧", "propagate_mode": "change_one", "edit_timestamp": 9,
            }),
        );
        assert!(out.effects.contains(&Effect::Backfill));
        assert_eq!(s.conversations.get(&new).unwrap().last_message_id, 6);
        assert_eq!(s.conversations.next_backfill(), Some(old.clone()));
        assert_eq!(s.unread.ids(&old), [5]);
        assert_eq!(s.unread.ids(&new), [6]);
    }

    #[test]
    fn case_only_rename_updates_title() {
        let mut s = state();
        let key = ConversationKey::topic(10, "abc");
        apply_event(&mut s, &message_event(5, 2, "abc", json!([])));
        let out = apply_event(
            &mut s,
            &json!({
                "type": "update_message", "message_id": 5, "message_ids": [5], "stream_id": 10,
                "orig_subject": "abc", "subject": "ABC", "propagate_mode": "change_all",
            }),
        );
        assert!(
            out.events
                .contains(&SessionEvent::ConversationChanged(key.clone()))
        );
        assert_eq!(s.conversations.get(&key).unwrap().title, "ABC");
    }

    #[test]
    fn delete_marks_and_queues_backfill() {
        let mut s = state();
        let key = ConversationKey::topic(10, "t");
        s.messages
            .merge_page(&key, vec![], PageKind::Newest, true, true);
        apply_event(&mut s, &message_event(5, 2, "t", json!([])));
        let out = apply_event(
            &mut s,
            &json!({"type": "delete_message", "message_id": 5, "stream_id": 10}),
        );
        assert!(out.events.contains(&SessionEvent::MessageUpdated {
            key: key.clone(),
            id: 5
        }));
        assert!(s.messages.get(5).unwrap().is_deleted());
        assert_eq!(s.unread.count(&key), 0);
        assert_eq!(out.effects, [Effect::Backfill]);
        assert_eq!(s.conversations.next_backfill(), Some(key));
    }

    #[test]
    fn read_flags() {
        let mut s = state();
        let key = ConversationKey::topic(10, "t");
        apply_event(&mut s, &message_event(5, 2, "t", json!([])));
        apply_event(&mut s, &message_event(6, 2, "t", json!([])));
        let read = json!({"type": "update_message_flags", "op": "add", "flag": "read", "messages": [5], "all": false});
        let out = apply_event(&mut s, &read);
        assert!(
            out.events
                .contains(&SessionEvent::UnreadChanged(vec![key.clone()]))
        );
        assert_eq!(s.unread.ids(&key), [6]);

        let unread = json!({
            "type": "update_message_flags", "op": "remove", "flag": "read", "messages": [5, 8], "all": false,
            "message_details": {"5": {"type": "stream", "stream_id": 10, "topic": "t", "mentioned": true}},
        });
        let out = apply_event(&mut s, &unread);
        assert_eq!(s.unread.ids(&key), [5, 6]);
        assert_eq!(s.unread.notice(&key), ConversationNotice::AtMe);
        assert_eq!(out.effects, [Effect::FetchUnread(8)]);

        let all = json!({"type": "update_message_flags", "op": "add", "flag": "read", "messages": [], "all": true});
        apply_event(&mut s, &all);
        assert!(s.unread.keys().is_empty());
    }

    #[test]
    fn starred_flag() {
        let mut s = state();
        let event = json!({"type": "update_message_flags", "op": "add", "flag": "starred", "messages": [5]});
        let out = apply_event(&mut s, &event);
        assert!(out.events.contains(&SessionEvent::StarredChanged {
            id: 5,
            starred: true
        }));
    }

    #[test]
    fn reactions_and_submessages() {
        let mut s = state();
        let key = ConversationKey::topic(10, "t");
        s.messages
            .merge_page(&key, vec![], PageKind::Newest, true, true);
        apply_event(&mut s, &message_event(5, 2, "t", json!([])));
        apply_event(
            &mut s,
            &json!({"type": "reaction", "op": "add", "message_id": 5, "user_id": 2,
            "emoji_name": "+1", "emoji_code": "1f44d", "reaction_type": "unicode_emoji"}),
        );
        assert_eq!(s.messages.get(5).unwrap().reactions.len(), 1);
        apply_event(
            &mut s,
            &json!({"type": "reaction", "op": "remove", "message_id": 5, "user_id": 2,
            "emoji_name": "+1", "emoji_code": "1f44d", "reaction_type": "unicode_emoji"}),
        );
        assert!(s.messages.get(5).unwrap().reactions.is_empty());
        apply_event(
            &mut s,
            &json!({"type": "submessage", "message_id": 5, "submessage_id": 3,
            "sender_id": 2, "msg_type": "widget", "content": "{}"}),
        );
        assert_eq!(s.messages.get(5).unwrap().submessages[0].id, 3);
    }

    #[test]
    fn settings_and_realm() {
        let mut s = state();
        let out = apply_event(
            &mut s,
            &json!({"type": "user_settings", "op": "update", "property": "enter_sends", "value": true}),
        );
        assert!(s.settings.enter_sends);
        assert_eq!(
            out.events,
            [SessionEvent::SettingsChanged("enter_sends".into())]
        );
        let out = apply_event(
            &mut s,
            &json!({"type": "user_settings", "property": "enable_drafts_synchronization", "value": false}),
        );
        assert_eq!(out.effects, [Effect::SwitchDraftSync(false)]);
        let out = apply_event(
            &mut s,
            &json!({"type": "realm", "op": "update_dict", "data": {"name": "新名", "allow_message_editing": false}}),
        );
        assert_eq!(s.realm.name, "新名");
        assert!(!s.inputs.allow_message_editing);
        assert!(out.events.contains(&SessionEvent::PermissionsChanged));
    }

    #[test]
    fn typing_ignores_self_and_muted() {
        let mut s = state();
        let key = ConversationKey::dm([2], 1);
        let event = json!({"type": "typing", "op": "start", "sender": {"user_id": 2}, "recipients": [{"user_id": 1}, {"user_id": 2}]});
        let out = apply_event(&mut s, &event);
        assert_eq!(s.typing.users(&key), [2]);
        assert!(matches!(out.effects[0], Effect::ExpireTyping(_, _)));
        let stop = json!({"type": "typing", "op": "stop", "sender": {"user_id": 2}, "recipients": [{"user_id": 1}, {"user_id": 2}]});
        apply_event(&mut s, &stop);
        assert!(s.typing.users(&key).is_empty());
        let own = json!({"type": "typing", "op": "start", "sender": {"user_id": 1}, "stream_id": 10, "topic": "t"});
        assert!(apply_event(&mut s, &own).events.is_empty());
    }

    #[test]
    fn users_presence_and_groups() {
        let mut s = state();
        apply_event(
            &mut s,
            &json!({"type": "realm_user", "op": "add", "person": {"user_id": 5, "full_name": "新人"}}),
        );
        assert_eq!(s.users.name_of(5), "新人");
        apply_event(
            &mut s,
            &json!({"type": "realm_user", "op": "update", "person": {"user_id": 5, "full_name": "改名"}}),
        );
        assert_eq!(s.users.name_of(5), "改名");
        apply_event(
            &mut s,
            &json!({"type": "realm_user", "op": "remove", "person": {"user_id": 5}}),
        );
        assert!(!s.users.get(5).unwrap().is_active);
        let out = apply_event(
            &mut s,
            &json!({"type": "presence", "presences": {"2": {"active_timestamp": 1000, "idle_timestamp": 1000}}}),
        );
        assert_eq!(out.events, [SessionEvent::PresenceChanged(vec![2])]);
        assert_eq!(s.users.presence(2, 1000), PresenceState::Online);
        apply_event(
            &mut s,
            &json!({"type": "user_status", "user_id": 2, "status_text": "开会"}),
        );
        assert_eq!(s.users.status_text(2), Some("开会"));
        let out = apply_event(
            &mut s,
            &json!({"type": "user_group", "op": "add", "group": {"id": 7, "name": "g"}}),
        );
        assert_eq!(out.events, [SessionEvent::PermissionsChanged]);
    }

    #[test]
    fn unknown_events_are_ignored() {
        let mut s = state();
        assert!(
            apply_event(&mut s, &json!({"type": "heartbeat"}))
                .events
                .is_empty()
        );
        assert!(
            apply_event(&mut s, &json!({"type": "message"}))
                .events
                .is_empty()
        );
    }
}
