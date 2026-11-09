//! Safe wrappers around the host functions: callable from any thread, synchronous and blocking.

use std::ffi::{CString, c_char, c_void};
use std::mem::offset_of;
use std::sync::OnceLock;

use serde_json::{Map, Value, json};

use crate::abi::{
    self, HostApi, RawAccount, RawChannel, RawConversation, RawMessage, RawTopic, RawUser,
};
use crate::types::{Account, Channel, Conversation, Message, Topic, User, read_string};

// Session handed to the host: the pointer is valid for as long as it lives.
struct ConvArg {
    raw: RawConversation,
    _topic: Option<CString>,
}

impl ConvArg {
    fn new(conv: &Conversation) -> Self {
        let topic = match conv {
            Conversation::Channel { topic, .. } => Some(c_string(topic)),
            Conversation::Direct { .. } => None,
        };
        let (kind, stream_id, ids, count) = match conv {
            Conversation::Channel { stream_id, .. } => {
                (abi::CONV_CHANNEL, *stream_id, std::ptr::null(), 0)
            }
            Conversation::Direct { user_ids } => (
                abi::CONV_DIRECT,
                0,
                user_ids.as_ptr(),
                user_ids.len() as i32,
            ),
        };
        Self {
            raw: RawConversation {
                struct_size: size_of::<RawConversation>() as u32,
                kind,
                stream_id,
                stream_name: std::ptr::null(),
                topic: opt_ptr(&topic),
                dm_user_ids: ids,
                dm_user_count: count,
            },
            _topic: topic,
        }
    }
}

/// Error code returned by a host function (`abi::ERR_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self.0 {
            abi::ERR_INVALID_ARG => "参数错误",
            abi::ERR_PERMISSION => "没有权限",
            abi::ERR_NOT_LOGGED_IN => "没有登录",
            abi::ERR_NETWORK => "网络错误",
            abi::ERR_SERVER => "服务器返回错误",
            abi::ERR_NOT_FOUND => "不存在",
            abi::ERR_WRONG_CONTEXT => "不能在这里调用",
            abi::ERR_UNSUPPORTED => "宿主版本太旧，不支持这个函数",
            abi::ERR_DISABLED => "插件已停用",
            _ => "未知错误",
        };
        write!(f, "{text}（{}）", self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

struct Installed {
    api: &'static HostApi,
    category: CString,
}

// SAFETY: the function table is immutable, ctx is only passed back unchanged; host functions may be
// called from any thread.
unsafe impl Send for Installed {}
// SAFETY: same as above.
unsafe impl Sync for Installed {}

static HOST: OnceLock<Installed> = OnceLock::new();

// The function table covers at least the v1 base table.
fn covers_v1(struct_size: u32) -> bool {
    struct_size as usize >= abi::HOST_API_V1_SIZE
}

// The function table provides the function at this offset.
fn provides(struct_size: u32, offset: usize) -> bool {
    (struct_size as usize) > offset
}

/// Checks the version and stores the function table. An old host's table may be shorter than
/// [`HostApi`]; functions appended later are fetched with [`appended!`].
///
/// # Safety
/// `api` is null or points to a function table that stays valid forever.
pub(crate) unsafe fn install(api: *const HostApi, category: &str) -> bool {
    // SAFETY: see the function docs.
    let Some(table) = (unsafe { api.as_ref() }) else {
        return false;
    };
    if table.api_version < abi::API_VERSION || !covers_v1(table.struct_size) {
        return false;
    }
    HOST.get_or_init(|| Installed {
        api: table,
        category: c_string(category),
    });
    true
}

fn api() -> Result<&'static HostApi> {
    HOST.get()
        .map(|h| h.api)
        .ok_or(Error(abi::ERR_WRONG_CONTEXT))
}

// Functions appended after v1: ERR_UNSUPPORTED when the host is too old.
macro_rules! appended {
    ($api:expr, $field:ident) => {
        if provides($api.struct_size, offset_of!(HostApi, $field)) {
            $api.$field
        } else {
            return Err(Error(abi::ERR_UNSUPPORTED));
        }
    };
}

fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

fn code(result: i32) -> Result<()> {
    if result < 0 {
        Err(Error(result))
    } else {
        Ok(())
    }
}

fn id(result: i64) -> Result<i64> {
    if result < 0 {
        Err(Error(result as i32))
    } else {
        Ok(result)
    }
}

fn opt_ptr(text: &Option<CString>) -> *const c_char {
    text.as_ref().map_or(std::ptr::null(), |t| t.as_ptr())
}

unsafe extern "C" fn on_string(s: *const c_char, user: *mut c_void) {
    // SAFETY: user is the &mut String passed in by the caller; the callback runs synchronously
    // during the call.
    unsafe { *user.cast::<String>() = read_string(s) };
}

macro_rules! collect_cb {
    ($name:ident, $raw:ty, $ty:ty) => {
        unsafe extern "C" fn $name(raw: *const $raw, user: *mut c_void) {
            // SAFETY: user is the &mut Vec passed in by the caller; the callback runs synchronously
            // during the call; raw is valid during the callback.
            unsafe { (*user.cast::<Vec<$ty>>()).push(<$ty>::from_raw(&*raw)) };
        }
    };
}

collect_cb!(on_message_item, RawMessage, Message);
collect_cb!(on_user_item, RawUser, User);
collect_cb!(on_channel_item, RawChannel, Channel);
collect_cb!(on_topic_item, RawTopic, Topic);
collect_cb!(on_account_item, RawAccount, Account);

/// Writes a plugin log entry.
pub fn log(level: i32, text: &str) {
    if let Some(host) = HOST.get() {
        let text = c_string(text);
        // SAFETY: the string is valid for the duration of the call.
        unsafe {
            (host.api.add_log)(host.api.ctx, level, host.category.as_ptr(), text.as_ptr());
        }
    }
}

pub fn accounts() -> Result<Vec<Account>> {
    let api = api()?;
    let mut items: Vec<Account> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: the callback only uses items during the call.
    code(unsafe { (api.get_accounts)(api.ctx, Some(on_account_item), user) })?;
    Ok(items)
}

pub fn current_account() -> Result<i64> {
    let api = api()?;
    // SAFETY: no pointer arguments.
    id(unsafe { (api.get_current_account)(api.ctx) })
}

/// Switches to the account the message belongs to, before calling host functions outside a
/// callback.
pub fn select_account(account_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: no pointer arguments.
    code(unsafe { (api.select_account)(api.ctx, account_id) })
}

/// The plugin's data directory (created by the host, readable and writable), ending with `\`.
pub fn data_dir() -> Result<String> {
    let api = api()?;
    let mut text = String::new();
    let user = (&raw mut text).cast();
    // SAFETY: the callback only uses text during the call.
    code(unsafe { (api.get_data_dir)(api.ctx, Some(on_string), user) })?;
    Ok(text)
}

/// Plugin settings (a JSON object); an empty object if reading fails.
pub fn settings() -> Value {
    let empty = || Value::Object(Map::new());
    let Ok(api) = api() else {
        return empty();
    };
    let mut text = String::new();
    let user = (&raw mut text).cast();
    // SAFETY: the callback only uses text during the call.
    if unsafe { (api.get_settings)(api.ctx, Some(on_string), user) } < 0 {
        return empty();
    }
    serde_json::from_str(&text).unwrap_or_else(|_| empty())
}

/// Replies to the settings window; for the format see the header file `set_settings_feedback`.
pub fn settings_feedback(feedback: &Value) -> Result<()> {
    let api = api()?;
    let json = c_string(&feedback.to_string());
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.set_settings_feedback)(api.ctx, json.as_ptr()) })
}

/// Shows text below a setting item; `row` is the row number received by the button callback.
pub fn settings_text(key: &str, row: Option<i64>, text: &str) -> Result<()> {
    let mut feedback = json!({"key": key, "text": text});
    if let Some(row) = row {
        feedback["row"] = json!(row);
    }
    settings_feedback(&feedback)
}

/// Result of a button next to one entry of a tags setting item; on failure `error` is the reason.
pub fn settings_item_result(
    key: &str,
    row: Option<i64>,
    item: &str,
    error: Option<&str>,
) -> Result<()> {
    let status = if error.is_none() { "ok" } else { "error" };
    let mut feedback = json!({"key": key, "item": item, "status": status});
    if let Some(error) = error {
        feedback["text"] = json!(error);
    }
    if let Some(row) = row {
        feedback["row"] = json!(row);
    }
    settings_feedback(&feedback)
}

pub fn send_channel_message(stream_id: i64, topic: &str, markdown: &str) -> Result<i64> {
    let api = api()?;
    let topic = c_string(topic);
    let markdown = c_string(markdown);
    // SAFETY: the string is valid for the duration of the call.
    id(
        unsafe {
            (api.send_channel_message)(api.ctx, stream_id, topic.as_ptr(), markdown.as_ptr())
        },
    )
}

pub fn send_direct_message(user_ids: &[i64], markdown: &str) -> Result<i64> {
    let api = api()?;
    let markdown = c_string(markdown);
    // SAFETY: the array and the strings are valid for the duration of the call.
    id(unsafe {
        (api.send_direct_message)(
            api.ctx,
            user_ids.as_ptr(),
            user_ids.len() as i32,
            markdown.as_ptr(),
        )
    })
}

pub fn message(message_id: i64) -> Result<Message> {
    let api = api()?;
    let mut items: Vec<Message> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: the callback only uses items during the call.
    code(unsafe { (api.get_message)(api.ctx, message_id, Some(on_message_item), user) })?;
    items.pop().ok_or(Error(abi::ERR_NOT_FOUND))
}

/// Fetches messages. `conv` of `None` means all; `anchor`: 0 = newest, -1 = oldest, anything else =
/// a message id (inclusive).
pub fn messages(
    conv: Option<&Conversation>,
    anchor: i64,
    num_before: i32,
    num_after: i32,
) -> Result<Vec<Message>> {
    let api = api()?;
    let mut items: Vec<Message> = Vec::new();
    let user = (&raw mut items).cast();
    let raw = conv.map(ConvArg::new);
    let raw_ptr = raw.as_ref().map_or(std::ptr::null(), |r| &raw const r.raw);
    // SAFETY: what raw points to and the items used by the callback are valid for the duration of
    // the call.
    code(unsafe {
        (api.get_messages)(
            api.ctx,
            raw_ptr,
            anchor,
            num_before,
            num_after,
            Some(on_message_item),
            user,
        )
    })?;
    Ok(items)
}

/// The `count` messages before this one (excluding it), in chronological order. Requires
/// read_message.
pub fn context_messages(message: &Message, count: usize) -> Result<Vec<Message>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut items = messages(Some(&message.conv), message.id, count as i32, 0)?;
    items.retain(|m| m.id != message.id);
    Ok(items)
}

pub fn users() -> Result<Vec<User>> {
    let api = api()?;
    let mut items: Vec<User> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: the callback only uses items during the call.
    code(unsafe { (api.get_users)(api.ctx, Some(on_user_item), user) })?;
    Ok(items)
}

pub fn user(user_id: i64) -> Result<User> {
    let api = api()?;
    let mut items: Vec<User> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: the callback only uses items during the call.
    code(unsafe { (api.get_user)(api.ctx, user_id, Some(on_user_item), user) })?;
    items.pop().ok_or(Error(abi::ERR_NOT_FOUND))
}

pub fn channels() -> Result<Vec<Channel>> {
    let api = api()?;
    let mut items: Vec<Channel> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: the callback only uses items during the call.
    code(unsafe { (api.get_channels)(api.ctx, Some(on_channel_item), user) })?;
    Ok(items)
}

pub fn topics(stream_id: i64) -> Result<Vec<Topic>> {
    let api = api()?;
    let mut items: Vec<Topic> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: the callback only uses items during the call.
    code(unsafe { (api.get_topics)(api.ctx, stream_id, Some(on_topic_item), user) })?;
    Ok(items)
}

pub fn add_reaction(message_id: i64, emoji_name: &str) -> Result<()> {
    let api = api()?;
    let name = c_string(emoji_name);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.add_reaction)(api.ctx, message_id, name.as_ptr()) })
}

pub fn set_starred(message_id: i64, starred: bool) -> Result<()> {
    let api = api()?;
    // SAFETY: no pointer arguments.
    code(unsafe { (api.set_starred)(api.ctx, message_id, i32::from(starred)) })
}

pub fn mark_read(message_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: no pointer arguments.
    code(unsafe { (api.mark_read)(api.ctx, message_id) })
}

/// Uploads a file and returns `/user_uploads/...`.
pub fn upload_file(local_path: &str) -> Result<String> {
    let api = api()?;
    let path = c_string(local_path);
    let mut text = String::new();
    let user = (&raw mut text).cast();
    // SAFETY: the string is valid for the duration of the call; the callback only uses text during
    // the call.
    code(unsafe { (api.upload_file)(api.ctx, path.as_ptr(), Some(on_string), user) })?;
    Ok(text)
}

/// Rewrites the content being sent; only valid inside the `filter_outgoing` callback.
pub fn set_outgoing_content(markdown: &str) -> Result<()> {
    let api = api()?;
    let markdown = c_string(markdown);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.set_outgoing_content)(api.ctx, markdown.as_ptr()) })
}

/// Sets extra content for a message; for the format see the header file `set_annotation`.
pub fn set_annotation(message_id: i64, annotation: &Value) -> Result<()> {
    let api = api()?;
    let json = c_string(&annotation.to_string());
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.set_annotation)(api.ctx, message_id, json.as_ptr()) })
}

/// Sets extra content for a message; on failure writes to the plugin log.
pub fn annotate(message_id: i64, annotation: &Value) {
    if let Err(error) = set_annotation(message_id, annotation) {
        log(abi::LOG_WARNING, &format!("显示内容失败：{error}"));
    }
}

/// Clears this plugin's extra content on this message.
pub fn clear_annotation(message_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: a null pointer means clear.
    code(unsafe { (api.set_annotation)(api.ctx, message_id, std::ptr::null()) })
}

pub fn launch(target: &str, args: Option<&str>) -> Result<()> {
    let api = api()?;
    let target = c_string(target);
    let args = args.map(c_string);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.launch)(api.ctx, target.as_ptr(), opt_ptr(&args)) })
}

pub fn edit_message(message_id: i64, markdown: &str) -> Result<()> {
    let api = api()?;
    let markdown = c_string(markdown);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.edit_message)(api.ctx, message_id, markdown.as_ptr()) })
}

pub fn delete_message(message_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: no pointer arguments.
    code(unsafe { (api.delete_message)(api.ctx, message_id) })
}

pub fn update_channel(stream_id: i64, name: Option<&str>, description: Option<&str>) -> Result<()> {
    let api = api()?;
    let name = name.map(c_string);
    let description = description.map(c_string);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.update_channel)(api.ctx, stream_id, opt_ptr(&name), opt_ptr(&description)) })
}

/// A `role` of 0 means no change.
pub fn update_user(user_id: i64, full_name: Option<&str>, role: i32) -> Result<()> {
    let api = api()?;
    let name = full_name.map(c_string);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.update_user)(api.ctx, user_id, opt_ptr(&name), role) })
}

/// Publishes resources, replacing the previously published ones; an empty array clears them.
pub fn publish_resources(kind: &str, resources: &Value) -> Result<()> {
    let api = api()?;
    let kind = c_string(kind);
    let json = c_string(&resources.to_string());
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.publish_resources)(api.ctx, kind.as_ptr(), json.as_ptr()) })
}

/// Uses another plugin's resource (worker thread; blocks until a reply or timeout).
pub fn request_resource(resource: &Value, request: &Value) -> Result<String> {
    let api = api()?;
    let json = c_string(&json!({"resource": resource, "request": request}).to_string());
    let mut text = String::new();
    let user = (&raw mut text).cast();
    // SAFETY: the string is valid for the duration of the call; the callback only uses text during
    // the call.
    code(unsafe { (api.request_resource)(api.ctx, json.as_ptr(), Some(on_string), user) })?;
    Ok(text)
}

/// Replies to another plugin's resource request.
pub fn reply_resource(request_id: i64, reply: &Value) -> Result<()> {
    let api = api()?;
    let json = c_string(&reply.to_string());
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.reply_resource)(api.ctx, request_id, json.as_ptr()) })
}

/// Replies to a panel request (only callable inside the `on_panel` callback); for the format see
/// the header file `reply_panel`.
pub fn reply_panel(reply: &Value) -> Result<()> {
    let api = api()?;
    let json = c_string(&reply.to_string());
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.reply_panel)(api.ctx, json.as_ptr()) })
}

// Calls that take a string callback; returns the string the callback received.
fn string_call(call: impl FnOnce(*mut c_void) -> i32) -> Result<String> {
    let mut text = String::new();
    code(call((&raw mut text).cast()))?;
    Ok(text)
}

fn json_call(call: impl FnOnce(*mut c_void) -> i32) -> Result<Value> {
    let text = string_call(call)?;
    serde_json::from_str(&text).map_err(|_| Error(abi::ERR_UNKNOWN))
}

/// One page of search results.
#[derive(Clone, Debug, Default)]
pub struct SearchPage {
    /// Newest first.
    pub messages: Vec<Message>,
    /// The `cursor` of the next page; `None` when the end is reached.
    pub next_cursor: Option<i64>,
}

/// Searches messages; for the format of `query` see the header file `search_messages`.
pub fn search_messages(query: &Value) -> Result<SearchPage> {
    let api = api()?;
    let json = c_string(&query.to_string());
    let mut items: Vec<Message> = Vec::new();
    let mut next = 0i64;
    let user = (&raw mut items).cast();
    // SAFETY: the strings and next are valid for the duration of the call; the callback only uses
    // items during the call.
    code(unsafe {
        (api.search_messages)(
            api.ctx,
            json.as_ptr(),
            &raw mut next,
            Some(on_message_item),
            user,
        )
    })?;
    Ok(SearchPage {
        messages: items,
        next_cursor: (next > 0).then_some(next),
    })
}

/// Conversations and unread counts of the messages page; for the format see the header file
/// `get_conversations`.
pub fn conversations() -> Result<Value> {
    let api = api()?;
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { (api.get_conversations)(api.ctx, Some(on_string), user) })
}

/// The conversation the user currently has open; `None` if there is none.
pub fn current_conversation() -> Result<Option<Conversation>> {
    let api = api()?;
    // SAFETY: the callback only uses text during the call.
    let result =
        json_call(|user| unsafe { (api.get_current_conversation)(api.ctx, Some(on_string), user) });
    match result {
        Ok(value) => Ok(Conversation::from_json(&value)),
        Err(Error(abi::ERR_NOT_FOUND)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Presence and status text; a `user_id` of 0 means all active users. For the format see the header
/// file `get_presence`.
pub fn presence(user_id: i64) -> Result<Value> {
    let api = api()?;
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { (api.get_presence)(api.ctx, user_id, Some(on_string), user) })
}

/// User groups; for the format see the header file `get_user_groups`.
pub fn user_groups() -> Result<Value> {
    let api = api()?;
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { (api.get_user_groups)(api.ctx, Some(on_string), user) })
}

/// All channels visible in the organization (including unsubscribed ones).
pub fn all_channels() -> Result<Vec<Channel>> {
    let api = api()?;
    let mut items: Vec<Channel> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: the callback only uses items during the call.
    code(unsafe { (api.get_all_channels)(api.ctx, Some(on_channel_item), user) })?;
    Ok(items)
}

/// A topic's follow state (`abi::TOPIC_*`).
pub fn topic_visibility(stream_id: i64, topic: &str) -> Result<i32> {
    let api = api()?;
    let topic = c_string(topic);
    // SAFETY: the string is valid for the duration of the call.
    let result = unsafe { (api.get_topic_visibility)(api.ctx, stream_id, topic.as_ptr()) };
    code(result)?;
    Ok(result)
}

/// Edit history on the server; for the format see the header file `get_edit_history`.
pub fn edit_history(message_id: i64) -> Result<Value> {
    let api = api()?;
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { (api.get_edit_history)(api.ctx, message_id, Some(on_string), user) })
}

pub fn remove_reaction(message_id: i64, emoji_name: &str) -> Result<()> {
    let api = api()?;
    let name = c_string(emoji_name);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.remove_reaction)(api.ctx, message_id, name.as_ptr()) })
}

/// Marks this message and all later messages in the conversation as unread.
pub fn mark_unread(message_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: no pointer arguments.
    code(unsafe { (api.mark_unread)(api.ctx, message_id) })
}

/// Moves or renames a topic; a `new_stream_id` of 0 means the same channel.
pub fn move_topic(stream_id: i64, topic: &str, new_stream_id: i64, new_topic: &str) -> Result<()> {
    let api = api()?;
    let topic = c_string(topic);
    let new_topic = c_string(new_topic);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe {
        (api.move_topic)(
            api.ctx,
            stream_id,
            topic.as_ptr(),
            new_stream_id,
            new_topic.as_ptr(),
        )
    })
}

pub fn set_topic_resolved(stream_id: i64, topic: &str, resolved: bool) -> Result<()> {
    let api = api()?;
    let topic = c_string(topic);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe {
        (api.set_topic_resolved)(api.ctx, stream_id, topic.as_ptr(), i32::from(resolved))
    })
}

pub fn delete_topic(stream_id: i64, topic: &str) -> Result<()> {
    let api = api()?;
    let topic = c_string(topic);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.delete_topic)(api.ctx, stream_id, topic.as_ptr()) })
}

/// Makes the UI switch to a conversation; if `message_id` is non-empty, jumps to that message.
pub fn open_conversation(conv: &Conversation, message_id: Option<i64>) -> Result<()> {
    let api = api()?;
    let arg = ConvArg::new(conv);
    // SAFETY: arg is valid for the duration of the call.
    code(unsafe {
        (api.open_conversation)(api.ctx, &raw const arg.raw, message_id.unwrap_or_default())
    })
}

/// Puts text into the current conversation's composer (without sending); with `replace` it replaces
/// existing content, otherwise it inserts at the cursor.
pub fn fill_composer(text: &str, replace: bool) -> Result<()> {
    let api = api()?;
    let text = c_string(text);
    let mode = if replace {
        abi::FILL_REPLACE
    } else {
        abi::FILL_INSERT
    };
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.fill_composer)(api.ctx, text.as_ptr(), mode) })
}

/// Scheduled send; `deliver_at` is Unix seconds; returns the scheduled message id.
pub fn schedule_message(conv: &Conversation, markdown: &str, deliver_at: i64) -> Result<i64> {
    let api = api()?;
    let arg = ConvArg::new(conv);
    let markdown = c_string(markdown);
    // SAFETY: arg and the strings are valid for the duration of the call.
    id(unsafe {
        (api.schedule_message)(api.ctx, &raw const arg.raw, markdown.as_ptr(), deliver_at)
    })
}

pub fn show_toast(text: &str) -> Result<()> {
    let api = api()?;
    let text = c_string(text);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.show_toast)(api.ctx, text.as_ptr()) })
}

pub fn show_notification(title: &str, body: &str) -> Result<()> {
    let api = api()?;
    let title = c_string(title);
    let body = c_string(body);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.show_notification)(api.ctx, title.as_ptr(), body.as_ptr()) })
}

/// Status bar hint; an empty string clears it.
pub fn set_status_text(text: &str) -> Result<()> {
    let api = api()?;
    let text = c_string(text);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { (api.set_status_text)(api.ctx, text.as_ptr()) })
}

/// Downloads an attachment (`/user_uploads/...`) into the cache and returns the local path.
pub fn download_attachment(upload_path: &str) -> Result<String> {
    let api = api()?;
    let path = c_string(upload_path);
    // SAFETY: the string is valid for the duration of the call; the callback only uses text during
    // the call.
    string_call(|user| unsafe {
        (api.download_attachment)(api.ctx, path.as_ptr(), Some(on_string), user)
    })
}

/// Calls the Zulip API as the current account; `path` must be under `/api/v1/`. Returns the
/// server's JSON reply.
pub fn api_request(method: &str, path: &str, params: &Value) -> Result<Value> {
    let api = api()?;
    let method = c_string(method);
    let path = c_string(path);
    let params = c_string(&params.to_string());
    // SAFETY: the string is valid for the duration of the call; the callback only uses text during
    // the call.
    json_call(|user| unsafe {
        (api.api_request)(
            api.ctx,
            method.as_ptr(),
            path.as_ptr(),
            params.as_ptr(),
            Some(on_string),
            user,
        )
    })
}

/// Binds the account used by this thread; 0 unbinds. Priority: callback account > thread binding >
/// [`select_account`] > first logged in. After the bound account logs out, this thread's calls
/// return `ERR_NOT_LOGGED_IN` until it is unbound or rebound.
pub fn set_thread_account(account_id: i64) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_thread_account);
    // SAFETY: no pointer arguments.
    code(unsafe { f(api.ctx, account_id) })
}

/// Detail text of this thread's last failure; an empty string if it never failed.
pub fn last_error() -> Result<String> {
    let api = api()?;
    let f = appended!(api, get_last_error);
    // SAFETY: the callback only uses text during the call.
    string_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// The message's Markdown source.
pub fn message_source(message_id: i64) -> Result<String> {
    let api = api()?;
    let f = appended!(api, get_message_source);
    // SAFETY: the callback only uses text during the call.
    string_call(|user| unsafe { f(api.ctx, message_id, Some(on_string), user) })
}

/// Scheduled messages; for the format see the header file `get_scheduled_messages`.
pub fn scheduled_messages() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_scheduled_messages);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

pub fn cancel_scheduled_message(id: i64) -> Result<()> {
    let api = api()?;
    let f = appended!(api, cancel_scheduled_message);
    // SAFETY: no pointer arguments.
    code(unsafe { f(api.ctx, id) })
}

/// Shows a dialog and waits for the result (worker thread; blocking). For the format see the header
/// file `show_dialog`.
pub fn show_dialog(spec: &Value) -> Result<Value> {
    let api = api()?;
    let f = appended!(api, show_dialog);
    let json = c_string(&spec.to_string());
    // SAFETY: the string is valid for the duration of the call; the callback only uses text during
    // the call.
    json_call(|user| unsafe { f(api.ctx, json.as_ptr(), Some(on_string), user) })
}

/// Modifies this plugin's settings (merging keys). Does not call `on_settings_changed`.
pub fn set_settings(values: &Value) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_settings);
    let json = c_string(&values.to_string());
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { f(api.ctx, json.as_ptr()) })
}

/// Puts text into the specified conversation's composer (without sending); with `replace` it
/// replaces existing content, otherwise it inserts at the cursor.
pub fn fill_composer_to(conv: &Conversation, text: &str, replace: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, fill_composer_to);
    let arg = ConvArg::new(conv);
    let text = c_string(text);
    let mode = if replace {
        abi::FILL_REPLACE
    } else {
        abi::FILL_INSERT
    };
    // SAFETY: arg and the strings are valid for the duration of the call.
    code(unsafe { f(api.ctx, &raw const arg.raw, text.as_ptr(), mode) })
}

/// Sets a topic's follow state (`abi::TOPIC_*`).
pub fn set_topic_visibility(stream_id: i64, topic: &str, visibility: i32) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_topic_visibility);
    let topic = c_string(topic);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { f(api.ctx, stream_id, topic.as_ptr(), visibility) })
}

pub fn set_subscribed(stream_id: i64, subscribed: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_subscribed);
    // SAFETY: no pointer arguments.
    code(unsafe { f(api.ctx, stream_id, i32::from(subscribed)) })
}

/// Modifies a channel's personal settings; `name` is the Zulip property name (color, is_muted,
/// desktop_notifications, etc.).
pub fn set_subscription_property(stream_id: i64, name: &str, value: &Value) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_subscription_property);
    let name = c_string(name);
    let json = c_string(&value.to_string());
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { f(api.ctx, stream_id, name.as_ptr(), json.as_ptr()) })
}

/// Creates a channel; for the format see the header file `create_channel`. Returns the channel id.
pub fn create_channel(spec: &Value) -> Result<i64> {
    let api = api()?;
    let f = appended!(api, create_channel);
    let json = c_string(&spec.to_string());
    // SAFETY: the string is valid for the duration of the call.
    id(unsafe { f(api.ctx, json.as_ptr()) })
}

/// Array of the channel's subscriber ids.
pub fn subscribers(stream_id: i64) -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_subscribers);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, stream_id, Some(on_string), user) })
}

/// Creates a message reminder; `deliver_at` is Unix seconds; returns the reminder id.
pub fn create_reminder(message_id: i64, deliver_at: i64) -> Result<i64> {
    let api = api()?;
    let f = appended!(api, create_reminder);
    // SAFETY: no pointer arguments.
    id(unsafe { f(api.ctx, message_id, deliver_at) })
}

pub fn cancel_reminder(reminder_id: i64) -> Result<()> {
    let api = api()?;
    let f = appended!(api, cancel_reminder);
    // SAFETY: no pointer arguments.
    code(unsafe { f(api.ctx, reminder_id) })
}

/// Reminders not yet delivered; for the format see the header file `get_reminders`.
pub fn reminders() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_reminders);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// Drafts; for the format see the header file `get_drafts`.
pub fn drafts() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_drafts);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// Saves a draft; an empty string deletes it.
pub fn save_draft(conv: &Conversation, markdown: &str) -> Result<()> {
    let api = api()?;
    let f = appended!(api, save_draft);
    let arg = ConvArg::new(conv);
    let markdown = c_string(markdown);
    // SAFETY: arg and the strings are valid for the duration of the call.
    code(unsafe { f(api.ctx, &raw const arg.raw, markdown.as_ptr()) })
}

/// A conversation's do-not-disturb level (`abi::LEVEL_*`).
pub fn conversation_level(conv: &Conversation) -> Result<i32> {
    let api = api()?;
    let f = appended!(api, get_conversation_level);
    let arg = ConvArg::new(conv);
    // SAFETY: arg is valid for the duration of the call.
    let result = unsafe { f(api.ctx, &raw const arg.raw) };
    code(result)?;
    Ok(result)
}

pub fn set_conversation_level(conv: &Conversation, level: i32) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_conversation_level);
    let arg = ConvArg::new(conv);
    // SAFETY: arg is valid for the duration of the call.
    code(unsafe { f(api.ctx, &raw const arg.raw, level) })
}

pub fn set_conversation_pinned(conv: &Conversation, pinned: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_conversation_pinned);
    let arg = ConvArg::new(conv);
    // SAFETY: arg is valid for the duration of the call.
    code(unsafe { f(api.ctx, &raw const arg.raw, i32::from(pinned)) })
}

pub fn set_user_muted(user_id: i64, muted: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_user_muted);
    // SAFETY: no pointer arguments.
    code(unsafe { f(api.ctx, user_id, i32::from(muted)) })
}

/// Adds (`on`) or removes an alert word.
pub fn set_alert_word(word: &str, on: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_alert_word);
    let word = c_string(word);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { f(api.ctx, word.as_ptr(), i32::from(on)) })
}

/// Sets your own status text; an empty string clears it.
pub fn set_own_status(text: &str) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_own_status);
    let text = c_string(text);
    // SAFETY: the string is valid for the duration of the call.
    code(unsafe { f(api.ctx, text.as_ptr()) })
}

/// Array of muted user ids.
pub fn muted_users() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_muted_users);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// Array of alert words.
pub fn alert_words() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_alert_words);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// Array of ids of users who have read this message.
pub fn read_receipts(message_id: i64) -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_read_receipts);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, message_id, Some(on_string), user) })
}

/// Actions available on this message; for the format see the header file `get_message_actions`.
pub fn message_actions(message_id: i64) -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_message_actions);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, message_id, Some(on_string), user) })
}

/// Organization info; for the format see the header file `get_realm_info`.
pub fn realm_info() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_realm_info);
    // SAFETY: the callback only uses text during the call.
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

// Calls with progress share the same user.
struct ProgressCall<'a> {
    text: String,
    progress: &'a mut (dyn FnMut(i64, i64) + Send),
}

unsafe extern "C" fn on_progress_result(s: *const c_char, user: *mut c_void) {
    // SAFETY: user is the &mut ProgressCall passed in by the caller; the callback runs
    // synchronously during the call.
    unsafe { (*user.cast::<ProgressCall<'_>>()).text = read_string(s) };
}

unsafe extern "C" fn on_progress(done: i64, total: i64, user: *mut c_void) {
    // SAFETY: same as above; the host never invokes two callbacks at the same time.
    unsafe { ((*user.cast::<ProgressCall<'_>>()).progress)(done, total) };
}

/// Downloads an attachment into the cache and returns the local path. `progress(downloaded,
/// total)`; total is 0 when unknown; not called when already cached.
pub fn download_attachment_ex(
    upload_path: &str,
    mut progress: impl FnMut(i64, i64) + Send,
) -> Result<String> {
    let api = api()?;
    let f = appended!(api, download_attachment_ex);
    let path = c_string(upload_path);
    let mut call = ProgressCall {
        text: String::new(),
        progress: &mut progress,
    };
    let user = (&raw mut call).cast();
    // SAFETY: the string and call are valid for the duration of the call; the callback is not
    // invoked after the host function returns.
    code(unsafe {
        f(
            api.ctx,
            path.as_ptr(),
            Some(on_progress),
            Some(on_progress_result),
            user,
        )
    })?;
    Ok(call.text)
}

/// Uploads a file and returns `/user_uploads/...`. `progress(uploaded, total)` may be called on
/// another thread.
pub fn upload_file_ex(
    local_path: &str,
    mut progress: impl FnMut(i64, i64) + Send,
) -> Result<String> {
    let api = api()?;
    let f = appended!(api, upload_file_ex);
    let path = c_string(local_path);
    let mut call = ProgressCall {
        text: String::new(),
        progress: &mut progress,
    };
    let user = (&raw mut call).cast();
    // SAFETY: the string and call are valid for the duration of the call; the callback is not
    // invoked after the host function returns.
    code(unsafe {
        f(
            api.ctx,
            path.as_ptr(),
            Some(on_progress),
            Some(on_progress_result),
            user,
        )
    })?;
    Ok(call.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_table_only_needs_to_cover_v1() {
        assert!(covers_v1(abi::HOST_API_V1_SIZE as u32));
        assert!(covers_v1(size_of::<HostApi>() as u32));
        assert!(!covers_v1(abi::HOST_API_V1_SIZE as u32 - 8));
    }

    #[test]
    fn appended_functions_are_checked_against_the_table_size() {
        let v1 = abi::HOST_API_V1_SIZE as u32;
        assert!(provides(v1, offset_of!(HostApi, api_request)));
        assert!(!provides(v1, offset_of!(HostApi, set_thread_account)));
        assert!(provides(v1 + 8, offset_of!(HostApi, set_thread_account)));
        assert!(!provides(v1 + 8, offset_of!(HostApi, get_last_error)));
        assert!(provides(
            size_of::<HostApi>() as u32,
            offset_of!(HostApi, upload_file_ex)
        ));
    }
}
