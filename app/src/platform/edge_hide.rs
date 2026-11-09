//! Window edge-hide: when the window is docked to the top, left or right screen edge, it slides
//! into the edge 300ms after the mouse leaves, leaving only a strip visible, and slides back when
//! the mouse moves onto the visible part. It does not hide while dragging (left button down) or
//! while an overlay is open.

use super::system::hwnd_of;
use dioxus::desktop::tao::dpi::PhysicalPosition;
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::{DesktopContext, DesktopService, use_window, use_wry_event_handler};
use dioxus::prelude::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::mem::size_of;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
use zulip_core::window::{self as dock, PixelBox};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const HIDE_DELAY: Duration = Duration::from_millis(300);
const ANIMATION_STEP: Duration = Duration::from_millis(19);
const ANIMATION_STEPS: i32 = 8;
const POINTER_MARGIN: i32 = 8;
const REVEAL_MARGIN: i32 = 2;

thread_local! {
    static HIDDEN: RefCell<HashSet<dioxus::desktop::tao::window::WindowId>> = RefCell::default();
}

/// The window is tucked into the screen edge (even the active window shows no content then).
pub fn is_hidden_at_edge(window: &DesktopService) -> bool {
    HIDDEN.with_borrow(|h| h.contains(&window.window.id()))
}

/// Control handle for edge-hide.
#[derive(Clone, Copy)]
pub struct EdgeHide {
    shown: Signal<Option<(i32, i32)>>,
    window: CopyValue<DesktopContext>,
}

impl EdgeHide {
    /// The window's normal position: after sliding back when hidden.
    pub fn resting_position(&self) -> Option<(i32, i32)> {
        *self.shown.peek()
    }

    /// Slide back immediately.
    pub fn reveal(&self) {
        let window = self.window.read().clone();
        let mut shown = self.shown;
        if let Some((x, y)) = shown.take() {
            set_hidden(&window, false);
            window.set_outer_position(PhysicalPosition::new(x, y));
        }
    }
}

fn set_hidden(window: &DesktopService, hidden: bool) {
    let id = window.window.id();
    HIDDEN.with_borrow_mut(|h| {
        if hidden {
            h.insert(id);
        } else {
            h.remove(&id);
        }
    });
}

fn to_box(r: RECT) -> PixelBox {
    PixelBox::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// The window's visible frame and its offset relative to the window position. The window rect
/// includes invisible resize borders (about 7 pixels) on the left, right and bottom, so computing
/// from the window rect would expose exactly the invisible border; hence the DWM visible frame is
/// used.
fn visible_box(window: &DesktopService) -> Option<(PixelBox, (i32, i32))> {
    let mut rect = RECT::default();
    // SAFETY: rect is a valid output location with the correct length; hwnd belongs to a live
    // window.
    unsafe {
        DwmGetWindowAttribute(
            hwnd_of(window),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    let position = window.outer_position().ok()?;
    Some((
        to_box(rect),
        (rect.left - position.x, rect.top - position.y),
    ))
}

/// Work area and full extent of the screen the window is on.
fn monitor_boxes(window: &DesktopService) -> Option<(PixelBox, PixelBox)> {
    // SAFETY: hwnd belongs to a live window.
    let monitor = unsafe { MonitorFromWindow(hwnd_of(window), MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info is a valid output location and cbSize is set.
    unsafe { GetMonitorInfoW(monitor, &mut info) }.ok().ok()?;
    Some((to_box(info.rcWork), to_box(info.rcMonitor)))
}

/// Work area (physical pixels) of the screen the window is on.
pub fn work_area(window: &DesktopService) -> Option<PixelBox> {
    monitor_boxes(window).map(|(work, _)| work)
}

fn other_screens(window: &DesktopService, current: PixelBox) -> Vec<PixelBox> {
    window
        .available_monitors()
        .map(|m| {
            let (p, s) = (m.position(), m.size());
            PixelBox::new(p.x, p.y, s.width as i32, s.height as i32)
        })
        .filter(|b| *b != current)
        .collect()
}

fn cursor() -> Option<(i32, i32)> {
    let mut point = POINT::default();
    // SAFETY: point is a valid output location.
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some((point.x, point.y))
}

fn left_button_down() -> bool {
    // SAFETY: there are no pointer arguments.
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) as u16 & 0x8000 != 0 }
}

/// Moves to the target position in steps.
async fn animate(window: &DesktopService, target: (i32, i32)) {
    let Ok(start) = window.outer_position() else {
        return;
    };
    for step in 1..=ANIMATION_STEPS {
        let lerp = |from: i32, to: i32| from + (to - from) * step / ANIMATION_STEPS;
        window.set_outer_position(PhysicalPosition::new(
            lerp(start.x, target.0),
            lerp(start.y, target.1),
        ));
        tokio::time::sleep(ANIMATION_STEP).await;
    }
}

/// Enables edge-hide for the current window. When `enabled` is false it slides back; when `blocked`
/// returns true it does not hide (overlay open, etc.).
pub fn use_edge_hide(enabled: ReadSignal<bool>, blocked: Callback<(), bool>) -> EdgeHide {
    let window = use_window();
    let mut shown = use_signal(|| None::<(i32, i32)>);
    let handle = EdgeHide {
        shown,
        window: use_hook(|| CopyValue::new(window.clone())),
    };
    // Slide back when the window is activated via the taskbar, tray, etc.
    let mut focused = use_signal(|| false);
    use_wry_event_handler(move |event, _| {
        if let Event::WindowEvent {
            event: WindowEvent::Focused(true),
            ..
        } = event
        {
            focused.set(true);
        }
    });
    use_future(move || {
        let window = window.clone();
        async move {
            let mut outside_since = None::<Instant>;
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let visible =
                    window.is_visible() && !window.is_maximized() && !window.is_minimized();
                let hidden_position = *shown.peek();
                if !*enabled.peek() || !visible {
                    outside_since = None;
                    if let Some(target) = shown.take() {
                        set_hidden(&window, false);
                        animate(&window, target).await;
                    }
                    continue;
                }
                let (Some((px, py)), Some((vis, inset))) = (cursor(), visible_box(&window)) else {
                    continue;
                };
                if let Some(target) = hidden_position {
                    let activated = std::mem::take(&mut *focused.write());
                    if vis.contains(px, py, REVEAL_MARGIN) || activated {
                        shown.set(None);
                        set_hidden(&window, false);
                        animate(&window, target).await;
                    }
                    continue;
                }
                focused.set(false);
                // Do not hide while dragging or resizing the window; only decide whether it is
                // docked after release
                let Some((work, screen)) = monitor_boxes(&window).filter(|_| !left_button_down())
                else {
                    outside_since = None;
                    continue;
                };
                let edge = dock::detect(vis, work)
                    .filter(|e| !dock::has_neighbor(*e, screen, &other_screens(&window, screen)));
                let Some(edge) =
                    edge.filter(|_| !vis.contains(px, py, POINTER_MARGIN) && !blocked.call(()))
                else {
                    outside_since = None;
                    continue;
                };
                let since = *outside_since.get_or_insert_with(Instant::now);
                if since.elapsed() < HIDE_DELAY {
                    continue;
                }
                outside_since = None;
                // The computed position is that of the visible frame; subtract the invisible border
                // to get the window position
                let (sx, sy) = dock::shown_position(edge, vis, work);
                let (hx, hy) = dock::hidden_position(edge, vis, work);
                shown.set(Some((sx - inset.0, sy - inset.1)));
                set_hidden(&window, true);
                animate(&window, (hx - inset.0, hy - inset.1)).await;
            }
        }
    });
    handle
}
