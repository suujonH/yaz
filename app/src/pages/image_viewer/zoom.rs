//! 图片的缩放、平移、旋转。滚轮以鼠标位置为中心缩放，处于“适应窗口”时随窗口大小重新适应。

pub const MIN_SCALE: f64 = 0.1;
pub const MAX_SCALE: f64 = 10.0;
/// 每次放大或缩小当前比例的 20%。
const ZOOM_IN: f64 = 1.2;
const ZOOM_OUT: f64 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom {
    /// 图片的像素大小。
    natural: (f64, f64),
    /// 显示区域大小。
    stage: (f64, f64),
    /// 向左旋转的次数（0 到 3）。
    rotation: u8,
    scale: f64,
    /// 旋转后的图片左上角在显示区域中的位置。
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

    /// 这张图片已经按显示区域算好了位置，可以显示。
    pub fn placed(&self, natural: (f64, f64)) -> bool {
        self.natural == natural && self.stage.0 > 0.0 && self.stage.1 > 0.0
    }

    /// 旋转后的图片大小。
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

    /// 换了一张图片：回到适应窗口，旋转复位。
    pub fn set_image(&mut self, natural: (f64, f64)) {
        self.natural = natural;
        self.rotation = 0;
        self.apply_fit();
    }

    /// 显示区域大小变了：适应状态下重新适应，否则图片跟着中心移动。
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

    /// 适应窗口：完整显示并居中，小图保持原始大小。
    pub fn apply_fit(&mut self) {
        self.fit = true;
        let (w, h) = self.display();
        if w <= 0.0 || h <= 0.0 || self.stage.0 <= 0.0 || self.stage.1 <= 0.0 {
            return;
        }
        self.scale = (self.stage.0 / w).min(self.stage.1 / h).min(1.0);
        self.center();
    }

    /// 原始大小，居中。
    pub fn actual_size(&mut self) {
        self.fit = false;
        self.scale = 1.0;
        self.center();
    }

    /// 向左转 90 度，比例不变；适应状态下重新适应。
    pub fn rotate_left(&mut self) {
        self.rotation = (self.rotation + 1) % 4;
        if self.fit {
            self.apply_fit();
        } else {
            self.center();
        }
    }

    /// 以 `center`（显示区域坐标）为中心缩放：该点下的图片位置不动。
    pub fn zoom_at(&mut self, factor: f64, center: (f64, f64)) {
        if !self.has_image() {
            return;
        }
        self.fit = false;
        let old = self.scale;
        // 超大图适应窗口时比例可能已低于下限：缩小不能变成放大
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

    /// 滚轮：`notches` 为正向上滚（放大）。
    pub fn wheel(&mut self, notches: f64, at: (f64, f64)) {
        if notches != 0.0 {
            self.zoom_at(ZOOM_IN.powf(notches), at);
        }
    }

    /// 双击：适应窗口时以 `at`（显示区域坐标）为中心放大到原始大小，点击处的图片位置不动；
    /// 否则回到适应窗口。
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

    /// 图片元素的样式：原始大小，用变换缩放、旋转、定位。
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
        // 光标下的图片像素
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
        // 超大图适应后的比例低于下限时，缩小不会变大
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
        // 旋转后 1000x2000 适应 1000x800
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
        // 适应时图片在 (0,150)，比例 0.5：点击处 (300,400) 是图片像素 (600,500)
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
