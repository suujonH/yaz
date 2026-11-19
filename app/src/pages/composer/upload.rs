//! Composer attachments: pasted, dropped or chosen files are kept on this computer and put in the
//! text as a pending link (`[name](yaz-upload:…)`, shown like an uploaded file). They are uploaded
//! only when the message is sent, and the pending addresses are then replaced with the uploaded
//! ones. An image copied from a message is pasted as the link of the file already uploaded.

use super::control::Ctl;
use crate::services::services;
use crate::widgets::UploadProgress;
use chrono::Local;
use dioxus::prelude::*;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use zulip_core::html::{encode, is_image_path};
use zulip_core::markdown::{PENDING_UPLOAD_SCHEME, link_markdown};
use zulip_core::model::PendingAttachment;

/// Characters kept as they are in the file name of a pending address.
const NAME_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'.').remove(b'-').remove(b'_');

/// A file being uploaded (while sending).
#[derive(Clone)]
pub struct UploadRow {
    pub id: u64,
    pub name: String,
    pub progress: f64,
    abort: tokio::task::AbortHandle,
}

/// A file being pasted: saved chunk by chunk, or an image copied from a message whose link is used
/// (its chunks are ignored).
pub(super) enum PastedFile {
    Saving {
        path: PathBuf,
        name: String,
        file: std::fs::File,
    },
    Reused(String),
}

impl PastedFile {
    fn create(name: &str) -> std::io::Result<Self> {
        let name = if name.is_empty() {
            screenshot_name()
        } else {
            name.to_owned()
        };
        let path = unique_paste_path(&name)?;
        let file = std::fs::File::create(&path)?;
        Ok(Self::Saving { path, name, file })
    }
}

impl Ctl {
    pub(super) fn pasted_chunk(self, id: u64, name: &str, hex: &str, last: bool) {
        if !self.pasting.peek().contains_key(&id) {
            // An image copied from a message of this account: use its link, nothing is uploaded
            let entry = match crate::platform::copied_image(self.account()) {
                Some(markdown) => Ok(PastedFile::Reused(markdown)),
                None => PastedFile::create(name),
            };
            match entry {
                Ok(entry) => {
                    self.pasting.write_unchecked().insert(id, entry);
                }
                Err(error) => {
                    self.overlays.report_error("粘贴失败", error);
                    return;
                }
            }
        }
        let written = match self.pasting.write_unchecked().get_mut(&id) {
            Some(PastedFile::Saving { file, .. }) => {
                decode_hex(hex).and_then(|bytes| file.write_all(&bytes))
            }
            _ => Ok(()),
        };
        if let Err(error) = written {
            self.pasting.write_unchecked().remove(&id);
            self.overlays.report_error("粘贴失败", error);
            return;
        }
        if !last {
            return;
        }
        match self.pasting.write_unchecked().remove(&id) {
            Some(PastedFile::Reused(markdown)) => {
                self.when_idle(move |ctl| ctl.edit(|c| c.insert_attachment(&markdown)));
            }
            Some(PastedFile::Saving { path, name, file }) => {
                drop(file);
                self.attach_pasted(path, name);
            }
            None => {}
        }
    }

    pub(super) fn too_big(self) {
        self.overlays
            .report_error("上传失败", self.session().upload_too_large());
    }

    /// Whether the file can be uploaded (exists, within the server's size limit); shows why not.
    fn size_ok(self, path: &Path) -> bool {
        let error = match std::fs::metadata(path) {
            Ok(meta) => match self.session().check_upload_size(meta.len()) {
                Ok(()) => return true,
                Err(error) => error.to_string(),
            },
            Err(error) => error.to_string(),
        };
        self.overlays.report_error("添加附件失败", error);
        false
    }

    /// Put a file that is already in the paste folder into the text as a pending link.
    pub(super) fn attach_pasted(self, path: PathBuf, name: String) {
        if !self.size_ok(&path) {
            return;
        }
        let Some(address) = pending_address(&path) else {
            return;
        };
        if zulip_core::html::is_image_path(&name)
            && let Some(local) = crate::shell::url_for(&path)
        {
            self.core
                .write_unchecked()
                .lookup
                .images
                .insert(address.clone(), local);
        }
        let markdown = link_markdown(&name, &address);
        self.when_idle(move |ctl| ctl.edit(|c| c.insert_attachment(&markdown)));
    }

    /// Files that were dragged and dropped or chosen: copied into the paste folder (so the draft
    /// still has them if the original moves), then attached.
    pub fn upload_path(self, path: PathBuf) {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if !self.size_ok(&path) {
            return;
        }
        self.spawn(async move {
            let copied = match unique_paste_path(&name) {
                Ok(target) => tokio::fs::copy(&path, &target).await.map(|_| target),
                Err(error) => Err(error),
            };
            match copied {
                Ok(target) => self.attach_pasted(target, name),
                Err(error) => self.overlays.report_error("添加附件失败", error),
            }
        });
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

    /// Upload the pending attachments in `text` and return it with their uploaded addresses.
    /// `None` when an upload failed (the error is shown) or was cancelled.
    pub(super) async fn upload_pending(self, mut text: String) -> Option<String> {
        let addresses = pending_addresses(&text);
        if addresses.is_empty() {
            return Some(text);
        }
        if !*self.connected.peek() {
            self.overlays.toast("未连接服务器，无法上传");
            return None;
        }
        let session = self.session();
        for address in addresses {
            let Some(path) = resolve_pending(&address).filter(|p| p.is_file()) else {
                self.overlays.toast("附件已不存在，请重新添加");
                return None;
            };
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let total = std::fs::metadata(&path).map_or(0, |m| m.len());
            let progress = UploadProgress::default();
            let on_sent = progress.callback();
            let task = {
                let (session, path) = (session.clone(), path.clone());
                tokio::spawn(async move { session.upload_file(&path, on_sent).await })
            };
            let id = *self.next_upload.peek();
            *self.next_upload.write_unchecked() = id + 1;
            let mut uploads = self.uploads;
            uploads.write().push(UploadRow {
                id,
                name,
                progress: 0.0,
                abort: task.abort_handle(),
            });
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
                    if let Some(local) = crate::shell::url_for(&path) {
                        let core = &mut *self.core.write_unchecked();
                        core.lookup.images.insert(uploaded.url.clone(), local);
                    }
                    text = text.replace(&address, &uploaded.url);
                }
                Ok(Err(error)) => {
                    self.overlays.report_error("上传失败", error);
                    return None;
                }
                // Cancelled
                Err(_) => return None,
            }
        }
        Some(text)
    }

    pub fn cancel_upload(self, id: u64) {
        let mut uploads = self.uploads;
        let row = uploads.peek().iter().find(|r| r.id == id).cloned();
        if let Some(row) = row {
            row.abort.abort();
            uploads.write().retain(|r| r.id != id);
        }
    }
}

pub(super) fn screenshot_name() -> String {
    format!("截图{}.png", Local::now().format("%Y%m%d%H%M%S"))
}

fn paste_root() -> PathBuf {
    services().paths.cache_dir().join("paste")
}

fn unique_paste_path(name: &str) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = paste_root().join(format!(
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

/// `yaz-upload:<folder>/<encoded name>` of a file in the paste folder.
fn pending_address(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let dir = path.parent()?.file_name()?.to_str()?;
    Some(pending_address_of(dir, name))
}

fn pending_address_of(dir: &str, name: &str) -> String {
    format!(
        "{PENDING_UPLOAD_SCHEME}{dir}/{}",
        utf8_percent_encode(name, NAME_SET)
    )
}

/// Where a pending address points in the paste folder; `None` for a malformed address (the
/// folder must be one made by [`unique_paste_path`] and the name a single segment).
pub(crate) fn resolve_pending(address: &str) -> Option<PathBuf> {
    let (dir, name) = split_pending(address)?;
    Some(paste_root().join(dir).join(name))
}

fn split_pending(address: &str) -> Option<(String, String)> {
    let rest = address.strip_prefix(PENDING_UPLOAD_SCHEME)?;
    let (dir, encoded) = rest.split_once('/')?;
    let valid_dir = !dir.is_empty() && dir.bytes().all(|b| b.is_ascii_digit() || b == b'-');
    let name = percent_decode_str(encoded).decode_utf8().ok()?.into_owned();
    let single = Path::new(&name)
        .file_name()
        .is_some_and(|n| n == name.as_str());
    (valid_dir && single).then(|| (dir.to_owned(), name))
}

/// The pending attachments of a message about to be sent; `None` when one of their files is gone
/// (a draft from before the cache was cleared).
pub(super) fn pending_attachments(text: &str) -> Option<Vec<PendingAttachment>> {
    pending_addresses(text)
        .into_iter()
        .map(|address| {
            let path = resolve_pending(&address).filter(|p| p.is_file())?;
            Some(PendingAttachment { address, path })
        })
        .collect()
}

/// HTML of the local echo of a message with pending attachments: the text as typed, with each
/// image shown from its local file (as the server will show it) and each other file by its name.
pub(super) fn echo_html(text: &str, attachments: &[PendingAttachment]) -> String {
    let mut out = String::new();
    let mut paragraph = String::new();
    let flush = |out: &mut String, paragraph: &mut String| {
        let trimmed = paragraph.trim_matches('\n');
        if !trimmed.trim().is_empty() {
            out.push_str("<p>");
            out.push_str(&encode(trimmed).replace('\n', "<br>"));
            out.push_str("</p>");
        }
        paragraph.clear();
    };
    let mut rest = text;
    while let Some((start, end, attachment, label)) = next_attachment_link(rest, attachments) {
        paragraph.push_str(&rest[..start]);
        if is_image_path(&attachment.address) {
            flush(&mut out, &mut paragraph);
            let address = encode(&attachment.address);
            let size = image::image_dimensions(&attachment.path)
                .map(|(w, h)| format!(" data-original-dimensions=\"{w}x{h}\""))
                .unwrap_or_default();
            out.push_str(&format!(
                "<div class=\"message_inline_image\"><a href=\"{address}\"><img src=\"{address}\"{size}></a></div>"
            ));
        } else {
            paragraph.push_str(label);
        }
        rest = &rest[end..];
    }
    paragraph.push_str(rest);
    flush(&mut out, &mut paragraph);
    out
}

/// The first `[label](address)` of an attachment in `text`: (start, end, attachment, label).
fn next_attachment_link<'t, 'a>(
    text: &'t str,
    attachments: &'a [PendingAttachment],
) -> Option<(usize, usize, &'a PendingAttachment, &'t str)> {
    attachments
        .iter()
        .filter_map(|attachment| {
            let close = format!("]({})", attachment.address);
            let at = text.find(&close)?;
            let open = text[..at].rfind('[')?;
            let start = if text[..open].ends_with('!') {
                open - 1
            } else {
                open
            };
            Some((start, at + close.len(), attachment, &text[open + 1..at]))
        })
        .min_by_key(|(start, ..)| *start)
}

/// Distinct pending addresses in the text, in order.
fn pending_addresses(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for (at, _) in text.match_indices(PENDING_UPLOAD_SCHEME) {
        let end = text[at..]
            .find([')', '>', ' ', '\t', '\n'])
            .map_or(text.len(), |e| at + e);
        let address = &text[at..end];
        if !found.iter().any(|a| a == address) {
            found.push(address.to_owned());
        }
    }
    found
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

    #[test]
    fn pending_addresses_round_trip() {
        let address = pending_address_of("1700000000000-3", "截图 (1).png");
        assert!(!address.contains([' ', '(', ')']));
        assert_eq!(
            split_pending(&address),
            Some(("1700000000000-3".into(), "截图 (1).png".into()))
        );
    }

    #[test]
    fn malformed_pending_addresses_are_rejected() {
        for address in [
            "yaz-upload:../x/a.png",
            "yaz-upload:1-0/..%5C..%5Ca.png",
            "yaz-upload:1-0/a%2Fb.png",
            "yaz-upload:1-0",
            "yaz-upload:/a.png",
        ] {
            assert_eq!(split_pending(address), None, "{address}");
        }
    }

    #[test]
    fn echo_shows_images_as_blocks_and_files_by_name() {
        let attachment = |address: &str| PendingAttachment {
            address: address.into(),
            path: "missing".into(),
        };
        let attachments = [
            attachment("yaz-upload:1-0/a.png"),
            attachment("yaz-upload:2-0/b.zip"),
        ];
        let text = "看<这个>\n[a.png](yaz-upload:1-0/a.png)\n和 [b.zip](yaz-upload:2-0/b.zip)";
        assert_eq!(
            echo_html(text, &attachments),
            concat!(
                "<p>看&lt;这个&gt;</p>",
                r#"<div class="message_inline_image"><a href="yaz-upload:1-0/a.png"><img src="yaz-upload:1-0/a.png"></a></div>"#,
                "<p>和 b.zip</p>",
            )
        );
    }

    #[test]
    fn finds_each_pending_address_once() {
        let text = "a [x.png](yaz-upload:1-0/x.png) b [x.png](yaz-upload:1-0/x.png)\n\
                    [y.zip](<yaz-upload:2-0/y.zip>)";
        assert_eq!(
            pending_addresses(text),
            ["yaz-upload:1-0/x.png", "yaz-upload:2-0/y.zip"]
        );
    }
}
