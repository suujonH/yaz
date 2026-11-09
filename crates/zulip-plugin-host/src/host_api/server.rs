//! 服务器：通用 API 请求与组织信息。

use std::ffi::c_char;
use std::time::Duration;

use serde_json::json;
use zulip_plugin_sdk::abi::{self, Ctx, StringCb, UserData};

use super::{Fail, HOST_CATEGORY, invalid, run32, send_json, send_string};
use crate::marshal::read_text;
use crate::permission;
use crate::request;

const API_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) unsafe extern "C" fn get_realm_info(
    ctx: Ctx,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    run32(
        ctx,
        "get_realm_info",
        Some(permission::READ_CONTACTS),
        Box::new(move |call| {
            let realm = call.session()?.realm();
            send_json(
                cb,
                &json!({
                    "url": realm.url,
                    "name": realm.name,
                    "icon_url": realm.icon_url,
                    "zulip_version": realm.zulip_version,
                    "feature_level": realm.feature_level,
                    "max_message_length": realm.max_message_length,
                    "max_topic_length": realm.max_topic_length,
                    "max_file_upload_size_mib": realm.max_file_upload_size_mib,
                    "empty_topic_display_name": realm.empty_topic_display_name,
                    "allow_message_editing": realm.allow_message_editing,
                    "enable_read_receipts": realm.enable_read_receipts,
                    "edit_limit_seconds": realm.edit_limit_seconds,
                    "delete_limit_seconds": realm.delete_limit_seconds,
                }),
                user,
            )?;
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn api_request(
    ctx: Ctx,
    method: *const c_char,
    path: *const c_char,
    params: *const c_char,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let (method, path, params) = unsafe { (read_text(method), read_text(path), read_text(params)) };
    run32(
        ctx,
        "api_request",
        Some(permission::ZULIP_API),
        Box::new(move |call| {
            let path = path.unwrap_or_default();
            if !request::is_allowed_api_path(&path) {
                call.warn(format!("api_request：不允许访问 {path}"));
                return Err(Fail::Code(abi::ERR_PERMISSION));
            }
            let method = method.as_deref().and_then(request::api_method);
            let form = request::api_form(params.as_deref());
            let (Some(cb), Some(method), Some(form)) = (cb, method, form) else {
                return invalid();
            };
            call.plugin.add_log(
                abi::LOG_INFO,
                HOST_CATEGORY,
                &format!("api_request：{method} {path}"),
            );
            let reply = call.wait(call.session()?.client().request(
                method,
                &path,
                Some(&form),
                Some(API_TIMEOUT),
            ))?;
            send_string(cb, &reply.to_string(), user);
            Ok(0)
        }),
    )
}
