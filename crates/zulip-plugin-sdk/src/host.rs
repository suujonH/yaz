//! 宿主函数的安全封装：可以在任何线程调用，同步阻塞。

use std::ffi::{CString, c_char, c_void};
use std::mem::offset_of;
use std::sync::OnceLock;

use serde_json::{Map, Value, json};

use crate::abi::{
    self, HostApi, RawAccount, RawChannel, RawConversation, RawMessage, RawTopic, RawUser,
};
use crate::types::{Account, Channel, Conversation, Message, Topic, User, read_string};

// 传给宿主的会话：指针在它存活期间有效。
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

/// 宿主函数返回的错误码（`abi::ERR_*`）。
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

// SAFETY: 函数表不变，ctx 只原样传回；宿主函数可以在任何线程调用。
unsafe impl Send for Installed {}
// SAFETY: 同上。
unsafe impl Sync for Installed {}

static HOST: OnceLock<Installed> = OnceLock::new();

// 函数表至少覆盖 v1 基础表。
fn covers_v1(struct_size: u32) -> bool {
    struct_size as usize >= abi::HOST_API_V1_SIZE
}

// 函数表提供了这个偏移处的函数。
fn provides(struct_size: u32, offset: usize) -> bool {
    (struct_size as usize) > offset
}

/// 检查版本并保存函数表。旧宿主的表可能比 [`HostApi`] 短，追加的函数用 [`appended!`] 取。
///
/// # Safety
/// `api` 为空或指向一直有效的函数表。
pub(crate) unsafe fn install(api: *const HostApi, category: &str) -> bool {
    // SAFETY: 见函数说明。
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

// 追加到 v1 之后的函数：宿主太旧时 ERR_UNSUPPORTED。
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
    // SAFETY: user 是调用方传入的 &mut String，回调在调用期间同步执行。
    unsafe { *user.cast::<String>() = read_string(s) };
}

macro_rules! collect_cb {
    ($name:ident, $raw:ty, $ty:ty) => {
        unsafe extern "C" fn $name(raw: *const $raw, user: *mut c_void) {
            // SAFETY: user 是调用方传入的 &mut Vec，回调在调用期间同步执行；raw 在回调期间有效。
            unsafe { (*user.cast::<Vec<$ty>>()).push(<$ty>::from_raw(&*raw)) };
        }
    };
}

collect_cb!(on_message_item, RawMessage, Message);
collect_cb!(on_user_item, RawUser, User);
collect_cb!(on_channel_item, RawChannel, Channel);
collect_cb!(on_topic_item, RawTopic, Topic);
collect_cb!(on_account_item, RawAccount, Account);

/// 写插件日志。
pub fn log(level: i32, text: &str) {
    if let Some(host) = HOST.get() {
        let text = c_string(text);
        // SAFETY: 字符串在调用期间有效。
        unsafe {
            (host.api.add_log)(host.api.ctx, level, host.category.as_ptr(), text.as_ptr());
        }
    }
}

pub fn accounts() -> Result<Vec<Account>> {
    let api = api()?;
    let mut items: Vec<Account> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: 回调只在调用期间使用 items。
    code(unsafe { (api.get_accounts)(api.ctx, Some(on_account_item), user) })?;
    Ok(items)
}

pub fn current_account() -> Result<i64> {
    let api = api()?;
    // SAFETY: 没有指针参数。
    id(unsafe { (api.get_current_account)(api.ctx) })
}

/// 回调之外调用宿主函数前，切换到消息所属的账号。
pub fn select_account(account_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: 没有指针参数。
    code(unsafe { (api.select_account)(api.ctx, account_id) })
}

/// 插件的数据目录（宿主创建，可读写），以 `\` 结尾。
pub fn data_dir() -> Result<String> {
    let api = api()?;
    let mut text = String::new();
    let user = (&raw mut text).cast();
    // SAFETY: 回调只在调用期间使用 text。
    code(unsafe { (api.get_data_dir)(api.ctx, Some(on_string), user) })?;
    Ok(text)
}

/// 插件设置（JSON 对象）；读取失败时为空对象。
pub fn settings() -> Value {
    let empty = || Value::Object(Map::new());
    let Ok(api) = api() else {
        return empty();
    };
    let mut text = String::new();
    let user = (&raw mut text).cast();
    // SAFETY: 回调只在调用期间使用 text。
    if unsafe { (api.get_settings)(api.ctx, Some(on_string), user) } < 0 {
        return empty();
    }
    serde_json::from_str(&text).unwrap_or_else(|_| empty())
}

/// 回复设置窗口，格式见头文件 `set_settings_feedback`。
pub fn settings_feedback(feedback: &Value) -> Result<()> {
    let api = api()?;
    let json = c_string(&feedback.to_string());
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.set_settings_feedback)(api.ctx, json.as_ptr()) })
}

/// 在设置项的下方显示文字；`row` 为按钮回调收到的行号。
pub fn settings_text(key: &str, row: Option<i64>, text: &str) -> Result<()> {
    let mut feedback = json!({"key": key, "text": text});
    if let Some(row) = row {
        feedback["row"] = json!(row);
    }
    settings_feedback(&feedback)
}

/// tags 设置项中某一项旁的按钮的结果；失败时 `error` 为原因。
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
    // SAFETY: 字符串在调用期间有效。
    id(
        unsafe {
            (api.send_channel_message)(api.ctx, stream_id, topic.as_ptr(), markdown.as_ptr())
        },
    )
}

pub fn send_direct_message(user_ids: &[i64], markdown: &str) -> Result<i64> {
    let api = api()?;
    let markdown = c_string(markdown);
    // SAFETY: 数组和字符串在调用期间有效。
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
    // SAFETY: 回调只在调用期间使用 items。
    code(unsafe { (api.get_message)(api.ctx, message_id, Some(on_message_item), user) })?;
    items.pop().ok_or(Error(abi::ERR_NOT_FOUND))
}

/// 取消息。`conv` 为 `None` 表示全部；`anchor`：0 = 最新，-1 = 最旧，其它 = 消息 id（含它本身）。
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
    // SAFETY: raw 指向的内容和回调用到的 items 在调用期间有效。
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

/// 这条消息之前的 `count` 条消息（不含它），按时间先后。需要 read_message。
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
    // SAFETY: 回调只在调用期间使用 items。
    code(unsafe { (api.get_users)(api.ctx, Some(on_user_item), user) })?;
    Ok(items)
}

pub fn user(user_id: i64) -> Result<User> {
    let api = api()?;
    let mut items: Vec<User> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: 回调只在调用期间使用 items。
    code(unsafe { (api.get_user)(api.ctx, user_id, Some(on_user_item), user) })?;
    items.pop().ok_or(Error(abi::ERR_NOT_FOUND))
}

pub fn channels() -> Result<Vec<Channel>> {
    let api = api()?;
    let mut items: Vec<Channel> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: 回调只在调用期间使用 items。
    code(unsafe { (api.get_channels)(api.ctx, Some(on_channel_item), user) })?;
    Ok(items)
}

pub fn topics(stream_id: i64) -> Result<Vec<Topic>> {
    let api = api()?;
    let mut items: Vec<Topic> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: 回调只在调用期间使用 items。
    code(unsafe { (api.get_topics)(api.ctx, stream_id, Some(on_topic_item), user) })?;
    Ok(items)
}

pub fn add_reaction(message_id: i64, emoji_name: &str) -> Result<()> {
    let api = api()?;
    let name = c_string(emoji_name);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.add_reaction)(api.ctx, message_id, name.as_ptr()) })
}

pub fn set_starred(message_id: i64, starred: bool) -> Result<()> {
    let api = api()?;
    // SAFETY: 没有指针参数。
    code(unsafe { (api.set_starred)(api.ctx, message_id, i32::from(starred)) })
}

pub fn mark_read(message_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: 没有指针参数。
    code(unsafe { (api.mark_read)(api.ctx, message_id) })
}

/// 上传文件，返回 `/user_uploads/...`。
pub fn upload_file(local_path: &str) -> Result<String> {
    let api = api()?;
    let path = c_string(local_path);
    let mut text = String::new();
    let user = (&raw mut text).cast();
    // SAFETY: 字符串在调用期间有效；回调只在调用期间使用 text。
    code(unsafe { (api.upload_file)(api.ctx, path.as_ptr(), Some(on_string), user) })?;
    Ok(text)
}

/// 改写正在发送的内容，只在 `filter_outgoing` 回调内有效。
pub fn set_outgoing_content(markdown: &str) -> Result<()> {
    let api = api()?;
    let markdown = c_string(markdown);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.set_outgoing_content)(api.ctx, markdown.as_ptr()) })
}

/// 设置消息的附加内容，格式见头文件 `set_annotation`。
pub fn set_annotation(message_id: i64, annotation: &Value) -> Result<()> {
    let api = api()?;
    let json = c_string(&annotation.to_string());
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.set_annotation)(api.ctx, message_id, json.as_ptr()) })
}

/// 设置消息的附加内容；失败时写入插件日志。
pub fn annotate(message_id: i64, annotation: &Value) {
    if let Err(error) = set_annotation(message_id, annotation) {
        log(abi::LOG_WARNING, &format!("显示内容失败：{error}"));
    }
}

/// 清除本插件在这条消息上的附加内容。
pub fn clear_annotation(message_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: 空指针表示清除。
    code(unsafe { (api.set_annotation)(api.ctx, message_id, std::ptr::null()) })
}

pub fn launch(target: &str, args: Option<&str>) -> Result<()> {
    let api = api()?;
    let target = c_string(target);
    let args = args.map(c_string);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.launch)(api.ctx, target.as_ptr(), opt_ptr(&args)) })
}

pub fn edit_message(message_id: i64, markdown: &str) -> Result<()> {
    let api = api()?;
    let markdown = c_string(markdown);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.edit_message)(api.ctx, message_id, markdown.as_ptr()) })
}

pub fn delete_message(message_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: 没有指针参数。
    code(unsafe { (api.delete_message)(api.ctx, message_id) })
}

pub fn update_channel(stream_id: i64, name: Option<&str>, description: Option<&str>) -> Result<()> {
    let api = api()?;
    let name = name.map(c_string);
    let description = description.map(c_string);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.update_channel)(api.ctx, stream_id, opt_ptr(&name), opt_ptr(&description)) })
}

/// `role` 为 0 表示不修改。
pub fn update_user(user_id: i64, full_name: Option<&str>, role: i32) -> Result<()> {
    let api = api()?;
    let name = full_name.map(c_string);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.update_user)(api.ctx, user_id, opt_ptr(&name), role) })
}

/// 发布资源，替换之前发布的；空数组表示清除。
pub fn publish_resources(kind: &str, resources: &Value) -> Result<()> {
    let api = api()?;
    let kind = c_string(kind);
    let json = c_string(&resources.to_string());
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.publish_resources)(api.ctx, kind.as_ptr(), json.as_ptr()) })
}

/// 使用其他插件的资源（工作线程，阻塞到回复或超时）。
pub fn request_resource(resource: &Value, request: &Value) -> Result<String> {
    let api = api()?;
    let json = c_string(&json!({"resource": resource, "request": request}).to_string());
    let mut text = String::new();
    let user = (&raw mut text).cast();
    // SAFETY: 字符串在调用期间有效；回调只在调用期间使用 text。
    code(unsafe { (api.request_resource)(api.ctx, json.as_ptr(), Some(on_string), user) })?;
    Ok(text)
}

/// 回复其他插件的资源请求。
pub fn reply_resource(request_id: i64, reply: &Value) -> Result<()> {
    let api = api()?;
    let json = c_string(&reply.to_string());
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.reply_resource)(api.ctx, request_id, json.as_ptr()) })
}

/// 回复面板请求（只能在 `on_panel` 回调中调用），格式见头文件 `reply_panel`。
pub fn reply_panel(reply: &Value) -> Result<()> {
    let api = api()?;
    let json = c_string(&reply.to_string());
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.reply_panel)(api.ctx, json.as_ptr()) })
}

// 带字符串回调的调用，返回回调收到的字符串。
fn string_call(call: impl FnOnce(*mut c_void) -> i32) -> Result<String> {
    let mut text = String::new();
    code(call((&raw mut text).cast()))?;
    Ok(text)
}

fn json_call(call: impl FnOnce(*mut c_void) -> i32) -> Result<Value> {
    let text = string_call(call)?;
    serde_json::from_str(&text).map_err(|_| Error(abi::ERR_UNKNOWN))
}

/// 一页搜索结果。
#[derive(Clone, Debug, Default)]
pub struct SearchPage {
    /// 新的在前。
    pub messages: Vec<Message>,
    /// 下一页的 `cursor`；到头时为 `None`。
    pub next_cursor: Option<i64>,
}

/// 搜索消息，`query` 的格式见头文件 `search_messages`。
pub fn search_messages(query: &Value) -> Result<SearchPage> {
    let api = api()?;
    let json = c_string(&query.to_string());
    let mut items: Vec<Message> = Vec::new();
    let mut next = 0i64;
    let user = (&raw mut items).cast();
    // SAFETY: 字符串和 next 在调用期间有效；回调只在调用期间使用 items。
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

/// 消息页的会话和未读数，格式见头文件 `get_conversations`。
pub fn conversations() -> Result<Value> {
    let api = api()?;
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { (api.get_conversations)(api.ctx, Some(on_string), user) })
}

/// 用户当前打开的会话；没有时为 `None`。
pub fn current_conversation() -> Result<Option<Conversation>> {
    let api = api()?;
    // SAFETY: 回调只在调用期间使用 text。
    let result =
        json_call(|user| unsafe { (api.get_current_conversation)(api.ctx, Some(on_string), user) });
    match result {
        Ok(value) => Ok(Conversation::from_json(&value)),
        Err(Error(abi::ERR_NOT_FOUND)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// 在线状态和签名，`user_id` 为 0 时是所有活跃用户，格式见头文件 `get_presence`。
pub fn presence(user_id: i64) -> Result<Value> {
    let api = api()?;
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { (api.get_presence)(api.ctx, user_id, Some(on_string), user) })
}

/// 用户组，格式见头文件 `get_user_groups`。
pub fn user_groups() -> Result<Value> {
    let api = api()?;
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { (api.get_user_groups)(api.ctx, Some(on_string), user) })
}

/// 组织内可见的全部频道（含未订阅）。
pub fn all_channels() -> Result<Vec<Channel>> {
    let api = api()?;
    let mut items: Vec<Channel> = Vec::new();
    let user = (&raw mut items).cast();
    // SAFETY: 回调只在调用期间使用 items。
    code(unsafe { (api.get_all_channels)(api.ctx, Some(on_channel_item), user) })?;
    Ok(items)
}

/// 话题的关注状态（`abi::TOPIC_*`）。
pub fn topic_visibility(stream_id: i64, topic: &str) -> Result<i32> {
    let api = api()?;
    let topic = c_string(topic);
    // SAFETY: 字符串在调用期间有效。
    let result = unsafe { (api.get_topic_visibility)(api.ctx, stream_id, topic.as_ptr()) };
    code(result)?;
    Ok(result)
}

/// 服务器上的编辑历史，格式见头文件 `get_edit_history`。
pub fn edit_history(message_id: i64) -> Result<Value> {
    let api = api()?;
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { (api.get_edit_history)(api.ctx, message_id, Some(on_string), user) })
}

pub fn remove_reaction(message_id: i64, emoji_name: &str) -> Result<()> {
    let api = api()?;
    let name = c_string(emoji_name);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.remove_reaction)(api.ctx, message_id, name.as_ptr()) })
}

/// 从这条消息开始，会话中之后的消息全部标记为未读。
pub fn mark_unread(message_id: i64) -> Result<()> {
    let api = api()?;
    // SAFETY: 没有指针参数。
    code(unsafe { (api.mark_unread)(api.ctx, message_id) })
}

/// 移动或改名话题；`new_stream_id` 为 0 表示同一频道。
pub fn move_topic(stream_id: i64, topic: &str, new_stream_id: i64, new_topic: &str) -> Result<()> {
    let api = api()?;
    let topic = c_string(topic);
    let new_topic = c_string(new_topic);
    // SAFETY: 字符串在调用期间有效。
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
    // SAFETY: 字符串在调用期间有效。
    code(unsafe {
        (api.set_topic_resolved)(api.ctx, stream_id, topic.as_ptr(), i32::from(resolved))
    })
}

pub fn delete_topic(stream_id: i64, topic: &str) -> Result<()> {
    let api = api()?;
    let topic = c_string(topic);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.delete_topic)(api.ctx, stream_id, topic.as_ptr()) })
}

/// 让界面切换到会话，`message_id` 不为空时定位到消息。
pub fn open_conversation(conv: &Conversation, message_id: Option<i64>) -> Result<()> {
    let api = api()?;
    let arg = ConvArg::new(conv);
    // SAFETY: arg 在调用期间有效。
    code(unsafe {
        (api.open_conversation)(api.ctx, &raw const arg.raw, message_id.unwrap_or_default())
    })
}

/// 把文字填入当前会话的输入框（不发送）；`replace` 时替换已有内容，否则插入到光标处。
pub fn fill_composer(text: &str, replace: bool) -> Result<()> {
    let api = api()?;
    let text = c_string(text);
    let mode = if replace {
        abi::FILL_REPLACE
    } else {
        abi::FILL_INSERT
    };
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.fill_composer)(api.ctx, text.as_ptr(), mode) })
}

/// 定时发送，`deliver_at` 为 Unix 秒，返回定时消息 id。
pub fn schedule_message(conv: &Conversation, markdown: &str, deliver_at: i64) -> Result<i64> {
    let api = api()?;
    let arg = ConvArg::new(conv);
    let markdown = c_string(markdown);
    // SAFETY: arg 和字符串在调用期间有效。
    id(unsafe {
        (api.schedule_message)(api.ctx, &raw const arg.raw, markdown.as_ptr(), deliver_at)
    })
}

pub fn show_toast(text: &str) -> Result<()> {
    let api = api()?;
    let text = c_string(text);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.show_toast)(api.ctx, text.as_ptr()) })
}

pub fn show_notification(title: &str, body: &str) -> Result<()> {
    let api = api()?;
    let title = c_string(title);
    let body = c_string(body);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.show_notification)(api.ctx, title.as_ptr(), body.as_ptr()) })
}

/// 状态栏提示；空字符串表示清除。
pub fn set_status_text(text: &str) -> Result<()> {
    let api = api()?;
    let text = c_string(text);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { (api.set_status_text)(api.ctx, text.as_ptr()) })
}

/// 下载附件（`/user_uploads/...`）到缓存，返回本地路径。
pub fn download_attachment(upload_path: &str) -> Result<String> {
    let api = api()?;
    let path = c_string(upload_path);
    // SAFETY: 字符串在调用期间有效；回调只在调用期间使用 text。
    string_call(|user| unsafe {
        (api.download_attachment)(api.ctx, path.as_ptr(), Some(on_string), user)
    })
}

/// 以当前账号调用 Zulip API，`path` 必须在 `/api/v1/` 下；返回服务器的 JSON 回复。
pub fn api_request(method: &str, path: &str, params: &Value) -> Result<Value> {
    let api = api()?;
    let method = c_string(method);
    let path = c_string(path);
    let params = c_string(&params.to_string());
    // SAFETY: 字符串在调用期间有效；回调只在调用期间使用 text。
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

/// 绑定本线程使用的账号，0 解除。优先级：回调账号 > 线程绑定 > [`select_account`] > 最先登录。
/// 绑定的账号退出后，本线程的调用返回 `ERR_NOT_LOGGED_IN`，直到解除或重新绑定。
pub fn set_thread_account(account_id: i64) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_thread_account);
    // SAFETY: 没有指针参数。
    code(unsafe { f(api.ctx, account_id) })
}

/// 本线程上次失败的详情文字；没有失败过时为空字符串。
pub fn last_error() -> Result<String> {
    let api = api()?;
    let f = appended!(api, get_last_error);
    // SAFETY: 回调只在调用期间使用 text。
    string_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// 消息的 Markdown 原文。
pub fn message_source(message_id: i64) -> Result<String> {
    let api = api()?;
    let f = appended!(api, get_message_source);
    // SAFETY: 回调只在调用期间使用 text。
    string_call(|user| unsafe { f(api.ctx, message_id, Some(on_string), user) })
}

/// 定时消息，格式见头文件 `get_scheduled_messages`。
pub fn scheduled_messages() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_scheduled_messages);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

pub fn cancel_scheduled_message(id: i64) -> Result<()> {
    let api = api()?;
    let f = appended!(api, cancel_scheduled_message);
    // SAFETY: 没有指针参数。
    code(unsafe { f(api.ctx, id) })
}

/// 显示对话框并等待结果（工作线程，阻塞），格式见头文件 `show_dialog`。
pub fn show_dialog(spec: &Value) -> Result<Value> {
    let api = api()?;
    let f = appended!(api, show_dialog);
    let json = c_string(&spec.to_string());
    // SAFETY: 字符串在调用期间有效；回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, json.as_ptr(), Some(on_string), user) })
}

/// 修改本插件的设置（合并键）。不回调 `on_settings_changed`。
pub fn set_settings(values: &Value) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_settings);
    let json = c_string(&values.to_string());
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { f(api.ctx, json.as_ptr()) })
}

/// 把文字填入指定会话的输入框（不发送）；`replace` 时替换已有内容，否则插入到光标处。
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
    // SAFETY: arg 和字符串在调用期间有效。
    code(unsafe { f(api.ctx, &raw const arg.raw, text.as_ptr(), mode) })
}

/// 设置话题的关注状态（`abi::TOPIC_*`）。
pub fn set_topic_visibility(stream_id: i64, topic: &str, visibility: i32) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_topic_visibility);
    let topic = c_string(topic);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { f(api.ctx, stream_id, topic.as_ptr(), visibility) })
}

pub fn set_subscribed(stream_id: i64, subscribed: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_subscribed);
    // SAFETY: 没有指针参数。
    code(unsafe { f(api.ctx, stream_id, i32::from(subscribed)) })
}

/// 修改频道的个人设置，`name` 为 Zulip 的属性名（color、is_muted、desktop_notifications 等）。
pub fn set_subscription_property(stream_id: i64, name: &str, value: &Value) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_subscription_property);
    let name = c_string(name);
    let json = c_string(&value.to_string());
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { f(api.ctx, stream_id, name.as_ptr(), json.as_ptr()) })
}

/// 创建频道，格式见头文件 `create_channel`；返回频道 id。
pub fn create_channel(spec: &Value) -> Result<i64> {
    let api = api()?;
    let f = appended!(api, create_channel);
    let json = c_string(&spec.to_string());
    // SAFETY: 字符串在调用期间有效。
    id(unsafe { f(api.ctx, json.as_ptr()) })
}

/// 频道的订阅者 id 数组。
pub fn subscribers(stream_id: i64) -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_subscribers);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, stream_id, Some(on_string), user) })
}

/// 创建消息提醒，`deliver_at` 为 Unix 秒，返回提醒 id。
pub fn create_reminder(message_id: i64, deliver_at: i64) -> Result<i64> {
    let api = api()?;
    let f = appended!(api, create_reminder);
    // SAFETY: 没有指针参数。
    id(unsafe { f(api.ctx, message_id, deliver_at) })
}

pub fn cancel_reminder(reminder_id: i64) -> Result<()> {
    let api = api()?;
    let f = appended!(api, cancel_reminder);
    // SAFETY: 没有指针参数。
    code(unsafe { f(api.ctx, reminder_id) })
}

/// 尚未送达的提醒，格式见头文件 `get_reminders`。
pub fn reminders() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_reminders);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// 草稿，格式见头文件 `get_drafts`。
pub fn drafts() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_drafts);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// 保存草稿，空字符串表示删除。
pub fn save_draft(conv: &Conversation, markdown: &str) -> Result<()> {
    let api = api()?;
    let f = appended!(api, save_draft);
    let arg = ConvArg::new(conv);
    let markdown = c_string(markdown);
    // SAFETY: arg 和字符串在调用期间有效。
    code(unsafe { f(api.ctx, &raw const arg.raw, markdown.as_ptr()) })
}

/// 会话的免打扰级别（`abi::LEVEL_*`）。
pub fn conversation_level(conv: &Conversation) -> Result<i32> {
    let api = api()?;
    let f = appended!(api, get_conversation_level);
    let arg = ConvArg::new(conv);
    // SAFETY: arg 在调用期间有效。
    let result = unsafe { f(api.ctx, &raw const arg.raw) };
    code(result)?;
    Ok(result)
}

pub fn set_conversation_level(conv: &Conversation, level: i32) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_conversation_level);
    let arg = ConvArg::new(conv);
    // SAFETY: arg 在调用期间有效。
    code(unsafe { f(api.ctx, &raw const arg.raw, level) })
}

pub fn set_conversation_pinned(conv: &Conversation, pinned: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_conversation_pinned);
    let arg = ConvArg::new(conv);
    // SAFETY: arg 在调用期间有效。
    code(unsafe { f(api.ctx, &raw const arg.raw, i32::from(pinned)) })
}

pub fn set_user_muted(user_id: i64, muted: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_user_muted);
    // SAFETY: 没有指针参数。
    code(unsafe { f(api.ctx, user_id, i32::from(muted)) })
}

/// 添加（`on`）或删除提醒词。
pub fn set_alert_word(word: &str, on: bool) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_alert_word);
    let word = c_string(word);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { f(api.ctx, word.as_ptr(), i32::from(on)) })
}

/// 设置自己的签名，空字符串表示清除。
pub fn set_own_status(text: &str) -> Result<()> {
    let api = api()?;
    let f = appended!(api, set_own_status);
    let text = c_string(text);
    // SAFETY: 字符串在调用期间有效。
    code(unsafe { f(api.ctx, text.as_ptr()) })
}

/// 静音的用户 id 数组。
pub fn muted_users() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_muted_users);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// 提醒词数组。
pub fn alert_words() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_alert_words);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

/// 已读这条消息的用户 id 数组。
pub fn read_receipts(message_id: i64) -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_read_receipts);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, message_id, Some(on_string), user) })
}

/// 这条消息上可以执行的操作，格式见头文件 `get_message_actions`。
pub fn message_actions(message_id: i64) -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_message_actions);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, message_id, Some(on_string), user) })
}

/// 组织信息，格式见头文件 `get_realm_info`。
pub fn realm_info() -> Result<Value> {
    let api = api()?;
    let f = appended!(api, get_realm_info);
    // SAFETY: 回调只在调用期间使用 text。
    json_call(|user| unsafe { f(api.ctx, Some(on_string), user) })
}

// 带进度的调用共用同一个 user。
struct ProgressCall<'a> {
    text: String,
    progress: &'a mut (dyn FnMut(i64, i64) + Send),
}

unsafe extern "C" fn on_progress_result(s: *const c_char, user: *mut c_void) {
    // SAFETY: user 是调用方传入的 &mut ProgressCall，回调在调用期间同步执行。
    unsafe { (*user.cast::<ProgressCall<'_>>()).text = read_string(s) };
}

unsafe extern "C" fn on_progress(done: i64, total: i64, user: *mut c_void) {
    // SAFETY: 同上；宿主不会同时调用两个回调。
    unsafe { ((*user.cast::<ProgressCall<'_>>()).progress)(done, total) };
}

/// 下载附件到缓存，返回本地路径。`progress(已下载, 总大小)`，总大小未知时为 0；已缓存时不调用。
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
    // SAFETY: 字符串和 call 在调用期间有效；宿主函数返回后不再调用回调。
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

/// 上传文件，返回 `/user_uploads/...`。`progress(已上传, 总大小)` 可能在其他线程上调用。
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
    // SAFETY: 字符串和 call 在调用期间有效；宿主函数返回后不再调用回调。
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
