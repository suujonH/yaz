//! Reminders.

use serde_json::{Value, json};
use zulip_plugin_sdk::abi::{Ctx, StringCb, UserData};
use zulip_session::DeleteTarget;

use super::{guard, invalid, run32, send_json};
use crate::permission;

pub(super) unsafe extern "C" fn create_reminder(ctx: Ctx, message_id: i64, deliver_at: i64) -> i64 {
    guard(
        ctx,
        "create_reminder",
        Some(permission::MODIFY_MESSAGE),
        Box::new(move |call| {
            if message_id <= 0 || deliver_at <= 0 {
                return invalid();
            }
            Ok(call.wait(call.session()?.create_reminder(message_id, deliver_at))?)
        }),
    )
}

pub(super) unsafe extern "C" fn cancel_reminder(ctx: Ctx, id: i64) -> i32 {
    run32(
        ctx,
        "cancel_reminder",
        Some(permission::MODIFY_MESSAGE),
        Box::new(move |call| {
            if id <= 0 {
                return invalid();
            }
            call.wait(call.session()?.delete(DeleteTarget::Reminder(id)))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn get_reminders(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_reminders",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let list: Vec<Value> = call
                .session()?
                .pending_reminders()
                .iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "message_id": r.target_message_id,
                        "content": r.content,
                        "rendered_content": r.rendered_content,
                        "deliver_at": r.deliver_at,
                        "failed": r.failed,
                    })
                })
                .collect();
            send_json(cb, &json!(list), user)?;
            Ok(list.len() as i64)
        }),
    )
}
