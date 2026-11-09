//! Composer uploads: pasted files saved to disk, placeholder, progress, cancel, and replaced with a
//! link on completion.

use super::control::Ctl;
use super::core::upload_placeholder;
use crate::services::services;
use crate::widgets::UploadProgress;
use chrono::Local;
use dioxus::prelude::*;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use zulip_core::markdown::link_markdown;

/// A file being uploaded.
#[derive(Clone)]
pub struct UploadRow {
    pub id: u64,
    pub name: String,
    pub progress: f64,
    placeholder: String,
    abort: tokio::task::AbortHandle,
}

pub(super) struct PastedFile {
    path: PathBuf,
    name: String,
    file: std::fs::File,
}

impl Ctl {
    pub(super) fn pasted_chunk(self, id: u64, name: &str, hex: &str, last: bool) {
        let result = (|| -> std::io::Result<()> {
            let mut pasting = self.pasting.write_unchecked();
            if let std::collections::hash_map::Entry::Vacant(entry) = pasting.entry(id) {
                let name = if name.is_empty() {
                    screenshot_name()
                } else {
                    name.to_owned()
                };
                let path = unique_paste_path(&name)?;
                let file = std::fs::File::create(&path)?;
                entry.insert(PastedFile { path, name, file });
            }
            if let Some(entry) = pasting.get_mut(&id) {
                entry.file.write_all(&decode_hex(hex)?)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.pasting.write_unchecked().remove(&id);
            self.overlays.report_error("粘贴失败", error);
            return;
        }
        if last && let Some(entry) = self.pasting.write_unchecked().remove(&id) {
            self.upload(entry.path, entry.name);
        }
    }

    pub(super) fn too_big(self) {
        self.overlays
            .report_error("上传失败", self.session().upload_too_large());
    }

    /// Upload a file: put a placeholder at the cursor, and replace it with a link on completion.
    pub fn upload(self, path: PathBuf, name: String) {
        let session = self.session();
        let total = match std::fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(error) => {
                self.overlays.report_error("上传失败", error);
                return;
            }
        };
        if let Err(error) = session.check_upload_size(total) {
            self.overlays.report_error("上传失败", error);
            return;
        }
        let placeholder = {
            let core = self.core.peek();
            upload_placeholder(&name, |p| core.text().contains(p))
        };
        let inserted = placeholder.clone();
        self.when_idle(move |ctl| ctl.edit(|c| c.insert_attachment(&inserted)));

        let progress = UploadProgress::default();
        let on_sent = progress.callback();
        let local = path.clone();
        let task = {
            let session = session.clone();
            tokio::spawn(async move { session.upload_file(&local, on_sent).await })
        };
        let id = *self.next_upload.peek();
        *self.next_upload.write_unchecked() = id + 1;
        let mut uploads = self.uploads;
        uploads.write().push(UploadRow {
            id,
            name: name.clone(),
            progress: 0.0,
            placeholder: placeholder.clone(),
            abort: task.abort_handle(),
        });
        self.update_view();
        self.spawn(async move {
            let result = progress
                .track(total, task, |done| {
                    if let Some(row) = uploads.write().iter_mut().find(|r| r.id == id) {
                        row.progress = done;
                    }
                })
                .await;
            uploads.write().retain(|r| r.id != id);
            match result {
                Ok(Ok(uploaded)) => {
                    let label = uploaded.filename.clone().unwrap_or(name);
                    let link = link_markdown(&label, &uploaded.url);
                    self.when_idle(move |ctl| {
                        if let Some(local) = crate::shell::url_for(&path) {
                            let core = &mut *ctl.core.write_unchecked();
                            core.lookup.images.insert(uploaded.url.clone(), local);
                        }
                        ctl.replace_async(&placeholder, &link);
                    });
                }
                Ok(Err(error)) => {
                    self.overlays.report_error("上传失败", error);
                    self.when_idle(move |ctl| ctl.replace_async(&placeholder, ""));
                }
                // On cancel the placeholder has already been removed
                Err(_) => {}
            }
        });
    }

    pub(super) fn replace_async(self, placeholder: &str, replacement: &str) {
        self.core
            .write_unchecked()
            .replace_placeholder(placeholder, replacement);
        self.push(true, false, false);
        self.after_change(true);
    }

    pub fn cancel_upload(self, id: u64) {
        let mut uploads = self.uploads;
        let row = uploads.peek().iter().find(|r| r.id == id).cloned();
        if let Some(row) = row {
            row.abort.abort();
            uploads.write().retain(|r| r.id != id);
            self.when_idle(move |ctl| ctl.replace_async(&row.placeholder, ""));
        }
    }

    /// Upload after choosing a file.
    pub fn pick_and_upload(self, images_only: bool) {
        self.spawn(async move {
            let filters = [crate::platform::IMAGE_FILTER];
            let (title, filters): (&str, &[_]) = if images_only {
                ("发送图片", &filters)
            } else {
                ("发送文件", &[])
            };
            for path in crate::platform::pick_files(title, filters).await {
                self.upload_path(path);
            }
            self.refocus();
        });
    }

    /// Files that were dragged and dropped or chosen.
    pub fn upload_path(self, path: PathBuf) {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.upload(path, name);
    }
}

pub(super) fn screenshot_name() -> String {
    format!("截图{}.png", Local::now().format("%Y%m%d%H%M%S"))
}

fn unique_paste_path(name: &str) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = services().paths.cache_dir().join("paste").join(format!(
        "{}-{}",
        Local::now().timestamp_millis(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)?;
    // The file name comes from the clipboard: take only the last segment
    let name = std::path::Path::new(name)
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_owned());
    Ok(dir.join(name))
}

pub(super) fn write_paste_file(name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let path = unique_paste_path(name)?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn decode_hex(text: &str) -> std::io::Result<Vec<u8>> {
    hex::decode(text).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(decode_hex("00ff10").unwrap(), vec![0, 255, 16]);
    }

    #[test]
    fn invalid_hex_is_an_error() {
        assert!(decode_hex("0g").is_err());
        assert!(decode_hex("0").is_err());
    }

    #[test]
    fn paste_names_keep_only_the_last_segment() {
        let name = std::path::Path::new("..\\..\\evil.png")
            .file_name()
            .unwrap();
        assert_eq!(name, "evil.png");
    }
}
