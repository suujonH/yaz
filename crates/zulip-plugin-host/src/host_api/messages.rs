//! 消息：发送、读取、搜索、修改、定时与附加内容。

use std::ffi::c_char;

use serde_json::{Value, json};
use zulip_api::{MessageQuery, NarrowTerm, OutgoingMessage};
use zulip_core::model::ChatMessage;
use zulip_plugin_sdk::abi::{self, Ctx, MessageCb, RawConversation, StringCb, UserData};
use zulip_session::convert::message_from_dto;
use zulip_session::{DeleteTarget, MessagePager, Session};

use super::{Call, Fail, guard, invalid, required, run32, send_json, send_string};
use crate::annotation::PluginAnnotation;
use crate::marshal::{Conversation, MessageSnapshot, Scope, conversation_json, read_text};
use crate::permission;
use crate::request;
use crate::thread;

fn send(call: &Call<'_>, message: OutgoingMessage) -> Result<i64, Fail> {
    let session = call.session()?;
    Ok(call.wait(session.client().send_message(&message))?.id)
}

pub(super) unsafe extern "C" fn send_channel_message(
    ctx: Ctx,
    stream_id: i64,
    topic: *const c_char,
    markdown: *const c_char,
) -> i64 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let (topic, content) = unsafe { (read_text(topic), read_text(markdown)) };
    guard(
        ctx,
        "send_channel_message",
        Some(permission::SEND_MESSAGE),
        Box::new(move |call| {
            let content = content.filter(|c| !c.trim().is_empty());
            let (Some(topic), Some(content), true) = (topic, content, stream_id > 0) else {
                return invalid();
            };
            send(
                call,
                OutgoingMessage {
                    stream_id: Some(stream_id),
                    topic: Some(topic),
                    content,
                    ..OutgoingMessage::default()
                },
            )
        }),
    )
}

pub(super) unsafe extern "C" fn send_direct_message(
    ctx: Ctx,
    user_ids: *const i64,
    count: i32,
    markdown: *const c_char,
) -> i64 {
    // SAFETY: 插件传入 count 个数和以 0 结尾的字符串。
    let (ids, content) = unsafe {
        let ids = match count {
            0 => Some(Vec::new()),
            count if count > 0 && !user_ids.is_null() => {
                Some(std::slice::from_raw_parts(user_ids, count as usize).to_vec())
            }
            _ => None,
        };
        (ids, read_text(markdown))
    };
    guard(
        ctx,
        "send_direct_message",
        Some(permission::SEND_MESSAGE),
        Box::new(move |call| {
            let content = content.filter(|c| !c.trim().is_empty());
            let (Some(mut ids), Some(content)) = (ids, content) else {
                return invalid();
            };
            if !ids.iter().all(|id| *id > 0) {
                return invalid();
            }
            ids.sort_unstable();
            ids.dedup();
            // 空数组是与自己私聊。
            if ids.is_empty() {
                ids.push(call.session()?.self_id());
            }
            send(
                call,
                OutgoingMessage {
                    user_ids: ids,
                    content,
                    ..OutgoingMessage::default()
                },
            )
        }),
    )
}

fn send_message(cb: MessageCb, message: &ChatMessage, session: &Session, user: UserData) -> bool {
    let Some(snapshot) = MessageSnapshot::new(message, session, true) else {
        return false;
    };
    let mut scope = Scope::default();
    let raw = snapshot.to_raw(&mut scope);
    // SAFETY: raw 在 scope 存活期间有效。
    unsafe { cb(&raw, user) };
    true
}

pub(super) unsafe extern "C" fn get_message(
    ctx: Ctx,
    message_id: i64,
    cb: Option<MessageCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_message",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let (Some(cb), true) = (cb, message_id > 0) else {
                return invalid();
            };
            let session = call.session()?;
            let dto = call.wait(session.client().get_message(message_id, true))?;
            let message = message_from_dto(&dto, session.self_id());
            if send_message(cb, &message, session, user) {
                Ok(0)
            } else {
                Err(Fail::Code(abi::ERR_NOT_FOUND))
            }
        }),
    )
}

pub(super) unsafe extern "C" fn get_messages(
    ctx: Ctx,
    conv: *const RawConversation,
    anchor: i64,
    num_before: i32,
    num_after: i32,
    cb: Option<MessageCb>,
    user: UserData,
) -> i32 {
    // SAFETY: conv 为空或指向插件提供的有效结构。
    let conversation = unsafe { conv.as_ref().map(|c| Conversation::from_raw(c)) };
    run32(
        ctx,
        "get_messages",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let Some(cb) = cb else {
                return invalid();
            };
            let session = call.session()?;
            let narrow = match conversation {
                None => Vec::new(),
                Some(None) => return invalid(),
                Some(Some(Conversation::Channel {
                    stream_id, topic, ..
                })) => vec![NarrowTerm::channel(stream_id), NarrowTerm::topic(&topic)],
                Some(Some(Conversation::Direct { user_ids })) if user_ids.is_empty() => {
                    vec![NarrowTerm::dm(&[session.self_id()])]
                }
                Some(Some(Conversation::Direct { user_ids })) => vec![NarrowTerm::dm(&user_ids)],
            };
            let anchor = match anchor {
                0 => "newest".to_owned(),
                -1 => "oldest".to_owned(),
                id if id > 0 => id.to_string(),
                _ => return invalid(),
            };
            let query = MessageQuery::new(
                narrow,
                anchor,
                num_before.clamp(0, 1000) as u32,
                num_after.clamp(0, 1000) as u32,
            );
            let result = call.wait(session.client().get_messages(&query))?;
            let mut count = 0;
            for dto in &result.messages {
                let message = message_from_dto(dto, session.self_id());
                if send_message(cb, &message, session, user) {
                    count += 1;
                }
            }
            Ok(count)
        }),
    )
}

pub(super) unsafe extern "C" fn add_reaction(
    ctx: Ctx,
    message_id: i64,
    emoji_name: *const c_char,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let name = unsafe { read_text(emoji_name) };
    run32(
        ctx,
        "add_reaction",
        Some(permission::MODIFY_MESSAGE),
        Box::new(move |call| {
            let name = name.filter(|n| !n.is_empty());
            let (Some(name), true) = (name, message_id > 0) else {
                return invalid();
            };
            let session = call.session()?;
            let catalog = session.emoji_catalog();
            let emoji = catalog.get(&name).ok_or(Fail::Code(abi::ERR_NOT_FOUND))?;
            call.wait(session.client().add_reaction(
                message_id,
                &emoji.name,
                &emoji.code,
                emoji.kind.reaction_type(),
            ))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_starred(ctx: Ctx, message_id: i64, starred: i32) -> i32 {
    run32(
        ctx,
        "set_starred",
        Some(permission::MODIFY_MESSAGE),
        Box::new(move |call| {
            if message_id <= 0 {
                return invalid();
            }
            call.wait(call.session()?.set_starred(&[message_id], starred != 0))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn mark_read(ctx: Ctx, message_id: i64) -> i32 {
    run32(
        ctx,
        "mark_read",
        Some(permission::MODIFY_MESSAGE),
        Box::new(move |call| {
            if message_id <= 0 {
                return invalid();
            }
            call.wait(call.session()?.mark_read(&[message_id]))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_outgoing_content(ctx: Ctx, markdown: *const c_char) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let content = unsafe { read_text(markdown) };
    run32(
        ctx,
        "set_outgoing_content",
        Some(permission::MODIFY_MESSAGE),
        Box::new(move |call| {
            let Some(content) = content else {
                return invalid();
            };
            if call.host.is_plugin_thread() && thread::try_set_outgoing(content) {
                Ok(0)
            } else {
                Err(Fail::Code(abi::ERR_WRONG_CONTEXT))
            }
        }),
    )
}

pub(super) unsafe extern "C" fn set_annotation(
    ctx: Ctx,
    message_id: i64,
    json: *const c_char,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(json) };
    run32(
        ctx,
        "set_annotation",
        Some(permission::ANNOTATE_MESSAGE),
        Box::new(move |call| {
            if message_id <= 0 {
                return invalid();
            }
            let plugin = call.plugin;
            let mut annotation = None;
            if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                annotation = PluginAnnotation::parse(&plugin.id(), &text).map_err(|error| {
                    call.warn(format!("set_annotation：{error}"));
                    Fail::Code(abi::ERR_INVALID_ARG)
                })?;
            }
            if let Some(a) = &annotation {
                let missing = if !a.replace.is_empty()
                    && !plugin.has_permission(permission::RENDER_MESSAGE)
                {
                    Some(permission::RENDER_MESSAGE)
                } else if a.actions.iter().any(|x| x.is_fill())
                    && !plugin.has_permission(permission::FILL_COMPOSER)
                {
                    Some(permission::FILL_COMPOSER)
                } else if a.actions.iter().any(|x| !x.is_fill())
                    && !plugin.has_permission(permission::LAUNCH)
                {
                    Some(permission::LAUNCH)
                } else {
                    None
                };
                if let Some(missing) = missing {
                    call.warn(format!("set_annotation：plugin.json 未声明权限 {missing}"));
                    return Err(Fail::Code(abi::ERR_PERMISSION));
                }
            }
            let account_id = call.session()?.account_id();
            call.host
                .set_annotation(account_id, message_id, &plugin.id(), annotation);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn edit_message(
    ctx: Ctx,
    message_id: i64,
    markdown: *const c_char,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let content = unsafe { read_text(markdown) };
    run32(
        ctx,
        "edit_message",
        Some(permission::EDIT_MESSAGE),
        Box::new(move |call| {
            let content = content.filter(|c| !c.trim().is_empty());
            let (Some(content), true) = (content, message_id > 0) else {
                return invalid();
            };
            call.wait(call.session()?.edit_message(message_id, &content))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn delete_message(ctx: Ctx, message_id: i64) -> i32 {
    run32(
        ctx,
        "delete_message",
        Some(permission::DELETE_MESSAGE),
        Box::new(move |call| {
            if message_id <= 0 {
                return invalid();
            }
            call.wait(call.session()?.delete(DeleteTarget::Message(message_id)))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn search_messages(
    ctx: Ctx,
    query: *const c_char,
    next_cursor: *mut i64,
    cb: Option<MessageCb>,
    user: UserData,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(query) };
    let next_cursor = next_cursor as usize;
    run32(
        ctx,
        "search_messages",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let Some(cb) = cb else {
                return invalid();
            };
            let session = call.session()?;
            let Some(query) = text
                .as_deref()
                .and_then(|t| request::parse_search(t, session.self_id()))
            else {
                return invalid();
            };
            let mut pager = match query.cursor {
                Some(cursor) => MessagePager::resume(query.search, query.page_size, cursor),
                None => MessagePager::with_page_size(query.search, query.page_size),
            };
            let items = call.wait(session.next_page(&mut pager))?;
            let mut count = 0;
            for item in &items {
                if send_message(cb, &item.message, session, user) {
                    count += 1;
                }
            }
            let next = if pager.is_done() {
                0
            } else {
                pager.cursor().unwrap_or_default()
            };
            if next_cursor != 0 {
                // SAFETY: 不为空时插件传入有效的指针，调用期间有效。
                unsafe { *(next_cursor as *mut i64) = next };
            }
            Ok(count)
        }),
    )
}

pub(super) unsafe extern "C" fn get_edit_history(
    ctx: Ctx,
    message_id: i64,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_edit_history",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            if message_id <= 0 {
                return invalid();
            }
            let history = call.wait(call.session()?.message_history(message_id))?;
            send_json(cb, &json!(history), user)?;
            Ok(history.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn remove_reaction(
    ctx: Ctx,
    message_id: i64,
    emoji_name: *const c_char,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let name = unsafe { read_text(emoji_name) };
    run32(
        ctx,
        "remove_reaction",
        Some(permission::MODIFY_MESSAGE),
        Box::new(move |call| {
            let name = name.filter(|n| !n.is_empty());
            let (Some(name), true) = (name, message_id > 0) else {
                return invalid();
            };
            let session = call.session()?;
            let catalog = session.emoji_catalog();
            let emoji = catalog.get(&name).ok_or(Fail::Code(abi::ERR_NOT_FOUND))?;
            call.wait(session.client().remove_reaction(
                message_id,
                &emoji.name,
                &emoji.code,
                emoji.kind.reaction_type(),
            ))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn mark_unread(ctx: Ctx, message_id: i64) -> i32 {
    run32(
        ctx,
        "mark_unread",
        Some(permission::MODIFY_MESSAGE),
        Box::new(move |call| {
            if message_id <= 0 {
                return invalid();
            }
            let session = call.session()?;
            let key = match session.message(message_id) {
                Some(message) => message.key.clone(),
                None => {
                    let dto = call.wait(session.client().get_message(message_id, false))?;
                    message_from_dto(&dto, session.self_id()).key
                }
            };
            call.wait(session.mark_unread_from(&key, message_id))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn schedule_message(
    ctx: Ctx,
    conv: *const RawConversation,
    markdown: *const c_char,
    deliver_at: i64,
) -> i64 {
    // SAFETY: conv 为空或指向插件提供的有效结构；插件传入以 0 结尾的字符串。
    let (conversation, content) = unsafe {
        (
            conv.as_ref().and_then(|c| Conversation::from_raw(c)),
            read_text(markdown),
        )
    };
    guard(
        ctx,
        "schedule_message",
        Some(permission::SEND_MESSAGE),
        Box::new(move |call| {
            let content = content.filter(|c| !c.trim().is_empty());
            let (Some(conversation), Some(content), true) = (conversation, content, deliver_at > 0)
            else {
                return invalid();
            };
            let session = call.session()?;
            let key = conversation.to_key(session.self_id());
            Ok(call.wait(session.schedule_message(&key, &content, deliver_at))?)
        }),
    )
}

pub(super) unsafe extern "C" fn get_message_source(
    ctx: Ctx,
    message_id: i64,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_message_source",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let cb = required(cb)?;
            if message_id <= 0 {
                return invalid();
            }
            let source = call.wait(call.session()?.raw_content(message_id))?;
            send_string(cb, &source, user);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn get_scheduled_messages(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_scheduled_messages",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let session = call.session()?;
            let list: Vec<Value> = session
                .scheduled_messages()
                .iter()
                .map(|m| {
                    json!({
                        "id": m.id,
                        "conv": conversation_json(Some(&m.key), session),
                        "content": m.content,
                        "rendered_content": m.rendered_content,
                        "deliver_at": m.deliver_at,
                        "failed": m.failed,
                    })
                })
                .collect();
            send_json(cb, &json!(list), user)?;
            Ok(list.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn cancel_scheduled_message(ctx: Ctx, id: i64) -> i32 {
    run32(
        ctx,
        "cancel_scheduled_message",
        Some(permission::SEND_MESSAGE),
        Box::new(move |call| {
            if id <= 0 {
                return invalid();
            }
            call.wait(call.session()?.delete(DeleteTarget::ScheduledMessage(id)))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn get_read_receipts(
    ctx: Ctx,
    message_id: i64,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_read_receipts",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            if message_id <= 0 {
                return invalid();
            }
            let ids = call.wait(call.session()?.read_receipts(message_id))?;
            send_json(cb, &json!(ids), user)?;
            Ok(ids.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn get_message_actions(
    ctx: Ctx,
    message_id: i64,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_message_actions",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let session = call.session()?;
            if session.message(message_id).is_none() {
                return Err(Fail::Code(abi::ERR_NOT_FOUND));
            }
            let actions = session.message_actions(message_id);
            send_json(
                cb,
                &json!({
                    "can_edit": actions.can_edit,
                    "can_delete": actions.can_delete,
                    "can_show_edit_history": actions.can_show_edit_history,
                    "can_show_read_receipts": actions.can_show_read_receipts,
                }),
                user,
            )?;
            Ok(0)
        }),
    )
}
