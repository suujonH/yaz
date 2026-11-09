//! 文件的共用流程：上传进度、下载（带进度）、打开、保存、另存为、所在文件夹、右键菜单，以及文件卡片。
//! 收藏、聊天记录、文件窗口里的文件都走这里。

use super::message_flows::{forward_text, locate_in_chat};
use crate::state::use_app_state;
use crate::ui::*;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use zulip_core::ConversationKey;
use zulip_core::file::FileIcon;
use zulip_core::html::UploadLink;
use zulip_core::markdown::link_markdown;
use zulip_core::text::time::{from_unix, list_time};
use zulip_session::{FileEntry, OpenAction, Session, SessionError};

/// 进行中的下载：上传路径 → 进度（总大小未知时为 `None`）。每个窗口各自一份。
static PROGRESS: GlobalSignal<HashMap<String, Option<f64>>> = Signal::global(HashMap::new);
/// 每完成一次下载加一，文件状态据此重查。
static FINISHED: GlobalSignal<u64> = Signal::global(|| 0);

/// 进度每变化这么多才通知界面。
const PROGRESS_STEP: f64 = 0.01;

/// 上传进度的轮询间隔。
const UPLOAD_POLL: Duration = Duration::from_millis(100);

/// 一次上传已发送的字节数：[`callback`](Self::callback) 交给 `Session::upload_file`，
/// [`track`](Self::track) 在等待上传时定时报告进度。
#[derive(Clone, Default)]
pub struct UploadProgress(Arc<AtomicU64>);

impl UploadProgress {
    pub fn callback(&self) -> impl Fn(u64) + Send + Sync + 'static {
        let sent = self.0.clone();
        move |bytes| sent.store(bytes, Ordering::Relaxed)
    }

    /// 等待 `task` 完成，期间定时用进度（0 到 1）调用 `report`。`task` 可以是可取消的任务句柄。
    pub async fn track<F: Future>(
        &self,
        total: u64,
        task: F,
        mut report: impl FnMut(f64),
    ) -> F::Output {
        tokio::pin!(task);
        loop {
            tokio::select! {
                result = &mut task => return result,
                () = tokio::time::sleep(UPLOAD_POLL) => {
                    report(self.0.load(Ordering::Relaxed) as f64 / total.max(1) as f64);
                }
            }
        }
    }
}

/// 一个文件在哪条消息里，下载记录和定位要用。
#[derive(Clone, Debug, PartialEq)]
pub struct FileSource {
    pub link: UploadLink,
    pub message_id: i64,
    pub key: ConversationKey,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FileStatus {
    Remote,
    Downloading(Option<f64>),
    Local(PathBuf),
}

impl FileStatus {
    pub fn text(&self) -> String {
        match self {
            Self::Remote => "未下载".to_owned(),
            Self::Downloading(Some(p)) => format!("下载中 {}%", (p * 100.0).round()),
            Self::Downloading(None) => "下载中".to_owned(),
            Self::Local(_) => "已下载".to_owned(),
        }
    }

    pub fn progress(&self) -> Option<f64> {
        match self {
            Self::Downloading(p) => Some(p.unwrap_or(0.0)),
            _ => None,
        }
    }

    pub fn local_path(&self) -> Option<PathBuf> {
        match self {
            Self::Local(path) => Some(path.clone()),
            _ => None,
        }
    }
}

/// 下载完成的次数（响应式）：最近下载列表据此重读。
pub fn downloads_epoch() -> u64 {
    *FINISHED.read()
}

/// 文件的下载状态（响应式）。
pub fn use_file_status(path: &str) -> FileStatus {
    let state = use_app_state();
    let lookup = path.to_owned();
    let local = use_resource(use_reactive!(|lookup| async move {
        let _ = *FINISHED.read();
        let session = state.session.peek().clone()?;
        session.downloaded_path(&lookup).await
    }));
    if let Some(progress) = PROGRESS.read().get(path) {
        return FileStatus::Downloading(*progress);
    }
    match local.read().clone().flatten() {
        Some(path) => FileStatus::Local(path),
        None => FileStatus::Remote,
    }
}

/// 下载进度的记录。
struct Tracker {
    path: String,
    last: f64,
}

impl Tracker {
    fn start(path: &str) -> Self {
        PROGRESS.write().insert(path.to_owned(), None);
        Self {
            path: path.to_owned(),
            last: 0.0,
        }
    }

    fn update(&mut self, done: u64, total: Option<u64>) {
        let Some(total) = total.filter(|t| *t > 0) else {
            return;
        };
        let fraction = done as f64 / total as f64;
        if fraction - self.last >= PROGRESS_STEP {
            self.last = fraction;
            PROGRESS.write().insert(self.path.clone(), Some(fraction));
        }
    }

    fn finish(self) {
        PROGRESS.write().remove(&self.path);
        *FINISHED.write() += 1;
    }
}

/// 确保文件已下载到默认保存目录，返回本地路径。
async fn download(session: &Arc<Session>, source: &FileSource) -> Result<PathBuf, SessionError> {
    if let Some(path) = session.downloaded_path(&source.link.path).await {
        return Ok(path);
    }
    let mut tracker = Tracker::start(&source.link.path);
    let result = session
        .save_to_default_folder(
            &source.link,
            source.message_id,
            &source.key.to_string(),
            |done, total| tracker.update(done, total),
        )
        .await;
    tracker.finish();
    result
}

/// 打开文件：没有下载过时先下载。可执行文件只在文件夹中定位。
pub async fn open_file(overlays: Overlays, session: &Arc<Session>, source: &FileSource) {
    if overlays
        .attempt("下载失败", download(session, source))
        .await
        .is_none()
    {
        return;
    }
    match overlays
        .attempt("打开失败", session.open_downloaded(&source.link.path))
        .await
    {
        Some(OpenAction::Open(path)) => overlays.open_file(&path),
        Some(OpenAction::Reveal(path)) => reveal(overlays, &path),
        None => {}
    }
}

/// 保存到默认保存目录。
pub async fn save_file(overlays: Overlays, session: &Arc<Session>, source: &FileSource) {
    if let Some(path) = overlays
        .attempt("保存失败", download(session, source))
        .await
    {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        overlays.toast(format!("已保存为 {name}"));
    }
}

/// 选择位置另存为。
pub async fn save_file_as(overlays: Overlays, session: &Arc<Session>, source: &FileSource) {
    let session = session.clone();
    let owned = source.clone();
    overlays
        .save_as(&source.link.file_name, move |target| async move {
            let mut tracker = Tracker::start(&owned.link.path);
            let result = session
                .save_as(
                    &owned.link,
                    &target,
                    owned.message_id,
                    &owned.key.to_string(),
                    |done, total| tracker.update(done, total),
                )
                .await;
            tracker.finish();
            result
        })
        .await;
}

/// 在资源管理器中选中文件。
pub fn reveal(overlays: Overlays, path: &Path) {
    let select = format!("/select,{}", path.display());
    if let Err(error) = std::process::Command::new("explorer").arg(select).spawn() {
        overlays.report_error("打开文件夹失败", error);
    }
}

/// 菜单项：在后台执行一个需要会话和文件的操作。
fn entry<F: Future<Output = ()> + 'static>(
    label: &str,
    overlays: Overlays,
    session: &Arc<Session>,
    source: &FileSource,
    action: impl Fn(Overlays, Arc<Session>, FileSource) -> F + 'static,
) -> MenuEntry {
    let (session, source) = (session.clone(), source.clone());
    MenuEntry::item(label, move || {
        spawn_forever(action(overlays, session.clone(), source.clone()));
    })
}

/// 文件右键菜单：打开、保存、另存为、所在文件夹、转发、定位到聊天。`downloaded` 是已下载文件的路径。
pub fn file_menu(
    overlays: Overlays,
    session: &Arc<Session>,
    source: &FileSource,
    downloaded: Option<PathBuf>,
) -> Vec<MenuEntry> {
    let mut items = vec![
        entry("打开", overlays, session, source, |o, s, f| async move {
            open_file(o, &s, &f).await;
        }),
        entry("保存", overlays, session, source, |o, s, f| async move {
            save_file(o, &s, &f).await;
        }),
        entry("另存为", overlays, session, source, |o, s, f| async move {
            save_file_as(o, &s, &f).await;
        }),
    ];
    if let Some(path) = downloaded {
        items.push(MenuEntry::item("打开所在文件夹", move || {
            reveal(overlays, &path);
        }));
    }
    items.push(MenuEntry::Separator);
    items.push(entry(
        "转发",
        overlays,
        session,
        source,
        |o, s, f| async move {
            let text = link_markdown(&f.link.file_name, &f.link.path);
            forward_text(o, &s, text).await;
        },
    ));
    items.push(entry(
        "定位到聊天",
        overlays,
        session,
        source,
        |o, s, f| async move {
            locate_in_chat(o, &s, Some(&f.key), f.message_id).await;
        },
    ));
    items
}

/// 文件类型对应的图标。
pub fn file_icon(path: &str) -> IconName {
    match FileIcon::of(path) {
        FileIcon::Pdf => IconName::FilePdf,
        FileIcon::Word => IconName::FileWord,
        FileIcon::Excel => IconName::FileExcel,
        FileIcon::Powerpoint => IconName::FilePowerpoint,
        FileIcon::Image => IconName::Image,
        FileIcon::Video => IconName::Video,
        FileIcon::Zip => IconName::FileZip,
        FileIcon::File => IconName::File,
        FileIcon::Unknown => IconName::FileUnknown,
    }
}

/// 字节数的文字：`123B`、`1.5KB`、`120KB`。
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes}B"),
        _ if value >= 100.0 => format!("{value:.0}{}", UNITS[unit]),
        _ => format!("{value:.1}{}", UNITS[unit]),
    }
}

/// 消息里的文件卡片：显示下载状态和进度；`interactive` 为 false 时点击和右键交给外层的行处理。
#[component]
pub fn FileItemCard(
    link: UploadLink,
    message_id: i64,
    conversation: ConversationKey,
    #[props(into)] detail: Option<String>,
    #[props(default = true)] interactive: bool,
) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let status = use_file_status(&link.path);
    let source = FileSource {
        link: link.clone(),
        message_id,
        key: conversation,
    };
    let detail = match detail {
        Some(detail) => format!("{detail} · {}", status.text()),
        None => status.text(),
    };
    let menu_source = source.clone();
    let menu_status = status.clone();
    rsx! {
        FileCard {
            name: link.file_name.clone(),
            icon: file_icon(&link.path),
            detail,
            progress: status.progress(),
            onclick: move |_| {
                if interactive && let Some(session) = state.session.peek().clone() {
                    let source = source.clone();
                    spawn(async move { open_file(overlays, &session, &source).await });
                }
            },
            menu: interactive.then(|| {
                Callback::new(move |()| {
                    state.session.peek().as_ref().map_or_else(Vec::new, |session| {
                        file_menu(overlays, session, &menu_source, menu_status.local_path())
                    })
                })
            }),
        }
    }
}

/// 文件列表的一行：文件卡片，右侧是来源和时间。点击打开，右键菜单是 [`file_menu`] 加上 `extra`（删除等）。
#[component]
pub fn FileListRow(
    entry: FileEntry,
    use_24h: bool,
    extra: Option<Callback<(), Vec<MenuEntry>>>,
) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let status = use_file_status(&entry.upload.path);
    let source = FileSource {
        link: entry.upload.clone(),
        message_id: entry.message_id,
        key: entry.key.clone(),
    };
    let time = from_unix(entry.timestamp)
        .map(|t| list_time(t, chrono::Local::now().naive_local(), use_24h));
    let open_source = source.clone();
    rsx! {
        ListRow {
            density: RowDensity::Large,
            leading: rsx! {
                FileItemCard {
                    link: entry.upload.clone(),
                    message_id: entry.message_id,
                    conversation: entry.key.clone(),
                    detail: entry.size.map(format_size),
                    interactive: false,
                }
            },
            title: entry.source.clone(),
            meta: time,
            onclick: move |_| {
                if let Some(session) = state.session.peek().clone() {
                    let source = open_source.clone();
                    spawn(async move { open_file(overlays, &session, &source).await });
                }
            },
            menu: move |()| {
                let Some(session) = state.session.peek().clone() else {
                    return Vec::new();
                };
                let mut items = file_menu(overlays, &session, &source, status.local_path());
                if let Some(extra) = extra {
                    items.push(MenuEntry::Separator);
                    items.extend(extra.call(()));
                }
                items
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_one_decimal_below_100() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(1023), "1023B");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(120 * 1024), "120KB");
        assert_eq!(format_size(5 * 1024 * 1024 + 512 * 1024), "5.5MB");
    }

    #[test]
    fn status_text_shows_percent() {
        assert_eq!(FileStatus::Remote.text(), "未下载");
        assert_eq!(FileStatus::Downloading(Some(0.426)).text(), "下载中 43%");
        assert_eq!(FileStatus::Downloading(None).text(), "下载中");
        assert_eq!(FileStatus::Local(PathBuf::from("a")).text(), "已下载");
        assert_eq!(FileStatus::Downloading(None).progress(), Some(0.0));
        assert_eq!(FileStatus::Remote.progress(), None);
    }
}
