//! 从通知弹窗和托盘列表打开会话或消息提醒。

use crate::services::services;
use crate::state::{AppCommand, send_command};
use crate::windows;
use zulip_core::ConversationKey;

/// 切到这个账号并显示主窗口，再打开会话（`key`）；`key` 为空时打开消息提醒。
pub fn open_notification_target(
    account: i64,
    key: Option<ConversationKey>,
    message_id: Option<i64>,
) {
    let hub = &services().hub;
    if hub.active_account() != Some(account) {
        hub.set_active(Some(account));
    }
    send_command(AppCommand::ShowMain);
    match key {
        Some(key) => windows::open_chat(account, key, message_id),
        None => windows::open_reminders(account),
    }
}
