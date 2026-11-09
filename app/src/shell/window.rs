//! Window config, multiple windows (open / activate singletons), window position saving.

use super::state::AppShared;
use super::{bundled, local_files};
use crate::state::AccountScope;
use dioxus::desktop::tao::dpi::{LogicalPosition, PhysicalPosition};
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::tao::platform::windows::{IconExtWindows, WindowBuilderExtWindows};
use dioxus::desktop::tao::window::{Icon, Window};
use dioxus::desktop::{
    Config, DesktopService, LogicalSize, WeakDesktopContext, WindowBuilder, WindowCloseBehaviour,
    use_wry_event_handler, window,
};
use dioxus::dioxus_core::{Task, spawn_forever};
use dioxus::prelude::*;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use zulip_core::window::WindowBounds;

const THEME_CSS: &str = include_str!("../../assets/theme.css");
const UI_CSS: &str = include_str!("../../assets/ui.css");
const UI_JS: &str = include_str!("../../assets/ui.js");
/// After a window move or resize, wait this long before saving the position.
const SAVE_BOUNDS_DELAY: Duration = Duration::from_millis(500);

/// Where window positions are stored; implemented by the settings layer.
pub trait WindowBoundsStore: Send + Sync {
    fn load(&self, key: &str) -> Option<WindowBounds>;
    fn save(&self, key: &str, bounds: WindowBounds);
}

/// Creation parameters of a window.
#[derive(Clone, Debug)]
pub struct WindowSpec {
    /// Singleton key.
    pub key: String,
    /// Key under which the window position is saved; windows of the same kind (chat windows) can
    /// share it.
    pub bounds_key: String,
    pub title: String,
    pub size: (f64, f64),
    pub min_size: (f64, f64),
    pub resizable: bool,
    /// Only hide on close (the main window, together with the tray).
    pub hide_on_close: bool,
    pub remember_bounds: bool,
    /// Small notification window: always on top, not in the taskbar, and does not steal focus when
    /// opened or updated.
    pub popup: bool,
    /// Background color before the window content loads; by default the theme's background for the
    /// system's light/dark mode.
    pub background: Option<(u8, u8, u8)>,
    /// Hidden first, shown by [`WindowShell`](super::WindowShell) after the content mounts, so
    /// opening does not flash. Small notification windows and the main window decide by themselves
    /// when to show; false for them.
    pub show_when_ready: bool,
    /// When created, it is centered on the work area of the screen of the window that opened it. On
    /// by default; off for the main window and popups, which place themselves.
    pub centered: bool,
    /// Restores the saved position when it is still on some screen (possibly another one than the
    /// opener's); otherwise `centered` applies. Off by default: only the size is remembered.
    pub restore_position: bool,
}

impl WindowSpec {
    pub fn new(key: impl Into<String>, title: impl Into<String>, size: (f64, f64)) -> Self {
        let key = key.into();
        Self {
            bounds_key: key.clone(),
            key,
            title: title.into(),
            size,
            min_size: (320.0, 240.0),
            resizable: true,
            hide_on_close: false,
            remember_bounds: true,
            popup: false,
            background: None,
            show_when_ready: true,
            centered: true,
            restore_position: false,
        }
    }

    pub fn min_size(mut self, size: (f64, f64)) -> Self {
        self.min_size = size;
        self
    }

    /// Window with a fixed color scheme: background matches the content.
    pub fn fixed_background(mut self, color: (u8, u8, u8)) -> Self {
        self.background = Some(color);
        self
    }

    /// Opens where it was last closed (also on another screen); centered the first time or when
    /// that place is no longer on any screen.
    pub fn keep_position(mut self) -> Self {
        self.restore_position = true;
        self
    }

    pub fn bounds_key(mut self, key: impl Into<String>) -> Self {
        self.bounds_key = key.into();
        self
    }

    /// Fixed-size window: cannot be resized and the size is not remembered.
    pub fn fixed(mut self) -> Self {
        self.resizable = false;
        self.min_size = self.size;
        self.remember_bounds = false;
        self
    }

    pub fn popup(mut self) -> Self {
        self.popup = true;
        self.show_when_ready = false;
        self.centered = false;
        self
    }

    pub fn hide_on_close(mut self) -> Self {
        self.hide_on_close = true;
        self
    }
}

pub fn app_icon() -> Option<Icon> {
    Icon::from_resource(crate::platform::ICON_RESOURCE_ID, None)
        .inspect_err(|e| log::warn!("加载窗口图标失败：{e}"))
        .ok()
}

fn index_html() -> String {
    let pages = crate::pages::PAGES_CSS;
    let developer = crate::pages::DEVELOPER_CSS;
    let katex = bundled::KATEX_CSS;
    format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>Zulip</title>\
         <style>{katex}\n{THEME_CSS}\n{UI_CSS}\n{pages}\n{developer}</style><script>{UI_JS}</script></head>\
         <body><div id=\"main\"></div></body></html>"
    )
}

/// `%LOCALAPPDATA%`. The environment variable takes precedence: tests use it to put data in a temp
/// directory.
pub fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
}

/// Not placed in the data directory: during a data directory migration the migration window's
/// WebView is using these files.
fn webview_data_dir() -> PathBuf {
    local_app_data().join("YaZ.WebView2")
}

/// Whether at least part of the window lies within some screen.
fn on_screen(window: &Window, x: i32, y: i32) -> bool {
    window.available_monitors().any(|m| {
        let (p, s) = (m.position(), m.size());
        x + 64 > p.x && x < p.x + s.width as i32 && y >= p.y - 8 && y < p.y + s.height as i32
    })
}

/// Position of a window of `size` (logical pixels) when centered on the work area of the screen of
/// the current window.
fn center_on_caller(size: (f64, f64)) -> Option<PhysicalPosition<i32>> {
    let caller = window();
    let area = crate::platform::work_area(&caller)?;
    let scale = caller.scale_factor();
    Some(PhysicalPosition::new(
        area.x + ((f64::from(area.width) - size.0 * scale) / 2.0).round() as i32,
        area.y + ((f64::from(area.height) - size.1 * scale) / 2.0).round() as i32,
    ))
}

/// Gives an existing hidden window (the spare window) the title, size and position a new window of
/// `spec` would get from [`window_config`].
pub(super) fn place_window(window: &DesktopService, spec: &WindowSpec, shared: &AppShared) {
    let saved = spec
        .remember_bounds
        .then(|| shared.bounds.load(&spec.bounds_key))
        .flatten();
    let size = saved.map_or(spec.size, |b| (b.width, b.height));
    window.set_title(&spec.title);
    window.set_resizable(spec.resizable);
    window.set_min_inner_size(Some(LogicalSize::new(spec.min_size.0, spec.min_size.1)));
    let restored = saved
        .filter(|b| spec.restore_position && on_screen(&window.window, b.x, b.y))
        .map(|b| PhysicalPosition::new(b.x, b.y));
    let position = restored.or_else(|| spec.centered.then(|| center_on_caller(size)).flatten());
    // Move first: on a screen with another scale the logical size is applied with that scale
    if let Some(at) = position {
        window.set_outer_position(at);
    }
    window.set_inner_size(LogicalSize::new(size.0, size.1));
}

/// Config of a borderless window. All windows are created from here, keeping theme styles, icon and
/// data directory consistent.
pub fn window_config(spec: &WindowSpec, shared: &AppShared) -> Config {
    let saved = spec
        .remember_bounds
        .then(|| shared.bounds.load(&spec.bounds_key))
        .flatten();
    let size = saved.map_or(spec.size, |b| (b.width, b.height));
    let mut builder = WindowBuilder::new()
        .with_title(&spec.title)
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .with_resizable(spec.resizable)
        .with_inner_size(LogicalSize::new(size.0, size.1))
        .with_min_inner_size(LogicalSize::new(spec.min_size.0, spec.min_size.1))
        .with_window_icon(app_icon())
        .with_visible(!spec.show_when_ready)
        .with_always_on_top(spec.popup)
        .with_skip_taskbar(spec.popup)
        // A window created hidden gets the focus when it is shown (`activate`); taking it now would
        // pull it away from the window the user is typing in
        .with_focused(!spec.popup && !spec.show_when_ready);
    if spec.centered
        && let Some(at) = center_on_caller(size)
    {
        builder = builder.with_position(at);
    }
    let dark_system = shared.starts_dark();
    let background = match spec.background {
        Some((r, g, b)) => (r, g, b, 255),
        None if dark_system => (0x1F, 0x1F, 0x1F, 255),
        None => (0xEB, 0xEB, 0xEB, 255),
    };
    let close = if spec.hide_on_close {
        WindowCloseBehaviour::WindowHides
    } else {
        WindowCloseBehaviour::WindowCloses
    };
    let mut config = Config::new()
        .with_window(builder)
        .with_custom_index(index_html())
        .with_background_color(background)
        .with_disable_context_menu(false)
        .with_close_behaviour(close)
        .with_tray_icon_show_window_on_click(false)
        .with_exits_when_last_window_closes(true)
        .with_data_directory(webview_data_dir())
        .with_asynchronous_custom_protocol(local_files::SCHEME, |_, request, responder| {
            local_files::handle(request, responder);
        })
        .with_asynchronous_custom_protocol(bundled::SCHEME, |_, request, responder| {
            bundled::handle(request, responder);
        });
    if let Some(bounds) = saved.filter(|_| spec.restore_position) {
        config = config.with_on_window(move |window: Arc<Window>, _| {
            if on_screen(&window, bounds.x, bounds.y) {
                window.set_outer_position(PhysicalPosition::new(bounds.x, bounds.y));
            }
        });
    }
    config
}

/// The window is hidden at creation and shown by [`WindowShell`](super::WindowShell) after the
/// content mounts.
#[derive(Clone, Copy)]
pub struct ShowWhenReady(pub bool);

/// Windows that are open (or being created).
enum WindowSlot {
    Pending,
    Open(WeakDesktopContext),
}

struct OpenWindow {
    slot: WindowSlot,
    /// The `watch::Sender<T>` of the window parameters.
    args: Option<Box<dyn Any>>,
}

thread_local! {
    // All VirtualDoms are on the main thread
    static WINDOWS: RefCell<HashMap<String, OpenWindow>> = RefCell::default();
}

/// Shows and activates the window.
pub fn activate(window: &dioxus::desktop::DesktopService) {
    window.set_visible(true);
    window.set_minimized(false);
    window.set_focus();
}

/// Records a window opened outside [`create_window`] (the spare window once shown).
pub(super) fn register_window(key: String, window: WeakDesktopContext, args: Box<dyn Any>) {
    WINDOWS.with_borrow_mut(|w| {
        w.insert(
            key,
            OpenWindow {
                slot: WindowSlot::Open(window),
                args: Some(args),
            },
        );
    });
}

/// Whether the window `key` is open or being created.
fn is_open(key: &str) -> bool {
    WINDOWS.with_borrow(|w| {
        w.get(key).is_some_and(|entry| match &entry.slot {
            WindowSlot::Pending => true,
            WindowSlot::Open(weak) => weak.upgrade().is_some(),
        })
    })
}

/// When the window is already open, hands it the new parameters and activates it where it is.
fn reuse_window<T: 'static>(key: &str, args: &mut Option<T>, activate_window: bool) -> bool {
    WINDOWS.with_borrow_mut(|windows| {
        let Some(entry) = windows.get(key) else {
            return false;
        };
        let live = match &entry.slot {
            WindowSlot::Pending => None,
            WindowSlot::Open(weak) => match weak.upgrade() {
                Some(window) => Some(window),
                None => {
                    windows.remove(key);
                    return false;
                }
            },
        };
        if let (Some(args), Some(tx)) = (
            args.take(),
            entry
                .args
                .as_ref()
                .and_then(|tx| tx.downcast_ref::<watch::Sender<T>>()),
        ) {
            tx.send_replace(args);
        }
        if let Some(window) = live.filter(|_| activate_window) {
            activate(&window);
        }
        true
    })
}

fn create_window(
    spec: WindowSpec,
    root: fn() -> Element,
    args: Option<Box<dyn Any>>,
    setup: impl FnOnce(VirtualDom) -> VirtualDom,
) {
    let shared = consume_context::<AppShared>();
    let dom = setup(
        VirtualDom::new(root)
            .with_root_context(shared.clone())
            .with_root_context(shared.theme.clone())
            .with_root_context(WindowKey {
                bounds_key: spec.bounds_key.clone(),
                remember: spec.remember_bounds,
            })
            .with_root_context(ShowWhenReady(spec.show_when_ready)),
    );
    let pending = window().new_window(dom, window_config(&spec, &shared));
    let key = spec.key;
    WINDOWS.with_borrow_mut(|w| {
        w.insert(
            key.clone(),
            OpenWindow {
                slot: WindowSlot::Pending,
                args,
            },
        );
    });
    spawn_forever(async move {
        match pending.try_resolve().await {
            Ok(window) => WINDOWS.with_borrow_mut(|w| {
                if let Some(entry) = w.get_mut(&key) {
                    entry.slot = WindowSlot::Open(Rc::downgrade(&window));
                }
            }),
            Err(_) => WINDOWS.with_borrow_mut(|w| {
                w.remove(&key);
            }),
        }
    });
}

/// Closes the window `key` when it is open.
pub fn close_window(key: &str) {
    let entry = WINDOWS.with_borrow_mut(|w| w.remove(key));
    if let Some(OpenWindow {
        slot: WindowSlot::Open(weak),
        ..
    }) = entry
        && let Some(window) = weak.upgrade()
    {
        window.close();
    }
}

/// Opens a singleton window: when already open, activates it and returns false.
///
/// `root` is the window's root component, whose content is usually wrapped in
/// [`WindowShell`](super::WindowShell); `setup` passes parameters with
/// `VirtualDom::with_root_context`.
pub fn open_singleton(
    spec: WindowSpec,
    root: fn() -> Element,
    setup: impl FnOnce(VirtualDom) -> VirtualDom,
) -> bool {
    if reuse_window::<()>(&spec.key, &mut None, !spec.popup) {
        return false;
    }
    create_window(spec, root, None, setup);
    true
}

/// Context of the window parameters, used with [`use_window_args`].
pub(super) struct WindowArgs<T>(pub(super) watch::Sender<T>);

impl<T> Clone for WindowArgs<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// Opens a singleton window with parameters: when already open, updates the parameters and
/// activates it; read them inside the window with [`use_window_args`]. `account` is the window's
/// [`AccountScope`]. Uses the spare window when there is one (see [`prepare_with_args`]).
pub fn open_with_args<T: Clone + PartialEq + 'static>(
    spec: WindowSpec,
    root: fn() -> Element,
    args: T,
    account: Option<i64>,
) {
    let mut args = Some(args);
    if reuse_window(&spec.key, &mut args, !spec.popup) {
        return;
    }
    let Some(args) = args else { return };
    if !spec.popup && super::spare::show(&spec, root, &args, account) {
        return;
    }
    let tx = watch::Sender::new(args);
    let context = WindowArgs(tx.clone());
    create_window(spec, root, Some(Box::new(tx)), move |dom| {
        let dom = dom.with_root_context(context);
        match account {
            Some(account) => dom.with_root_context(AccountScope(account)),
            None => dom,
        }
    });
}

/// Renders the window [`open_with_args`] would open in the hidden spare window, so that opening it
/// only has to show it. Nothing happens when it is already open.
pub fn prepare_with_args<T: Clone + PartialEq + 'static>(
    spec: WindowSpec,
    root: fn() -> Element,
    args: T,
    account: Option<i64>,
) {
    if spec.popup || is_open(&spec.key) {
        return;
    }
    super::spare::prepare(&spec, root, &args, account);
}

/// The window's parameters; updated when it is opened again.
pub fn use_window_args<T: Clone + 'static>() -> Signal<T> {
    let args = use_context::<WindowArgs<T>>();
    let mut value = use_signal(|| args.0.borrow().clone());
    use_future(move || {
        let mut rx = args.0.subscribe();
        async move {
            while rx.changed().await.is_ok() {
                let next = rx.borrow_and_update().clone();
                value.set(next);
            }
        }
    });
    value
}

/// Closes all child windows. Their components are destroyed, so the cleanup in `use_drop` (saving
/// drafts) runs.
pub fn close_all_windows() {
    super::spare::close_spare();
    let windows = WINDOWS.with_borrow_mut(std::mem::take);
    for entry in windows.into_values() {
        if let WindowSlot::Open(weak) = entry.slot
            && let Some(window) = weak.upgrade()
        {
            window.close();
        }
    }
}

/// Position-save key of the current window. The main window gets it from `main`; other windows get
/// it from the function that opens the window.
#[derive(Clone, Debug)]
pub struct WindowKey {
    pub bounds_key: String,
    pub remember: bool,
}

/// Saves the position after a window move or resize. Skipped while `paused` returns true (when
/// tucked into the screen edge).
pub fn use_save_window_bounds() {
    let key = use_context::<WindowKey>();
    let shared = use_context::<AppShared>();
    let mut pending = use_signal(|| None::<Task>);
    let this_window = use_hook(window);
    use_wry_event_handler(move |event, _| {
        if !key.remember {
            return;
        }
        let Event::WindowEvent {
            event: WindowEvent::Moved(_) | WindowEvent::Resized(_),
            window_id,
            ..
        } = event
        else {
            return;
        };
        if *window_id != this_window.id() {
            return;
        }
        if let Some(task) = pending.take() {
            task.cancel();
        }
        let shared = shared.clone();
        let window = this_window.clone();
        let bounds_key = key.bounds_key.clone();
        let task = spawn(async move {
            tokio::time::sleep(SAVE_BOUNDS_DELAY).await;
            // Hidden: the spare window being placed for a page, not the user moving it
            if !window.is_visible()
                || window.is_maximized()
                || window.is_minimized()
                || super::edge_hidden(&window)
            {
                return;
            }
            let Ok(position) = window.outer_position() else {
                return;
            };
            let size: LogicalSize<f64> = window.inner_size().to_logical(window.scale_factor());
            shared.bounds.save(
                &bounds_key,
                WindowBounds {
                    x: position.x,
                    y: position.y,
                    width: size.width,
                    height: size.height,
                },
            );
        });
        pending.set(Some(task));
    });
}

/// Moves the window to logical coordinates (used by the preview page for verification).
pub fn move_window(x: f64, y: f64) {
    window().set_outer_position(LogicalPosition::new(x, y));
}
