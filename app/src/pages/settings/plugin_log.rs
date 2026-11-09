//! Plugin log window: one per plugin.

use crate::services::plugins;
use crate::shell::{WindowShell, WindowSpec, open_with_args, use_window_args};
use crate::state::use_plugin_logs;
use crate::ui::*;
use dioxus::prelude::*;
use std::time::Duration;
use zulip_plugin_host::LogKind;

/// Logs arriving in batches are coalesced before refreshing.
const REFRESH: Duration = Duration::from_millis(100);

const fn kind_class(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Debug => "settings-log-line debug",
        LogKind::Info => "settings-log-line",
        LogKind::Success => "settings-log-line success",
        LogKind::Io => "settings-log-line io",
        LogKind::Warning => "settings-log-line warning",
        LogKind::Error => "settings-log-line error",
    }
}

#[component]
fn PluginLog(id: String) -> Element {
    let overlays = use_overlays();
    let mut rev = use_signal(|| 0u64);
    let mut pending = use_signal(|| false);
    let watched = id.clone();
    use_plugin_logs(move |changed| {
        if changed.is_none_or(|changed| changed == watched) && !*pending.peek() {
            pending.set(true);
            spawn(async move {
                tokio::time::sleep(REFRESH).await;
                pending.set(false);
                rev += 1;
            });
        }
    });
    let _ = rev();
    let Some(plugin) = plugins().plugin(&id) else {
        return rsx! { div { class: "picker-empty", "插件已不存在" } };
    };
    let logs = plugin.logs();
    // Scroll to the bottom after new logs are added
    use_effect(use_reactive!(|rev| {
        let _ = rev;
        document::eval(
            "const el = document.getElementById('settings-log'); if (el) el.scrollTop = el.scrollHeight;",
        );
    }));
    let text = logs.iter().map(|l| l.line()).collect::<Vec<_>>().join("\n");
    rsx! {
        div { class: "settings-log-window",
            div { id: "settings-log", class: "settings-log scroll selectable",
                for (index, entry) in logs.iter().enumerate() {
                    div { key: "{index}", class: kind_class(entry.kind()),
                        "{entry.time_text()} [{entry.level_name()}] "
                        b { "{entry.category}" }
                        ": {entry.text}"
                    }
                }
            }
            div { class: "settings-log-foot",
                Button { onclick: move |_| overlays.copy_text(&text), "复制" }
                Button {
                    onclick: {
                        let plugin = plugin.clone();
                        move |_| plugin.clear_logs()
                    },
                    "清空"
                }
            }
        }
    }
}

#[component]
fn PluginLogWindow() -> Element {
    let id = use_window_args::<String>();
    let id = id();
    let title = plugins()
        .plugin(&id)
        .map_or_else(|| id.clone(), |p| p.name());
    rsx! {
        WindowShell { title: format!("插件日志 - {title}"), PluginLog { id } }
    }
}

/// Opens the plugin's log window; activates it if already open.
pub fn open_plugin_log(id: &str) {
    let title = plugins()
        .plugin(id)
        .map_or_else(|| id.to_owned(), |p| p.name());
    open_with_args(
        WindowSpec::new(
            format!("plugin-log:{id}"),
            format!("插件日志 - {title}"),
            (720.0, 480.0),
        )
        .min_size((420.0, 300.0)),
        PluginLogWindow,
        id.to_owned(),
        None,
    );
}
