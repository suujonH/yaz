//! Forward, edit history, read-by members.

use super::content::{RenderOptions, Rendered, Segment, render};
use super::store::Chat;
use crate::ui::{DialogFrame, ListRow, RowDensity};
use crate::widgets::{
    self, RemoteAvatar, SessionRef, downloaded_image, local_image, open_user_card,
};
use crate::windows;
use dioxus::prelude::*;
use std::sync::Arc;
use zulip_core::html::extract_inline_images;
use zulip_core::model::MessageRevision;
use zulip_core::text::time;
use zulip_session::Session;

/// Forward after choosing the target. When `merged` is true, merge into one message. After a
/// successful forward, leave multi-select; on cancel or failure keep the selected messages.
pub fn forward(chat: Chat, ids: Vec<i64>, merged: bool) {
    chat.spawn(async move {
        let session = chat.session();
        let forwarded = if merged {
            let sender = session.clone();
            widgets::forward(chat.overlays, &session, move |keys| async move {
                sender.forward_merged(&ids, &keys).await
            })
            .await
        } else {
            widgets::forward_messages(chat.overlays, &session, ids).await
        };
        if forwarded {
            chat.exit_multi();
        }
    });
}

fn revision_rendered(session: &Session, html: &str) -> Rendered {
    render(
        html,
        &RenderOptions {
            me: None,
            local_image: &|url| downloaded_image(session, url),
        },
    )
}

fn static_html(rendered: Rendered) -> String {
    rendered
        .segments
        .into_iter()
        .map(|s| match s {
            Segment::Html(h) => h,
            Segment::File(link) => format!("<p>[文件] {}</p>", link.file_name),
        })
        .collect()
}

fn revision_html(revision: &MessageRevision) -> &str {
    revision
        .content_html_diff
        .as_deref()
        .unwrap_or(&revision.rendered_content)
}

/// Replace the images already shown in the dialog with the downloaded files; the content is not
/// reset, so the text selection is not lost.
fn set_history_image(url: &str, local: Option<&str>) {
    let url = serde_json::to_string(url).unwrap_or_default();
    let local = serde_json::to_string(&local).unwrap_or_default();
    document::eval(&format!(
        "for (const img of document.querySelectorAll('.history-body img[data-remote]')) {{
            if (img.dataset.remote !== {url}) continue;
            const local = {local};
            if (local) img.src = local;
            else if (img.parentElement) img.parentElement.classList.add('failed');
        }}"
    ));
}

/// Index of the image at the click position within this version of the content.
async fn clicked_image(at: dioxus::html::geometry::ClientPoint) -> Option<usize> {
    document::eval(&format!(
        "const target = document.elementFromPoint({}, {});
        const image = target && target.closest('.img-preview');
        return image ? Number(image.dataset.index) : null;",
        at.x, at.y
    ))
    .join::<Option<usize>>()
    .await
    .ok()
    .flatten()
}

/// Description of one edit: who, and what they did.
fn revision_title(session: &Session, revision: &MessageRevision, first: bool) -> String {
    let editor = revision
        .user_id
        .map_or_else(|| "系统".to_owned(), |id| session.display_name(id));
    let moved = revision.prev_topic.is_some() || revision.prev_stream.is_some();
    let action = if first {
        "发送"
    } else if moved && revision.prev_content.is_none() {
        "移动"
    } else {
        "编辑"
    };
    format!("{editor} {action}")
}

fn revision_moves(session: &Session, revision: &MessageRevision) -> Vec<String> {
    let mut lines = Vec::new();
    if let (Some(from), Some(to)) = (revision.prev_stream, revision.stream) {
        lines.push(format!(
            "频道：#{} → #{}",
            session.channel_display_name(from),
            session.channel_display_name(to)
        ));
    }
    if let Some(from) = &revision.prev_topic {
        lines.push(format!("话题：{from} → {}", revision.topic));
    }
    lines
}

pub fn show_history(chat: Chat, id: i64) {
    let session = chat.session();
    let overlays = chat.overlays;
    overlays.open_dialog(true, move |handle| {
        rsx! {
            HistoryDialog { session: SessionRef(session.clone()), id, on_close: move |()| handle.close() }
        }
    });
}

#[component]
fn HistoryDialog(session: SessionRef, id: i64, on_close: EventHandler<()>) -> Element {
    let session = session.0;
    let loader = session.clone();
    let revisions = use_resource(move || {
        let session = loader.clone();
        async move { session.message_history(id).await }
    });
    let downloader = session.clone();
    use_effect(move || {
        let current = revisions.read();
        let Some(Ok(list)) = &*current else {
            return;
        };
        let mut urls: Vec<String> = Vec::new();
        for revision in list {
            for url in revision_rendered(&downloader, revision_html(revision)).pending {
                if !urls.contains(&url) {
                    urls.push(url);
                }
            }
        }
        for url in urls {
            let session = downloader.clone();
            spawn(async move {
                let local = local_image(session, url.clone()).await;
                set_history_image(&url, local.as_deref());
            });
        }
    });
    let account = session.account_id();
    let use_24h = session.use_24h();
    let now = chrono::Local::now().naive_local();
    rsx! {
        DialogFrame { title: "编辑历史", width: 480.0, on_close,
            div { class: "history-list scroll",
                match &*revisions.read() {
                    None => rsx! { div { class: "dialog-empty", "加载中…" } },
                    Some(Err(error)) => rsx! { div { class: "dialog-empty", "加载失败：{error}" } },
                    Some(Ok(list)) if list.is_empty() => rsx! { div { class: "dialog-empty", "没有可以查看的编辑历史" } },
                    Some(Ok(list)) => rsx! {
                        for (i, revision) in list.iter().enumerate().rev() {
                            {
                                let title = revision_title(&session, revision, i == 0);
                                let when = time::from_unix(revision.timestamp)
                                    .map_or_else(String::new, |t| time::precise_time(t, now, use_24h));
                                let moves = revision_moves(&session, revision);
                                let raw = revision_html(revision).to_owned();
                                let html = static_html(revision_rendered(&session, &raw));
                                rsx! {
                                    div { key: "{i}", class: "history-entry",
                                        div { class: "history-head",
                                            span { "{title}" }
                                            span { class: "text-caption", "{when}" }
                                        }
                                        for line in moves {
                                            div { class: "text-sub", "{line}" }
                                        }
                                        div {
                                            class: "msg-html selectable history-body",
                                            dangerous_inner_html: "{html}",
                                            ondoubleclick: move |e: MouseEvent| {
                                                let raw = raw.clone();
                                                let at = e.client_coordinates();
                                                async move {
                                                    if let Some(index) = clicked_image(at).await {
                                                        let album: Vec<String> = extract_inline_images(&raw)
                                                            .iter()
                                                            .map(|image| image.original())
                                                            .collect();
                                                        if index < album.len() {
                                                            windows::open_image_viewer(account, album, index);
                                                        }
                                                    }
                                                }
                                            },
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

pub fn show_receipts(chat: Chat, id: i64) {
    let session = chat.session();
    let overlays = chat.overlays;
    let sender = chat.message(id).map_or(0, |m| m.sender_id);
    overlays.open_dialog(true, move |handle| {
        rsx! {
            ReceiptsDialog { session: SessionRef(session.clone()), id, sender, on_close: move |()| handle.close() }
        }
    });
}

#[component]
fn ReceiptsDialog(
    session: SessionRef,
    id: i64,
    sender: i64,
    on_close: EventHandler<()>,
) -> Element {
    let session: Arc<Session> = session.0;
    let loader = session.clone();
    let overlays = crate::ui::use_overlays();
    let readers = use_resource(move || {
        let session = loader.clone();
        async move {
            let self_id = session.self_id();
            session.read_receipts(id).await.map(|ids| {
                ids.into_iter()
                    .filter(|u| *u != self_id && *u != sender)
                    .collect::<Vec<i64>>()
            })
        }
    });
    let current = readers.read();
    let title = match &*current {
        Some(Ok(list)) => format!("已读成员 ({})", list.len()),
        _ => "已读成员".to_owned(),
    };
    rsx! {
        DialogFrame { title, width: 360.0, on_close,
            div { class: "history-list scroll",
                match &*current {
                    None => rsx! { div { class: "dialog-empty", "加载中…" } },
                    Some(Err(error)) => rsx! { div { class: "dialog-empty", "加载失败：{error}" } },
                    Some(Ok(list)) if list.is_empty() => rsx! { div { class: "dialog-empty", "暂时没有成员已读" } },
                    Some(Ok(list)) => rsx! {
                        for user in list.iter().copied() {
                            {
                                let name = session.display_name(user);
                                let avatar = session.user(user).and_then(|u| u.avatar_url);
                                rsx! {
                                    ListRow {
                                        key: "{user}",
                                        density: RowDensity::Compact,
                                        leading: rsx! { RemoteAvatar { text: name.clone(), url: avatar, size: 28.0 } },
                                        title: name.clone(),
                                        onclick: move |e: MouseEvent| {
                                            let at = e.client_coordinates();
                                            open_user_card(
                                                overlays,
                                                crate::ui::Rect { x: at.x, y: at.y, w: 4.0, h: 4.0 },
                                                user,
                                            );
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
}
