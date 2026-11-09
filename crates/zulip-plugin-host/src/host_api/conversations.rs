//! Conversations: current conversation, opening, pinning and reminder level, drafts.

use std::ffi::c_char;

use serde_json::{Value, json};
use zulip_core::model::{PinItem, PinScope};
use zulip_core::push::DndLevel;
use zulip_core::{ConversationFolder, ConversationKey};
use zulip_plugin_sdk::abi::{self, Ctx, RawConversation, StringCb, UserData};

use super::{Call, Fail, invalid, run32, send_json};
use crate::event::PluginEvent;
use crate::marshal::{Conversation, conversation_json, read_text};
use crate::permission;

pub(super) unsafe extern "C" fn get_conversations(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_conversations",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let session = call.session()?;
            let mut rows = session.message_rows();
            rows.extend(session.folder_rows(ConversationFolder::Helper));
            rows.extend(session.folder_rows(ConversationFolder::Resolved));
            let list: Vec<Value> = rows
                .iter()
                .filter_map(|row| {
                    let key = row.key()?;
                    Some(json!({
                        "conv": conversation_json(Some(key), session),
                        "title": row.title,
                        "preview": row.preview,
                        "time": row.time,
                        "unread": row.unread_count,
                        "pinned": row.pinned,
                    }))
                })
                .collect();
            let total = session.total_unread_for_badge();
            send_json(
                cb,
                &json!({"total_unread": total, "conversations": list}),
                user,
            )?;
            Ok(list.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn get_current_conversation(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_current_conversation",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let session = call.session()?;
            let key = session
                .current_conversation()
                .ok_or(Fail::Code(abi::ERR_NOT_FOUND))?;
            send_json(cb, &conversation_json(Some(&key), session), user)?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn open_conversation(
    ctx: Ctx,
    conv: *const RawConversation,
    message_id: i64,
) -> i32 {
    // SAFETY: conv is null or points to a valid struct provided by the plugin.
    let conversation = unsafe { conv.as_ref().and_then(|c| Conversation::from_raw(c)) };
    run32(
        ctx,
        "open_conversation",
        Some(permission::OPEN_CONVERSATION),
        Box::new(move |call| {
            let (Some(conversation), true) = (conversation, message_id >= 0) else {
                return invalid();
            };
            let session = call.session()?;
            if let Conversation::Channel { stream_id, .. } = &conversation
                && session.channel(*stream_id).is_none()
            {
                return Err(Fail::Code(abi::ERR_NOT_FOUND));
            }
            call.host.emit(PluginEvent::OpenConversation {
                account_id: session.account_id(),
                key: conversation.to_key(session.self_id()),
                message_id: (message_id > 0).then_some(message_id),
            });
            Ok(0)
        }),
    )
}

// Conversation passed by the plugin; ERR_NOT_FOUND if the channel does not exist.
pub(super) fn conversation_key(
    call: &Call<'_>,
    conversation: Option<Conversation>,
) -> Result<ConversationKey, Fail> {
    let Some(conversation) = conversation else {
        return invalid();
    };
    let session = call.session()?;
    if let Conversation::Channel { stream_id, .. } = &conversation
        && session.channel(*stream_id).is_none()
    {
        return Err(Fail::Code(abi::ERR_NOT_FOUND));
    }
    Ok(conversation.to_key(session.self_id()))
}

pub(super) unsafe extern "C" fn get_drafts(ctx: Ctx, cb: Option<StringCb>, user: UserData) -> i32 {
    run32(
        ctx,
        "get_drafts",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let session = call.session()?;
            let list: Vec<Value> = session
                .drafts()
                .iter()
                .map(|(key, draft)| {
                    json!({
                        "conv": conversation_json(Some(key), session),
                        "content": draft.content,
                        "updated_at": draft.updated_at,
                    })
                })
                .collect();
            send_json(cb, &json!(list), user)?;
            Ok(list.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn save_draft(
    ctx: Ctx,
    conv: *const RawConversation,
    markdown: *const c_char,
) -> i32 {
    // SAFETY: conv is null or points to a valid struct provided by the plugin; the plugin passes a
    // NUL-terminated string.
    let (conversation, content) = unsafe {
        (
            conv.as_ref().and_then(|c| Conversation::from_raw(c)),
            read_text(markdown),
        )
    };
    run32(
        ctx,
        "save_draft",
        Some(permission::FILL_COMPOSER),
        Box::new(move |call| {
            let Some(content) = content else {
                return invalid();
            };
            let key = conversation_key(call, conversation)?;
            call.wait(call.session()?.save_draft(&key, &content))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn get_conversation_level(
    ctx: Ctx,
    conv: *const RawConversation,
) -> i32 {
    // SAFETY: conv is null or points to a valid struct provided by the plugin.
    let conversation = unsafe { conv.as_ref().and_then(|c| Conversation::from_raw(c)) };
    run32(
        ctx,
        "get_conversation_level",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let key = conversation_key(call, conversation)?;
            Ok(call.session()?.conversation_level(&key) as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn set_conversation_level(
    ctx: Ctx,
    conv: *const RawConversation,
    level: i32,
) -> i32 {
    // SAFETY: conv is null or points to a valid struct provided by the plugin.
    let conversation = unsafe { conv.as_ref().and_then(|c| Conversation::from_raw(c)) };
    run32(
        ctx,
        "set_conversation_level",
        Some(permission::MANAGE_PREFERENCES),
        Box::new(move |call| {
            let Some(level) = DndLevel::from_i32(level) else {
                return invalid();
            };
            let key = conversation_key(call, conversation)?;
            call.session()?.set_conversation_level(&key, level);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_conversation_pinned(
    ctx: Ctx,
    conv: *const RawConversation,
    pinned: i32,
) -> i32 {
    // SAFETY: conv is null or points to a valid struct provided by the plugin.
    let conversation = unsafe { conv.as_ref().and_then(|c| Conversation::from_raw(c)) };
    run32(
        ctx,
        "set_conversation_pinned",
        Some(permission::MANAGE_PREFERENCES),
        Box::new(move |call| {
            let key = conversation_key(call, conversation)?;
            call.session()?.set_pinned(
                PinScope::Messages,
                &PinItem::Conversation(key),
                pinned != 0,
            );
            Ok(0)
        }),
    )
}
