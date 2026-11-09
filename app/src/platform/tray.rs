//! System tray: icon, menu, left click, flashing.

use super::ICON_RESOURCE_ID;
use dioxus::desktop::tao::dpi::PhysicalPosition;
use dioxus::desktop::trayicon::menu::{Menu, MenuItem, PredefinedMenuItem};
use dioxus::desktop::trayicon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use dioxus::desktop::{use_muda_event_handler, use_tray_icon_event_handler};
use dioxus::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

const FLASH_INTERVAL: Duration = Duration::from_millis(500);

/// Tray menu item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayItem {
    /// Menu event ID, text.
    Action(&'static str, &'static str),
    Separator,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrayEvent {
    /// Left click (also stops flashing).
    LeftClick,
    /// Mouse hover while flashing; the argument is the top-left corner of the icon on screen
    /// (physical pixels).
    Hover(PhysicalPosition<f64>),
    /// Flashing stopped.
    FlashingStopped,
    Menu(String),
}

struct TrayInner {
    icon: TrayIcon,
    app_icon: Option<Icon>,
    blank_icon: Option<Icon>,
    tooltip: RefCell<String>,
    flashing: Cell<bool>,
    showing_blank: Cell<bool>,
}

/// Handle of the tray icon; destroyed with the component that created it.
#[derive(Clone)]
pub struct Tray {
    inner: Rc<TrayInner>,
    on_event: Callback<TrayEvent>,
}

impl PartialEq for Tray {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Tray {
    pub fn set_tooltip(&self, text: &str) {
        self.inner.tooltip.replace(text.to_string());
        if !self.inner.flashing.get() {
            let _ = self.inner.icon.set_tooltip(Some(text));
        }
    }

    pub fn set_visible(&self, visible: bool) {
        if !visible {
            self.stop_flashing();
        }
        let _ = self.inner.icon.set_visible(visible);
    }

    pub fn is_flashing(&self) -> bool {
        self.inner.flashing.get()
    }

    /// Alternates with a transparent icon. While flashing, hovering shows no tooltip and instead
    /// emits [`TrayEvent::Hover`].
    pub fn start_flashing(&self) {
        if !self.inner.flashing.replace(true) {
            let _ = self.inner.icon.set_tooltip(None::<&str>);
        }
    }

    pub fn stop_flashing(&self) {
        if self.inner.flashing.replace(false) {
            self.inner.showing_blank.set(false);
            let _ = self.inner.icon.set_icon(self.inner.app_icon.clone());
            let _ = self
                .inner
                .icon
                .set_tooltip(Some(self.inner.tooltip.borrow().as_str()));
            self.on_event.call(TrayEvent::FlashingStopped);
        }
    }

    fn flash_tick(&self) {
        if self.inner.flashing.get() {
            let blank = !self.inner.showing_blank.get();
            self.inner.showing_blank.set(blank);
            let icon = if blank {
                &self.inner.blank_icon
            } else {
                &self.inner.app_icon
            };
            let _ = self.inner.icon.set_icon(icon.clone());
        }
    }
}

fn build_menu(items: &[TrayItem]) -> Menu {
    let menu = Menu::new();
    for item in items {
        let appended = match item {
            TrayItem::Action(id, label) => menu.append(&MenuItem::with_id(*id, *label, true, None)),
            TrayItem::Separator => menu.append(&PredefinedMenuItem::separator()),
        };
        if let Err(e) = appended {
            log::warn!("添加托盘菜单项失败：{e}");
        }
    }
    menu
}

/// Creates the tray icon (one per program, called in the main window's component). [`None`] if
/// creation fails.
pub fn use_tray(
    tooltip: &str,
    items: &[TrayItem],
    on_event: impl FnMut(TrayEvent) + 'static,
) -> Option<Tray> {
    let on_event = use_callback(on_event);
    let tray = use_hook(|| {
        let app_icon = Icon::from_resource(ICON_RESOURCE_ID, Some((32, 32)))
            .inspect_err(|e| log::warn!("加载托盘图标失败：{e}"))
            .ok();
        let mut builder = TrayIconBuilder::new()
            .with_tooltip(tooltip)
            .with_menu(Box::new(build_menu(items)))
            .with_menu_on_left_click(false);
        if let Some(icon) = app_icon.clone() {
            builder = builder.with_icon(icon);
        }
        let built = builder.build();
        match built {
            Ok(icon) => Some(Tray {
                inner: Rc::new(TrayInner {
                    icon,
                    app_icon,
                    blank_icon: Icon::from_rgba(vec![0; 32 * 32 * 4], 32, 32).ok(),
                    tooltip: RefCell::new(tooltip.to_string()),
                    flashing: Cell::new(false),
                    showing_blank: Cell::new(false),
                }),
                on_event,
            }),
            Err(e) => {
                log::warn!("创建托盘图标失败：{e}");
                None
            }
        }
    });

    let for_events = tray.clone();
    use_tray_icon_event_handler(move |event| match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => {
            if let Some(tray) = &for_events {
                tray.stop_flashing();
            }
            on_event.call(TrayEvent::LeftClick);
        }
        TrayIconEvent::Enter { rect, .. } if for_events.as_ref().is_some_and(Tray::is_flashing) => {
            on_event.call(TrayEvent::Hover(rect.position));
        }
        _ => {}
    });
    // Only the first registered global handler for menu events takes effect; the framework
    // registers muda first, so tray menu events arrive through it too.
    use_muda_event_handler(move |event| {
        on_event.call(TrayEvent::Menu(event.id.0.clone()));
    });

    let for_flash = tray.clone();
    use_future(move || {
        let tray = for_flash.clone();
        async move {
            let Some(tray) = tray else { return };
            loop {
                tokio::time::sleep(FLASH_INTERVAL).await;
                tray.flash_tick();
            }
        }
    });
    tray
}
