//! Host functions (called by plugins through the function table). Plugins may call them on any
//! thread; the functions block synchronously. They must not be called on the program's async
//! runtime threads; all errors are converted to error codes, with details available via
//! get_last_error. Network and server errors are also written to the plugin log.

mod accounts;
mod channels;
mod contacts;
mod conversations;
mod files;
mod messages;
mod reminders;
mod resources;
mod server;
mod settings;
mod ui;

use std::cell::RefCell;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Weak};

use serde_json::Value;
use zulip_api::ApiError;
use zulip_plugin_sdk::abi::{self, API_VERSION, Ctx, HostApi, StringCb, UserData};
use zulip_session::{Session, SessionError};

use crate::host::PluginHost;
use crate::marshal::Scope;
use crate::plugin::NativePlugin;
use crate::thread;

use accounts::*;
use channels::*;
use contacts::*;
use conversations::*;
use files::*;
use messages::*;
use reminders::*;
use resources::*;
use server::*;
use settings::*;
use ui::*;

pub(crate) const HOST_CATEGORY: &str = "宿主";

// Functions that a disabled plugin may also call.
const ALWAYS_AVAILABLE: [&str; 3] = ["add_log", "get_data_dir", "get_settings"];

struct PluginCtx {
    plugin: Arc<NativePlugin>,
    host: Weak<PluginHost>,
}

/// The function table and ctx stay valid (plugins are never unloaded).
pub(crate) fn create(plugin: Arc<NativePlugin>, host: Weak<PluginHost>) -> &'static HostApi {
    let ctx: &'static mut PluginCtx = Box::leak(Box::new(PluginCtx { plugin, host }));
    Box::leak(Box::new(HostApi {
        struct_size: size_of::<HostApi>() as u32,
        api_version: API_VERSION,
        ctx: std::ptr::from_mut(ctx).cast(),
        add_log,
        get_accounts,
        get_current_account,
        select_account,
        get_data_dir,
        get_settings,
        set_settings_feedback,
        send_channel_message,
        send_direct_message,
        get_message,
        get_messages,
        get_users,
        get_user,
        get_channels,
        get_topics,
        add_reaction,
        set_starred,
        mark_read,
        upload_file,
        set_outgoing_content,
        set_annotation,
        launch: launch_target,
        edit_message,
        delete_message,
        update_channel,
        update_user,
        publish_resources,
        request_resource,
        reply_resource,
        reply_panel,
        search_messages,
        get_conversations,
        get_current_conversation,
        get_presence,
        get_user_groups,
        get_all_channels,
        get_topic_visibility,
        get_edit_history,
        remove_reaction,
        mark_unread,
        move_topic,
        set_topic_resolved,
        delete_topic,
        open_conversation,
        fill_composer,
        schedule_message,
        show_toast,
        show_notification,
        set_status_text,
        download_attachment,
        api_request,
        set_thread_account,
        get_last_error,
        get_message_source,
        get_scheduled_messages,
        cancel_scheduled_message,
        show_dialog,
        set_settings,
        fill_composer_to,
        set_topic_visibility,
        set_subscribed,
        set_subscription_property,
        create_channel,
        get_subscribers,
        create_reminder,
        cancel_reminder,
        get_reminders,
        get_drafts,
        save_draft,
        get_conversation_level,
        set_conversation_level,
        set_conversation_pinned,
        set_user_muted,
        set_alert_word,
        set_own_status,
        get_muted_users,
        get_alert_words,
        get_read_receipts,
        get_message_actions,
        get_realm_info,
        download_attachment_ex,
        upload_file_ex,
    }))
}

enum Fail {
    Code(i32),
    Api(Box<ApiError>),
    Session(Box<SessionError>),
}

impl From<ApiError> for Fail {
    fn from(error: ApiError) -> Self {
        Self::Api(Box::new(error))
    }
}

impl From<SessionError> for Fail {
    fn from(error: SessionError) -> Self {
        Self::Session(Box::new(error))
    }
}

fn invalid<T>() -> Result<T, Fail> {
    Err(Fail::Code(abi::ERR_INVALID_ARG))
}

type Body<'a> = dyn FnOnce(&Call<'_>) -> Result<i64, Fail> + 'a;

struct Call<'a> {
    plugin: &'a Arc<NativePlugin>,
    host: Arc<PluginHost>,
    session: Option<Arc<Session>>,
    // The reason written by warn; when the function body then only returns an error code, it is
    // used as the get_last_error text.
    detail: RefCell<Option<String>>,
}

impl Call<'_> {
    fn session(&self) -> Result<&Arc<Session>, Fail> {
        self.session
            .as_ref()
            .ok_or(Fail::Code(abi::ERR_NOT_LOGGED_IN))
    }

    fn wait<F: Future>(&self, future: F) -> F::Output {
        self.host.runtime().block_on(future)
    }

    fn warn(&self, text: String) {
        self.plugin.add_log(abi::LOG_WARNING, HOST_CATEGORY, &text);
        *self.detail.borrow_mut() = Some(text);
    }
}

// Calls rejected directly by the guard.
fn deny(plugin: &NativePlugin, code: i32, text: String) -> i64 {
    plugin.add_log(abi::LOG_WARNING, HOST_CATEGORY, &text);
    thread::set_last_error(plugin, text);
    i64::from(code)
}

// Unified checks and error handling: plugin -> disabled or not -> permission -> account -> function
// body.
fn guard(ctx: Ctx, name: &str, permission: Option<&str>, body: Box<Body<'_>>) -> i64 {
    if ctx.is_null() {
        return i64::from(abi::ERR_INVALID_ARG);
    }
    // SAFETY: ctx is created by create and stays valid; the plugin passes it back unchanged.
    let ctx = unsafe { &*ctx.cast_const().cast::<PluginCtx>() };
    let plugin = &ctx.plugin;
    let Some(host) = ctx.host.upgrade() else {
        return i64::from(abi::ERR_NOT_LOGGED_IN);
    };
    if !plugin.accepts_calls() && !ALWAYS_AVAILABLE.contains(&name) {
        return deny(
            plugin,
            abi::ERR_DISABLED,
            format!("{name}：插件已停用，不能调用"),
        );
    }
    if let Some(permission) = permission
        && !plugin.has_permission(permission)
    {
        return deny(
            plugin,
            abi::ERR_PERMISSION,
            format!("{name}：plugin.json 未声明权限 {permission}"),
        );
    }
    let session = host.caller_session(plugin);
    let call = Call {
        plugin,
        host,
        session,
        detail: RefCell::new(None),
    };
    match catch_unwind(AssertUnwindSafe(|| body(&call))) {
        Ok(Ok(value)) => value,
        Ok(Err(fail)) => i64::from(report(plugin, name, fail, call.detail.take())),
        Err(_) => {
            let text = format!("{name}：内部错误");
            plugin.add_log(abi::LOG_ERROR, HOST_CATEGORY, &text);
            thread::set_last_error(plugin, text);
            i64::from(abi::ERR_UNKNOWN)
        }
    }
}

fn report(plugin: &NativePlugin, name: &str, fail: Fail, detail: Option<String>) -> i32 {
    let (code, logged) = describe(name, fail);
    if let Some(text) = &logged {
        plugin.add_log(abi::LOG_ERROR, HOST_CATEGORY, text);
    }
    let text = logged
        .or(detail)
        .unwrap_or_else(|| format!("{name}：{}", zulip_plugin_sdk::Error(code)));
    thread::set_last_error(plugin, text);
    code
}

// Error code and the text to write to the log.
fn describe(name: &str, fail: Fail) -> (i32, Option<String>) {
    let api = |api_error: &ApiError| match api_error {
        ApiError::Network(_) | ApiError::Timeout(_) => (
            abi::ERR_NETWORK,
            Some(format!("{name}：网络错误：{api_error}")),
        ),
        ApiError::Cancelled => (abi::ERR_NOT_LOGGED_IN, None),
        ApiError::Api { code, message, .. } => (
            abi::ERR_SERVER,
            Some(format!("{name}：服务器返回错误 {code}：{message}")),
        ),
        other => (abi::ERR_UNKNOWN, Some(format!("{name}：{other}"))),
    };
    match fail {
        Fail::Code(code) => (code, None),
        Fail::Api(e) => api(&e),
        Fail::Session(e) => match *e {
            SessionError::Api(e) => api(&e),
            SessionError::Invalid(text) => (abi::ERR_INVALID_ARG, Some(format!("{name}：{text}"))),
            e @ SessionError::TopicMoveLimited { .. } => {
                (abi::ERR_SERVER, Some(format!("{name}：{e}")))
            }
            e => (abi::ERR_UNKNOWN, Some(format!("{name}：{e}"))),
        },
    }
}

fn run32(ctx: Ctx, name: &str, permission: Option<&str>, body: Box<Body<'_>>) -> i32 {
    guard(ctx, name, permission, body) as i32
}

fn required(cb: Option<StringCb>) -> Result<StringCb, Fail> {
    cb.ok_or(Fail::Code(abi::ERR_INVALID_ARG))
}

fn send_string(cb: StringCb, text: &str, user: UserData) {
    let mut scope = Scope::default();
    let pointer = scope.text(text);
    // SAFETY: the string is valid while the scope is alive; the callback is provided by the plugin.
    unsafe { cb(pointer, user) };
}

fn send_json(cb: Option<StringCb>, value: &Value, user: UserData) -> Result<(), Fail> {
    send_string(required(cb)?, &value.to_string(), user);
    Ok(())
}
