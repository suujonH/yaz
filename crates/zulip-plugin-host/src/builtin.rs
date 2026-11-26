//! Built-in plugins: features compiled into the program (model center, translation) that use the
//! same mechanisms as DLL plugins — a manifest (settings, message menus, dependencies), settings
//! storage, resources, message menus with shortcuts, attached content — but are always enabled,
//! need no confirmation and do not appear in the plugin list. They are called through
//! [`BuiltinPlugin`] instead of the C ABI and reach the host through [`BuiltinContext`].
//!
//! Callbacks run on the plugin thread (like a DLL plugin's) and must return quickly: slow work goes
//! to [`BuiltinContext::spawn`]. [`BuiltinPlugin::on_resource_request`] is the exception: it runs on
//! the requesting plugin's worker thread and may block.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use serde_json::{Value, json};
use zulip_plugin_sdk::abi;

use crate::annotation::PluginAnnotation;
use crate::event::PluginEvent;
use crate::host::PluginHost;
use crate::plugin::NativePlugin;
use crate::resources::{self, ResourceSelection};
use crate::settings::SettingsFeedback;

/// The message a message menu (or a tool button of attached content) was clicked on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinMessage {
    pub id: i64,
    pub account_id: i64,
    /// The message's Zulip Markdown.
    pub text: String,
}

/// A built-in feature. All methods have empty defaults.
pub trait BuiltinPlugin: Send + Sync + 'static {
    /// After the host is ready (on the plugin thread), before any other callback.
    fn init(&self, _ctx: &BuiltinContext) {}

    /// The user saved the settings.
    fn on_settings_changed(&self, _ctx: &BuiltinContext) {}

    /// A button in the settings: `{"key", "values", "settings", "row"?, "item"?}` (as a DLL
    /// plugin's `on_settings_button`).
    fn on_settings_button(&self, _ctx: &BuiltinContext, _click: &Value) {}

    /// A message menu, or a tool button of this plugin's attached content.
    fn on_message_menu(&self, _ctx: &BuiltinContext, _menu_id: i32, _message: &BuiltinMessage) {}

    /// Messages were shown in a chat window.
    fn on_messages_shown(&self, _ctx: &BuiltinContext, _account_id: i64, _message_ids: &[i64]) {}

    /// A request for a resource this plugin published: `{"type", "resource", "item", "request",
    /// "from"}`; returns the reply object (`{"error"}` on failure). Runs on the caller's thread.
    fn on_resource_request(&self, _ctx: &BuiltinContext, _request: &Value) -> Value {
        json!({"error": "不支持此请求"})
    }
}

/// What a built-in plugin can do with the host.
#[derive(Clone)]
pub struct BuiltinContext {
    pub(crate) plugin: Weak<NativePlugin>,
    pub(crate) host: Weak<PluginHost>,
}

impl BuiltinContext {
    fn parts(&self) -> Option<(Arc<NativePlugin>, Arc<PluginHost>)> {
        Some((self.plugin.upgrade()?, self.host.upgrade()?))
    }

    /// The saved settings (decrypted, with defaults).
    pub fn settings(&self) -> Value {
        self.plugin
            .upgrade()
            .and_then(|p| serde_json::from_str(&p.settings_json()).ok())
            .unwrap_or_else(|| json!({}))
    }

    /// This plugin's data directory (created if missing).
    pub fn data_dir(&self) -> Option<PathBuf> {
        let (plugin, host) = self.parts()?;
        let dir = host.plugin_data_dir(&plugin.id());
        std::fs::create_dir_all(&dir)
            .inspect_err(|e| log::warn!("无法创建数据目录：{}：{e}", dir.display()))
            .ok()?;
        Some(dir)
    }

    /// Writes to this plugin's log (`abi::LOG_*`).
    pub fn log(&self, level: i32, text: &str) {
        if let Some(plugin) = self.plugin.upgrade() {
            plugin.add_log(level, &plugin.name(), text);
        }
    }

    /// Replaces one kind of resource this plugin publishes, in the format of a DLL plugin's
    /// `publish_resources` (`[{"id", "name", "items"}]`); an empty list clears it.
    pub fn publish_resources(&self, kind: &str, resources: &Value) {
        let Some((plugin, host)) = self.parts() else {
            return;
        };
        match resources::parse(&plugin.id(), &resources.to_string()) {
            Some(list) if resources::is_valid_type(kind) => {
                host.publish_resources(&plugin.id(), kind, list);
            }
            _ => self.log(abi::LOG_WARNING, &format!("发布 {kind} 失败：格式错误")),
        }
    }

    /// Sends a request to the resource chosen in a `resource` setting (blocking: call it from
    /// [`spawn`](Self::spawn)); an `{"error"}` reply is returned as `Err`.
    pub fn request_resource(&self, selection: &Value, request: &Value) -> Result<Value, String> {
        const UNKNOWN_REPLY: &str = "返回的格式无法识别";
        let (plugin, host) = self.parts().ok_or("程序正在退出")?;
        let target = ResourceSelection::from_json(selection).ok_or("请先选择模型")?;
        let reply =
            host.request_resource(&plugin, &target, request)
                .map_err(|code| match code {
                    abi::ERR_NOT_FOUND => {
                        "选择的模型不可用：请检查模型是否仍在“设置 > 模型中心”中".to_owned()
                    }
                    abi::ERR_NETWORK => "等待回复超时".to_owned(),
                    code => format!("请求失败（{code}）"),
                })?;
        let value: Value = serde_json::from_str(&reply).map_err(|_| UNKNOWN_REPLY.to_owned())?;
        if !value.is_object() {
            return Err(UNKNOWN_REPLY.to_owned());
        }
        match value["error"].as_str() {
            Some(error) if !error.is_empty() => Err(error.to_owned()),
            _ => Ok(value),
        }
    }

    /// Sets (`Some`) or clears (`None`) this plugin's attached content on a message, in the format
    /// of a DLL plugin's `set_annotation`.
    pub fn set_annotation(&self, account_id: i64, message_id: i64, annotation: Option<&Value>) {
        let Some((plugin, host)) = self.parts() else {
            return;
        };
        let parsed = match annotation {
            None => None,
            Some(value) => match PluginAnnotation::parse(&plugin.id(), &value.to_string()) {
                Ok(parsed) => parsed,
                Err(error) => {
                    self.log(abi::LOG_WARNING, &format!("附加内容无效：{error}"));
                    return;
                }
            },
        };
        host.set_annotation(account_id, message_id, &plugin.id(), parsed);
    }

    /// Replies to the settings form (as a DLL plugin's `set_settings_feedback`).
    pub fn settings_feedback(&self, feedback: &Value) {
        let Some((plugin, host)) = self.parts() else {
            return;
        };
        match SettingsFeedback::parse(&feedback.to_string()) {
            Some(feedback) => host.emit(PluginEvent::SettingsFeedback {
                plugin_id: plugin.id(),
                feedback,
            }),
            None => self.log(abi::LOG_WARNING, "设置反馈格式错误"),
        }
    }

    /// Text below a settings button (`row` for a button inside a list row).
    pub fn settings_text(&self, key: &str, row: Option<i64>, text: &str) {
        let mut feedback = json!({"key": key, "text": text});
        if let Some(row) = row {
            feedback["row"] = json!(row);
        }
        self.settings_feedback(&feedback);
    }

    /// Runs slow work (network requests) off the plugin thread.
    pub fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        if let Some(host) = self.host.upgrade() {
            drop(host.runtime().spawn_blocking(job));
        }
    }
}
