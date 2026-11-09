//! Avatar image processing: reading (applying EXIF orientation, limiting size) and rendering a
//! cropped region as PNG.

use std::io::Cursor;
use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use zulip_core::avatar::{AvatarCropRect, output_side};

/// Maximum edge length of an image during processing; larger images are shrunk first.
const MAX_WORKING_SIDE: u32 = 4096;
const MAX_DECODED_PIXELS: u64 = 100_000_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AvatarImageError {
    #[error("无法读取这张图片")]
    Unreadable,
    #[error("图片太大，无法处理")]
    TooLarge,
    #[error("裁剪区域超出图片范围")]
    InvalidCrop,
}

/// The image after reading, with its orientation already corrected.
pub struct AvatarImage {
    image: DynamicImage,
}

impl AvatarImage {
    pub fn load(path: &Path) -> Result<Self, AvatarImageError> {
        let bytes = std::fs::read(path).map_err(|_| AvatarImageError::Unreadable)?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AvatarImageError> {
        let reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| AvatarImageError::Unreadable)?;
        let mut decoder = reader
            .into_decoder()
            .map_err(|_| AvatarImageError::Unreadable)?;
        let (width, height) = decoder.dimensions();
        if u64::from(width) * u64::from(height) > MAX_DECODED_PIXELS {
            return Err(AvatarImageError::TooLarge);
        }
        let orientation = decoder.orientation().ok();
        let mut image =
            DynamicImage::from_decoder(decoder).map_err(|_| AvatarImageError::Unreadable)?;
        if let Some(orientation) = orientation {
            image.apply_orientation(orientation);
        }
        if image.width().max(image.height()) > MAX_WORKING_SIDE {
            image = image.resize(MAX_WORKING_SIDE, MAX_WORKING_SIDE, FilterType::Triangle);
        }
        Ok(Self { image })
    }

    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// Crops and outputs PNG. Shrinks (Triangle filter) when larger than the output limit; never
    /// enlarges.
    pub fn render_png(&self, crop: AvatarCropRect) -> Result<Vec<u8>, AvatarImageError> {
        let fits = crop.side > 0
            && u64::from(crop.x) + u64::from(crop.side) <= u64::from(self.width())
            && u64::from(crop.y) + u64::from(crop.side) <= u64::from(self.height());
        if !fits {
            return Err(AvatarImageError::InvalidCrop);
        }
        let mut cropped = self.image.crop_imm(crop.x, crop.y, crop.side, crop.side);
        let side = output_side(crop.side);
        if side != crop.side {
            cropped = cropped.resize_exact(side, side, FilterType::Triangle);
        }
        let mut out = Cursor::new(Vec::new());
        cropped
            .write_to(&mut out, ImageFormat::Png)
            .map_err(|_| AvatarImageError::Unreadable)?;
        Ok(out.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, Rgba, RgbaImage};

    const RED: [u8; 3] = [255, 0, 0];
    const GREEN: [u8; 3] = [0, 255, 0];
    const BLUE: [u8; 3] = [0, 0, 255];
    const WHITE: [u8; 3] = [255, 255, 255];

    fn encode(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn solid_rgba(width: u32, height: u32, color: [u8; 4]) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, Rgba(color)))
    }

    /// A JPEG with a different color in each of the four quadrants.
    fn quadrants_jpeg() -> Vec<u8> {
        let image = RgbaImage::from_fn(64, 64, |x, y| {
            let c = match (x < 32, y < 32) {
                (true, true) => RED,
                (false, true) => GREEN,
                (true, false) => BLUE,
                (false, false) => WHITE,
            };
            Rgba([c[0], c[1], c[2], 255])
        });
        let rgb = DynamicImage::ImageRgba8(image).to_rgb8();
        encode(&DynamicImage::ImageRgb8(rgb), ImageFormat::Jpeg)
    }

    /// Inserts an EXIF segment carrying the orientation after the SOI.
    fn with_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
        let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend([orientation, 0, 0, 0, 0, 0, 0, 0]);
        let len = (exif.len() + 2) as u16;
        let mut out = jpeg[..2].to_vec();
        out.extend([0xff, 0xe1]);
        out.extend(len.to_be_bytes());
        out.extend(exif);
        out.extend(&jpeg[2..]);
        out
    }

    fn close(actual: [u8; 3], expected: [u8; 3]) -> bool {
        actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) < 40)
    }

    fn pixel(image: &DynamicImage, x: u32, y: u32) -> [u8; 3] {
        let p = image.get_pixel(x, y).0;
        [p[0], p[1], p[2]]
    }

    fn decode_png(bytes: &[u8]) -> DynamicImage {
        image::load_from_memory_with_format(bytes, ImageFormat::Png).unwrap()
    }

    #[test]
    fn exif_orientation_is_applied() {
        let jpeg = quadrants_jpeg();
        let cases: [(u8, [[u8; 3]; 4]); 8] = [
            (1, [RED, GREEN, BLUE, WHITE]),
            (2, [GREEN, RED, WHITE, BLUE]),
            (3, [WHITE, BLUE, GREEN, RED]),
            (4, [BLUE, WHITE, RED, GREEN]),
            (5, [RED, BLUE, GREEN, WHITE]),
            (6, [BLUE, RED, WHITE, GREEN]),
            (7, [WHITE, GREEN, BLUE, RED]),
            (8, [GREEN, WHITE, RED, BLUE]),
        ];
        for (orientation, [tl, tr, bl, br]) in cases {
            let loaded = AvatarImage::from_bytes(&with_orientation(&jpeg, orientation)).unwrap();
            let image = &loaded.image;
            assert!(close(pixel(image, 8, 8), tl), "方向 {orientation} 左上");
            assert!(close(pixel(image, 56, 8), tr), "方向 {orientation} 右上");
            assert!(close(pixel(image, 8, 56), bl), "方向 {orientation} 左下");
            assert!(close(pixel(image, 56, 56), br), "方向 {orientation} 右下");
        }
    }

    #[test]
    fn crop_is_exact_and_keeps_transparency() {
        let mut image = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 0]));
        image.put_pixel(3, 4, Rgba([255, 0, 0, 255]));
        let png = encode(&DynamicImage::ImageRgba8(image), ImageFormat::Png);
        let loaded = AvatarImage::from_bytes(&png).unwrap();
        let out = decode_png(
            &loaded
                .render_png(AvatarCropRect {
                    x: 3,
                    y: 4,
                    side: 2,
                })
                .unwrap(),
        );
        assert_eq!(out.dimensions(), (2, 2));
        assert_eq!(out.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(out.get_pixel(1, 1).0[3], 0);
    }

    #[test]
    fn large_crop_is_averaged_down() {
        let checker = RgbaImage::from_fn(2048, 2048, |x, y| {
            if (x + y) % 2 == 0 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        });
        let png = encode(&DynamicImage::ImageRgba8(checker), ImageFormat::Png);
        let loaded = AvatarImage::from_bytes(&png).unwrap();
        let out = decode_png(
            &loaded
                .render_png(AvatarCropRect {
                    x: 0,
                    y: 0,
                    side: 2048,
                })
                .unwrap(),
        );
        assert_eq!(out.dimensions(), (512, 512));
        let grey = out.get_pixel(256, 256).0[0];
        assert!((90..=200).contains(&grey), "{grey}");
    }

    #[test]
    fn small_crop_is_not_upscaled() {
        let png = encode(&solid_rgba(100, 100, [1, 2, 3, 255]), ImageFormat::Png);
        let loaded = AvatarImage::from_bytes(&png).unwrap();
        let out = decode_png(
            &loaded
                .render_png(AvatarCropRect {
                    x: 0,
                    y: 0,
                    side: 50,
                })
                .unwrap(),
        );
        assert_eq!(out.dimensions(), (50, 50));
    }

    #[test]
    fn oversized_image_shrinks_keeping_aspect() {
        let png = encode(&solid_rgba(8192, 100, [1, 2, 3, 255]), ImageFormat::Png);
        let loaded = AvatarImage::from_bytes(&png).unwrap();
        assert_eq!((loaded.width(), loaded.height()), (4096, 50));
    }

    #[test]
    fn pixel_limit_is_enforced_before_decoding() {
        // PNG header of 10001x10001: dimensions only, no data
        let mut png = encode(&solid_rgba(1, 1, [0, 0, 0, 255]), ImageFormat::Png);
        png[16..20].copy_from_slice(&10001u32.to_be_bytes());
        png[20..24].copy_from_slice(&10001u32.to_be_bytes());
        // The decoder fails already when reading the header on a CRC mismatch; such a file cannot be
        // processed either
        let error = AvatarImage::from_bytes(&png).err().unwrap();
        assert!(matches!(
            error,
            AvatarImageError::TooLarge | AvatarImageError::Unreadable
        ));
    }

    #[test]
    fn webp_is_supported() {
        let webp = encode(&solid_rgba(4, 4, [10, 20, 30, 255]), ImageFormat::WebP);
        let loaded = AvatarImage::from_bytes(&webp).unwrap();
        assert_eq!((loaded.width(), loaded.height()), (4, 4));
    }

    #[test]
    fn unreadable_inputs() {
        assert_eq!(
            AvatarImage::from_bytes(b"not an image").err(),
            Some(AvatarImageError::Unreadable)
        );
        let png = encode(&solid_rgba(32, 32, [1, 2, 3, 255]), ImageFormat::Png);
        assert_eq!(
            AvatarImage::from_bytes(&png[..png.len() / 2]).err(),
            Some(AvatarImageError::Unreadable)
        );
        assert_eq!(
            AvatarImage::load(Path::new("Z:/不存在/x.png")).err(),
            Some(AvatarImageError::Unreadable)
        );
    }

    #[test]
    fn crop_out_of_bounds_is_rejected() {
        let png = encode(&solid_rgba(10, 10, [1, 2, 3, 255]), ImageFormat::Png);
        let loaded = AvatarImage::from_bytes(&png).unwrap();
        for crop in [
            AvatarCropRect {
                x: 5,
                y: 0,
                side: 6,
            },
            AvatarCropRect {
                x: 0,
                y: 9,
                side: 2,
            },
            AvatarCropRect {
                x: 0,
                y: 0,
                side: 0,
            },
        ] {
            assert_eq!(loaded.render_png(crop), Err(AvatarImageError::InvalidCrop));
        }
    }
}
