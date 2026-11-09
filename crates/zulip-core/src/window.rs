//! 窗口位置的保存，以及边缘自动隐藏的计算（贴住屏幕上、左、右边缘的窗口在鼠标离开后缩进边缘，只露出一条）。

use std::fmt;

/// 隐藏后露出的宽度（像素）。
pub const VISIBLE_STRIP: i32 = 4;

/// 主窗口停靠的屏幕边缘。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DockEdge {
    Top,
    Left,
    Right,
}

/// 屏幕像素矩形。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PixelBox {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl PixelBox {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    /// 点在矩形内（各边向外扩 `margin` 像素）。
    pub const fn contains(&self, x: i32, y: i32, margin: i32) -> bool {
        x >= self.x - margin
            && x < self.right() + margin
            && y >= self.y - margin
            && y < self.bottom() + margin
    }
}

/// 窗口贴住的边缘：先判断上边，再判断左边、右边。
pub fn detect(window: PixelBox, work: PixelBox) -> Option<DockEdge> {
    if window.y <= work.y {
        Some(DockEdge::Top)
    } else if window.x <= work.x {
        Some(DockEdge::Left)
    } else if window.right() >= work.right() {
        Some(DockEdge::Right)
    } else {
        None
    }
}

/// 屏幕的这条边外侧紧挨着另一块屏幕。这样的边不能隐藏：缩进去的部分会显示在相邻的屏幕上。
/// `screen` 与 `others` 为屏幕的完整范围。
pub fn has_neighbor(edge: DockEdge, screen: PixelBox, others: &[PixelBox]) -> bool {
    others.iter().any(|o| {
        let overlaps_vertically = o.y < screen.bottom() && o.bottom() > screen.y;
        match edge {
            DockEdge::Top => o.bottom() == screen.y && o.x < screen.right() && o.right() > screen.x,
            DockEdge::Left => o.right() == screen.x && overlaps_vertically,
            DockEdge::Right => o.x == screen.right() && overlaps_vertically,
        }
    })
}

/// 停靠时的显示位置：把窗口贴齐边缘（拖出屏幕外的部分拉回来）。
pub fn shown_position(edge: DockEdge, window: PixelBox, work: PixelBox) -> (i32, i32) {
    match edge {
        DockEdge::Top => (window.x, work.y),
        DockEdge::Left => (work.x, window.y),
        DockEdge::Right => (work.right() - window.width, window.y),
    }
}

/// 隐藏后的位置：窗口移到边缘外，只露出 [`VISIBLE_STRIP`] 像素。
pub fn hidden_position(edge: DockEdge, window: PixelBox, work: PixelBox) -> (i32, i32) {
    match edge {
        DockEdge::Top => (window.x, work.y - window.height + VISIBLE_STRIP),
        DockEdge::Left => (work.x - window.width + VISIBLE_STRIP, window.y),
        DockEdge::Right => (work.right() - VISIBLE_STRIP, window.y),
    }
}

/// 窗口位置与大小：位置为屏幕像素，大小为逻辑像素。保存为 `x,y,w,h`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowBounds {
    pub x: i32,
    pub y: i32,
    pub width: f64,
    pub height: f64,
}

impl WindowBounds {
    /// 解析 [`Display`](fmt::Display) 的结果；格式不对或宽高不为正时为 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.split(',');
        let x = parts.next()?.parse().ok()?;
        let y = parts.next()?.parse().ok()?;
        let width: f64 = parts.next()?.parse().ok()?;
        let height: f64 = parts.next()?.parse().ok()?;
        let valid = |v: f64| v > 0.0 && v.is_finite();
        (parts.next().is_none() && valid(width) && valid(height)).then_some(Self {
            x,
            y,
            width,
            height,
        })
    }
}

impl fmt::Display for WindowBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},{},{}", self.x, self.y, self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: PixelBox = PixelBox::new(0, 0, 1920, 1040);

    #[test]
    fn detects_edge() {
        for (x, y, expected) in [
            (100, 0, Some(DockEdge::Top)),
            (100, -20, Some(DockEdge::Top)),
            (0, 0, Some(DockEdge::Top)),
            (0, 100, Some(DockEdge::Left)),
            (-30, 100, Some(DockEdge::Left)),
            (1546, 100, Some(DockEdge::Right)),
            (1600, 100, Some(DockEdge::Right)),
            (100, 100, None),
        ] {
            assert_eq!(
                detect(PixelBox::new(x, y, 374, 700), WORK),
                expected,
                "{x},{y}"
            );
        }
    }

    #[test]
    fn detects_on_secondary_screen() {
        let work = PixelBox::new(-1280, 0, 1280, 984);
        assert_eq!(
            detect(PixelBox::new(-1280, 100, 374, 700), work),
            Some(DockEdge::Left)
        );
        assert_eq!(
            detect(PixelBox::new(-374, 100, 374, 700), work),
            Some(DockEdge::Right)
        );
        assert_eq!(detect(PixelBox::new(-1000, 100, 374, 700), work), None);
    }

    #[test]
    fn positions() {
        let w = PixelBox::new(100, -20, 374, 700);
        assert_eq!(shown_position(DockEdge::Top, w, WORK), (100, 0));
        assert_eq!(hidden_position(DockEdge::Top, w, WORK), (100, -696));
        assert_eq!(hidden_position(DockEdge::Left, w, WORK), (-370, -20));
        assert_eq!(hidden_position(DockEdge::Right, w, WORK), (1916, -20));
        assert_eq!(shown_position(DockEdge::Right, w, WORK), (1546, -20));
    }

    #[test]
    fn contains_with_margin() {
        let b = PixelBox::new(10, 10, 100, 100);
        assert!(b.contains(109, 109, 0));
        assert!(!b.contains(110, 50, 0));
        assert!(b.contains(115, 50, 8));
        assert!(!b.contains(2, 50, 7));
    }

    #[test]
    fn neighbors() {
        let left = PixelBox::new(0, 0, 1920, 1080);
        let right = PixelBox::new(1920, 200, 1280, 1024);
        let above = PixelBox::new(0, -1080, 1920, 1080);
        assert!(has_neighbor(DockEdge::Right, left, &[right]));
        assert!(has_neighbor(DockEdge::Left, right, &[left]));
        assert!(!has_neighbor(DockEdge::Left, left, &[right]));
        assert!(has_neighbor(DockEdge::Top, left, &[above, right]));
        assert!(!has_neighbor(DockEdge::Top, right, &[above, left]));
        // 只在角上相接（没有重叠的一段边）不算相邻
        assert!(!has_neighbor(
            DockEdge::Right,
            left,
            &[PixelBox::new(1920, 1080, 800, 600)]
        ));
        assert!(!has_neighbor(DockEdge::Right, left, &[]));
    }

    #[test]
    fn bounds_round_trip() {
        let b = WindowBounds {
            x: -1200,
            y: 35,
            width: 374.5,
            height: 694.0,
        };
        assert_eq!(b.to_string(), "-1200,35,374.5,694");
        assert_eq!(WindowBounds::parse(&b.to_string()), Some(b));
    }

    #[test]
    fn bounds_parse_invalid() {
        for s in [
            "",
            "1,2,3",
            "a,2,3,4",
            "1,2,0,4",
            "1,2,3,-4",
            "1,2,NaN,4",
            "1,2,inf,4",
            "1,2,3,4,5",
        ] {
            assert_eq!(WindowBounds::parse(s), None, "{s}");
        }
    }
}
