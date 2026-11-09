//! Shared file flows: upload progress, download (with progress), open, save, save as, containing
//! folder, context menu, and the file card. Files in favorites, chat history, and the files window
//! all go through here.

use super::message_flows::{forward_text, locate_in_chat};
use super::remote_image::RemoteImage;
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

/// Downloads in progress: upload path -> progress (`None` when the total size is unknown). One per
/// window.
static PROGRESS: GlobalSignal<HashMap<String, Option<f64>>> = Signal::global(HashMap::new);
/// Incremented on every completed download; file status is re-checked based on it.
static FINISHED: GlobalSignal<u64> = Signal::global(|| 0);

/// The UI is notified only after progress changes by this much.
const PROGRESS_STEP: f64 = 0.01;

/// Polling interval for upload progress.
const UPLOAD_POLL: Duration = Duration::from_millis(100);

/// Bytes sent so far in one upload: [`callback`](Self::callback) is handed to
/// `Session::upload_file`, and [`track`](Self::track) periodically reports progress while waiting
/// for the upload.
#[derive(Clone, Default)]
pub struct UploadProgress(Arc<AtomicU64>);

impl UploadProgress {
    pub fn callback(&self) -> impl Fn(u64) + Send + Sync + 'static {
        let sent = self.0.clone();
        move |bytes| sent.store(bytes, Ordering::Relaxed)
    }

    /// Wait for `task` to finish, periodically calling `report` with the progress (0 to 1). `task`
    /// may be a cancellable task handle.
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

/// Which message a file belongs to; needed by the download record and by locate.
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

/// Number of completed downloads (reactive): the recent-downloads list re-reads based on it.
pub fn downloads_epoch() -> u64 {
    *FINISHED.read()
}

/// Download status of a file (reactive).
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

/// Record of download progress.
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

/// Ensure the file is downloaded to the default save directory; returns the local path.
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

/// Open a file: download first if it has not been downloaded. Executables are only located in the
/// folder.
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

/// Save to the default save directory.
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

/// Choose a location and save as.
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

/// Select the file in Explorer.
pub fn reveal(overlays: Overlays, path: &Path) {
    let select = format!("/select,{}", path.display());
    if let Err(error) = std::process::Command::new("explorer").arg(select).spawn() {
        overlays.report_error("打开文件夹失败", error);
    }
}

/// Menu item: run in the background an action that needs a session and a file.
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

/// File context menu: open, save, save as, containing folder, forward, locate in chat. `downloaded`
/// is the path of the downloaded file.
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

/// Icon for the file type.
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

/// Text for a byte count: `123B`, `1.5KB`, `120KB`.
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

/// File card inside a message: shows download status and progress; when `interactive` is false,
/// clicks and right-clicks are handled by the outer row.
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

/// Size of the preview / icon at the left of a file list row.
const ROW_PREVIEW: f64 = 40.0;

/// One row of the file list: preview (images) or type icon on the left; name, then size and
/// download status; time, then where it came from on the right. Click opens; the context menu is
/// [`file_menu`] plus `extra` (delete, etc.).
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
    let detail = match entry.size {
        Some(size) => format!("{} · {}", format_size(size), status.text()),
        None => status.text(),
    };
    let open_source = source.clone();
    rsx! {
        ListRow {
            density: RowDensity::Large,
            leading: rsx! {
                div { class: "file-row-preview",
                    if entry.upload.is_image {
                        RemoteImage { url: entry.upload.path.clone(), size: ROW_PREVIEW }
                    } else {
                        Icon { name: file_icon(&entry.upload.path), size: ROW_PREVIEW }
                    }
                }
            },
            title: entry.upload.file_name.clone(),
            meta: time,
            subtitle: detail,
            trailing: rsx! {
                span { class: "text-sub file-row-source", title: "{entry.source}", "{entry.source}" }
            },
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
