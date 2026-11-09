//! Safe wrapper for Rust plugins. Implement [`Plugin`] and export it with [`export_plugin!`].
//!
//! All exported functions are called on the host's plugin thread; move slow work to a worker thread
//! with [`post`]. A panic inside the plugin is caught and written to the plugin log.

pub mod abi;
#[cfg(feature = "annotation-store")]
pub mod annotation_store;
#[doc(hidden)]
pub mod exports;
mod host;
pub mod model_center;
mod types;
mod worker;

pub use host::*;
pub use serde_json;
pub use types::*;
pub use worker::post;

/// Plugin callbacks, all invoked by the host on the plugin thread. Callbacks that are not
/// implemented do nothing.
#[allow(unused_variables)]
pub trait Plugin: Send + Sync + 'static {
    /// Called when the plugin loads. Settings can be read and resources published at this point.
    fn init(&self) -> Result<()> {
        Ok(())
    }
    fn on_enable(&self) {}
    fn on_disable(&self) {}
    fn on_account_login(&self, account_id: i64) {}
    fn on_account_logout(&self, account_id: i64) {}
    fn on_message(&self, message: &Message) -> Event {
        Event::Ignore
    }
    fn filter_incoming(&self, message: &Message) -> Event {
        Event::Ignore
    }
    /// Use [`set_outgoing_content`] to rewrite the content.
    fn filter_outgoing(&self, outgoing: &Outgoing) -> Event {
        Event::Ignore
    }
    fn on_menu(&self, menu_id: i32) {}
    fn on_message_menu(&self, menu_id: i32, message: &Message) {}
    fn on_messages_shown(&self, shown: &MessagesShown) {}
    fn on_settings_changed(&self) {}
    fn on_settings_button(&self, button: &SettingsButton) {}
    fn on_resource_request(&self, request_id: i64, request: &serde_json::Value) {}
    /// Reply inside the callback with [`reply_panel`].
    fn on_panel(&self, request: &PanelRequest) {}
    fn on_message_edited(&self, event: &MessageEdited) {}
    fn on_message_deleted(&self, event: &MessagesDeleted) {}
    fn on_reaction_changed(&self, event: &ReactionChanged) {}
    fn on_read_state_changed(&self, event: &ReadStateChanged) {}
    fn on_topic_moved(&self, event: &TopicMoved) {}
    fn on_channel_changed(&self, event: &ChannelsChanged) {}
    fn on_user_changed(&self, event: &UserChanged) {}
    fn on_presence_changed(&self, event: &PresenceChanged) {}
    fn on_conversation_opened(&self, event: &ConversationOpened) {}
    fn on_connection_changed(&self, event: &ConnectionChanged) {}
    fn on_typing_changed(&self, event: &TypingChanged) {}
    fn on_scheduled_changed(&self, event: &ScheduledChanged) {}
    fn on_reminders_changed(&self, event: &RemindersChanged) {}
    fn on_draft_changed(&self, event: &DraftChanged) {}
    fn on_user_topic_changed(&self, event: &UserTopicChanged) {}
    fn on_muted_users_changed(&self, event: &MutedUsersChanged) {}
    fn on_realm_changed(&self, event: &RealmChanged) {}
    /// A menu item of conversation_menus, user_menus or channel_menus was clicked.
    fn on_context_menu(&self, menu_id: i32, click: &ContextMenuClick) {}
    /// A button of composer_buttons was clicked; use [`fill_composer`] to replace the composer
    /// content.
    fn on_composer_button(&self, button_id: i32, click: &ComposerClick) {}
}

#[doc(hidden)]
#[macro_export]
macro_rules! export_notifications {
    ($($export:ident => $name:ident),* $(,)?) => {
        $(
            /// # Safety
            /// Called by the host.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $export(json: *const ::std::ffi::c_char) {
                unsafe { $crate::exports::$name(json) }
            }
        )*
    };
}

/// Exports the plugin: `export_plugin!(MyPlugin::default(), "log category");`
#[macro_export]
macro_rules! export_plugin {
    ($plugin:expr, $category:expr) => {
        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_init(api: *const $crate::abi::HostApi) -> i32 {
            unsafe { $crate::exports::init(api, $category, || ::std::boxed::Box::new($plugin)) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn zulip_plugin_on_enable() {
            $crate::exports::on_enable()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn zulip_plugin_on_disable() {
            $crate::exports::on_disable()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn zulip_plugin_on_account_login(account_id: i64) {
            $crate::exports::on_account_login(account_id)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn zulip_plugin_on_account_logout(account_id: i64) {
            $crate::exports::on_account_logout(account_id)
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_on_message(
            msg: *const $crate::abi::RawMessage,
        ) -> i32 {
            unsafe { $crate::exports::on_message(msg) }
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_filter_incoming(
            msg: *const $crate::abi::RawMessage,
        ) -> i32 {
            unsafe { $crate::exports::filter_incoming(msg) }
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_filter_outgoing(
            out: *const $crate::abi::RawOutgoing,
        ) -> i32 {
            unsafe { $crate::exports::filter_outgoing(out) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn zulip_plugin_on_menu(menu_id: i32) {
            $crate::exports::on_menu(menu_id)
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_on_message_menu(
            menu_id: i32,
            msg: *const $crate::abi::RawMessage,
        ) {
            unsafe { $crate::exports::on_message_menu(menu_id, msg) }
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_on_messages_shown(json: *const ::std::ffi::c_char) {
            unsafe { $crate::exports::on_messages_shown(json) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn zulip_plugin_on_settings_changed() {
            $crate::exports::on_settings_changed()
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_on_settings_button(json: *const ::std::ffi::c_char) {
            unsafe { $crate::exports::on_settings_button(json) }
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_on_resource_request(
            request_id: i64,
            json: *const ::std::ffi::c_char,
        ) {
            unsafe { $crate::exports::on_resource_request(request_id, json) }
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_on_panel(json: *const ::std::ffi::c_char) {
            unsafe { $crate::exports::on_panel(json) }
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_on_context_menu(
            menu_id: i32,
            json: *const ::std::ffi::c_char,
        ) {
            unsafe { $crate::exports::on_context_menu(menu_id, json) }
        }

        /// # Safety
        /// Called by the host.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zulip_plugin_on_composer_button(
            button_id: i32,
            json: *const ::std::ffi::c_char,
        ) {
            unsafe { $crate::exports::on_composer_button(button_id, json) }
        }

        $crate::export_notifications! {
            zulip_plugin_on_message_edited => on_message_edited,
            zulip_plugin_on_message_deleted => on_message_deleted,
            zulip_plugin_on_reaction_changed => on_reaction_changed,
            zulip_plugin_on_read_state_changed => on_read_state_changed,
            zulip_plugin_on_topic_moved => on_topic_moved,
            zulip_plugin_on_channel_changed => on_channel_changed,
            zulip_plugin_on_user_changed => on_user_changed,
            zulip_plugin_on_presence_changed => on_presence_changed,
            zulip_plugin_on_conversation_opened => on_conversation_opened,
            zulip_plugin_on_connection_changed => on_connection_changed,
            zulip_plugin_on_typing_changed => on_typing_changed,
            zulip_plugin_on_scheduled_changed => on_scheduled_changed,
            zulip_plugin_on_reminders_changed => on_reminders_changed,
            zulip_plugin_on_draft_changed => on_draft_changed,
            zulip_plugin_on_user_topic_changed => on_user_topic_changed,
            zulip_plugin_on_muted_users_changed => on_muted_users_changed,
            zulip_plugin_on_realm_changed => on_realm_changed,
        }
    };
}
