//! Shown before the database opens: checks for a new version first and, when there is one, downloads
//! it and restarts into it, so the user starts straight into the new version. Everything else (no
//! new version, offline, a failed check) continues the normal startup.

use crate::root::RootStage;
use crate::services::boot;
use crate::shell::{ThemeHub, WindowShell, activate};
use crate::state::{AppCommand, send_command};
use crate::ui::*;
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use std::time::{Duration, Instant};
use zulip_update::UpdateCheckStatus;

/// The window stays hidden while the check is quick (no new version: the usual case); it shows
/// once a download starts or the check drags on.
const SHOW_AFTER: Duration = Duration::from_millis(1500);
const POLL: Duration = Duration::from_millis(150);

/// Percentage in a status text such as "正在下载新版本 0.0.6（42%）".
fn percent_of(text: &str) -> Option<f64> {
    let start = text.rfind('（')? + '（'.len_utf8();
    let end = text.rfind('%')?;
    text.get(start..end)?.parse().ok()
}

#[component]
pub fn StartupUpdatePage() -> Element {
    let stage = use_context::<RootStage>();
    let theme = use_context::<ThemeHub>();
    let window = use_window();
    let mut text = use_signal(|| "正在检查更新…".to_owned());
    let mut skipped = use_signal(|| false);

    use_future({
        let theme = theme.clone();
        move || {
            let theme = theme.clone();
            let window = window.clone();
            async move {
                let Some(update) = boot().update.clone() else {
                    stage.data_available(&theme);
                    return;
                };
                let started = Instant::now();
                let mut shown = false;
                let check = update.check(false);
                tokio::pin!(check);
                let mut tick = tokio::time::interval(POLL);
                let result = loop {
                    tokio::select! {
                        result = &mut check => break Some(result),
                        _ = tick.tick() => {
                            if skipped() {
                                break None;
                            }
                            let status = update.status();
                            if status.busy {
                                text.set(status.text.clone());
                            }
                            let downloading = status.busy && status.text.starts_with("正在下载");
                            if !shown && (downloading || started.elapsed() > SHOW_AFTER) {
                                shown = true;
                                activate(&window);
                            }
                        }
                    }
                };
                // A new version is ready: hand over to the updater and leave at once, before any UI
                if result.is_some_and(|r| r.status == UpdateCheckStatus::Ready)
                    && update.try_apply_staged(&boot().args)
                {
                    std::process::exit(0);
                }
                stage.data_available(&theme);
            }
        }
    });

    let status = text();
    let percent = percent_of(&status);
    rsx! {
        WindowShell { resizable: false, on_close: move |_| send_command(AppCommand::Exit),
            div { class: "data-move",
                div { class: "text-title", "{status}" }
                if let Some(percent) = percent {
                    div { class: "data-move-bar",
                        div { class: "data-move-fill", style: "width:{percent}%" }
                    }
                }
                div { class: "data-move-actions",
                    Button { onclick: move |_| skipped.set(true), "跳过" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_read_from_the_download_status() {
        assert_eq!(percent_of("正在下载新版本 0.0.6（42%）"), Some(42.0));
        assert_eq!(percent_of("正在检查更新…"), None);
    }
}
