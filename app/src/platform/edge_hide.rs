//! 窗口贴边隐藏：窗口贴住屏幕上、左、右边缘时，鼠标离开 300ms 后缩进边缘只露出一条，鼠标移到露出的部分时移回。
//! 拖动中（左键按下）、有弹层时不隐藏。

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

/// 窗口缩进在屏幕边缘（此时即使是活动窗口也看不到内容）。
pub fn is_hidden_at_edge(window: &DesktopService) -> bool {
    HIDDEN.with_borrow(|h| h.contains(&window.window.id()))
}

/// 贴边隐藏的控制句柄。
#[derive(Clone, Copy)]
pub struct EdgeHide {
    shown: Signal<Option<(i32, i32)>>,
    window: CopyValue<DesktopContext>,
}

impl EdgeHide {
    /// 窗口的正常位置：隐藏时为移回后的位置。
    pub fn resting_position(&self) -> Option<(i32, i32)> {
        *self.shown.peek()
    }

    /// 立即移回。
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

/// 窗口的可见边框及它相对窗口位置的偏移。窗口矩形包含左、右、下的不可见缩放边框（约 7 像素），
/// 按窗口矩形计算时露出的正好是不可见边框，所以取 DWM 的可见边框。
fn visible_box(window: &DesktopService) -> Option<(PixelBox, (i32, i32))> {
    let mut rect = RECT::default();
    // SAFETY: rect 是有效的输出位置，长度正确；hwnd 属于存活的窗口。
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

/// 窗口所在屏幕的工作区与完整范围。
fn monitor_boxes(window: &DesktopService) -> Option<(PixelBox, PixelBox)> {
    // SAFETY: hwnd 属于存活的窗口。
    let monitor = unsafe { MonitorFromWindow(hwnd_of(window), MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info 是有效的输出位置，cbSize 已设置。
    unsafe { GetMonitorInfoW(monitor, &mut info) }.ok().ok()?;
    Some((to_box(info.rcWork), to_box(info.rcMonitor)))
}

/// 窗口所在屏幕的工作区（物理像素）。
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
    // SAFETY: point 是有效的输出位置。
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some((point.x, point.y))
}

fn left_button_down() -> bool {
    // SAFETY: 没有指针参数。
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) as u16 & 0x8000 != 0 }
}

/// 分步移动到目标位置。
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

/// 启用当前窗口的贴边隐藏。`enabled` 为 false 时移回；`blocked` 返回 true 时不隐藏（有弹层等）。
pub fn use_edge_hide(enabled: ReadSignal<bool>, blocked: Callback<(), bool>) -> EdgeHide {
    let window = use_window();
    let mut shown = use_signal(|| None::<(i32, i32)>);
    let handle = EdgeHide {
        shown,
        window: use_hook(|| CopyValue::new(window.clone())),
    };
    // 通过任务栏、托盘等激活窗口时移回
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
                // 拖动窗口或调整大小时不隐藏；松开后才判断是否贴边
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
                // 计算得到的是可见边框的位置，窗口位置要减去不可见边框
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
