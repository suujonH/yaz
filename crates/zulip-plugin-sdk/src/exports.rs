//! 导出函数的实现，由 [`export_plugin!`](crate::export_plugin) 生成的函数调用。

use std::ffi::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use serde_json::Value;

use crate::abi::{self, HostApi, RawMessage, RawOutgoing};
use crate::host;
use crate::types::{
    ChannelsChanged, ComposerClick, ConnectionChanged, ContextMenuClick, ConversationOpened,
    DraftChanged, MessageEdited, MessagesDeleted, MessagesShown, MutedUsersChanged, PanelRequest,
    PresenceChanged, ReactionChanged, ReadStateChanged, RealmChanged, RemindersChanged,
    ScheduledChanged, SettingsButton, TopicMoved, TypingChanged, UserChanged, UserTopicChanged,
    read_string,
};
use crate::{Event, Message, Outgoing, Plugin};

static PLUGIN: OnceLock<Box<dyn Plugin>> = OnceLock::new();

fn guarded<T>(default: T, f: impl FnOnce(&dyn Plugin) -> T) -> T {
    let Some(plugin) = PLUGIN.get() else {
        return default;
    };
    catch_unwind(AssertUnwindSafe(|| f(plugin.as_ref()))).unwrap_or_else(|_| {
        host::log(abi::LOG_ERROR, "插件回调发生 panic");
        default
    })
}

fn json_of(json: *const c_char) -> Value {
    // SAFETY: 宿主传入的字符串在回调期间有效。
    let text = unsafe { read_string(json) };
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn event_code(event: Event) -> i32 {
    match event {
        Event::Ignore => abi::EVENT_IGNORE,
        Event::Block => abi::EVENT_BLOCK,
    }
}

/// # Safety
/// `api` 是宿主传入的函数表。
pub unsafe fn init(
    api: *const HostApi,
    category: &'static str,
    create: impl FnOnce() -> Box<dyn Plugin>,
) -> i32 {
    // SAFETY: 宿主保证函数表一直有效。
    if !unsafe { host::install(api, category) } {
        return abi::ERR_INVALID_ARG;
    }
    match catch_unwind(AssertUnwindSafe(|| PLUGIN.get_or_init(create).init())) {
        Ok(Ok(())) => abi::OK,
        Ok(Err(error)) => error.0,
        Err(_) => {
            host::log(abi::LOG_ERROR, "初始化时发生 panic");
            abi::ERR_UNKNOWN
        }
    }
}

pub fn on_enable() {
    guarded((), |p| p.on_enable());
}

pub fn on_disable() {
    guarded((), |p| p.on_disable());
}

pub fn on_account_login(account_id: i64) {
    guarded((), |p| p.on_account_login(account_id));
}

pub fn on_account_logout(account_id: i64) {
    guarded((), |p| p.on_account_logout(account_id));
}

/// # Safety
/// `msg` 是宿主传入的消息。
pub unsafe fn on_message(msg: *const RawMessage) -> i32 {
    guarded(abi::EVENT_IGNORE, |p| {
        // SAFETY: 回调期间有效。
        event_code(p.on_message(&unsafe { Message::from_raw(&*msg) }))
    })
}

/// # Safety
/// `msg` 是宿主传入的消息。
pub unsafe fn filter_incoming(msg: *const RawMessage) -> i32 {
    guarded(abi::EVENT_IGNORE, |p| {
        // SAFETY: 回调期间有效。
        event_code(p.filter_incoming(&unsafe { Message::from_raw(&*msg) }))
    })
}

/// # Safety
/// `out` 是宿主传入的内容。
pub unsafe fn filter_outgoing(out: *const RawOutgoing) -> i32 {
    guarded(abi::EVENT_IGNORE, |p| {
        // SAFETY: 回调期间有效。
        event_code(p.filter_outgoing(&unsafe { Outgoing::from_raw(&*out) }))
    })
}

pub fn on_menu(menu_id: i32) {
    guarded((), |p| p.on_menu(menu_id));
}

/// # Safety
/// `msg` 是宿主传入的消息。
pub unsafe fn on_message_menu(menu_id: i32, msg: *const RawMessage) {
    guarded((), |p| {
        // SAFETY: 回调期间有效。
        p.on_message_menu(menu_id, &unsafe { Message::from_raw(&*msg) });
    });
}

/// # Safety
/// `json` 是宿主传入的字符串。
pub unsafe fn on_messages_shown(json: *const c_char) {
    guarded((), |p| {
        p.on_messages_shown(&MessagesShown::from_json(&json_of(json)));
    });
}

pub fn on_settings_changed() {
    guarded((), |p| p.on_settings_changed());
}

/// # Safety
/// `json` 是宿主传入的字符串。
pub unsafe fn on_settings_button(json: *const c_char) {
    guarded((), |p| {
        p.on_settings_button(&SettingsButton::from_json(&json_of(json)));
    });
}

/// # Safety
/// `json` 是宿主传入的字符串。
pub unsafe fn on_resource_request(request_id: i64, json: *const c_char) {
    guarded((), |p| p.on_resource_request(request_id, &json_of(json)));
}

/// # Safety
/// `json` 是宿主传入的字符串。
pub unsafe fn on_panel(json: *const c_char) {
    guarded((), |p| p.on_panel(&PanelRequest::from_json(&json_of(json))));
}

macro_rules! notification {
    ($($name:ident: $ty:ident),* $(,)?) => {
        $(
            /// # Safety
            /// `json` 是宿主传入的字符串。
            pub unsafe fn $name(json: *const c_char) {
                guarded((), |p| p.$name(&$ty::from_json(&json_of(json))));
            }
        )*
    };
}

notification! {
    on_message_edited: MessageEdited,
    on_message_deleted: MessagesDeleted,
    on_reaction_changed: ReactionChanged,
    on_read_state_changed: ReadStateChanged,
    on_topic_moved: TopicMoved,
    on_channel_changed: ChannelsChanged,
    on_user_changed: UserChanged,
    on_presence_changed: PresenceChanged,
    on_conversation_opened: ConversationOpened,
    on_connection_changed: ConnectionChanged,
    on_typing_changed: TypingChanged,
    on_scheduled_changed: ScheduledChanged,
    on_reminders_changed: RemindersChanged,
    on_draft_changed: DraftChanged,
    on_user_topic_changed: UserTopicChanged,
    on_muted_users_changed: MutedUsersChanged,
    on_realm_changed: RealmChanged,
}

/// # Safety
/// `json` 是宿主传入的字符串。
pub unsafe fn on_context_menu(menu_id: i32, json: *const c_char) {
    guarded((), |p| {
        p.on_context_menu(menu_id, &ContextMenuClick::from_json(&json_of(json)));
    });
}

/// # Safety
/// `json` 是宿主传入的字符串。
pub unsafe fn on_composer_button(button_id: i32, json: *const c_char) {
    guarded((), |p| {
        p.on_composer_button(button_id, &ComposerClick::from_json(&json_of(json)));
    });
}
