//! Channels and topics: listing, subscribing, topic operations.

use std::ffi::c_char;

use serde_json::{Value, json};
use zulip_core::ConversationKey;
use zulip_core::model::Channel;
use zulip_core::push::TopicVisibility;
use zulip_plugin_sdk::abi::{
    self, ChannelCb, Ctx, RawChannel, RawTopic, StringCb, TopicCb, UserData,
};
use zulip_plugin_sdk::str_of;
use zulip_session::topic::is_resolved_name;
use zulip_session::{DeleteTarget, TopicDestination};

use super::{Fail, guard, invalid, run32, send_json};
use crate::marshal::{Scope, read_text};
use crate::permission;

fn channel_to_raw(channel: &Channel, scope: &mut Scope) -> RawChannel {
    RawChannel {
        struct_size: size_of::<RawChannel>() as u32,
        stream_id: channel.stream_id,
        name: scope.text(&channel.name),
        description: scope.text(&channel.description),
        is_private: i32::from(channel.invite_only),
        is_subscribed: i32::from(channel.is_subscribed()),
    }
}

pub(super) unsafe extern "C" fn get_channels(
    ctx: Ctx,
    cb: Option<ChannelCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_channels",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let Some(cb) = cb else {
                return invalid();
            };
            let channels = call.session()?.subscribed_channels();
            for channel in &channels {
                let mut scope = Scope::default();
                let raw = channel_to_raw(channel, &mut scope);
                // SAFETY: raw is valid while the scope is alive.
                unsafe { cb(&raw, user) };
            }
            Ok(channels.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn get_topics(
    ctx: Ctx,
    stream_id: i64,
    cb: Option<TopicCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_topics",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let Some(cb) = cb else {
                return invalid();
            };
            let session = call.session()?;
            if !session
                .channel(stream_id)
                .is_some_and(|c| c.is_subscribed())
            {
                return Err(Fail::Code(abi::ERR_NOT_FOUND));
            }
            if session.topics(stream_id).is_none() {
                call.wait(session.load_topics(stream_id))?;
            }
            let topics = session.topics(stream_id).unwrap_or_default();
            for topic in &topics {
                let mut scope = Scope::default();
                let raw = RawTopic {
                    struct_size: size_of::<RawTopic>() as u32,
                    stream_id,
                    max_message_id: topic.max_id,
                    name: scope.text(&topic.name),
                };
                // SAFETY: raw is valid while the scope is alive.
                unsafe { cb(&raw, user) };
            }
            Ok(topics.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn update_channel(
    ctx: Ctx,
    stream_id: i64,
    name: *const c_char,
    description: *const c_char,
) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let (name, description) = unsafe { (read_text(name), read_text(description)) };
    run32(
        ctx,
        "update_channel",
        Some(permission::MANAGE_CHANNEL),
        Box::new(move |call| {
            let name = name.as_deref().map(str::trim);
            if stream_id <= 0
                || (name.is_none() && description.is_none())
                || name.is_some_and(str::is_empty)
            {
                return invalid();
            }
            let mut args = Vec::new();
            if let Some(name) = name {
                args.push(("new_name", Value::from(name)));
            }
            if let Some(description) = description {
                args.push(("description", Value::from(description)));
            }
            call.wait(call.session()?.update_channel(stream_id, &args))?;
            Ok(0)
        }),
    )
}

// Parameters for operations that take a channel and a topic.
fn topic_key(stream_id: i64, topic: Option<String>) -> Result<ConversationKey, Fail> {
    match topic.filter(|t| !t.is_empty()) {
        Some(topic) if stream_id > 0 => Ok(ConversationKey::topic(stream_id, topic)),
        _ => invalid(),
    }
}

pub(super) unsafe extern "C" fn get_all_channels(
    ctx: Ctx,
    cb: Option<ChannelCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_all_channels",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let Some(cb) = cb else {
                return invalid();
            };
            let channels = call.session()?.channels();
            for channel in &channels {
                let mut scope = Scope::default();
                let raw = channel_to_raw(channel, &mut scope);
                // SAFETY: raw is valid while the scope is alive.
                unsafe { cb(&raw, user) };
            }
            Ok(channels.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn get_topic_visibility(
    ctx: Ctx,
    stream_id: i64,
    topic: *const c_char,
) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let topic = unsafe { read_text(topic) };
    run32(
        ctx,
        "get_topic_visibility",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let topic = topic.filter(|t| !t.is_empty());
            let (Some(topic), true) = (topic, stream_id > 0) else {
                return invalid();
            };
            Ok(call.session()?.topic_visibility(stream_id, &topic) as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn move_topic(
    ctx: Ctx,
    stream_id: i64,
    topic: *const c_char,
    new_stream_id: i64,
    new_topic: *const c_char,
) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let (topic, new_topic) = unsafe { (read_text(topic), read_text(new_topic)) };
    run32(
        ctx,
        "move_topic",
        Some(permission::MANAGE_TOPIC),
        Box::new(move |call| {
            let key = topic_key(stream_id, topic)?;
            let new_topic = new_topic
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let (Some(new_topic), true) = (new_topic, new_stream_id >= 0) else {
                return invalid();
            };
            let destination = TopicDestination {
                stream_id: if new_stream_id == 0 {
                    stream_id
                } else {
                    new_stream_id
                },
                topic: new_topic.to_owned(),
            };
            call.wait(call.session()?.move_topic(&key, &destination))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_topic_resolved(
    ctx: Ctx,
    stream_id: i64,
    topic: *const c_char,
    resolved: i32,
) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let topic = unsafe { read_text(topic) };
    run32(
        ctx,
        "set_topic_resolved",
        Some(permission::MANAGE_TOPIC),
        Box::new(move |call| {
            let key = topic_key(stream_id, topic)?;
            let session = call.session()?;
            if is_resolved_name(&session.topic_name(&key)) != (resolved != 0) {
                call.wait(session.toggle_topic_resolved(&key))?;
            }
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn delete_topic(
    ctx: Ctx,
    stream_id: i64,
    topic: *const c_char,
) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let topic = unsafe { read_text(topic) };
    run32(
        ctx,
        "delete_topic",
        Some(permission::MANAGE_TOPIC),
        Box::new(move |call| {
            let key = topic_key(stream_id, topic)?;
            call.wait(call.session()?.delete(DeleteTarget::Topic(key)))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_topic_visibility(
    ctx: Ctx,
    stream_id: i64,
    topic: *const c_char,
    visibility: i32,
) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let topic = unsafe { read_text(topic) };
    run32(
        ctx,
        "set_topic_visibility",
        Some(permission::MANAGE_SUBSCRIPTION),
        Box::new(move |call| {
            let key = topic_key(stream_id, topic)?;
            let Some(visibility) = TopicVisibility::from_i32(visibility) else {
                return invalid();
            };
            call.wait(call.session()?.set_topic_visibility(&key, visibility))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_subscribed(ctx: Ctx, stream_id: i64, subscribed: i32) -> i32 {
    run32(
        ctx,
        "set_subscribed",
        Some(permission::MANAGE_SUBSCRIPTION),
        Box::new(move |call| {
            let session = call.session()?;
            let channel = session
                .channel(stream_id)
                .ok_or(Fail::Code(abi::ERR_NOT_FOUND))?;
            let names = [channel.name];
            if subscribed != 0 {
                call.wait(session.subscribe_channel(&names))?;
            } else {
                call.wait(session.unsubscribe_channel(&names))?;
            }
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_subscription_property(
    ctx: Ctx,
    stream_id: i64,
    name: *const c_char,
    json: *const c_char,
) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let (name, value) = unsafe { (read_text(name), read_text(json)) };
    run32(
        ctx,
        "set_subscription_property",
        Some(permission::MANAGE_SUBSCRIPTION),
        Box::new(move |call| {
            let name = name.filter(|n| !n.is_empty());
            let value = value.and_then(|v| serde_json::from_str::<Value>(&v).ok());
            let (Some(name), Some(value), true) = (name, value, stream_id > 0) else {
                return invalid();
            };
            call.wait(
                call.session()?
                    .set_subscription_property(stream_id, &name, value),
            )?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn create_channel(ctx: Ctx, json: *const c_char) -> i64 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let text = unsafe { read_text(json) };
    guard(
        ctx,
        "create_channel",
        Some(permission::MANAGE_SUBSCRIPTION),
        Box::new(move |call| {
            let value: Value = text
                .as_deref()
                .and_then(|t| serde_json::from_str(t).ok())
                .unwrap_or(Value::Null);
            let name = str_of(&value, "name");
            if name.trim().is_empty() {
                return invalid();
            }
            let subscribers: Vec<i64> = value["subscribers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .collect();
            Ok(call.wait(call.session()?.create_channel(
                name,
                str_of(&value, "description"),
                value["private"].as_bool().unwrap_or(false),
                &subscribers,
            ))?)
        }),
    )
}

pub(super) unsafe extern "C" fn get_subscribers(
    ctx: Ctx,
    stream_id: i64,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_subscribers",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            if stream_id <= 0 {
                return invalid();
            }
            let ids = call.wait(call.session()?.load_subscribers(stream_id))?;
            send_json(cb, &json!(ids), user)?;
            Ok(ids.len() as i64)
        }),
    )
}
