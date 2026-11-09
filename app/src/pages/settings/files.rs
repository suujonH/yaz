//! 数据目录：接收或下载的文件保存到哪里，聊天信息（数据目录）保存到哪里。

use super::field::*;
use crate::platform;
use crate::services::{boot, services, unavailable_data_root};
use crate::state::{AppCommand, send_command};
use crate::ui::*;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::path::{Path, PathBuf};
use zulip_store::DataLocation;

/// 一个位置：路径、更改、打开。
#[component]
fn PathRow(
    #[props(into)] path: String,
    title: &'static str,
    /// 说明文字。
    #[props(into)]
    description: Option<String>,
    /// 不能更改。
    #[props(default)]
    locked: bool,
    on_change: EventHandler<PathBuf>,
) -> Element {
    let overlays = use_overlays();
    let open = path.clone();
    rsx! {
        SettingRow { label: path, description,
            div { class: "settings-actions",
                Button {
                    disabled: locked,
                    onclick: move |_| {
                        spawn(async move {
                            if let Some(chosen) = platform::pick_folder(title).await {
                                on_change.call(chosen);
                            }
                        });
                    },
                    "更改"
                }
                Button {
                    onclick: move |_| overlays.open_dir(Path::new(&open)),
                    "打开"
                }
            }
        }
    }
}

/// 确认 → 检查 → 记下搬迁目标 → 重启，新进程启动时搬迁。
async fn move_data(overlays: Overlays, parent: PathBuf) {
    let target = parent.join(DataLocation::FOLDER_NAME);
    let confirm = Confirm::new(format!(
        "将把聊天信息移动到“{}”。YaZ 会先退出，移动完成后自动启动并重新登录。",
        target.display()
    ))
    .confirm_text("移动并重启");
    let current = services().paths.root().to_path_buf();
    let moved = overlays
        .confirm_then(confirm, "移动失败", || async move {
            tokio::task::spawn_blocking(move || {
                let location = &boot().location;
                location
                    .validate(&current, &target)
                    .map_err(|e| e.to_string())?;
                location.request_move(&target).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|_| Err("已中断".to_owned()))
        })
        .await;
    if moved.is_some() {
        send_command(AppCommand::Restart);
    }
}

#[component]
pub fn Files() -> Element {
    let overlays = use_overlays();
    let save_root = field(Source::App(
        |s| s.save_root().display().to_string(),
        |s, v| s.set_save_root(&v),
    ));
    let data_root = services().paths.root().display().to_string();
    let unavailable = unavailable_data_root();
    rsx! {
        SettingsPage { title: "数据目录",
            Group { title: "将接收或下载的文件保存到",
                PathRow {
                    title: "选择文件保存位置",
                    path: save_root.value.clone(),
                    on_change: move |path: PathBuf| save_root.set(path.display().to_string()),
                }
            }
            Group { title: "聊天信息默认保存到",
                PathRow {
                    title: "选择聊天信息保存位置",
                    path: data_root,
                    description: unavailable.map(|path| {
                        format!("数据目录“{}”不可用，本次使用默认目录，暂时不能更改。", path.display())
                    }),
                    locked: unavailable.is_some(),
                    on_change: move |parent| {
                        spawn_forever(move_data(overlays, parent));
                    },
                }
            }
        }
    }
}
