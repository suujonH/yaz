//! 用户：资料、在线状态、用户组、静音、关键词与自己的状态。

use std::ffi::c_char;

use serde_json::{Value, json};
use zulip_core::model::PresenceState;
use zulip_plugin_sdk::abi::{self, Ctx, StringCb, UserCb, UserData};

use super::{Fail, invalid, run32, send_json};
use crate::marshal::{Scope, read_text, user_to_raw};
use crate::permission;

const ROLES: [i32; 6] = [0, 100, 200, 300, 400, 600];

pub(super) unsafe extern "C" fn get_users(ctx: Ctx, cb: Option<UserCb>, user: UserData) -> i32 {
    run32(
        ctx,
        "get_users",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let Some(cb) = cb else {
                return invalid();
            };
            let session = call.session()?;
            let users: Vec<_> = session
                .users()
                .into_iter()
                .filter(|u| u.is_active)
                .collect();
            for item in &users {
                let mut scope = Scope::default();
                let raw = user_to_raw(item, session, &mut scope);
                // SAFETY: raw 在 scope 存活期间有效。
                unsafe { cb(&raw, user) };
            }
            Ok(users.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn get_user(
    ctx: Ctx,
    user_id: i64,
    cb: Option<UserCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_user",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let Some(cb) = cb else {
                return invalid();
            };
            let session = call.session()?;
            let found = session
                .user(user_id)
                .ok_or(Fail::Code(abi::ERR_NOT_FOUND))?;
            let mut scope = Scope::default();
            let raw = user_to_raw(&found, session, &mut scope);
            // SAFETY: raw 在 scope 存活期间有效。
            unsafe { cb(&raw, user) };
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn update_user(
    ctx: Ctx,
    user_id: i64,
    full_name: *const c_char,
    role: i32,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let name = unsafe { read_text(full_name) };
    run32(
        ctx,
        "update_user",
        Some(permission::MANAGE_USER),
        Box::new(move |call| {
            let name = name.as_deref().map(str::trim);
            if user_id <= 0
                || (name.is_none() && role == 0)
                || name.is_some_and(str::is_empty)
                || !ROLES.contains(&role)
            {
                return invalid();
            }
            let role = (role != 0).then_some(role);
            call.wait(call.session()?.update_user(user_id, name, role))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn get_presence(
    ctx: Ctx,
    user_id: i64,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_presence",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let session = call.session()?;
            let ids: Vec<i64> = match user_id {
                0 => session
                    .users()
                    .into_iter()
                    .filter(|u| u.is_active)
                    .map(|u| u.id)
                    .collect(),
                id if session.user(id).is_some() => vec![id],
                _ => return Err(Fail::Code(abi::ERR_NOT_FOUND)),
            };
            let list: Vec<Value> = ids
                .iter()
                .map(|id| {
                    let presence = match session.presence(*id) {
                        PresenceState::Online => "online",
                        PresenceState::Idle => "idle",
                        PresenceState::Offline => "offline",
                    };
                    json!({"user": id, "presence": presence, "status_text": session.status_text(*id)})
                })
                .collect();
            send_json(cb, &Value::Array(list), user)?;
            Ok(ids.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn get_user_groups(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_user_groups",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let groups = call.session()?.user_groups();
            send_json(cb, &json!(groups), user)?;
            Ok(groups.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn set_user_muted(ctx: Ctx, user_id: i64, muted: i32) -> i32 {
    run32(
        ctx,
        "set_user_muted",
        Some(permission::MANAGE_PREFERENCES),
        Box::new(move |call| {
            if user_id <= 0 {
                return invalid();
            }
            call.wait(call.session()?.set_user_muted(user_id, muted != 0))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_alert_word(ctx: Ctx, word: *const c_char, on: i32) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let word = unsafe { read_text(word) };
    run32(
        ctx,
        "set_alert_word",
        Some(permission::MANAGE_PREFERENCES),
        Box::new(move |call| {
            let word = word.as_deref().map(str::trim).unwrap_or_default();
            if word.is_empty() {
                return invalid();
            }
            let words = [word.to_owned()];
            let session = call.session()?;
            if on != 0 {
                call.wait(session.add_alert_words(&words))?;
            } else {
                call.wait(session.remove_alert_words(&words))?;
            }
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_own_status(ctx: Ctx, text: *const c_char) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(text) }.unwrap_or_default();
    run32(
        ctx,
        "set_own_status",
        Some(permission::MANAGE_PREFERENCES),
        Box::new(move |call| {
            call.wait(call.session()?.set_status_text(&text))?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn get_muted_users(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_muted_users",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let ids = call.session()?.muted_users();
            send_json(cb, &json!(ids), user)?;
            Ok(ids.len() as i64)
        }),
    )
}

pub(super) unsafe extern "C" fn get_alert_words(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_alert_words",
        Some(permission::READ_MESSAGE),
        Box::new(move |call| {
            let words = call.session()?.alert_words();
            send_json(cb, &json!(words), user)?;
            Ok(words.len() as i64)
        }),
    )
}
