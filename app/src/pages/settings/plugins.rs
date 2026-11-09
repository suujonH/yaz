//! Plugins: list of installed plugins (toggle, status, enable confirmation), menu, settings, logs,
//! and quick actions.

use super::field::*;
use super::plugin_form::open_plugin_settings;
use super::plugin_log::open_plugin_log;
use crate::services::plugins;
use crate::state::use_plugin_events;
use crate::ui::*;
use crate::widgets::PluginRef;
use dioxus::dioxus_core::{Task, spawn_forever};
use dioxus::html::input_data::MouseButton as DomButton;
use dioxus::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zulip_plugin_host::shortcuts::{normalize_key, normalize_mouse};
use zulip_plugin_host::{
    ApprovalInfo, Modifiers, MouseButton, NativePlugin, PluginEvent, PluginMenu, ShortcutRow,
    ToggleOutcome, mouse_gesture,
};

/// Double-click interval: pressing again within it counts as a double-click, otherwise it is
/// treated as a single click.
const DOUBLE_CLICK: Duration = Duration::from_millis(350);

fn section(title: &str, items: &[String]) -> Element {
    if items.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "settings-approval-section", "{title}" }
        for item in items {
            div { key: "{item}", class: "settings-approval-item", "• {item}" }
        }
    }
}

/// Confirmation before enabling a plugin: what it can read, what it can do, and which links /
/// programs it opens automatically.
#[component]
fn ApprovalDialog(info: ApprovalInfo, answer: Answer<bool>) -> Element {
    let (ok, cancel, close) = (answer.clone(), answer.clone(), answer);
    let no_permissions = info.reads.is_empty() && info.acts.is_empty() && info.declared.is_empty();
    rsx! {
        DialogFrame {
            title: info.title.clone(),
            width: 440.0,
            on_close: move |_| close.cancel(),
            footer: rsx! {
                DialogButtons {
                    confirm_text: "开启",
                    on_confirm: move |_| ok.send(true),
                    on_cancel: move |_| cancel.cancel(),
                }
            },
            div { class: "text-sub selectable", "{info.meta}" }
            if let Some(notice) = &info.notice {
                div { class: "settings-approval-notice", "{notice}" }
            }
            {section("可以读取", &info.reads)}
            {section("可以做", &info.acts)}
            {section("依赖的插件", &info.dependencies)}
            {section("会自动打开（白名单）", &info.launch_rules)}
            {section("接收的事件", &info.events)}
            {section("插件声明的其它行为", &info.declared)}
            if no_permissions {
                div { class: "settings-approval-section", "不需要任何权限" }
            }
            if !info.fingerprint.is_empty() {
                div { class: "text-caption", "文件指纹：{info.fingerprint}" }
            }
        }
    }
}

/// Toggle: prompts when a dependency is not enabled; a plugin that has not been confirmed first
/// shows the enable confirmation.
async fn set_enabled(overlays: Overlays, plugin: Arc<NativePlugin>, enabled: bool) {
    match plugins().toggle(&plugin, enabled).await {
        ToggleOutcome::NeedsApproval(info) => {
            let confirmed = overlays
                .ask_dialog(move |answer| rsx! { ApprovalDialog { info: (*info).clone(), answer } })
                .await;
            if confirmed.is_some() && !plugins().approve(&plugin).await {
                overlays.toast("插件文件已变化，请重启程序后再开启");
            }
        }
        outcome => {
            if let Some(text) = outcome.message() {
                overlays.toast(text);
            }
        }
    }
}

/// Items in the menu that can be clicked directly (submenus expanded).
fn leaf_menus(menus: &[PluginMenu]) -> Vec<&PluginMenu> {
    menus
        .iter()
        .flat_map(|m| {
            if m.is_group() {
                leaf_menus(&m.children)
            } else {
                vec![m]
            }
        })
        .collect()
}

/// Re-reads the plugin state when `rev` changes (plugins are compared by pointer, so a state change
/// does not change the props).
#[component]
fn PluginCard(plugin: PluginRef, rev: u64) -> Element {
    let _ = rev;
    let overlays = use_overlays();
    let plugin = plugin.0;
    let status = plugin.status_text();
    let has_settings = plugin
        .manifest()
        .is_some_and(|m| !m.settings.is_empty() || !m.panels.is_empty());
    let menus: Vec<(i32, String)> = leaf_menus(plugin.active_menus())
        .iter()
        .map(|m| (m.id, m.name.clone()))
        .collect();
    let id = plugin.id();
    rsx! {
        SettingCard {
            SettingRow { label: plugin.name(), description: plugin.meta(),
                div { class: "settings-actions",
                    for (menu_id, name) in menus {
                        Button {
                            key: "{menu_id}",
                            onclick: {
                                let plugin = plugin.clone();
                                move |_| plugins().invoke_menu(&plugin, menu_id)
                            },
                            "{name}"
                        }
                    }
                    if has_settings {
                        Button {
                            onclick: {
                                let plugin = plugin.clone();
                                move |_| {
                                    let plugin = plugin.clone();
                                    spawn_forever(async move {
                                        open_plugin_settings(overlays, plugin).await;
                                    });
                                }
                            },
                            "设置"
                        }
                    }
                    Button { onclick: move |_| open_plugin_log(&id), "日志" }
                    Toggle {
                        checked: plugin.is_enabled(),
                        disabled: plugin.manifest().is_none(),
                        onchange: {
                            let plugin = plugin.clone();
                            move |enabled| {
                                spawn_forever(set_enabled(overlays, plugin.clone(), enabled));
                            }
                        },
                    }
                }
            }
            if !status.is_empty() {
                div { class: if plugin.is_error() { "settings-error" } else { "" },
                    SettingRow { label: status }
                }
            }
        }
    }
}

/// Mouse gesture recording box: click or double-click while holding modifier keys; Backspace /
/// Delete clears.
#[component]
fn MouseRecorder(#[props(into)] label: String, onchange: EventHandler<String>) -> Element {
    let mut pending = use_signal(|| None::<Task>);
    let mut last = use_signal(|| None::<(MouseButton, Instant)>);
    rsx! {
        div { class: "input shortcut-recorder", tabindex: "0",
            onmousedown: move |e| {
                let button = match e.trigger_button() {
                    Some(DomButton::Primary) => MouseButton::Left,
                    Some(DomButton::Secondary) => MouseButton::Right,
                    Some(DomButton::Auxiliary) => MouseButton::Middle,
                    _ => return,
                };
                let mods = e.modifiers();
                let modifiers = Modifiers {
                    ctrl: mods.ctrl(),
                    alt: mods.alt(),
                    shift: mods.shift(),
                    meta: mods.meta(),
                };
                let is_double = last
                    .peek()
                    .is_some_and(|(b, at)| b == button && at.elapsed() < DOUBLE_CLICK);
                if let Some(task) = pending.take() {
                    task.cancel();
                }
                if is_double {
                    last.set(None);
                    if let Some(gesture) = mouse_gesture(button, 2, modifiers) {
                        onchange.call(gesture);
                    }
                    return;
                }
                last.set(Some((button, Instant::now())));
                pending.set(Some(spawn(async move {
                    tokio::time::sleep(DOUBLE_CLICK).await;
                    last.set(None);
                    if let Some(gesture) = mouse_gesture(button, 1, modifiers) {
                        onchange.call(gesture);
                    }
                })));
            },
            onkeydown: move |e| {
                if matches!(e.key(), Key::Backspace | Key::Delete) {
                    onchange.call(String::new());
                }
            },
            input { r#type: "text", readonly: true, value: "{label}", placeholder: "无" }
        }
    }
}

/// A new gesture / shortcut was recorded. Gestures already used by the host and shortcuts without
/// Ctrl / Alt cannot be bound.
fn record(overlays: Overlays, row: &ShortcutRow, gesture: &str) {
    let gesture = if gesture.is_empty() {
        Some(String::new())
    } else if row.is_mouse {
        normalize_mouse(gesture)
    } else {
        normalize_key(gesture)
    };
    let Some(gesture) = gesture else {
        overlays.toast("快捷键需要 Ctrl 或 Alt 加字母、数字（F1～F24 除外）");
        return;
    };
    for text in plugins().set_shortcut(row, &gesture) {
        overlays.toast(text);
    }
}

/// Re-reads the quick actions when `rev` changes.
#[component]
fn Shortcuts(rev: u64) -> Element {
    let _ = rev;
    let overlays = use_overlays();
    let messages = plugins().message_shortcuts();
    let keys = plugins().key_shortcuts();
    rsx! {
        if !messages.is_empty() {
            Group { title: "消息快捷操作",
                for row in messages {
                    SettingRow { key: "{row.key}", label: row.title.clone(),
                        MouseRecorder {
                            label: row.label(),
                            onchange: move |gesture: String| record(overlays, &row, &gesture),
                        }
                    }
                }
            }
        }
        if !keys.is_empty() {
            Group { title: "快捷键",
                for row in keys {
                    SettingRow { key: "{row.key}", label: row.title.clone(),
                        div { class: "settings-recorder",
                            ShortcutRecorder {
                                value: row.gesture.clone(),
                                onchange: move |gesture: String| record(overlays, &row, &gesture),
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn Plugins() -> Element {
    let overlays = use_overlays();
    let mut rev = use_signal(|| 0u64);
    use_plugin_events(move |event| {
        if matches!(
            event,
            PluginEvent::PluginsChanged | PluginEvent::ResourcesChanged
        ) {
            rev += 1;
        }
    });
    let version = rev();
    let list = plugins().plugins();
    let dir = plugins().plugins_dir().to_path_buf();
    rsx! {
        SettingsPage {
            title: "插件",
            actions: rsx! {
                Button {
                    onclick: move |_| overlays.open_dir(&dir),
                    "打开插件目录"
                }
            },
            if list.is_empty() {
                SettingCard { SettingRow { label: "还没有安装插件" } }
            }
            for plugin in list {
                PluginCard { key: "{plugin.id()}", plugin: PluginRef(plugin), rev: version }
            }
            Shortcuts { rev: version }
        }
    }
}
