//! Translation: target language, model and prompt, plus the mouse gesture that translates a
//! message.

use super::field::*;
use super::plugin_form::BuiltinSettings;
use super::plugins::{MouseRecorder, record};
use crate::services::plugins;
use crate::state::use_plugin_events;
use crate::ui::*;
use dioxus::prelude::*;
use zulip_builtins::translator::ID;
use zulip_plugin_host::PluginEvent;

#[component]
pub fn Translation() -> Element {
    rsx! {
        BuiltinSettings {
            id: ID,
            title: "翻译",
            extra: rsx! { TranslationShortcut {} },
        }
    }
}

/// The mouse gesture on a message that translates it (middle click by default).
#[component]
fn TranslationShortcut() -> Element {
    let overlays = use_overlays();
    let mut rev = use_signal(|| 0u64);
    use_plugin_events(move |event| {
        if matches!(event, PluginEvent::PluginsChanged) {
            rev += 1;
        }
    });
    let _ = rev();
    let rows: Vec<_> = plugins()
        .message_shortcuts()
        .into_iter()
        .filter(|row| row.plugin_id == ID)
        .collect();
    rsx! {
        if !rows.is_empty() {
            Group { title: "快捷操作",
                for row in rows {
                    SettingRow {
                        key: "{row.key}",
                        label: "在消息上翻译",
                        description: "在消息上按下这个鼠标操作即翻译；点击输入框录制，Backspace 清除",
                        MouseRecorder {
                            label: row.label(),
                            onchange: move |gesture: String| {
                                record(overlays, &row, &gesture);
                                rev += 1;
                            },
                        }
                    }
                }
            }
        }
    }
}
