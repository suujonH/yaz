//! 界面：输入框、提示、通知、状态栏文字、对话框、面板与打开目标。

use std::ffi::c_char;

use zulip_core::ConversationKey;
use zulip_plugin_sdk::abi::{self, Ctx, RawConversation, StringCb, UserData};

use super::conversations::conversation_key;
use super::{Call, Fail, HOST_CATEGORY, invalid, required, run32, send_json};
use crate::event::PluginEvent;
use crate::host::PluginHost;
use crate::launch;
use crate::marshal::{Conversation, read_text};
use crate::panel::DialogSpec;
use crate::permission;
use crate::thread;

const MAX_NOTICE_LENGTH: usize = 1000;
const MAX_STATUS_LENGTH: usize = 200;

pub(super) unsafe extern "C" fn launch_target(
    ctx: Ctx,
    target: *const c_char,
    args: *const c_char,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let (target, args) = unsafe { (read_text(target), read_text(args)) };
    run32(
        ctx,
        "launch",
        Some(permission::LAUNCH),
        Box::new(move |call| {
            let target = target.as_deref().map(str::trim).unwrap_or_default();
            if target.is_empty() {
                return invalid();
            }
            // 白名单之外的目标只能放在附加内容的按钮中，由用户点击。
            if !PluginHost::is_whitelisted(call.plugin, target) {
                call.warn(format!("launch：不在白名单中：{target}"));
                return Err(Fail::Code(abi::ERR_PERMISSION));
            }
            let args = args.as_deref().unwrap_or_default();
            match launch::launch(target, args) {
                Ok(()) => {
                    call.plugin.add_log(
                        abi::LOG_INFO,
                        HOST_CATEGORY,
                        &format!("已打开 {}", launch::describe(target, args)),
                    );
                    Ok(0)
                }
                Err(launch::LaunchError::InvalidTarget) => invalid(),
                Err(error) => {
                    call.plugin
                        .add_log(abi::LOG_ERROR, HOST_CATEGORY, &format!("launch：{error}"));
                    Err(Fail::Code(abi::ERR_NOT_FOUND))
                }
            }
        }),
    )
}

pub(super) unsafe extern "C" fn reply_panel(ctx: Ctx, json: *const c_char) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(json) }.unwrap_or_default();
    run32(
        ctx,
        "reply_panel",
        None,
        Box::new(move |call| {
            if call.host.is_plugin_thread() && thread::try_reply_panel(text) {
                Ok(0)
            } else {
                Err(Fail::Code(abi::ERR_WRONG_CONTEXT))
            }
        }),
    )
}

pub(super) unsafe extern "C" fn fill_composer(ctx: Ctx, text: *const c_char, mode: i32) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(text) };
    run32(
        ctx,
        "fill_composer",
        Some(permission::FILL_COMPOSER),
        Box::new(move |call| {
            let (Some(text), true) = (text, matches!(mode, abi::FILL_INSERT | abi::FILL_REPLACE))
            else {
                return invalid();
            };
            emit_fill(call, None, text, mode)
        }),
    )
}

fn emit_fill(
    call: &Call<'_>,
    conv: Option<ConversationKey>,
    text: String,
    mode: i32,
) -> Result<i64, Fail> {
    call.host.emit(PluginEvent::FillComposer {
        plugin_id: call.plugin.id(),
        account_id: call.session()?.account_id(),
        conv,
        text,
        replace: mode == abi::FILL_REPLACE,
    });
    Ok(0)
}

fn limited(text: Option<String>, max: usize) -> Option<String> {
    text.map(|t| t.chars().take(max).collect())
}

pub(super) unsafe extern "C" fn show_toast(ctx: Ctx, text: *const c_char) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(text) };
    run32(
        ctx,
        "show_toast",
        Some(permission::NOTIFY),
        Box::new(move |call| {
            let Some(text) =
                limited(text, MAX_NOTICE_LENGTH).filter(|t: &String| !t.trim().is_empty())
            else {
                return invalid();
            };
            call.host.emit(PluginEvent::Toast {
                plugin_id: call.plugin.id(),
                text,
            });
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn show_notification(
    ctx: Ctx,
    title: *const c_char,
    body: *const c_char,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let (title, body) = unsafe { (read_text(title), read_text(body)) };
    run32(
        ctx,
        "show_notification",
        Some(permission::NOTIFY),
        Box::new(move |call| {
            let Some(title) =
                limited(title, MAX_NOTICE_LENGTH).filter(|t: &String| !t.trim().is_empty())
            else {
                return invalid();
            };
            call.host.emit(PluginEvent::SystemNotification {
                plugin_id: call.plugin.id(),
                title,
                body: limited(body, MAX_NOTICE_LENGTH).unwrap_or_default(),
            });
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn set_status_text(ctx: Ctx, text: *const c_char) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(text) };
    run32(
        ctx,
        "set_status_text",
        Some(permission::NOTIFY),
        Box::new(move |call| {
            let text = limited(text, MAX_STATUS_LENGTH).unwrap_or_default();
            call.host.set_status_text(&call.plugin.id(), text.trim());
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn show_dialog(
    ctx: Ctx,
    json: *const c_char,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let text = unsafe { read_text(json) };
    run32(
        ctx,
        "show_dialog",
        Some(permission::UI_DIALOG),
        Box::new(move |call| {
            // 没有回调时结果无处交付，不显示对话框。
            required(cb)?;
            // 等待界面期间不能占住插件线程。
            if call.host.is_plugin_thread() {
                return Err(Fail::Code(abi::ERR_WRONG_CONTEXT));
            }
            let Some(spec) = text.as_deref().and_then(DialogSpec::parse) else {
                return invalid();
            };
            let account_id = call.session()?.account_id();
            let result = call
                .host
                .show_dialog(call.plugin, account_id, spec)
                .map_err(Fail::Code)?;
            send_json(cb, &result.to_json(), user)?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn fill_composer_to(
    ctx: Ctx,
    conv: *const RawConversation,
    text: *const c_char,
    mode: i32,
) -> i32 {
    // SAFETY: conv 为空或指向插件提供的有效结构；插件传入以 0 结尾的字符串。
    let (conversation, text) = unsafe {
        (
            conv.as_ref().and_then(|c| Conversation::from_raw(c)),
            read_text(text),
        )
    };
    run32(
        ctx,
        "fill_composer_to",
        Some(permission::FILL_COMPOSER),
        Box::new(move |call| {
            let (Some(text), true) = (text, matches!(mode, abi::FILL_INSERT | abi::FILL_REPLACE))
            else {
                return invalid();
            };
            let key = conversation_key(call, conversation)?;
            emit_fill(call, Some(key), text, mode)
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::host::test_support;
    use crate::host_api::create;

    #[tokio::test(flavor = "multi_thread")]
    async fn show_dialog_without_callback_is_rejected_before_any_dialog() {
        let host = test_support::host();
        let plugin = test_support::plugin(&host, &json!({"permissions": [permission::UI_DIALOG]}));
        let api = create(plugin, Arc::downgrade(&host));
        let mut events = host.subscribe();
        let spec = CString::new(json!({"kind": "confirm", "text": "继续？"}).to_string()).unwrap();
        // SAFETY: ctx 来自 create，字符串以 0 结尾；回调为空时不使用 user。
        let code = unsafe { (api.show_dialog)(api.ctx, spec.as_ptr(), None, std::ptr::null_mut()) };
        assert_eq!(code, abi::ERR_INVALID_ARG);
        assert!(host.pending_dialogs().is_empty());
        assert!(events.try_recv().is_err());
    }
}
