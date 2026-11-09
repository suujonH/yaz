//! Image zoom, pan and rotation. The wheel zooms around the mouse position, and in "fit window"
//! mode it re-fits as the window size changes.

pub const MIN_SCALE: f64 = 0.1;
pub const MAX_SCALE: f64 = 10.0;
/// Each zoom in or out changes the current ratio by 20%.
const ZOOM_IN: f64 = 1.2;
const ZOOM_OUT: f64 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom {
    /// Pixel size of the image.
    natural: (f64, f64),
    /// Size of the display area.
    stage: (f64, f64),
    /// Number of left rotations (0 to 3).
    rotation: u8,
    scale: f64,
    /// Position of the rotated image's top-left corner in the display area.
    offset: (f64, f64),
    fit: bool,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            natural: (0.0, 0.0),
            stage: (0.0, 0.0),
            rotation: 0,
            scale: 1.0,
            offset: (0.0, 0.0),
            fit: true,
        }
    }
}

impl Zoom {
    pub fn has_image(&self) -> bool {
        self.natural.0 > 0.0 && self.natural.1 > 0.0
    }

    /// This image's position has been computed against the display area and it can be shown.
    pub fn placed(&self, natural: (f64, f64)) -> bool {
        self.natural == natural && self.stage.0 > 0.0 && self.stage.1 > 0.0
    }

    /// Size of the rotated image.
    fn display(&self) -> (f64, f64) {
        if self.rotation.is_multiple_of(2) {
            self.natural
        } else {
            (self.natural.1, self.natural.0)
        }
    }

    pub fn percent(&self) -> u32 {
        (self.scale * 100.0).round() as u32
    }

    /// A different image: back to fit window, rotation reset.
    pub fn set_image(&mut self, natural: (f64, f64)) {
        self.natural = natural;
        self.rotation = 0;
        self.apply_fit();
    }

    /// Display area size changed: re-fit if in fit state, otherwise the image moves with the
    /// center.
    pub fn set_stage(&mut self, stage: (f64, f64)) {
        let old = self.stage;
        self.stage = stage;
        if self.fit {
            self.apply_fit();
        } else {
            self.offset.0 += (stage.0 - old.0) / 2.0;
            self.offset.1 += (stage.1 - old.1) / 2.0;
        }
    }

    /// Fit window: show completely and centered, small images keep their original size.
    pub fn apply_fit(&mut self) {
        self.fit = true;
        let (w, h) = self.display();
        if w <= 0.0 || h <= 0.0 || self.stage.0 <= 0.0 || self.stage.1 <= 0.0 {
            return;
        }
        self.scale = (self.stage.0 / w).min(self.stage.1 / h).min(1.0);
        self.center();
    }

    /// Original size, centered.
    pub fn actual_size(&mut self) {
        self.fit = false;
        self.scale = 1.0;
        self.center();
    }

    /// Rotate left 90 degrees, ratio unchanged; re-fit if in fit state.
    pub fn rotate_left(&mut self) {
        self.rotation = (self.rotation + 1) % 4;
        if self.fit {
            self.apply_fit();
        } else {
            self.center();
        }
    }

    /// Zoom around `center` (display area coordinates): the image position under that point does
    /// not move.
    pub fn zoom_at(&mut self, factor: f64, center: (f64, f64)) {
        if !self.has_image() {
            return;
        }
        self.fit = false;
        let old = self.scale;
        // For very large images fit to the window the ratio may already be below the lower limit:
        // zooming out must not turn into zooming in
        self.scale = (old * factor).clamp(MIN_SCALE.min(old), MAX_SCALE.max(old));
        let ratio = self.scale / old;
        self.offset = (
            center.0 - (center.0 - self.offset.0) * ratio,
            center.1 - (center.1 - self.offset.1) * ratio,
        );
    }

    fn stage_center(&self) -> (f64, f64) {
        (self.stage.0 / 2.0, self.stage.1 / 2.0)
    }

    pub fn zoom_in(&mut self) {
        self.zoom_at(ZOOM_IN, self.stage_center());
    }

    pub fn zoom_out(&mut self) {
        self.zoom_at(ZOOM_OUT, self.stage_center());
    }

    /// Wheel: `notches` positive means scroll up (zoom in).
    pub fn wheel(&mut self, notches: f64, at: (f64, f64)) {
        if notches != 0.0 {
            self.zoom_at(ZOOM_IN.powf(notches), at);
        }
    }

    /// Double click: in fit state, zoom in to the original size around `at` (display area
    /// coordinates) with the image position at the click unchanged; otherwise go back to fit
    /// window.
    pub fn toggle_fit_at(&mut self, at: (f64, f64)) {
        if self.fit && self.has_image() {
            self.zoom_at(1.0 / self.scale, at);
        } else {
            self.apply_fit();
        }
    }

    pub fn offset(&self) -> (f64, f64) {
        self.offset
    }

    pub fn set_offset(&mut self, offset: (f64, f64)) {
        self.fit = false;
        self.offset = offset;
    }

    fn center(&mut self) {
        let (w, h) = self.display();
        self.offset = (
            (self.stage.0 - w * self.scale) / 2.0,
            (self.stage.1 - h * self.scale) / 2.0,
        );
    }

    /// Style of the image element: original size, with a transform for zoom, rotation and
    /// positioning.
    pub fn style(&self) -> String {
        let (w, h) = self.display();
        let center = (
            self.offset.0 + w * self.scale / 2.0,
            self.offset.1 + h * self.scale / 2.0,
        );
        format!(
            "width:{}px;height:{}px;transform:translate({}px,{}px) rotate({}deg) scale({}) translate({}px,{}px)",
            self.natural.0,
            self.natural.1,
            center.0,
            center.1,
            -90 * i32::from(self.rotation),
            self.scale,
            -self.natural.0 / 2.0,
            -self.natural.1 / 2.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zoom(natural: (f64, f64), stage: (f64, f64)) -> Zoom {
        let mut zoom = Zoom::default();
        zoom.set_stage(stage);
        zoom.set_image(natural);
        zoom
    }

    #[test]
    fn large_images_fit_and_small_ones_keep_their_size() {
        let big = zoom((2000.0, 1000.0), (1000.0, 800.0));
        assert!((big.scale - 0.5).abs() < 1e-9);
        assert_eq!(big.offset(), (0.0, 150.0));
        assert_eq!(big.percent(), 50);
        let small = zoom((100.0, 100.0), (1000.0, 800.0));
        assert!((small.scale - 1.0).abs() < 1e-9);
        assert_eq!(small.offset(), (450.0, 350.0));
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let mut z = zoom((2000.0, 1000.0), (1000.0, 800.0));
        let point = (300.0, 400.0);
        // Image pixel under the cursor
        let before = (
            (point.0 - z.offset().0) / z.scale,
            (point.1 - z.offset().1) / z.scale,
        );
        z.zoom_at(2.0, point);
        let after = (
            (point.0 - z.offset().0) / z.scale,
            (point.1 - z.offset().1) / z.scale,
        );
        assert!((before.0 - after.0).abs() < 1e-9);
        assert!((before.1 - after.1).abs() < 1e-9);
    }

    #[test]
    fn scale_is_limited_but_never_inverted() {
        let mut z = zoom((100.0, 100.0), (1000.0, 800.0));
        for _ in 0..50 {
            z.zoom_in();
        }
        assert!((z.scale - MAX_SCALE).abs() < 1e-9);
        for _ in 0..100 {
            z.zoom_out();
        }
        assert!((z.scale - MIN_SCALE).abs() < 1e-9);
        // When the fit ratio of a very large image is below the lower limit, zooming out does not
        // enlarge it
        let mut huge = zoom((100_000.0, 100_000.0), (1000.0, 800.0));
        let fitted = huge.scale;
        assert!(fitted < MIN_SCALE);
        huge.zoom_out();
        assert!(huge.scale <= fitted);
    }

    #[test]
    fn resizing_refits_only_in_fit_mode() {
        let mut z = zoom((2000.0, 1000.0), (1000.0, 800.0));
        z.set_stage((500.0, 400.0));
        assert!((z.scale - 0.25).abs() < 1e-9);
        z.zoom_in();
        let offset = z.offset();
        z.set_stage((600.0, 500.0));
        assert_eq!(z.offset(), (offset.0 + 50.0, offset.1 + 50.0));
    }

    #[test]
    fn rotation_swaps_the_displayed_size() {
        let mut z = zoom((2000.0, 1000.0), (1000.0, 800.0));
        z.rotate_left();
        // After rotation, 1000x2000 fits 1000x800
        assert!((z.scale - 0.4).abs() < 1e-9);
        z.rotate_left();
        z.rotate_left();
        z.rotate_left();
        assert!((z.scale - 0.5).abs() < 1e-9);
        assert!(z.style().contains("rotate(0deg)"));
    }

    #[test]
    fn double_click_toggles_between_fit_and_actual_size() {
        let mut z = zoom((2000.0, 1000.0), (1000.0, 800.0));
        // When fit, the image is at (0,150) with ratio 0.5: the click point (300,400) is image
        // pixel (600,500)
        z.toggle_fit_at((300.0, 400.0));
        assert!((z.scale - 1.0).abs() < 1e-9);
        assert_eq!(z.offset(), (-300.0, -100.0));
        z.toggle_fit_at((300.0, 400.0));
        assert_eq!(z.percent(), 50);
        assert_eq!(z.offset(), (0.0, 150.0));
    }

    #[test]
    fn wheel_zooms_by_notches() {
        let mut z = zoom((100.0, 100.0), (1000.0, 800.0));
        z.wheel(2.0, (500.0, 400.0));
        assert!((z.scale - 1.44).abs() < 1e-9);
        z.wheel(0.0, (0.0, 0.0));
        assert!((z.scale - 1.44).abs() < 1e-9);
    }
}
