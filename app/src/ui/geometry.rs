//! Overlay positioning. Coordinates are CSS pixels within the window.

use dioxus::desktop::{LogicalSize, window};
use dioxus::prelude::*;

const SCREEN_MARGIN: f64 = 8.0;
/// Distance a menu with a toolbar is shifted down relative to the pointer.
const MENU_BAR_OFFSET: f64 = 8.0;
/// Gap between the toolbar and the menu.
const MENU_BAR_GAP: f64 = 18.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn point(x: f64, y: f64) -> Self {
        Self {
            x,
            y,
            w: 0.0,
            h: 0.0,
        }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Position of an overlay relative to its anchor. When it does not fit it flips to the opposite
/// side, then is clamped inside the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    /// Below the anchor, left-aligned.
    #[default]
    BottomStart,
    /// Below the anchor, centered.
    Bottom,
    /// Above the anchor, centered.
    Top,
    /// To the right of the anchor, top-aligned.
    RightStart,
}

/// Computes the top-left corner of an overlay. `gap` is the distance between the overlay and the
/// anchor.
pub fn place(
    anchor: Rect,
    size: (f64, f64),
    view: (f64, f64),
    placement: Placement,
    gap: f64,
) -> (f64, f64) {
    let (w, h) = size;
    let (view_w, view_h) = view;
    let below = anchor.bottom() + gap;
    let above = anchor.y - gap - h;
    let vertical = |prefer_below: bool| {
        let fits_below = below + h <= view_h - SCREEN_MARGIN;
        let fits_above = above >= SCREEN_MARGIN;
        let use_below = if prefer_below {
            fits_below || !fits_above
        } else {
            fits_below && !fits_above
        };
        if use_below { below } else { above }
    };
    let centered_x = anchor.x + (anchor.w - w) / 2.0;
    let (x, y) = match placement {
        Placement::BottomStart => (anchor.x, vertical(true)),
        Placement::Bottom => (centered_x, vertical(true)),
        Placement::Top => (centered_x, vertical(false)),
        Placement::RightStart => {
            let right = anchor.right() + gap;
            let x = if right + w <= view_w - SCREEN_MARGIN {
                right
            } else {
                anchor.x - gap - w
            };
            (x, anchor.y)
        }
    };
    (clamp(x, w, view_w), clamp(y, h, view_h))
}

/// Computes the top-left corners of the context menu and the toolbar above it: the menu is a little
/// lower than the pointer, and the toolbar is left-aligned with the menu; when the toolbar does not
/// fit above, both are shifted down together.
pub fn place_menu_with_bar(
    anchor: Rect,
    menu: (f64, f64),
    bar: (f64, f64),
    view: (f64, f64),
) -> ((f64, f64), (f64, f64)) {
    let below_pointer = Rect::point(anchor.x, anchor.y + MENU_BAR_OFFSET);
    let (x, mut menu_y) = place(below_pointer, menu, view, Placement::BottomStart, 0.0);
    let mut bar_y = menu_y - MENU_BAR_GAP - bar.1;
    if bar_y < SCREEN_MARGIN {
        menu_y += SCREEN_MARGIN - bar_y;
        bar_y = SCREEN_MARGIN;
    }
    ((x, menu_y), (clamp(x, bar.0, view.0), bar_y))
}

fn clamp(pos: f64, size: f64, view: f64) -> f64 {
    pos.min(view - size - SCREEN_MARGIN).max(SCREEN_MARGIN)
}

/// Logical size of the current window's content area.
pub fn viewport() -> (f64, f64) {
    let w = window();
    let size: LogicalSize<f64> = w.inner_size().to_logical(w.scale_factor());
    (size.width, size.height)
}

/// The element's rectangle relative to the window; `None` when the element has been unmounted.
pub async fn client_rect(el: &MountedData) -> Option<Rect> {
    let r = el.get_client_rect().await.ok()?;
    Some(Rect {
        x: r.origin.x,
        y: r.origin.y,
        w: r.size.width,
        h: r.size.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f64, f64) = (800.0, 600.0);

    #[test]
    fn opens_below_anchor() {
        let anchor = Rect {
            x: 100.0,
            y: 100.0,
            w: 50.0,
            h: 20.0,
        };
        assert_eq!(
            place(anchor, (200.0, 100.0), VIEW, Placement::BottomStart, 4.0),
            (100.0, 124.0)
        );
    }

    #[test]
    fn flips_above_when_no_room_below() {
        let anchor = Rect {
            x: 100.0,
            y: 560.0,
            w: 50.0,
            h: 20.0,
        };
        assert_eq!(
            place(anchor, (200.0, 100.0), VIEW, Placement::BottomStart, 4.0),
            (100.0, 456.0)
        );
    }

    #[test]
    fn top_flips_below_when_no_room_above() {
        let anchor = Rect {
            x: 100.0,
            y: 10.0,
            w: 40.0,
            h: 20.0,
        };
        assert_eq!(
            place(anchor, (40.0, 30.0), VIEW, Placement::Top, 4.0),
            (100.0, 34.0)
        );
    }

    #[test]
    fn keeps_inside_view() {
        let (x, y) = place(
            Rect::point(790.0, 590.0),
            (200.0, 100.0),
            VIEW,
            Placement::BottomStart,
            0.0,
        );
        assert_eq!((x, y), (592.0, 490.0));
    }

    #[test]
    fn bar_sits_above_menu() {
        let (menu, bar) = place_menu_with_bar(
            Rect::point(100.0, 200.0),
            (120.0, 200.0),
            (300.0, 42.0),
            VIEW,
        );
        assert_eq!(menu, (100.0, 208.0));
        assert_eq!(bar, (100.0, 148.0));
    }

    #[test]
    fn menu_and_bar_move_down_near_the_top() {
        let (menu, bar) = place_menu_with_bar(
            Rect::point(100.0, 10.0),
            (120.0, 200.0),
            (300.0, 42.0),
            VIEW,
        );
        assert_eq!(bar, (100.0, 8.0));
        assert_eq!(menu, (100.0, 68.0));
    }

    #[test]
    fn menu_flips_above_pointer_with_bar_on_top() {
        let (menu, bar) = place_menu_with_bar(
            Rect::point(100.0, 580.0),
            (120.0, 200.0),
            (300.0, 42.0),
            VIEW,
        );
        assert_eq!(menu, (100.0, 388.0));
        assert_eq!(bar, (100.0, 328.0));
    }

    #[test]
    fn bar_stays_inside_view_horizontally() {
        let (menu, bar) = place_menu_with_bar(
            Rect::point(700.0, 200.0),
            (80.0, 200.0),
            (300.0, 42.0),
            VIEW,
        );
        assert_eq!(menu.0, 700.0);
        assert_eq!(bar.0, 492.0);
    }

    #[test]
    fn submenu_flips_left() {
        let row = Rect {
            x: 650.0,
            y: 100.0,
            w: 120.0,
            h: 30.0,
        };
        assert_eq!(
            place(row, (150.0, 100.0), VIEW, Placement::RightStart, 5.0),
            (495.0, 100.0)
        );
    }
}
