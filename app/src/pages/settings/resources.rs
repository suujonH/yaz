//! 资源：自己上传到 Zulip 的文件和已使用的存储空间。

use super::Store;
use super::field::*;
use crate::ui::*;
use crate::widgets::{SessionRef, file_icon, format_size};
use crate::windows;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::sync::Arc;
use zulip_core::contains_ignore_case;
use zulip_core::html::upload_link_of;
use zulip_core::model::Attachment;
use zulip_core::text::time;
use zulip_session::{DeleteTarget, MessageReferences, OpenAction, Session};

const MIB: i64 = 1024 * 1024;

fn upload_path(attachment: &Attachment) -> String {
    format!("/user_uploads/{}", attachment.path_id)
}

/// 被多条消息引用时用最早的一条（消息 ID 随发送递增）。
fn first_message(attachment: &Attachment) -> Option<i64> {
    attachment.message_ids.iter().copied().min()
}

fn detail_text(attachment: &Attachment, use_24h: bool) -> String {
    let now = chrono::Local::now().naive_local();
    let uploaded = time::from_unix(attachment.created_at)
        .map(|t| time::list_time(t, now, use_24h))
        .unwrap_or_default();
    let usage = match attachment.message_ids.len() {
        0 => "未被引用".to_owned(),
        n => format!("{n} 条消息中引用"),
    };
    format!(
        "{} · {uploaded} 上传 · {usage}",
        format_size(attachment.size.max(0) as u64)
    )
}

/// 以原文件名保存到“接收或下载的文件保存到”的目录。
async fn save(
    overlays: Overlays,
    session: Arc<Session>,
    attachment: Attachment,
) -> Option<std::path::PathBuf> {
    let link = upload_link_of(&upload_path(&attachment), Some(&attachment.name));
    let message = first_message(&attachment).unwrap_or(0);
    overlays
        .attempt(
            "保存失败",
            session.save_to_default_folder(&link, message, "", |_, _| {}),
        )
        .await
}

async fn open_file(overlays: Overlays, session: Arc<Session>, attachment: Attachment) {
    if save(overlays, session.clone(), attachment.clone())
        .await
        .is_none()
    {
        return;
    }
    match overlays
        .attempt(
            "打开失败",
            session.open_downloaded(&upload_path(&attachment)),
        )
        .await
    {
        Some(OpenAction::Open(path)) => overlays.open_file(&path),
        Some(OpenAction::Reveal(path)) => {
            if let Some(dir) = path.parent() {
                overlays.open_file(dir);
            }
        }
        None => {}
    }
}

/// 在聊天中打开引用这个文件的消息。
async fn jump(overlays: Overlays, session: Arc<Session>, message_id: i64) {
    if let Some(located) = overlays
        .attempt("定位失败", session.locate_message(None, message_id))
        .await
    {
        windows::open_chat(session.account_id(), located.key, Some(located.message_id));
    }
}

/// 引用这个文件的消息：带表头的表格，点击一行跳转到消息。
#[component]
fn ReferenceTable(
    session: SessionRef,
    /// `None` 表示加载中。
    references: Option<MessageReferences>,
    use_24h: bool,
) -> Element {
    let overlays = use_overlays();
    let now = chrono::Local::now().naive_local();
    let failed = references.as_ref().is_some_and(|r| r.failed > 0);
    rsx! {
        div { class: "ref-table",
            if failed {
                div { class: "text-sub ref-table-note", "部分引用加载失败" }
            }
            match references.map(|r| r.references) {
                None => rsx! { div { class: "text-sub ref-table-note", "加载中…" } },
                Some(list) if list.is_empty() && failed => rsx! {},
                Some(list) if list.is_empty() => rsx! {
                    div { class: "text-sub ref-table-note", "引用它的消息已删除或无法查看" }
                },
                Some(list) => rsx! {
                    div { class: "ref-cells ref-table-head",
                        span { "会话" }
                        span { "时间" }
                        span { "内容" }
                    }
                    for reference in list {
                        {
                            let id = reference.message_id;
                            let when = time::from_unix(reference.timestamp)
                                .map(|t| time::list_time(t, now, use_24h))
                                .unwrap_or_default();
                            let session = session.0.clone();
                            rsx! {
                                ListRow {
                                    key: "{id}",
                                    density: RowDensity::Compact,
                                    cells: rsx! {
                                        div { class: "ref-cells",
                                            span { title: "{reference.title}", "{reference.title}" }
                                            span { "{when}" }
                                            span { title: "{reference.preview}", "{reference.preview}" }
                                        }
                                    },
                                    trailing: rsx! { Icon { name: IconName::ArrowRightUp, size: 14.0 } },
                                    onclick: move |_| {
                                        spawn_forever(jump(overlays, session.clone(), id));
                                    },
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

/// 一个文件：点击展开引用它的消息，每次展开时没有结果就（重新）加载。没有被引用的文件不能展开。
#[component]
fn ResourceRow(attachment: Attachment, use_24h: bool) -> Element {
    let store = consume_context::<Store>();
    let rev = store.rev;
    let session = store.session();
    let overlays = use_overlays();
    let mut open = use_signal(|| false);
    let mut references = use_signal(|| None::<MessageReferences>);
    let first = first_message(&attachment);
    let name = attachment.name.clone();
    let (id, for_open, for_save) = (attachment.id, attachment.clone(), attachment.clone());
    let toggle = {
        let session = session.clone();
        let ids = attachment.message_ids.clone();
        move |_: MouseEvent| {
            let opening = !open();
            open.set(opening);
            // 没有结果或上次有失败时重新加载。
            let complete = references
                .peek()
                .as_ref()
                .is_some_and(|r| !r.references.is_empty() && r.failed == 0);
            if opening && !complete {
                references.set(None);
                let (session, ids) = (session.clone(), ids.clone());
                spawn(async move {
                    references.set(Some(session.message_references(&ids).await));
                });
            }
        }
    };
    let chevron = match (first, open()) {
        (None, _) => "settings-chevron unavailable",
        (Some(_), true) => "settings-chevron open",
        (Some(_), false) => "settings-chevron",
    };
    rsx! {
        ListRow {
            density: RowDensity::Normal,
            leading: rsx! {
                Icon { name: IconName::ChevronRight, size: 14.0, class: chevron }
                Icon { name: file_icon(&upload_path(&attachment)), size: 30.0, class: "settings-file-icon" }
            },
            title: name.clone(),
            subtitle: detail_text(&attachment, use_24h),
            trailing: rsx! {
                div { class: "list-row-line", onclick: |e: MouseEvent| e.stop_propagation(),
                    if let Some(message_id) = first {
                        Tooltip { text: "跳转到消息",
                            IconButton {
                                icon: IconName::ArrowRightUp,
                                onclick: {
                                    let session = session.clone();
                                    move |_| {
                                        spawn_forever(jump(overlays, session.clone(), message_id));
                                    }
                                },
                            }
                        }
                        Tooltip { text: "打开",
                            IconButton {
                                icon: IconName::Folder,
                                onclick: {
                                    let session = session.clone();
                                    move |_| {
                                        spawn_forever(open_file(overlays, session.clone(), for_open.clone()));
                                    }
                                },
                            }
                        }
                    }
                    Tooltip { text: "保存",
                        IconButton {
                            icon: IconName::Download,
                            onclick: {
                                let session = session.clone();
                                move |_| {
                                    let (session, attachment) = (session.clone(), for_save.clone());
                                    spawn_forever(async move {
                                        if let Some(path) = save(overlays, session, attachment).await {
                                            overlays.toast(format!("已保存到“{}”", path.display()));
                                        }
                                    });
                                }
                            },
                        }
                    }
                    Tooltip { text: "删除",
                        IconButton {
                            icon: IconName::Delete,
                            onclick: {
                                let session = session.clone();
                                move |_| {
                                    let session = session.clone();
                                    let name = name.clone();
                                    let mut rev = rev;
                                    spawn(async move {
                                        let deleted = overlays
                                            .delete(
                                                &format!("“{name}”"),
                                                Some("引用它的消息中的链接将失效，删除后不能恢复。"),
                                                || session.delete(DeleteTarget::Attachment(id)),
                                            )
                                            .await;
                                        if deleted.is_some() {
                                            rev += 1;
                                        }
                                    });
                                }
                            },
                        }
                    }
                }
            },
            onclick: first.map(|_| EventHandler::new(toggle)),
        }
        if first.is_some() {
            ExpandPanel { open: open(),
                ReferenceTable {
                    session: SessionRef(session.clone()),
                    references: references(),
                    use_24h,
                }
            }
        }
    }
}

#[component]
pub fn Resources() -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let session = store.session();
    let rev = store.rev;
    let mut query = use_signal(String::new);
    let loader = session.clone();
    let mut list = use_resource(move || {
        let _ = rev.read();
        let session = loader.clone();
        async move { session.attachments().await.map_err(|e| e.to_string()) }
    });
    let use_24h = session.use_24h();
    let quota = session.realm().upload_quota_mib.map(|q| q * MIB);
    let result = list.read().clone();
    let (usage, items, status) = match &result {
        None => (None, Vec::new(), Some("加载中…".to_owned())),
        Some(Err(_)) => (None, Vec::new(), Some("加载失败".to_owned())),
        Some(Ok(all)) => {
            let query_text = query.read();
            let q = query_text.trim();
            let mut items: Vec<Attachment> = all
                .attachments
                .iter()
                .filter(|a| contains_ignore_case(&a.name, q))
                .cloned()
                .collect();
            items.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
            let status = if !items.is_empty() {
                None
            } else if all.attachments.is_empty() {
                Some("还没有上传过文件".to_owned())
            } else {
                Some("没有找到相关文件".to_owned())
            };
            (Some(all.upload_space_used), items, status)
        }
    };
    let failed = matches!(result, Some(Err(_)));
    let usage_text = usage.map(|used| {
        let used_text = format_size(used.max(0) as u64);
        match quota {
            Some(quota) => format!("已使用 {used_text} / {}", format_size(quota as u64)),
            None => format!("已使用 {used_text}"),
        }
    });
    let percent = usage.zip(quota).map(|(used, quota)| {
        if quota > 0 {
            (used as f64 * 100.0 / quota as f64).min(100.0)
        } else {
            100.0
        }
    });
    rsx! {
        SettingsPage { title: "资源",
            SettingCard {
                div { class: "settings-usage",
                    if let Some(text) = usage_text {
                        div { "{text}" }
                    }
                    if let Some(percent) = percent {
                        div { class: "progress",
                            div { class: "progress-bar", style: "width:{percent}%" }
                        }
                    }
                    TextInput {
                        value: query(),
                        placeholder: "搜索文件名",
                        clearable: true,
                        oninput: move |v| query.set(v),
                    }
                }
            }
            SettingCard {
                if let Some(status) = status {
                    div { class: "settings-empty",
                        div { class: "text-sub", "{status}" }
                        if failed {
                            Button { onclick: move |_| list.restart(), "重试" }
                        }
                    }
                }
                for attachment in items {
                    ResourceRow {
                        key: "{attachment.id}-{attachment.message_ids:?}",
                        attachment: attachment.clone(),
                        use_24h,
                    }
                }
            }
        }
    }
}
