//! 插件间共享的资源。

use std::ffi::c_char;

use serde_json::Value;
use zulip_plugin_sdk::abi::{self, Ctx, StringCb, UserData};

use super::{Fail, invalid, run32, send_string};
use crate::marshal::read_text;
use crate::resources::{self, ResourceSelection};

pub(super) unsafe extern "C" fn publish_resources(
    ctx: Ctx,
    kind: *const c_char,
    json: *const c_char,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let (kind, text) = unsafe { (read_text(kind), read_text(json)) };
    run32(
        ctx,
        "publish_resources",
        None,
        Box::new(move |call| {
            let kind = kind.unwrap_or_default();
            if !resources::is_valid_type(&kind) {
                return invalid();
            }
            let id = call.plugin.id();
            let list = match text.filter(|t| !t.trim().is_empty()) {
                Some(text) => match resources::parse(&id, &text) {
                    Some(list) => list,
                    None => return invalid(),
                },
                None => Vec::new(),
            };
            call.host.publish_resources(&id, &kind, list);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn request_resource(
    ctx: Ctx,
    json: *const c_char,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(json) };
    run32(
        ctx,
        "request_resource",
        None,
        Box::new(move |call| {
            // 插件线程上等待会卡住发布资源的插件（它的回调也在插件线程上）。
            if call.host.is_plugin_thread() {
                return Err(Fail::Code(abi::ERR_WRONG_CONTEXT));
            }
            let value: Value = text
                .as_deref()
                .and_then(|t| serde_json::from_str(t).ok())
                .unwrap_or(Value::Null);
            let Some(target) = ResourceSelection::from_json(&value["resource"]) else {
                return invalid();
            };
            if !call.plugin.depends_on(&target.plugin_id) {
                call.warn(format!(
                    "request_resource：plugin.json 的 dependencies 未声明 {}",
                    target.plugin_id
                ));
                return Err(Fail::Code(abi::ERR_PERMISSION));
            }
            let reply = call
                .host
                .request_resource(call.plugin, &target, &value["request"])
                .map_err(Fail::Code)?;
            if let Some(cb) = cb {
                send_string(cb, &reply, user);
            }
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn reply_resource(
    ctx: Ctx,
    request_id: i64,
    json: *const c_char,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(json) }.unwrap_or_default();
    run32(
        ctx,
        "reply_resource",
        None,
        Box::new(move |call| {
            if call.host.reply_resource(call.plugin, request_id, text) {
                Ok(0)
            } else {
                Err(Fail::Code(abi::ERR_NOT_FOUND))
            }
        }),
    )
}
