//! The plugin itself: log, data directory, settings and the last error.

use std::ffi::c_char;

use serde_json::{Map, Value};
use zulip_plugin_sdk::abi::{self, Ctx, StringCb, UserData};
use zulip_session::SessionError;

use super::{Fail, guard, invalid, required, run32, send_string};
use crate::event::PluginEvent;
use crate::marshal::read_text;
use crate::settings::SettingsFeedback;
use crate::thread;

pub(super) unsafe extern "C" fn add_log(
    ctx: Ctx,
    level: i32,
    category: *const c_char,
    text: *const c_char,
) {
    // SAFETY: the plugin passes a NUL-terminated string.
    let (category, text) = unsafe { (read_text(category), read_text(text)) };
    let _ = guard(
        ctx,
        "add_log",
        None,
        Box::new(move |call| {
            call.plugin.add_log(
                level,
                &category.unwrap_or_default(),
                &text.unwrap_or_default(),
            );
            Ok(0)
        }),
    );
}

pub(super) unsafe extern "C" fn get_data_dir(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_data_dir",
        None,
        Box::new(move |call| {
            let cb = required(cb)?;
            let dir = call.host.plugin_data_dir(&call.plugin.id());
            std::fs::create_dir_all(&dir).map_err(|e| Fail::from(SessionError::from(e)))?;
            let mut text = dir.to_string_lossy().into_owned();
            if !text.ends_with('\\') {
                text.push('\\');
            }
            send_string(cb, &text, user);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn get_settings(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_settings",
        None,
        Box::new(move |call| {
            send_string(required(cb)?, &call.plugin.settings_json(), user);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_settings_feedback(ctx: Ctx, json: *const c_char) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let json = unsafe { read_text(json) };
    run32(
        ctx,
        "set_settings_feedback",
        None,
        Box::new(move |call| {
            let Some(feedback) = json.as_deref().and_then(SettingsFeedback::parse) else {
                return invalid();
            };
            call.host.emit(PluginEvent::SettingsFeedback {
                plugin_id: call.plugin.id(),
                feedback,
            });
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn get_last_error(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_last_error",
        None,
        Box::new(move |call| {
            let text = thread::last_error(call.plugin).unwrap_or_default();
            send_string(required(cb)?, &text, user);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_settings(ctx: Ctx, json: *const c_char) -> i32 {
    // SAFETY: the plugin passes a NUL-terminated string.
    let text = unsafe { read_text(json) };
    run32(
        ctx,
        "set_settings",
        None,
        Box::new(move |call| {
            let changes = text
                .as_deref()
                .and_then(|t| serde_json::from_str::<Map<String, Value>>(t).ok());
            let Some(changes) = changes else {
                return invalid();
            };
            call.host
                .merge_settings(call.plugin, changes)
                .map_err(|error| {
                    call.warn(format!("set_settings：{error}"));
                    Fail::Code(abi::ERR_UNKNOWN)
                })?;
            Ok(0)
        }),
    )
}
