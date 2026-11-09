use arboard::{Clipboard, ImageData};
use std::borrow::Cow;
use std::io::Cursor;
use std::sync::Mutex;

/// 最后一次复制的（显示的文字，Markdown）。系统剪贴板里只放纯文字（arboard 不支持自定义格式），
/// 同一进程内粘贴时据此恢复格式。
static LAST_COPY: Mutex<Option<(String, String)>> = Mutex::new(None);

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 记下即将复制的文字对应的 Markdown。
pub fn remember_markdown(text: &str, markdown: &str) {
    if let Ok(mut last) = LAST_COPY.lock() {
        *last = Some((normalize_newlines(text), markdown.to_owned()));
    }
}

/// 剪贴板文字 `text` 来自本程序的复制时，它的 Markdown。
pub fn copied_markdown(text: &str) -> Option<String> {
    let last = LAST_COPY.lock().ok()?;
    let (copied, markdown) = last.as_ref()?;
    (*copied == normalize_newlines(text)).then(|| markdown.clone())
}

/// 剪贴板中的图片（RGBA）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl ClipboardImage {
    /// 编码为 PNG（用于上传）。
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
