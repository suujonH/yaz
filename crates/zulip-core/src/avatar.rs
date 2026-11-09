//! State and conversion for avatar cropping, plus avatar URLs.
//!
//! The square viewfinder is fixed; the image pans and zooms beneath it and always fills the
//! viewfinder. Zoom is expressed as a slider position from 0 to 1: 0 is the image's short side
//! exactly filling the viewfinder, 1 is the maximum magnification, and values in between change
//! geometrically.

use crate::util::ends_with_ignore_case;

/// Maximum magnification relative to "fill".
pub const MAX_ZOOM: f64 = 5.0;

/// Upper bound on the output side length: Zulip uses avatars of at most 500 pixels, larger only
/// increases upload size.
pub const MAX_OUTPUT_SIDE: u32 = 512;

const PNG: &str = ".png";
const MEDIUM_PNG: &str = "-medium.png";

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("图片和取景框的尺寸必须为正数")]
pub struct InvalidCropSize;

/// Square crop region on the original image (pixels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvatarCropRect {
    pub x: u32,
    pub y: u32,
    pub side: u32,
}

/// Position and size of the image in the viewfinder's coordinate system.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Methods that modify state return whether the position or zoom changed.
#[derive(Clone, Debug)]
pub struct AvatarCrop {
    image_width: u32,
    image_height: u32,
    viewport_side: f64,
    min_scale: f64,
    max_scale: f64,
    zoom_position: f64,
    center_x: f64,
    center_y: f64,
}

impl AvatarCrop {
    pub fn new(
        image_width: u32,
        image_height: u32,
        viewport_side: f64,
    ) -> Result<Self, InvalidCropSize> {
        if image_width == 0 || image_height == 0 || viewport_side <= 0.0 {
            return Err(InvalidCropSize);
        }
        let min_scale = viewport_side / f64::from(image_width.min(image_height));
        // Zoom in until only 1 source-image pixel is left in the viewfinder, so a very small image
        // may not be zoomable further
        let max_scale = min_scale.max((min_scale * MAX_ZOOM).min(viewport_side));
        Ok(Self {
            image_width,
            image_height,
            viewport_side,
            min_scale,
            max_scale,
            zoom_position: 0.0,
            center_x: f64::from(image_width) / 2.0,
            center_y: f64::from(image_height) / 2.0,
        })
    }

    pub fn min_scale(&self) -> f64 {
        self.min_scale
    }

    pub fn can_zoom(&self) -> bool {
        self.max_scale > self.min_scale
    }

    /// Position of the viewfinder center on the original image (pixels).
    pub fn center(&self) -> (f64, f64) {
        (self.center_x, self.center_y)
    }

    /// Screen length / original image length.
    pub fn scale(&self) -> f64 {
        self.min_scale * (self.max_scale / self.min_scale).powf(self.zoom_position)
    }

    pub fn zoom_position(&self) -> f64 {
        self.zoom_position
    }

    /// Set the slider position (0 to 1), zooming about the viewfinder center.
    pub fn set_zoom_position(&mut self, position: f64) -> bool {
        let half = self.viewport_side / 2.0;
        self.set_zoom(position, half, half)
    }

    /// Zoom to `position`, keeping the image content at viewfinder coordinates (`anchor_x`,
    /// `anchor_y`) fixed.
    pub fn set_zoom(&mut self, position: f64, anchor_x: f64, anchor_y: f64) -> bool {
        let position = if self.can_zoom() {
            position.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if position == self.zoom_position {
            return false;
        }
        let half = self.viewport_side / 2.0;
        let old_scale = self.scale();
        let image_x = self.center_x + (anchor_x - half) / old_scale;
        let image_y = self.center_y + (anchor_y - half) / old_scale;
        self.zoom_position = position;
        let scale = self.scale();
        self.move_center(
            image_x - (anchor_x - half) / scale,
            image_y - (anchor_y - half) / scale,
        );
        true
    }

    /// The image follows the pointer (`dx`, `dy` are distances within the viewfinder) and stops at
    /// the edges.
    pub fn pan(&mut self, dx: f64, dy: f64) -> bool {
        let before = self.center();
        let scale = self.scale();
        self.move_center(self.center_x - dx / scale, self.center_y - dy / scale);
        before != self.center()
    }

    pub fn image_rect(&self) -> ImageRect {
        let scale = self.scale();
        let half = self.viewport_side / 2.0;
        ImageRect {
            x: half - self.center_x * scale,
            y: half - self.center_y * scale,
            width: f64::from(self.image_width) * scale,
            height: f64::from(self.image_height) * scale,
        }
    }

    /// The original-image pixel region corresponding to the viewfinder: still square after
    /// rounding, and within the original image.
    pub fn crop_rect(&self) -> AvatarCropRect {
        let shorter = self.image_width.min(self.image_height);
        let side = ((self.viewport_side / self.scale()).round_ties_even() as u32).clamp(1, shorter);
        let half = f64::from(side) / 2.0;
        let position = |center: f64, size: u32| {
            ((center - half).round_ties_even().max(0.0) as u32).min(size - side)
        };
        AvatarCropRect {
            x: position(self.center_x, self.image_width),
            y: position(self.center_y, self.image_height),
            side,
        }
    }

    fn move_center(&mut self, x: f64, y: f64) {
        let half = self.viewport_side / 2.0 / self.scale();
        self.center_x = x.clamp(half, f64::from(self.image_width) - half);
        self.center_y = y.clamp(half, f64::from(self.image_height) - half);
    }
}

/// Output side length: small images are not enlarged, large images are scaled down to
/// [`MAX_OUTPUT_SIDE`].
pub fn output_side(crop_side: u32) -> u32 {
    crop_side.clamp(1, MAX_OUTPUT_SIDE)
}

/// URL for viewing the large image: a Zulip-uploaded avatar `xxx.png` is only 100 pixels, and
/// `xxx-medium.png` in the same directory is 500 pixels. Other URLs (such as Gravatar) are returned
/// as-is.
pub fn large_url(url: &str) -> String {
    let (path, query) = url.split_at(url.find('?').unwrap_or(url.len()));
    if !ends_with_ignore_case(path, PNG) || ends_with_ignore_case(path, MEDIUM_PNG) {
        return url.to_owned();
    }
    format!("{}{MEDIUM_PNG}{query}", &path[..path.len() - PNG.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: f64 = 320.0;

    fn crop(width: u32, height: u32) -> AvatarCrop {
        AvatarCrop::new(width, height, VIEWPORT).unwrap()
    }

    fn rect(x: u32, y: u32, side: u32) -> AvatarCropRect {
        AvatarCropRect { x, y, side }
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} != {b}");
    }

    #[test]
    fn initial_crop_is_the_centered_largest_square() {
        for (w, h, x, y, side) in [
            (1000, 1000, 0, 0, 1000),
            (1000, 600, 200, 0, 600),
            (600, 1000, 0, 200, 600),
            (7, 3, 2, 0, 3),
            (1, 1, 0, 0, 1),
        ] {
            assert_eq!(crop(w, h).crop_rect(), rect(x, y, side), "{w}x{h}");
        }
    }

    #[test]
    fn initial_image_rect_covers_the_viewport() {
        let r = crop(1000, 500).image_rect();
        close(r.width, 640.0);
        close(r.height, 320.0);
        close(r.x, -160.0);
        close(r.y, 0.0);
    }

    #[test]
    fn panning_converts_viewport_distance_to_image_pixels() {
        let mut c = crop(1000, 500); // 1 viewfinder unit = 1.5625 source-image pixels
        c.pan(64.0, 0.0);
        assert_eq!(c.crop_rect(), rect(150, 0, 500));
    }

    #[test]
    fn panning_stops_at_the_image_edges() {
        for (dx, dy, expected_x) in [
            (10000.0, 0.0, 0),
            (-10000.0, 0.0, 500),
            (0.0, 10000.0, 0),
            (0.0, -10000.0, 0),
        ] {
            let mut c = crop(1000, 500);
            if dx == 0.0 {
                c.pan(10000.0, 0.0); // Horizontally move to the far left first, then confirm it cannot move vertically
            }
            c.pan(dx, dy);
            assert_eq!(c.crop_rect(), rect(expected_x, 0, 500));
        }
    }

    #[test]
    fn zoom_position_maps_to_scale_geometrically() {
        let mut c = crop(1000, 1000);
        c.set_zoom_position(0.5);
        close(c.scale(), c.min_scale() * MAX_ZOOM.sqrt());
        c.set_zoom_position(1.0);
        assert_eq!(c.crop_rect(), rect(400, 400, 200));
    }

    #[test]
    fn zoom_position_is_clamped() {
        for (value, expected) in [(-1.0, 0.0), (2.0, 1.0)] {
            let mut c = crop(1000, 1000);
            c.set_zoom_position(value);
            assert_eq!(c.zoom_position(), expected);
        }
    }

    #[test]
    fn zooming_keeps_the_anchor_point_fixed() {
        let mut c = crop(1000, 1000);
        c.set_zoom_position(0.5);
        let half = VIEWPORT / 2.0;
        let image_at =
            |c: &AvatarCrop, anchor: f64, center: f64| center + (anchor - half) / c.scale();
        let (x, y) = c.center();
        let (image_x, image_y) = (image_at(&c, 100.0, x), image_at(&c, 120.0, y));

        c.set_zoom(0.8, 100.0, 120.0);

        let (x, y) = c.center();
        close(image_at(&c, 100.0, x), image_x);
        close(image_at(&c, 120.0, y), image_y);
    }

    #[test]
    fn zooming_out_at_an_edge_pulls_the_image_back_inside() {
        let mut c = crop(1000, 1000);
        c.set_zoom_position(1.0);
        c.pan(100000.0, 100000.0);
        assert_eq!(c.crop_rect(), rect(0, 0, 200));

        c.set_zoom_position(0.0);

        assert_eq!(c.crop_rect(), rect(0, 0, 1000));
        let r = c.image_rect();
        close(r.x, 0.0);
        close(r.y, 0.0);
    }

    #[test]
    fn tiny_images_never_produce_an_empty_or_out_of_range_crop() {
        for (w, h, can_zoom) in [(1, 1, false), (4, 4, true), (1, 5000, false)] {
            let mut c = crop(w, h);
            assert_eq!(c.can_zoom(), can_zoom);
            c.set_zoom_position(1.0);
            c.pan(-100000.0, -100000.0);
            let r = c.crop_rect();
            assert!((1..=w.min(h)).contains(&r.side));
            assert!(r.x <= w - r.side);
            assert!(r.y <= h - r.side);
        }
    }

    #[test]
    fn output_side_never_upscales_and_is_capped() {
        for (side, expected) in [(1, 1), (100, 100), (512, 512), (513, 512), (4096, 512)] {
            assert_eq!(output_side(side), expected);
        }
    }

    #[test]
    fn changes_are_reported_only_when_something_changed() {
        let mut c = crop(1000, 1000);
        assert!(!c.pan(10.0, 10.0)); // When not zoomed in the image exactly fills the frame and cannot move
        assert!(!c.set_zoom_position(0.0));
        assert!(c.set_zoom_position(0.5));
        assert!(c.pan(10.0, 10.0));
    }

    #[test]
    fn rejects_empty_images() {
        assert_eq!(
            AvatarCrop::new(0, 100, VIEWPORT).err(),
            Some(InvalidCropSize)
        );
        assert_eq!(
            AvatarCrop::new(100, 0, VIEWPORT).err(),
            Some(InvalidCropSize)
        );
        assert_eq!(AvatarCrop::new(100, 100, 0.0).err(), Some(InvalidCropSize));
    }

    #[test]
    fn large_uses_medium_image() {
        for (url, expected) in [
            (
                "/user_avatars/2/abc.png?version=3",
                "/user_avatars/2/abc-medium.png?version=3",
            ),
            ("/user_avatars/2/abc.png", "/user_avatars/2/abc-medium.png"),
            (
                "https://cdn.example.com/2/abc.png?x=x&version=3",
                "https://cdn.example.com/2/abc-medium.png?x=x&version=3",
            ),
        ] {
            assert_eq!(large_url(url), expected);
        }
    }

    #[test]
    fn large_leaves_other_urls_unchanged() {
        for url in [
            "/user_avatars/2/abc-medium.png?version=3",
            "https://secure.gravatar.com/avatar/abc?d=identicon&version=1",
            "https://example.com/a?file=abc.png",
        ] {
            assert_eq!(large_url(url), url);
        }
    }
}
