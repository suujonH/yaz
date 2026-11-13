//! Shared components wired to conversation data: avatar, card, conversation menu, account switcher.
//! Shared by all pages.

mod cards;
mod conversation_avatar;
mod datetime_input;
mod files_flow;
mod friend_groups;
mod menus;
mod message_flows;
mod message_results;
mod open_target;
mod panes;
mod plugin_ui;
mod push_controls;
mod remote;
mod remote_image;
mod window_keys;

pub use cards::{ProfilePane, open_topic_card, open_user_card};
pub use menus::{
    channel_section_menu, conversation_menu, level_menu_items, logout_confirm, menu_conv,
    mock_notice, plugin_context_menus, plugin_menu_entries, topic_delete_item, topic_more_submenu,
    topic_move_item, topic_rename_item, topic_resolve_item,
};
pub use push_controls::{
    ChannelPushFlags, PushEffect, PushEffectIcons, TopicPushFlags, channel_push, push_available,
    push_changed, topic_push,
};
pub use remote::{AccountSwitcher, RemoteAvatar, cached_avatar, downloaded_image, local_image};

pub use conversation_avatar::{
    AccountAvatar, ConversationAvatarView, conversation_rows, notice_label,
};
pub use datetime_input::{ask_future_time, end_of_day, local_timestamp, parse_date};
pub use files_flow::{
    FileItemCard, FileListRow, FileSource, UploadProgress, downloads_epoch, file_icon, format_size,
    open_file,
};
pub use friend_groups::{add_friend_group, friend_group_menu};
pub use message_flows::{
    can_send_to, forward, forward_messages, forward_text, locate_in_chat, open_link, unstar,
};
pub use message_results::{
    DateHeader, DateRow, MessageResultRow, ResultList, SEARCH_DEBOUNCE, header_key, message_images,
    message_rows, row_keys, use_debounced, use_message_results, with_headers,
};
pub use open_target::open_notification_target;
pub use panes::{
    NavRow, PaneHeader, Selection, SelectionBar, SplitView, set_if_alive, use_selection,
    use_window_account,
};
pub use plugin_ui::{
    FieldLayout, PluginPanelField, PluginPanelGroups, PluginRef, ask_plugin_dialog, plugin_text,
    topic_target,
};
pub use remote_image::RemoteImage;
pub use window_keys::{ArrowKey, use_close_shortcut, use_plugin_key_shortcuts, use_window_keys};

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use zulip_core::ConversationKey;
use zulip_session::Session;

/// Conversation in component props: props must be comparable, and conversations are compared by
/// pointer.
#[derive(Clone)]
pub struct SessionRef(pub Arc<Session>);

impl PartialEq for SessionRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Stable key for the virtual list.
pub fn key_id(key: &ConversationKey) -> u64 {
    text_id(&key.to_string())
}

pub fn text_id(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
