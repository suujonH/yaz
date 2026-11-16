//! Tray icons drawn from an avatar (shown while the tray flashes): an image avatar or a text swatch,
//! cut to a circle like the avatars in the window.

use dioxus::desktop::trayicon::Icon;
use std::path::Path;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::DEFAULT_PITCH;
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject,
    DrawTextW, FF_DONTCARE, FW_NORMAL, FillRect, GdiFlush, HGDIOBJ, OUT_DEFAULT_PRECIS,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::core::w;

/// Side of the icon in pixels.
const SIZE: u32 = 32;
/// Text height relative to the icon (same ratio as the avatar in the window).
const FONT_RATIO: f64 = 0.42;

/// An icon showing nothing. Every pixel has an alpha of 1 (not 0): when all are 0, Windows
/// ignores the alpha channel and shows the icon black.
pub fn blank_icon() -> Option<Icon> {
    let rgba = [0, 0, 0, 1].repeat((SIZE * SIZE) as usize);
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

/// Icon of an image avatar (a downloaded file): scaled to fill the icon and cut to a circle.
pub fn image_icon(path: &Path) -> Option<Icon> {
    let image = image::open(path)
        .inspect_err(|e| log::debug!("读取托盘头像失败：{e}"))
        .ok()?
        .resize_to_fill(SIZE, SIZE, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let mut rgba = image.into_raw();
    // An image avatar shows on white (as in the window), so a transparent image is not see-through
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(pixel[3]);
        for c in &mut pixel[..3] {
            *c = ((u32::from(*c) * a + 255 * (255 - a)) / 255) as u8;
        }
        pixel[3] = 255;
    }
    to_circle(&mut rgba);
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

/// Icon of a text avatar: the text (its first character) in white on `color` ("#rrggbb").
pub fn text_icon(text: &str, color: &str) -> Option<Icon> {
    let (r, g, b) = parse_color(color)?;
    let mut rgba = draw_text(text, (r, g, b))?;
    to_circle(&mut rgba);
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

fn parse_color(color: &str) -> Option<(u8, u8, u8)> {
    let hex = color.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let part = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some((part(0)?, part(2)?, part(4)?))
}

/// Alpha of the pixels outside the circle set to 0, with one pixel of smoothing at its edge.
fn to_circle(rgba: &mut [u8]) {
    let center = f64::from(SIZE) / 2.0;
    for (i, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (i as u32 % SIZE) as f64 + 0.5 - center;
        let y = (i as u32 / SIZE) as f64 + 0.5 - center;
        let coverage = (center - (x * x + y * y).sqrt() + 0.5).clamp(0.0, 1.0);
        pixel[3] = (f64::from(pixel[3]) * coverage).round() as u8;
    }
}

/// RGBA pixels of the text centered on an opaque swatch of `background`, drawn with GDI.
fn draw_text(text: &str, (r, g, b): (u8, u8, u8)) -> Option<Vec<u8>> {
    let side = SIZE as i32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            // Negative: rows from top to bottom
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: every GDI object created here is selected out and deleted before returning; `bits`
    // points to the bitmap's SIZE * SIZE pixels while the bitmap lives.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let mut bits = std::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        let rect = RECT {
            left: 0,
            top: 0,
            right: side,
            bottom: side,
        };
        let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(
            u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16),
        ));
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let font = CreateFontW(
            -((f64::from(SIZE) * FONT_RATIO).round() as i32),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            u32::from(DEFAULT_PITCH.0 | FF_DONTCARE.0),
            w!("Microsoft YaHei UI"),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00FF_FFFF));
        let mut chars: Vec<u16> = text.encode_utf16().collect();
        let mut text_rect = rect;
        DrawTextW(
            dc,
            &mut chars,
            &mut text_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = GdiFlush();
        let pixels = std::slice::from_raw_parts(bits as *const u8, (SIZE * SIZE * 4) as usize);
        // BGRA (alpha not set by GDI) to opaque RGBA
        let rgba = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect();
        SelectObject(dc, old_font);
        SelectObject(dc, old_bitmap);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        Some(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors() {
        assert_eq!(parse_color("#0052D9"), Some((0x00, 0x52, 0xD9)));
        assert_eq!(parse_color("0052D9"), None);
        assert_eq!(parse_color("#fff"), None);
    }

    #[test]
    fn circle_clears_corners_and_keeps_center() {
        let mut rgba = [255u8; (SIZE * SIZE * 4) as usize].to_vec();
        to_circle(&mut rgba);
        assert_eq!(rgba[3], 0);
        let center = ((SIZE / 2 * SIZE + SIZE / 2) * 4 + 3) as usize;
        assert_eq!(rgba[center], 255);
    }

    #[test]
    fn text_is_drawn_on_the_swatch() {
        let rgba = draw_text("张", (0, 0x52, 0xD9)).unwrap();
        // Some pixels are the background, some (the text) are lighter
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[..3] == [0, 0x52, 0xD9])
        );
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 0x80 && p[1] > 0x80)
        );
    }
}
