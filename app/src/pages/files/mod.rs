//! 文件窗口：全部文件、最近使用、按发送人、按会话、频道文件；筛选、排序、下载、打开、另存为、定位、删除、上传。

use crate::shell::{WindowShell, use_window_args};
use crate::state::{AppState, use_app_state, use_session_event};
use crate::ui::*;
use crate::widgets::*;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::file::FileKind;
use zulip_core::html::upload_link_of;
use zulip_core::markdown::link_markdown;
use zulip_core::text::time::{from_unix, month_group};
use zulip_core::{ConversationKey, contains_ignore_case};
use zulip_session::queries::FILES_PAGE_SIZE;
use zulip_session::{
    ContentFilter, DeleteTarget, FileEntry, FileSort, MessageSearch, QueryScope, Session,
    SessionEvent, file_count_text, sort_files,
};
use zulip_store::DownloadRecord;

const RECENT_LIMIT: usize = 200;
/// 每取到这么多文件大小就刷新一次列表。
const SIZE_BATCH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    All,
    Recent,
    Sender,
    Conversation,
    Channel,
}

impl View {
    const fn label(self) -> &'static str {
        match self {
            Self::All => "全部文件",
            Self::Recent => "最近使用",
            Self::Sender => "发送人",
            Self::Conversation => "会话",
            Self::Channel => "频道文件",
        }
    }

    const fn icon(self) -> IconName {
        match self {
            Self::All => IconName::App,
            Self::Recent => IconName::Time,
            Self::Sender => IconName::User,
            Self::Conversation => IconName::Chat,
            Self::Channel => IconName::Folder,
        }
    }

    const fn needs_source(self) -> bool {
        matches!(self, Self::Sender | Self::Conversation | Self::Channel)
    }

    const fn pick_text(self) -> &'static str {
        match self {
            Self::Sender => "请选择发送人",
            Self::Channel => "请选择频道",
            _ => "请选择会话",
        }
    }
}

/// 中间栏选中的来源。
#[derive(Clone, Debug, PartialEq)]
enum Source {
    Sender(i64),
    Conversation(ConversationKey),
}

const KINDS: [(Option<FileKind>, &str); 6] = [
    (None, "全部类型"),
    (Some(FileKind::Image), "图片"),
    (Some(FileKind::Video), "视频"),
    (Some(FileKind::Document), "文档"),
    (Some(FileKind::Archive), "压缩包"),
    (Some(FileKind::Other), "其他"),
];

const SORTS: [(FileSort, &str); 3] = [
    (FileSort::Time, "按最新时间"),
    (FileSort::Name, "按文件名"),
    (FileSort::Size, "按大小"),
];

fn kind_value(kind: Option<FileKind>) -> String {
    KINDS
        .iter()
        .position(|(k, _)| *k == kind)
        .unwrap_or_default()
        .to_string()
}

fn sort_value(sort: FileSort) -> String {
    SORTS
        .iter()
        .position(|(s, _)| *s == sort)
        .unwrap_or_default()
        .to_string()
}

#[component]
pub fn FilesWindow() -> Element {
    rsx! {
        WindowShell { title: "文件", FilesFrame {} }
    }
}

/// 窗口参数是账号 ID，窗口内可以切换。
#[component]
fn FilesFrame() -> Element {
    let account = use_window_args::<i64>();
    let epoch = use_window_account(account);
    use_window_keys(|_| {});
    rsx! {
        FilesBody { key: "{account}-{epoch}", account }
    }
}

fn recent_entries(session: &Session, records: Vec<DownloadRecord>) -> Vec<FileEntry> {
    records
        .into_iter()
        .filter_map(|record| {
            let key: ConversationKey = record.conv_key.parse().ok()?;
            Some(FileEntry {
                kind: FileKind::of(&record.upload_path),
                upload: upload_link_of(&record.upload_path, Some(&record.file_name)),
                message_id: record.message_id,
                source: session.display_title(&key),
                key,
                timestamp: record.last_opened_at.unwrap_or(record.downloaded_at),
                size: u64::try_from(record.size).ok(),
            })
        })
        .collect()
}

fn source_key(source: &Source) -> String {
    match source {
        Source::Sender(id) => format!("u{id}"),
        Source::Conversation(key) => format!("c{key}"),
    }
}

/// 增加计数，通知依赖它的资源重读。窗口已关闭时什么也不做。
fn bump(mut signal: Signal<u32>) {
    if let Ok(mut value) = signal.try_write() {
        *value += 1;
    }
}

#[component]
fn FilesBody(account: Signal<i64>) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let mut view = use_signal(|| View::All);
    let mut source = use_signal(|| None::<Source>);
    let mut query = use_signal(String::new);
    let applied = use_debounced(query.into(), SEARCH_DEBOUNCE);
    let mut kind = use_signal(|| None::<FileKind>);
    let mut sort = use_signal(FileSort::default);
    let refresh = use_signal(|| 0u32);
    let sizes = use_signal(HashMap::<String, Option<u64>>::new);
    let uploading = use_signal(|| None::<f64>);

    let search = use_memo(move || {
        let scope = match (view(), source()) {
            (View::All, _) => QueryScope::Everywhere,
            (View::Sender, Some(Source::Sender(id))) => QueryScope::Sender(id),
            (View::Conversation | View::Channel, Some(Source::Conversation(key))) => {
                QueryScope::Conversation(key)
            }
            _ => return None,
        };
        Some(MessageSearch {
            scope,
            filter: ContentFilter::Attachments,
            text: applied().trim().to_owned(),
            ..MessageSearch::default()
        })
    });
    let results = use_message_results(search.into(), FILES_PAGE_SIZE);
    use_session_event(move |event| {
        if event == SessionEvent::AttachmentsChanged {
            results.reload();
        }
    });

    let recent = use_resource(move || async move {
        let _ = (downloads_epoch(), refresh());
        if view() != View::Recent {
            return None;
        }
        let session = state.session.read().clone()?;
        let records = overlays
            .attempt("加载失败", session.recent_downloads(RECENT_LIMIT))
            .await?;
        Some(recent_entries(&session, records))
    });

    // 文件列表：筛选、排序，大小未知的排在最后
    let entries = use_memo(move || {
        let Some(session) = state.session.read().clone() else {
            return Rc::new(Vec::new());
        };
        let mut entries = if view() == View::Recent {
            let applied = applied();
            let needle = applied.trim();
            recent
                .read()
                .clone()
                .flatten()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| contains_ignore_case(&e.upload.file_name, needle))
                .collect()
        } else {
            session.file_entries(&results.items())
        };
        let known = sizes.read();
        for entry in &mut entries {
            if entry.size.is_none() {
                entry.size = known.get(&entry.upload.path).copied().flatten();
            }
        }
        if let Some(kind) = kind() {
            entries.retain(|e| e.kind == kind);
        }
        sort_files(&mut entries, sort());
        Rc::new(entries)
    });
    // 文件大小逐个请求，攒几个再刷新一次列表
    use_resource(move || async move {
        let Some(session) = state.session.read().clone() else {
            return;
        };
        let missing: Vec<String> = entries
            .read()
            .iter()
            .filter(|e| e.size.is_none())
            .map(|e| e.upload.path.clone())
            .filter(|path| !sizes.peek().contains_key(path))
            .collect();
        let mut fetched = HashMap::new();
        for (index, path) in missing.iter().enumerate() {
            let size = session.file_size(path).await.ok().flatten();
            fetched.insert(path.clone(), size);
            if (index + 1) % SIZE_BATCH == 0 || index + 1 == missing.len() {
                let mut all = sizes;
                all.write().extend(std::mem::take(&mut fetched));
            }
        }
    });
    // 筛选后没有可见的文件但还有更早的消息：继续加载
    use_effect(move || {
        if view() != View::Recent
            && entries.read().is_empty()
            && !results.items().is_empty()
            && !results.loading_more()
        {
            results.load_more();
        }
    });

    // 发送人按最近发过文件的先后排序
    let ranked = use_resource(move || async move {
        if view() != View::Sender {
            return None;
        }
        state.session.read().clone()?.rank_file_senders().await.ok()
    });

    let session = state.session.read().clone();
    let own_messages: HashSet<i64> = session.as_ref().map_or_else(HashSet::new, |s| {
        results
            .items()
            .iter()
            .filter(|item| item.message.sender_id == s.self_id())
            .map(|item| item.message.id)
            .collect()
    });

    let current = view();
    let selected = source();
    let sources: Vec<(Source, String, Element)> = match (&session, current) {
        (Some(s), View::Sender) => ranked
            .read()
            .clone()
            .flatten()
            .unwrap_or_else(|| s.file_senders())
            .into_iter()
            .map(|id| {
                let name = if id == s.self_id() {
                    "我".to_owned()
                } else {
                    s.display_name(id)
                };
                let avatar = rsx! {
                    RemoteAvatar { text: name.clone(), url: s.user(id).and_then(|u| u.avatar_url), size: 28.0 }
                };
                (Source::Sender(id), name, avatar)
            })
            .collect(),
        (Some(s), View::Conversation | View::Channel) => {
            let rows = conversation_rows(s);
            s.file_conversations(current == View::Channel)
                .into_iter()
                .map(|key| {
                    let title = rows
                        .get(&key)
                        .map_or_else(|| s.display_title(&key), |row| row.title.clone());
                    let avatar = rows.get(&key).map_or_else(
                        || rsx! { Avatar { text: title.clone(), size: 28.0 } },
                        |row| rsx! {
                            ConversationAvatarView { avatar: row.avatar.clone(), title: title.clone(), size: 28.0 }
                        },
                    );
                    (Source::Conversation(key), title, avatar)
                })
                .collect()
        }
        _ => Vec::new(),
    };
    let source_title = sources
        .iter()
        .find(|(candidate, _, _)| Some(candidate) == selected.as_ref())
        .map(|(_, title, _)| title.clone());
    let title = match &source_title {
        Some(name) => format!("{} - {name}", current.label()),
        None => current.label().to_owned(),
    };

    let empty = if applied().trim().is_empty() && kind().is_none() {
        "暂无文件"
    } else {
        "未找到相关文件"
    };
    let status = if current.needs_source() && selected.is_none() {
        Some(current.pick_text().to_owned())
    } else if !entries.read().is_empty() {
        None
    } else if current == View::Recent {
        Some(
            if recent.read().is_none() {
                "加载中…"
            } else {
                empty
            }
            .to_owned(),
        )
    } else if results.items().is_empty() {
        results.status(empty)
    } else {
        Some(empty.to_owned())
    };

    // 按时间排序时按月分组，其它排序不分组
    let list = entries();
    let now = chrono::Local::now().naive_local();
    let rows: Rc<Vec<DateRow<FileEntry>>> = Rc::new(if sort() == FileSort::Time {
        let mut groups: Vec<(String, Vec<FileEntry>)> = Vec::new();
        for entry in list.iter() {
            let header =
                from_unix(entry.timestamp).map_or_else(String::new, |t| month_group(t, now));
            match groups.last_mut() {
                Some((title, items)) if *title == header => items.push(entry.clone()),
                _ => groups.push((header, vec![entry.clone()])),
            }
        }
        with_headers(groups)
    } else {
        list.iter().cloned().map(DateRow::Item).collect()
    });
    let keys = row_keys(&rows, |entry| text_id(&entry.upload.path));
    let use_24h = session.as_ref().is_some_and(|s| s.use_24h());
    let recent_view = current == View::Recent;
    let can_upload = current == View::Channel
        && uploading().is_none()
        && match (&session, &selected) {
            (Some(s), Some(Source::Conversation(key))) => can_send_to(s, key),
            _ => false,
        };
    let upload_key = match &selected {
        Some(Source::Conversation(key)) => Some(key.clone()),
        _ => None,
    };

    rsx! {
        SplitView {
            nav: rsx! {
                AccountSwitcher { value: account(), onchange: move |id| account.set(id) }
                TextInput {
                    value: query(),
                    placeholder: "搜索文件名",
                    style_kind: InputStyle::Search,
                    icon: IconName::Search,
                    clearable: true,
                    oninput: move |v| query.set(v),
                }
                for item in [View::All, View::Recent, View::Sender, View::Conversation, View::Channel] {
                    NavRow {
                        key: "{item.label()}",
                        icon: item.icon(),
                        label: item.label(),
                        selected: item == current,
                        onclick: move |_| {
                            if view() != item {
                                view.set(item);
                                source.set(None);
                            }
                        },
                    }
                }
            },
            div { class: "files-body",
            if current.needs_source() {
                div { class: "files-sources scroll",
                    for (candidate, name, avatar) in sources {
                        ListRow {
                            key: "{source_key(&candidate)}",
                            density: RowDensity::Compact,
                            leading: avatar,
                            title: name,
                            selected: Some(&candidate) == selected.as_ref(),
                            onclick: move |_| source.set(Some(candidate.clone())),
                        }
                    }
                }
            }
            div { class: "files-main",
                PaneHeader { title,
                    div { class: "pane-select",
                        Select {
                            options: KINDS.iter().enumerate().map(|(i, (_, name))| SelectOption::new(i.to_string(), *name)).collect::<Vec<_>>(),
                            value: kind_value(kind()),
                            onchange: move |value: String| {
                                let index: usize = value.parse().unwrap_or_default();
                                kind.set(KINDS.get(index).and_then(|(k, _)| *k));
                            },
                        }
                    }
                    div { class: "pane-select",
                        Select {
                            options: SORTS.iter().enumerate().map(|(i, (_, name))| SelectOption::new(i.to_string(), *name)).collect::<Vec<_>>(),
                            value: sort_value(sort()),
                            onchange: move |value: String| {
                                let index: usize = value.parse().unwrap_or_default();
                                sort.set(SORTS.get(index).map_or_else(FileSort::default, |(s, _)| *s));
                            },
                        }
                    }
                }
                ResultList {
                    keys,
                    status,
                    loading_more: results.loading_more() && !recent_view,
                    on_more: move |()| results.load_more(),
                    estimate: 84.0,
                    render: move |i: usize| match &rows[i] {
                        DateRow::Header(title) => rsx! { DateHeader { title: title.clone() } },
                        DateRow::Item(entry) => {
                            let removable = recent_view || own_messages.contains(&entry.message_id);
                            let (name, path) = (entry.upload.file_name.clone(), entry.upload.path.clone());
                            rsx! {
                                FileListRow {
                                    entry: entry.clone(),
                                    use_24h,
                                    extra: removable.then(|| Callback::new(move |()| {
                                        delete_menu(state, overlays, recent_view, &name, &path, refresh)
                                    })),
                                }
                            }
                        }
                    },
                }
                if current == View::Channel {
                    div { class: "files-footer",
                        span { class: "text-sub", {file_count_text(list.len())} }
                        div { class: "files-footer-actions",
                            IconButton {
                                icon: IconName::Refresh,
                                onclick: move |_| results.reload(),
                            }
                            Button {
                                kind: ButtonKind::Primary,
                                disabled: !can_upload,
                                onclick: move |_| {
                                    let (Some(session), Some(key)) = (state.session.peek().clone(), upload_key.clone()) else {
                                        return;
                                    };
                                    spawn(async move {
                                        upload_file(overlays, session, key, uploading).await;
                                        results.reload();
                                    });
                                },
                                match uploading() {
                                    Some(progress) => format!("上传中 {}%", (progress * 100.0).round()),
                                    None => "上传文件".to_owned(),
                                }
                            }
                        }
                    }
                }
            }
            }
        }
    }
}

/// 文件行菜单里的删除：最近使用里是清除下载记录，其它是删除自己上传的文件。
fn delete_menu(
    state: AppState,
    overlays: Overlays,
    recent: bool,
    name: &str,
    path: &str,
    refresh: Signal<u32>,
) -> Vec<MenuEntry> {
    let Some(session) = state.session.peek().clone() else {
        return Vec::new();
    };
    let (name, path) = (name.to_owned(), path.to_owned());
    vec![
        MenuEntry::item(if recent { "删除记录" } else { "删除" }, move || {
            let (session, name, path) = (session.clone(), name.clone(), path.clone());
            spawn_forever(async move {
                if recent {
                    delete_record(overlays, session, name, path).await;
                    bump(refresh);
                } else {
                    delete_upload(overlays, session, name, path).await;
                }
            });
        })
        .danger(),
    ]
}

/// 清除下载记录，已下载的文件不会被删除。
async fn delete_record(overlays: Overlays, session: Arc<Session>, name: String, path: String) {
    overlays
        .delete(
            &format!("下载记录“{name}”"),
            Some("已下载的文件不会被删除。"),
            || session.delete(DeleteTarget::DownloadRecord(path)),
        )
        .await;
}

/// 删除自己上传的文件，消息里的链接随之失效。
async fn delete_upload(overlays: Overlays, session: Arc<Session>, name: String, path: String) {
    let Some(list) = overlays.attempt("加载失败", session.attachments()).await else {
        return;
    };
    let path_id = path.trim_start_matches("/user_uploads/");
    let Some(id) = list
        .attachments
        .iter()
        .find(|a| a.path_id == path_id)
        .map(|a| a.id)
    else {
        overlays.toast("没有找到这个上传的文件");
        return;
    };
    overlays
        .delete(
            &format!("文件“{name}”"),
            Some("消息中的文件链接将失效。"),
            || session.delete(DeleteTarget::Attachment(id)),
        )
        .await;
}

/// 选择本地文件，上传并作为消息发到话题。
async fn upload_file(
    overlays: Overlays,
    session: Arc<Session>,
    key: ConversationKey,
    mut progress: Signal<Option<f64>>,
) {
    let Some(path) = crate::platform::pick_file("上传文件", &[]).await else {
        return;
    };
    let total = tokio::fs::metadata(&path).await.map_or(0, |m| m.len());
    let tracker = UploadProgress::default();
    progress.set(Some(0.0));
    let uploaded = tracker
        .track(
            total,
            session.upload_file(&path, tracker.callback()),
            |done| progress.set(Some(done)),
        )
        .await;
    progress.set(None);
    let Some(file) = overlays.attempt("上传失败", async { uploaded }).await else {
        return;
    };
    let name = file
        .filename
        .clone()
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let message = link_markdown(&name, &file.url);
    if overlays
        .attempt("发送失败", session.send_message(&key, &message))
        .await
        .is_some()
    {
        overlays.toast("已上传");
    }
}
