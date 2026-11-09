//! 与 `sdk/zulip_plugin.h` 一一对应的 C ABI（x64）。常量和结构体的说明见头文件。

use std::ffi::{c_char, c_void};

pub const API_VERSION: u32 = 1;

pub const EVENT_IGNORE: i32 = 0;
pub const EVENT_BLOCK: i32 = 1;

pub const OK: i32 = 0;
pub const ERR_UNKNOWN: i32 = -1;
pub const ERR_INVALID_ARG: i32 = -2;
pub const ERR_PERMISSION: i32 = -3;
pub const ERR_NOT_LOGGED_IN: i32 = -4;
pub const ERR_NETWORK: i32 = -5;
pub const ERR_SERVER: i32 = -6;
pub const ERR_NOT_FOUND: i32 = -7;
pub const ERR_WRONG_CONTEXT: i32 = -8;
pub const ERR_UNSUPPORTED: i32 = -9;
pub const ERR_DISABLED: i32 = -10;

pub const LOG_DEBUG: i32 = 0;
pub const LOG_INFO: i32 = 10;
pub const LOG_INFO_SUCCESS: i32 = 11;
pub const LOG_INFO_RECV: i32 = 12;
pub const LOG_INFO_SEND: i32 = 13;
pub const LOG_WARNING: i32 = 20;
pub const LOG_ERROR: i32 = 30;
pub const LOG_FATAL: i32 = 40;

pub const CONV_CHANNEL: i32 = 1;
pub const CONV_DIRECT: i32 = 2;

pub const MSG_OUTGOING: u32 = 0x01;
pub const MSG_MENTIONED: u32 = 0x02;
pub const MSG_WILDCARD_MENTION: u32 = 0x04;
pub const MSG_STARRED: u32 = 0x08;
pub const MSG_READ: u32 = 0x10;
pub const MSG_HISTORICAL: u32 = 0x20;

pub const OUTGOING_TEXT: i32 = 0;
pub const OUTGOING_FILE: i32 = 1;
pub const OUTGOING_FORWARD: i32 = 2;

pub const FILL_INSERT: i32 = 0;
pub const FILL_REPLACE: i32 = 1;

pub const TOPIC_INHERIT: i32 = 0;
pub const TOPIC_MUTED: i32 = 1;
pub const TOPIC_UNMUTED: i32 = 2;
pub const TOPIC_FOLLOWED: i32 = 3;

pub const LEVEL_NOTIFY: i32 = 0;
pub const LEVEL_SILENT: i32 = 1;
pub const LEVEL_ASSISTANT: i32 = 2;
pub const LEVEL_BLOCKED: i32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawConversation {
    pub struct_size: u32,
    pub kind: i32,
    pub stream_id: i64,
    pub stream_name: *const c_char,
    pub topic: *const c_char,
    pub dm_user_ids: *const i64,
    pub dm_user_count: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawMessage {
    pub struct_size: u32,
    pub flags: u32,
    pub message_id: i64,
    pub account_id: i64,
    pub timestamp: i64,
    pub sender_id: i64,
    pub sender_name: *const c_char,
    pub sender_email: *const c_char,
    pub content_html: *const c_char,
    pub content_text: *const c_char,
    pub conv: *const RawConversation,
    pub reactions_json: *const c_char,
    pub uploads_json: *const c_char,
    pub last_edit_timestamp: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawOutgoing {
    pub struct_size: u32,
    pub kind: i32,
    pub account_id: i64,
    pub conv: *const RawConversation,
    pub content: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawUser {
    pub struct_size: u32,
    pub role: i32,
    pub user_id: i64,
    pub full_name: *const c_char,
    pub email: *const c_char,
    pub remark: *const c_char,
    pub is_bot: i32,
    pub is_active: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawChannel {
    pub struct_size: u32,
    pub is_private: i32,
    pub stream_id: i64,
    pub name: *const c_char,
    pub description: *const c_char,
    pub is_subscribed: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawTopic {
    pub struct_size: u32,
    pub stream_id: i64,
    pub max_message_id: i64,
    pub name: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawAccount {
    pub struct_size: u32,
    pub account_id: i64,
    pub user_id: i64,
    pub full_name: *const c_char,
    pub email: *const c_char,
    pub realm_url: *const c_char,
}

pub type Ctx = *mut c_void;
pub type UserData = *mut c_void;
pub type StringCb = unsafe extern "C" fn(*const c_char, UserData);
pub type MessageCb = unsafe extern "C" fn(*const RawMessage, UserData);
pub type UserCb = unsafe extern "C" fn(*const RawUser, UserData);
pub type ChannelCb = unsafe extern "C" fn(*const RawChannel, UserData);
pub type TopicCb = unsafe extern "C" fn(*const RawTopic, UserData);
pub type AccountCb = unsafe extern "C" fn(*const RawAccount, UserData);
pub type ProgressCb = unsafe extern "C" fn(i64, i64, UserData);

/// 宿主函数表。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    pub struct_size: u32,
    pub api_version: u32,
    pub ctx: Ctx,
    pub add_log: unsafe extern "C" fn(Ctx, i32, *const c_char, *const c_char),
    pub get_accounts: unsafe extern "C" fn(Ctx, Option<AccountCb>, UserData) -> i32,
    pub get_current_account: unsafe extern "C" fn(Ctx) -> i64,
    pub select_account: unsafe extern "C" fn(Ctx, i64) -> i32,
    pub get_data_dir: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub get_settings: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub set_settings_feedback: unsafe extern "C" fn(Ctx, *const c_char) -> i32,
    pub send_channel_message: unsafe extern "C" fn(Ctx, i64, *const c_char, *const c_char) -> i64,
    pub send_direct_message: unsafe extern "C" fn(Ctx, *const i64, i32, *const c_char) -> i64,
    pub get_message: unsafe extern "C" fn(Ctx, i64, Option<MessageCb>, UserData) -> i32,
    pub get_messages: unsafe extern "C" fn(
        Ctx,
        *const RawConversation,
        i64,
        i32,
        i32,
        Option<MessageCb>,
        UserData,
    ) -> i32,
    pub get_users: unsafe extern "C" fn(Ctx, Option<UserCb>, UserData) -> i32,
    pub get_user: unsafe extern "C" fn(Ctx, i64, Option<UserCb>, UserData) -> i32,
    pub get_channels: unsafe extern "C" fn(Ctx, Option<ChannelCb>, UserData) -> i32,
    pub get_topics: unsafe extern "C" fn(Ctx, i64, Option<TopicCb>, UserData) -> i32,
    pub add_reaction: unsafe extern "C" fn(Ctx, i64, *const c_char) -> i32,
    pub set_starred: unsafe extern "C" fn(Ctx, i64, i32) -> i32,
    pub mark_read: unsafe extern "C" fn(Ctx, i64) -> i32,
    pub upload_file: unsafe extern "C" fn(Ctx, *const c_char, Option<StringCb>, UserData) -> i32,
    pub set_outgoing_content: unsafe extern "C" fn(Ctx, *const c_char) -> i32,
    pub set_annotation: unsafe extern "C" fn(Ctx, i64, *const c_char) -> i32,
    pub launch: unsafe extern "C" fn(Ctx, *const c_char, *const c_char) -> i32,
    pub edit_message: unsafe extern "C" fn(Ctx, i64, *const c_char) -> i32,
    pub delete_message: unsafe extern "C" fn(Ctx, i64) -> i32,
    pub update_channel: unsafe extern "C" fn(Ctx, i64, *const c_char, *const c_char) -> i32,
    pub update_user: unsafe extern "C" fn(Ctx, i64, *const c_char, i32) -> i32,
    pub publish_resources: unsafe extern "C" fn(Ctx, *const c_char, *const c_char) -> i32,
    pub request_resource:
        unsafe extern "C" fn(Ctx, *const c_char, Option<StringCb>, UserData) -> i32,
    pub reply_resource: unsafe extern "C" fn(Ctx, i64, *const c_char) -> i32,
    pub reply_panel: unsafe extern "C" fn(Ctx, *const c_char) -> i32,
    pub search_messages:
        unsafe extern "C" fn(Ctx, *const c_char, *mut i64, Option<MessageCb>, UserData) -> i32,
    pub get_conversations: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub get_current_conversation: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub get_presence: unsafe extern "C" fn(Ctx, i64, Option<StringCb>, UserData) -> i32,
    pub get_user_groups: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub get_all_channels: unsafe extern "C" fn(Ctx, Option<ChannelCb>, UserData) -> i32,
    pub get_topic_visibility: unsafe extern "C" fn(Ctx, i64, *const c_char) -> i32,
    pub get_edit_history: unsafe extern "C" fn(Ctx, i64, Option<StringCb>, UserData) -> i32,
    pub remove_reaction: unsafe extern "C" fn(Ctx, i64, *const c_char) -> i32,
    pub mark_unread: unsafe extern "C" fn(Ctx, i64) -> i32,
    pub move_topic: unsafe extern "C" fn(Ctx, i64, *const c_char, i64, *const c_char) -> i32,
    pub set_topic_resolved: unsafe extern "C" fn(Ctx, i64, *const c_char, i32) -> i32,
    pub delete_topic: unsafe extern "C" fn(Ctx, i64, *const c_char) -> i32,
    pub open_conversation: unsafe extern "C" fn(Ctx, *const RawConversation, i64) -> i32,
    pub fill_composer: unsafe extern "C" fn(Ctx, *const c_char, i32) -> i32,
    pub schedule_message:
        unsafe extern "C" fn(Ctx, *const RawConversation, *const c_char, i64) -> i64,
    pub show_toast: unsafe extern "C" fn(Ctx, *const c_char) -> i32,
    pub show_notification: unsafe extern "C" fn(Ctx, *const c_char, *const c_char) -> i32,
    pub set_status_text: unsafe extern "C" fn(Ctx, *const c_char) -> i32,
    pub download_attachment:
        unsafe extern "C" fn(Ctx, *const c_char, Option<StringCb>, UserData) -> i32,
    pub api_request: unsafe extern "C" fn(
        Ctx,
        *const c_char,
        *const c_char,
        *const c_char,
        Option<StringCb>,
        UserData,
    ) -> i32,
    pub set_thread_account: unsafe extern "C" fn(Ctx, i64) -> i32,
    pub get_last_error: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub get_message_source: unsafe extern "C" fn(Ctx, i64, Option<StringCb>, UserData) -> i32,
    pub get_scheduled_messages: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub cancel_scheduled_message: unsafe extern "C" fn(Ctx, i64) -> i32,
    pub show_dialog: unsafe extern "C" fn(Ctx, *const c_char, Option<StringCb>, UserData) -> i32,
    pub set_settings: unsafe extern "C" fn(Ctx, *const c_char) -> i32,
    pub fill_composer_to:
        unsafe extern "C" fn(Ctx, *const RawConversation, *const c_char, i32) -> i32,
    pub set_topic_visibility: unsafe extern "C" fn(Ctx, i64, *const c_char, i32) -> i32,
    pub set_subscribed: unsafe extern "C" fn(Ctx, i64, i32) -> i32,
    pub set_subscription_property:
        unsafe extern "C" fn(Ctx, i64, *const c_char, *const c_char) -> i32,
    pub create_channel: unsafe extern "C" fn(Ctx, *const c_char) -> i64,
    pub get_subscribers: unsafe extern "C" fn(Ctx, i64, Option<StringCb>, UserData) -> i32,
    pub create_reminder: unsafe extern "C" fn(Ctx, i64, i64) -> i64,
    pub cancel_reminder: unsafe extern "C" fn(Ctx, i64) -> i32,
    pub get_reminders: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub get_drafts: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub save_draft: unsafe extern "C" fn(Ctx, *const RawConversation, *const c_char) -> i32,
    pub get_conversation_level: unsafe extern "C" fn(Ctx, *const RawConversation) -> i32,
    pub set_conversation_level: unsafe extern "C" fn(Ctx, *const RawConversation, i32) -> i32,
    pub set_conversation_pinned: unsafe extern "C" fn(Ctx, *const RawConversation, i32) -> i32,
    pub set_user_muted: unsafe extern "C" fn(Ctx, i64, i32) -> i32,
    pub set_alert_word: unsafe extern "C" fn(Ctx, *const c_char, i32) -> i32,
    pub set_own_status: unsafe extern "C" fn(Ctx, *const c_char) -> i32,
    pub get_muted_users: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub get_alert_words: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub get_read_receipts: unsafe extern "C" fn(Ctx, i64, Option<StringCb>, UserData) -> i32,
    pub get_message_actions: unsafe extern "C" fn(Ctx, i64, Option<StringCb>, UserData) -> i32,
    pub get_realm_info: unsafe extern "C" fn(Ctx, Option<StringCb>, UserData) -> i32,
    pub download_attachment_ex: unsafe extern "C" fn(
        Ctx,
        *const c_char,
        Option<ProgressCb>,
        Option<StringCb>,
        UserData,
    ) -> i32,
    pub upload_file_ex: unsafe extern "C" fn(
        Ctx,
        *const c_char,
        Option<ProgressCb>,
        Option<StringCb>,
        UserData,
    ) -> i32,
}

/// v1 基础表的大小（到 api_request），比它长的部分是追加的函数。
pub const HOST_API_V1_SIZE: usize = std::mem::offset_of!(HostApi, api_request) + size_of::<usize>();

// 插件导出函数的类型。
pub type InitFn = unsafe extern "C" fn(*const HostApi) -> i32;
pub type VoidFn = unsafe extern "C" fn();
pub type AccountFn = unsafe extern "C" fn(i64);
pub type MessageFn = unsafe extern "C" fn(*const RawMessage) -> i32;
pub type OutgoingFn = unsafe extern "C" fn(*const RawOutgoing) -> i32;
pub type MenuFn = unsafe extern "C" fn(i32);
pub type MessageMenuFn = unsafe extern "C" fn(i32, *const RawMessage);
pub type JsonFn = unsafe extern "C" fn(*const c_char);
pub type ResourceRequestFn = unsafe extern "C" fn(i64, *const c_char);
pub type IdJsonFn = unsafe extern "C" fn(i32, *const c_char);

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    // 与 sdk/examples/echo.c 中的 _Static_assert 相同。
    #[test]
    fn layout_matches_header() {
        assert_eq!(size_of::<RawConversation>(), 48);
        assert_eq!(offset_of!(RawConversation, stream_id), 8);
        assert_eq!(offset_of!(RawConversation, dm_user_count), 40);
        assert_eq!(size_of::<RawMessage>(), 104);
        assert_eq!(offset_of!(RawMessage, message_id), 8);
        assert_eq!(offset_of!(RawMessage, conv), 72);
        assert_eq!(offset_of!(RawMessage, last_edit_timestamp), 96);
        assert_eq!(size_of::<RawOutgoing>(), 32);
        assert_eq!(offset_of!(RawOutgoing, content), 24);
        assert_eq!(size_of::<RawUser>(), 48);
        assert_eq!(offset_of!(RawUser, is_bot), 40);
        assert_eq!(size_of::<RawChannel>(), 40);
        assert_eq!(size_of::<RawTopic>(), 32);
        assert_eq!(size_of::<RawAccount>(), 48);
        assert_eq!(HOST_API_V1_SIZE, 424);
        assert_eq!(size_of::<HostApi>(), 672);
        assert_eq!(offset_of!(HostApi, add_log), 16);
        assert_eq!(offset_of!(HostApi, reply_panel), 248);
        assert_eq!(offset_of!(HostApi, api_request), 416);
        assert_eq!(offset_of!(HostApi, set_thread_account), 424);
        assert_eq!(offset_of!(HostApi, upload_file_ex), 664);
    }
}
