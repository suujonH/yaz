//! 原生 DLL 插件宿主，没有 UI 依赖：界面通过 [`PluginHost`] 的方法和 [`PluginEvent`] 使用。
//! 插件的 C ABI 见 `sdk/zulip_plugin.h`。

pub mod annotation;
mod chain;
pub mod event;
pub mod fingerprint;
pub mod host;
mod host_api;
pub mod launch;
pub mod manifest;
pub mod markup;
mod marshal;
pub mod panel;
pub mod permission;
pub mod plugin;
mod request;
pub mod resources;
pub mod setting_field;
pub mod settings;
pub mod shortcuts;
mod thread;

pub use event::PluginEvent;
pub use host::{
    ActionOutcome, ActionView, AnnotationCard, ApprovalInfo, CHAIN_TIMEOUT, ComposerButton,
    ContextMenuKind, ContextMenuTarget, DIALOG_TIMEOUT, MessageAnnotations, OutgoingRejected,
    PendingDialog, PluginError, PluginHost, PluginHostConfig, PluginStatusText, RenderedContent,
    ResourceOption, SettingsButtonClick, ShortcutRow, TargetCandidate, ToggleOutcome,
    default_plugins_dir,
};
pub use manifest::{
    ComposerIcon, ManifestError, MenuContext, MenuConv, MenuWhen, PluginComposerButton,
    PluginManifest, PluginMenu,
};
pub use panel::{
    DialogKind, DialogResult, DialogSpec, PanelEntry, PanelError, PanelField, PanelFieldValue,
    PanelOption, PanelRequest, PluginPanel, PluginTarget, PluginTargetKind, parse_entries,
    parse_fields,
};
pub use plugin::{LogEntry, LogKind, NativePlugin};
pub use resources::{PluginResource, ResourceSelection};
pub use setting_field::{PluginSettingField, PluginSettingOption, PluginSettingType, VisibleWhen};
pub use settings::SettingsFeedback;
pub use shortcuts::{Modifiers, MouseButton, key_gesture, mouse_gesture};
