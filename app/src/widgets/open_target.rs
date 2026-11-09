//! Open a conversation or message reminder from the notification popup and the tray list.

use crate::services::services;
use crate::state::{AppCommand, send_command};
use crate::windows;
use zulip_core::ConversationKey;

/// Switch to this account and show the main window, then open the conversation (`key`); when `key`
/// is empty, open the message reminder.
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
