use zulip_core::ConversationKey;

use crate::panel::DialogSpec;
use crate::settings::SettingsFeedback;

/// Events of the plugin host. May be emitted on any thread.
#[derive(Clone, Debug)]
pub enum PluginEvent {
    /// A plugin was loaded or enabled, or its quick actions changed.
    PluginsChanged,
    /// The attached content, collapsed state, or "show original" state of a message changed.
    AnnotationChanged {
        account_id: i64,
        message_id: i64,
    },
    /// A resource published by a plugin changed.
    ResourcesChanged,
    /// A plugin's reply to the settings window (ignored if the window is already closed).
    SettingsFeedback {
        plugin_id: String,
        feedback: SettingsFeedback,
    },
    /// Switch to a conversation; when `message_id` is non-empty, jump to that message.
    OpenConversation {
        account_id: i64,
        key: ConversationKey,
        message_id: Option<i64>,
    },
    /// Fill the composer of that account (without sending); when `conv` is empty it is the current
    /// conversation, otherwise that conversation; with `replace`, existing content is replaced,
    /// otherwise it is inserted at the cursor.
    FillComposer {
        plugin_id: String,
        account_id: i64,
        conv: Option<ConversationKey>,
        text: String,
        replace: bool,
    },
    Toast {
        plugin_id: String,
        text: String,
    },
    /// System notification.
    SystemNotification {
        plugin_id: String,
        title: String,
        body: String,
    },
    /// [`PluginHost::status_text`](crate::PluginHost::status_text) changed.
    StatusTextChanged,
    /// A plugin changed its own settings with set_settings.
    SettingsChanged(String),
    /// A plugin asks to show a dialog; reply with
    /// [`PluginHost::reply_dialog`](crate::PluginHost::reply_dialog). The event may be missed, so
    /// the UI should rely on [`PluginHost::pending_dialogs`](crate::PluginHost::pending_dialogs).
    ShowDialog {
        request_id: i64,
        plugin_id: String,
        account_id: i64,
        spec: DialogSpec,
    },
    /// The dialog wait timed out; the UI no longer needs to show it.
    DialogClosed {
        request_id: i64,
    },
}
