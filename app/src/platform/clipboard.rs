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
