//! About: version and manual update check.

use super::field::*;
use crate::services::boot;
use crate::state::{AppCommand, use_app_command};
use crate::ui::*;
use dioxus::prelude::*;

#[component]
pub fn About() -> Element {
    let service = boot().update.clone();
    let mut tick = use_signal(|| 0u64);
    use_app_command(move |command| {
        if command == AppCommand::UpdateStatusChanged {
            tick += 1;
        }
    });
    let mut result = use_signal(String::new);
    let _ = tick();
    let (busy, progress, last_error) = service.as_ref().map_or_else(
        || (false, String::new(), None),
        |s| {
            let status = s.status();
            (status.busy, status.text, s.last_error())
        },
    );
    let text = if !progress.is_empty() {
        progress
    } else if !result.read().is_empty() {
        result.read().clone()
    } else if let Some(error) = last_error {
        format!("上次更新失败：{error}")
    } else if service.is_none() {
        "无法检查更新".to_owned()
    } else {
        String::new()
    };
    rsx! {
        SettingsPage { title: "关于",
            SettingCard {
                div { class: "settings-about",
                    Icon { name: IconName::Logo, size: 48.0, class: "settings-about-logo" }
                    div { class: "settings-about-name", "YaZ" }
                    div { class: "text-sub", "版本 {env!(\"CARGO_PKG_VERSION\")}" }
                    Button {
                        disabled: busy || service.is_none(),
                        onclick: move |_| {
                            let Some(service) = service.clone() else { return };
                            spawn(async move {
                                result.set(service.check(true).await.message);
                            });
                        },
                        "确认版本"
                    }
                    if !text.is_empty() {
                        div { class: "text-sub settings-about-status", "{text}" }
                    }
                }
            }
        }
    }
}
