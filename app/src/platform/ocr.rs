//! Text recognition with the system OCR (Windows.Media.Ocr): words and where they are on the image.

use super::PlatformError;
use image::imageops::FilterType;
use std::path::Path;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;

/// A recognized word; the rectangle is in pixels of the original image.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrWord {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A recognized line: its words in reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrLine {
    pub words: Vec<OcrWord>,
}

fn ocr_error(error: windows::core::Error) -> PlatformError {
    PlatformError::Ocr(error.message().to_string())
}

/// Recognizes the text on the image at `path`. Blocks: call it from `spawn_blocking`. `None` when
/// the system has no OCR language; lines without words are left out.
pub fn recognize_text(path: &Path) -> Result<Option<Vec<OcrLine>>, PlatformError> {
    let Some(engine) = create_engine()? else {
        return Ok(None);
    };
    let mut img = image::open(path).map_err(|e| PlatformError::Ocr(e.to_string()))?;
    let max = f64::from(OcrEngine::MaxImageDimension().map_err(ocr_error)?);
    // Images over the OCR size limit are recognized shrunk; the rectangles are scaled back
    let factor = shrink_factor((img.width(), img.height()), max);
    if factor < 1.0 {
        let shrink = |side: u32| (f64::from(side) * factor).floor().max(1.0) as u32;
        img = img.resize(
            shrink(img.width()),
            shrink(img.height()),
            FilterType::Triangle,
        );
    }
    let rgba = img.to_rgba8();
    let writer = DataWriter::new().map_err(ocr_error)?;
    writer
        .WriteBytes(&to_opaque_bgra(&rgba))
        .map_err(ocr_error)?;
    let buffer = writer.DetachBuffer().map_err(ocr_error)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        rgba.width() as i32,
        rgba.height() as i32,
    )
    .map_err(ocr_error)?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(ocr_error)?
        .get()
        .map_err(ocr_error)?;
    let mut lines = Vec::new();
    for line in result.Lines().map_err(ocr_error)? {
        let mut words = Vec::new();
        for word in line.Words().map_err(ocr_error)? {
            let text = word.Text().map_err(ocr_error)?.to_string();
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let rect = word.BoundingRect().map_err(ocr_error)?;
            words.push(OcrWord {
                text: text.to_owned(),
                x: f64::from(rect.X) / factor,
                y: f64::from(rect.Y) / factor,
                width: f64::from(rect.Width) / factor,
                height: f64::from(rect.Height) / factor,
            });
        }
        if !words.is_empty() {
            lines.push(OcrLine { words });
        }
    }
    Ok(Some(lines))
}

/// The engine for the user's languages, else for the first OCR language installed; `None` when
/// there is none.
fn create_engine() -> Result<Option<OcrEngine>, PlatformError> {
    // Gives no engine (an error here) when none of the user's languages can be recognized
    if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
        return Ok(Some(engine));
    }
    let languages = OcrEngine::AvailableRecognizerLanguages().map_err(ocr_error)?;
    let Some(language) = languages.into_iter().next() else {
        return Ok(None);
    };
    Ok(OcrEngine::TryCreateFromLanguage(&language).ok())
}

/// How much to shrink an image of `size` so its longer side is at most `max` (1.0: not at all).
fn shrink_factor(size: (u32, u32), max: f64) -> f64 {
    let longer = size.0.max(size.1);
    if longer == 0 || max <= 0.0 {
        return 1.0;
    }
    (max / f64::from(longer)).min(1.0)
}

/// RGBA to opaque BGRA over a white background, so text on a transparent image stays readable.
fn to_opaque_bgra(rgba: &image::RgbaImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.as_raw().len());
    for [r, g, b, a] in rgba.as_raw().as_chunks::<4>().0 {
        let alpha = u32::from(*a);
        let over_white = |c: u8| ((u32::from(c) * alpha + 255 * (255 - alpha)) / 255) as u8;
        out.extend_from_slice(&[over_white(*b), over_white(*g), over_white(*r), 255]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_images_shrink_to_the_limit() {
        assert_eq!(shrink_factor((1000, 500), 2600.0), 1.0);
        assert_eq!(shrink_factor((5200, 100), 2600.0), 0.5);
        assert_eq!(shrink_factor((0, 0), 2600.0), 1.0);
    }

    #[test]
    fn pixels_become_opaque_bgra_over_white() {
        let image =
            image::RgbaImage::from_raw(3, 1, vec![255, 0, 0, 255, 10, 20, 30, 0, 0, 0, 0, 128])
                .unwrap();
        assert_eq!(
            to_opaque_bgra(&image),
            vec![0, 0, 255, 255, 255, 255, 255, 255, 127, 127, 127, 255]
        );
    }
}
