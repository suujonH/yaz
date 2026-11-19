use arboard::{Clipboard, ImageData};
use std::borrow::Cow;
use std::io::Cursor;
use std::sync::Mutex;

/// The last copy: (displayed text, Markdown). Only plain text goes into the system clipboard
/// (arboard does not support custom formats), and pasting within the same process uses this to
/// restore the formatting.
static LAST_COPY: Mutex<Option<(String, String)>> = Mutex::new(None);

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Records the Markdown that corresponds to the text about to be copied.
pub fn remember_markdown(text: &str, markdown: &str) {
    if let Ok(mut last) = LAST_COPY.lock() {
        *last = Some((normalize_newlines(text), markdown.to_owned()));
    }
}

/// The Markdown of clipboard text `text` when it came from a copy made by this program.
pub fn copied_markdown(text: &str) -> Option<String> {
    let last = LAST_COPY.lock().ok()?;
    let (copied, markdown) = last.as_ref()?;
    (*copied == normalize_newlines(text)).then(|| markdown.clone())
}

/// The last image copied from a message: (clipboard sequence number right after the copy, account,
/// Markdown link of the uploaded file). Pasting it into the same account reuses the link instead
/// of uploading the image again.
static LAST_IMAGE: Mutex<Option<(u32, i64, String)>> = Mutex::new(None);

fn clipboard_sequence() -> u32 {
    // SAFETY: no arguments; it only reads a counter the system keeps.
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// Records that the image now on the clipboard is the file uploaded at `url` (an upload path of
/// `account`'s server). Other addresses (external images) are not remembered.
pub fn remember_image(account: i64, url: &str) {
    let Some(path) = zulip_core::html::normalize_upload_path(url) else {
        return;
    };
    let name = zulip_core::html::upload_link_of(&path, None).file_name;
    let markdown = zulip_core::markdown::link_markdown(&name, &path);
    if let Ok(mut last) = LAST_IMAGE.lock() {
        *last = Some((clipboard_sequence(), account, markdown));
    }
}

/// The Markdown link of the clipboard image when it is still the one copied from a message of
/// `account` (nothing has been copied since).
pub fn copied_image(account: i64) -> Option<String> {
    let last = LAST_IMAGE.lock().ok()?;
    let (sequence, copied_account, markdown) = last.as_ref()?;
    (*sequence == clipboard_sequence() && *copied_account == account).then(|| markdown.clone())
}

/// An image on the clipboard (RGBA).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl ClipboardImage {
    /// Encodes as PNG (for upload).
    pub fn to_png(&self) -> Option<Vec<u8>> {
        let image =
            image::RgbaImage::from_raw(self.width as u32, self.height as u32, self.rgba.clone())?;
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some(out.into_inner())
    }
}

pub fn copy_text(text: &str) -> Result<(), arboard::Error> {
    Clipboard::new()?.set_text(text)
}

pub fn paste_text() -> Option<String> {
    Clipboard::new().ok()?.get_text().ok()
}

pub fn copy_image(image: &ClipboardImage) -> Result<(), arboard::Error> {
    Clipboard::new()?.set_image(ImageData {
        width: image.width,
        height: image.height,
        bytes: Cow::Borrowed(&image.rgba),
    })
}

pub fn paste_image() -> Option<ClipboardImage> {
    let data = Clipboard::new().ok()?.get_image().ok()?;
    Some(ClipboardImage {
        width: data.width,
        height: data.height,
        rgba: data.bytes.into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_matches_only_same_text() {
        remember_markdown("a\nb", "**a**\nb");
        assert_eq!(copied_markdown("a\r\nb").as_deref(), Some("**a**\nb"));
        assert_eq!(copied_markdown("c"), None);
    }

    #[test]
    fn encodes_png() {
        let image = ClipboardImage {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        };
        let png = image.to_png().unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn rejects_wrong_buffer_size() {
        let image = ClipboardImage {
            width: 2,
            height: 2,
            rgba: vec![0; 3],
        };
        assert_eq!(image.to_png(), None);
    }
}
