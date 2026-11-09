//! The two pages shown before the database opens: data directory migration (normal startup
//! continues after it finishes), and the notice when the data directory has a problem.

use crate::root::RootStage;
use crate::services::{boot, init_default_or_report, init_or_report, load_theme};
use crate::shell::{ThemeHub, WindowShell};
use crate::state::{AppCommand, send_command};
use crate::ui::*;
use dioxus::prelude::*;
use std::path::PathBuf;
use tokio::sync::{oneshot, watch};
use zulip_store::MoveError;

/// Data directory problems to tell the user about at startup.
#[derive(Clone, Debug, PartialEq)]
pub enum DataProblem {
    /// The configured directory is unusable. The config is not written during fallback, and it is
    /// checked again on the next startup.
    Unavailable(PathBuf),
    /// `location.json` is corrupt and has been moved aside: the renamed path, or the reason the
    /// rename failed. Shown only once.
    Corrupted(Result<PathBuf, String>),
}

impl DataProblem {
    fn message(&self) -> String {
        match self {
            Self::Unavailable(path) => {
                format!("数据目录“{}”不可用（移动硬盘可能未连接）。", path.display())
            }
            Self::Corrupted(_) => "数据目录配置已损坏。".to_owned(),
        }
    }

    /// Resolving again may fix the problem; when the config is corrupt there is no directory left
    /// to retry.
    const fn can_retry(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// Notice for a data directory problem. At this point the database is not open yet and the tray has
/// not been created.
#[component]
pub fn DataProblemPage(problem: DataProblem) -> Element {
    rsx! {
        WindowShell { resizable: false, on_close: move |_| send_command(AppCommand::Exit),
            DataProblemDialog { problem }
        }
    }
}

#[component]
fn DataProblemDialog(problem: DataProblem) -> Element {
    let stage = use_context::<RootStage>();
    let theme = use_context::<ThemeHub>();
    let overlays = use_overlays();
    let mut retrying = use_signal(|| false);
    let message = problem.message();
    let can_retry = problem.can_retry();
    let retry_theme = theme.clone();
    rsx! {
        div { class: "data-problem",
            DialogFrame {
                width: DIALOG_WIDTH,
                footer: rsx! {
                    if can_retry {
                        Button {
                            kind: ButtonKind::Primary,
                            disabled: retrying(),
                            onclick: move |_| {
                                let theme = retry_theme.clone();
                                retrying.set(true);
                                spawn(async move {
                                    // A disconnected network path may take a long time and must not
                                    // freeze the UI
                                    let resolved = tokio::task::spawn_blocking(|| boot().location.resolve()).await;
                                    retrying.set(false);
                                    match resolved {
                                        Ok(resolved) if resolved.unavailable.is_none() => {
                                            stage.data_available(&theme);
                                        }
                                        _ => overlays.toast("数据目录仍然不可用"),
                                    }
                                });
                            },
                            "重试"
                        }
                    }
                    Button {
                        kind: if can_retry { ButtonKind::Secondary } else { ButtonKind::Primary },
                        disabled: retrying(),
                        onclick: move |_| {
                            if init_default_or_report(&problem) {
                                load_theme(&theme);
                                stage.finish_moving();
                            } else {
                                std::process::exit(1);
                            }
                        },
                        "使用默认目录"
                    }
                    Button { onclick: move |_| send_command(AppCommand::Exit), "退出" }
                },
                div { class: "confirm-message", "{message}" }
            }
        }
    }
}

const DIALOG_WIDTH: f64 = 420.0;

/// Display text for a byte count.
fn size_text(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MB", bytes as f64 / MB)
}

#[derive(Clone, Copy)]
enum Phase {
    Moving,
    Failed,
}

#[component]
pub fn DataMovePage(target: PathBuf) -> Element {
    let stage = use_context::<RootStage>();
    let theme = use_context::<ThemeHub>();
    let retry_theme = theme.clone();
    let mut phase = use_signal(|| Phase::Moving);
    let mut progress = use_signal(|| (0u64, 0u64));
    let mut failure = use_signal(String::new);
    let source = use_hook(|| boot().location.resolve().root);

    use_future({
        let target = target.clone();
        let source = source.clone();
        move || {
            let target = target.clone();
            let source = source.clone();
            let theme = theme.clone();
            async move {
                let (tx, mut rx) = watch::channel((0u64, 0u64));
                let (done_tx, mut done) = oneshot::channel::<Result<(), MoveError>>();
                tokio::task::spawn_blocking(move || {
                    let result = boot()
                        .location
                        .perform_move(&source, &target, |copied, total| {
                            let _ = tx.send((copied, total));
                        });
                    let _ = done_tx.send(result);
                });
                let result = loop {
                    tokio::select! {
                        changed = rx.changed() => {
                            if changed.is_ok() {
                                progress.set(*rx.borrow_and_update());
                            }
                        }
                        result = &mut done => break result,
                    }
                };
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        log::error!("移动数据目录失败：{error}");
                        failure.set(error.to_string());
                        phase.set(Phase::Failed);
                        return;
                    }
                    Err(_) => {
                        failure.set("移动已中断".to_owned());
                        phase.set(Phase::Failed);
                        return;
                    }
                }
                if init_or_report() {
                    load_theme(&theme);
                    stage.finish_moving();
                } else {
                    std::process::exit(1);
                }
            }
        }
    });

    let (copied, total) = progress();
    let percent = if total > 0 {
        copied as f64 * 100.0 / total as f64
    } else {
        0.0
    };
    rsx! {
        WindowShell { resizable: false, on_close: move |_| {},
            div { class: "data-move",
                match phase() {
                    Phase::Moving => rsx! {
                        div { class: "text-title", "正在移动到“{target.display()}”，完成后 YaZ 将自动启动。" }
                        div { class: "data-move-bar",
                            div { class: "data-move-fill", style: "width:{percent}%" }
                        }
                        div { class: "text-sub", "{size_text(copied)} / {size_text(total)}" }
                    },
                    Phase::Failed => rsx! {
                        div { class: "text-title", "移动失败" }
                        div { class: "text-sub", "{failure}" }
                        div { class: "text-sub", "将继续使用原来的位置：{source.display()}" }
                        div { class: "data-move-actions",
                            Button {
                                kind: ButtonKind::Primary,
                                onclick: move |_| {
                                    if init_or_report() {
                                        load_theme(&retry_theme);
                                        stage.finish_moving();
                                    } else {
                                        std::process::exit(1);
                                    }
                                },
                                "继续"
                            }
                        }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_directory_names_the_path_and_can_retry() {
        let problem = DataProblem::Unavailable(PathBuf::from(r"E:\Data\YaZ"));
        assert_eq!(
            problem.message(),
            r"数据目录“E:\Data\YaZ”不可用（移动硬盘可能未连接）。"
        );
        assert!(problem.can_retry());
    }

    #[test]
    fn corrupted_config_cannot_retry() {
        for problem in [
            DataProblem::Corrupted(Ok(PathBuf::from("location.json.bad"))),
            DataProblem::Corrupted(Err("拒绝访问".to_owned())),
        ] {
            assert_eq!(problem.message(), "数据目录配置已损坏。");
            assert!(!problem.can_retry());
        }
    }
}
