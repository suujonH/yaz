//! Window-level overlays: dialogs, popup cards, context menus, notices, tooltips. Rendered at the
//! window root by [`OverlayHost`], not clipped by page `overflow`. Pages open them with
//! [`use_overlays`].

use super::controls::IconButton;
use super::geometry::{Placement, Rect, client_rect, place, viewport};
use super::icon::{Icon, IconName};
use super::menu::{MenuEntry, MenuHost, MenuState};
use dioxus::dioxus_core::{Task, spawn_forever};
use dioxus::prelude::*;
use std::rc::Rc;
use std::time::Duration;

const DIALOG_CLOSE_MS: u64 = 200;
const TOAST_MS: u64 = 3000;
const TOAST_CLOSE_MS: u64 = 180;
/// Number of notices shown at once; beyond that the earliest disappears first.
const MAX_TOASTS: usize = 5;
const TOOLTIP_DELAY_MS: u64 = 400;

type Content<H> = Rc<dyn Fn(H) -> Element>;

#[derive(Clone)]
struct DialogEntry {
    id: u64,
    dismissable: bool,
    closing: bool,
    content: Content<DialogHandle>,
}

#[derive(Clone)]
struct PopoverEntry {
    id: u64,
    anchor: Rect,
    placement: Placement,
    gap: f64,
    min_width: f64,
    content: Content<PopoverHandle>,
    on_close: Option<Rc<dyn Fn()>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastKind {
    #[default]
    Info,
    Success,
    Error,
}

#[derive(Clone, PartialEq)]
struct ToastEntry {
    id: u64,
    text: String,
    kind: ToastKind,
    closing: bool,
}

#[derive(Clone, PartialEq)]
struct TooltipEntry {
    anchor: Rect,
    text: String,
}

/// Handle of an open dialog.
#[derive(Clone, Copy, PartialEq)]
pub struct DialogHandle {
    id: u64,
    overlays: Overlays,
}

impl DialogHandle {
    /// Removed after the disappear animation plays.
    pub fn close(&self) {
        self.overlays.close_dialog(self.id);
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct PopoverHandle {
    id: u64,
    overlays: Overlays,
}

impl PopoverHandle {
    pub fn close(&self) {
        self.overlays.close_popover(self.id);
    }
}

/// Options of a popup card.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopoverOptions {
    pub placement: Placement,
    pub gap: f64,
    /// Minimum width of the card (a dropdown has the same width as its anchor).
    pub min_width: f64,
}

impl Default for PopoverOptions {
    fn default() -> Self {
        Self {
            placement: Placement::BottomStart,
            gap: 4.0,
            min_width: 0.0,
        }
    }
}

/// Entry point for overlay operations. `Copy`, usable in event handlers and async tasks.
#[derive(Clone, Copy, PartialEq)]
pub struct Overlays {
    next_id: Signal<u64>,
    dialogs: Signal<Vec<DialogEntry>, SynclessStorage>,
    popovers: Signal<Vec<PopoverEntry>, SynclessStorage>,
    toasts: Signal<Vec<ToastEntry>>,
    tooltip: Signal<Option<TooltipEntry>>,
    menu: Signal<Option<MenuState>, SynclessStorage>,
}

type SynclessStorage = UnsyncStorage;

/// The overlay entry point of the current window; must be used inside [`OverlayHost`] (included by
/// the window shell).
pub fn use_overlays() -> Overlays {
    use_context::<Overlays>()
}

impl Overlays {
    fn alloc_id(mut self) -> u64 {
        let id = *self.next_id.peek() + 1;
        self.next_id.set(id);
        id
    }

    /// Opens a modal dialog. When `dismissable` is true, Esc and clicking the backdrop close it.
    /// Put only components in `content` (do not use hooks directly).
    pub fn open_dialog(
        mut self,
        dismissable: bool,
        content: impl Fn(DialogHandle) -> Element + 'static,
    ) -> DialogHandle {
        let id = self.alloc_id();
        self.dialogs.write().push(DialogEntry {
            id,
            dismissable,
            closing: false,
            content: Rc::new(content),
        });
        DialogHandle { id, overlays: self }
    }

    fn close_dialog(mut self, id: u64) {
        {
            let mut list = self.dialogs.write();
            let Some(entry) = list.iter_mut().find(|d| d.id == id && !d.closing) else {
                return;
            };
            entry.closing = true;
        }
        spawn_forever(async move {
            tokio::time::sleep(Duration::from_millis(DIALOG_CLOSE_MS)).await;
            self.dialogs.write().retain(|d| d.id != id);
        });
    }

    /// Opens a card next to an anchor: clicking outside or Esc closes it.
    pub fn open_popover(
        mut self,
        anchor: Rect,
        options: PopoverOptions,
        content: impl Fn(PopoverHandle) -> Element + 'static,
    ) -> PopoverHandle {
        self.open_popover_inner(anchor, options, Rc::new(content), None)
    }

    /// Like [`Self::open_popover`]; `on_close` runs whenever the card closes (by the handle,
    /// clicking outside or Esc).
    pub fn open_popover_with_close(
        mut self,
        anchor: Rect,
        options: PopoverOptions,
        content: impl Fn(PopoverHandle) -> Element + 'static,
        on_close: impl Fn() + 'static,
    ) -> PopoverHandle {
        self.open_popover_inner(anchor, options, Rc::new(content), Some(Rc::new(on_close)))
    }

    pub(super) fn open_popover_inner(
        &mut self,
        anchor: Rect,
        options: PopoverOptions,
        content: Content<PopoverHandle>,
        on_close: Option<Rc<dyn Fn()>>,
    ) -> PopoverHandle {
        let id = self.alloc_id();
        self.popovers.write().push(PopoverEntry {
            id,
            anchor,
            placement: options.placement,
            gap: options.gap,
            min_width: options.min_width,
            content,
            on_close,
        });
        PopoverHandle {
            id,
            overlays: *self,
        }
    }

    fn close_popover(mut self, id: u64) {
        let mut on_close = None;
        self.popovers.write().retain(|p| {
            if p.id == id {
                on_close = p.on_close.clone();
                false
            } else {
                true
            }
        });
        if let Some(f) = on_close {
            f();
        }
    }

    /// Opens a context menu at coordinates (usually `evt.client_coordinates()`). Does not open when
    /// there are no menu items.
    pub fn show_menu(mut self, x: f64, y: f64, items: Vec<MenuEntry>) {
        if items.is_empty() {
            return;
        }
        self.menu.set(Some(MenuState {
            anchor: Rect::point(x, y),
            items: Rc::new(items),
            bar: None,
        }));
    }

    /// Opens a context menu with a separate toolbar above it. The toolbar callback's argument is
    /// used to close the menu.
    pub fn show_menu_with_bar(
        mut self,
        x: f64,
        y: f64,
        bar: impl Fn(Callback<()>) -> Element + 'static,
        items: Vec<MenuEntry>,
    ) {
        self.menu.set(Some(MenuState {
            anchor: Rect::point(x, y),
            items: Rc::new(items),
            bar: Some(Rc::new(bar)),
        }));
    }

    /// Identifies the open context menu (`None` when none is open), so a row can tell whether the
    /// menu it opened is still showing.
    pub(super) fn menu_token(&self) -> Option<usize> {
        self.menu
            .read()
            .as_ref()
            .map(|m| Rc::as_ptr(&m.items) as usize)
    }

    pub(super) fn close_menu(mut self) {
        self.menu.set(None);
    }

    pub fn toast(self, text: impl Into<String>) {
        self.toast_kind(ToastKind::Info, text);
    }

    pub fn toast_kind(mut self, kind: ToastKind, text: impl Into<String>) {
        let id = self.alloc_id();
        {
            let mut toasts = self.toasts.write();
            toasts.push(ToastEntry {
                id,
                text: text.into(),
                kind,
                closing: false,
            });
            let excess = toasts.len().saturating_sub(MAX_TOASTS);
            toasts.drain(..excess);
        }
        spawn_forever(async move {
            tokio::time::sleep(Duration::from_millis(TOAST_MS)).await;
            if let Some(t) = self.toasts.write().iter_mut().find(|t| t.id == id) {
                t.closing = true;
            }
            tokio::time::sleep(Duration::from_millis(TOAST_CLOSE_MS)).await;
            self.toasts.write().retain(|t| t.id != id);
        });
    }

    /// A menu, card or dialog is open (window edge-hide uses this to decide).
    pub fn any_open(&self) -> bool {
        self.menu.peek().is_some()
            || !self.popovers.peek().is_empty()
            || !self.dialogs.peek().is_empty()
    }

    fn escape(mut self) {
        if self.menu.peek().is_some() {
            self.menu.set(None);
            return;
        }
        let popover = self.popovers.peek().last().map(|p| p.id);
        if let Some(id) = popover {
            self.close_popover(id);
            return;
        }
        let dialog = self
            .dialogs
            .peek()
            .iter()
            .rev()
            .find(|d| !d.closing)
            .map(|d| (d.id, d.dismissable));
        if let Some((id, true)) = dialog {
            self.close_dialog(id);
        }
    }

    fn show_tooltip(mut self, entry: Option<TooltipEntry>) {
        self.tooltip.set(entry);
    }
}

/// Provides [`Overlays`] and renders overlays. Included by the window shell; no need to use it
/// manually.
#[component]
pub fn OverlayHost(children: Element) -> Element {
    let overlays = use_context_provider(|| Overlays {
        next_id: Signal::new(0),
        dialogs: Signal::new_maybe_sync(Vec::new()),
        popovers: Signal::new_maybe_sync(Vec::new()),
        toasts: Signal::new(Vec::new()),
        tooltip: Signal::new(None),
        menu: Signal::new_maybe_sync(None),
    });
    use_future(move || async move {
        let mut eval = document::eval("await window.zulipUi.onEscape((m) => dioxus.send(m));");
        while eval.recv::<String>().await.is_ok() {
            overlays.escape();
        }
    });
    let dialogs = overlays.dialogs;
    let popovers = overlays.popovers;
    let toasts = overlays.toasts;
    rsx! {
        {children}
        for entry in dialogs.read().iter().cloned() {
            DialogView { key: "{entry.id}", id: entry.id, closing: entry.closing, dismissable: entry.dismissable, overlays, render: RenderFn(entry.content.clone()) }
        }
        for entry in popovers.read().iter().cloned() {
            PopoverView { key: "{entry.id}", id: entry.id, anchor: entry.anchor, placement: entry.placement, gap: entry.gap, min_width: entry.min_width, overlays, render: RenderFn(entry.content.clone()) }
        }
        MenuHost { state: overlays.menu, on_close: move |_| overlays.close_menu() }
        TooltipView { overlays }
        if !toasts.read().is_empty() {
            div { class: "toast-layer",
                for toast in toasts.read().iter().cloned() {
                    div { key: "{toast.id}", class: if toast.closing { "toast closing {toast_class(toast.kind)}" } else { "toast {toast_class(toast.kind)}" },
                        Icon { name: toast_icon(toast.kind), size: 16.0 }
                        "{toast.text}"
                    }
                }
            }
        }
    }
}

const fn toast_class(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::Info => "info",
        ToastKind::Success => "success",
        ToastKind::Error => "error",
    }
}

const fn toast_icon(kind: ToastKind) -> IconName {
    match kind {
        ToastKind::Info => IconName::Notification,
        ToastKind::Success => IconName::CheckCircle,
        ToastKind::Error => IconName::ErrorCircle,
    }
}

/// Comparison looks only at the pointer; if the content closure is unchanged there is no re-render.
#[derive(Clone)]
struct RenderFn<H: 'static>(Content<H>);

impl<H> PartialEq for RenderFn<H> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

#[component]
fn DialogView(
    id: u64,
    closing: bool,
    dismissable: bool,
    overlays: Overlays,
    render: RenderFn<DialogHandle>,
) -> Element {
    let handle = DialogHandle { id, overlays };
    rsx! {
        div {
            class: if closing { "dialog-layer closing" } else { "dialog-layer" },
            onmousedown: move |e| {
                e.stop_propagation();
                if dismissable {
                    handle.close();
                }
            },
            oncontextmenu: move |e| e.prevent_default(),
            div {
                class: "dialog-box",
                onmousedown: move |e| e.stop_propagation(),
                {(render.0)(handle)}
            }
        }
    }
}

/// Standard dialog structure: title bar (centered title, close on the right), content, bottom
/// buttons (use `DialogButtons`). With no title only the close button remains.
#[component]
pub fn DialogFrame(
    #[props(default, into)] title: String,
    #[props(default = 360.0)] width: f64,
    on_close: Option<EventHandler<()>>,
    footer: Option<Element>,
    children: Element,
) -> Element {
    rsx! {
        div { style: "width:{width}px;display:flex;flex-direction:column;min-height:0;max-width:100%",
            div { class: if title.is_empty() { "dialog-head bare" } else { "dialog-head" },
                div { class: "dialog-title", "{title}" }
                if let Some(h) = on_close {
                    div { class: "dialog-close",
                        IconButton { icon: IconName::Close, size: 30.0, icon_size: 14.0, onclick: move |_| h.call(()) }
                    }
                }
            }
            div { class: "dialog-body scroll", {children} }
            if let Some(footer) = footer {
                div { class: "dialog-foot", {footer} }
            }
        }
    }
}

#[component]
fn PopoverView(
    id: u64,
    anchor: Rect,
    placement: Placement,
    gap: f64,
    min_width: f64,
    overlays: Overlays,
    render: RenderFn<PopoverHandle>,
) -> Element {
    let handle = PopoverHandle { id, overlays };
    let mut pos = use_signal(|| None::<(f64, f64)>);
    let style = match pos() {
        Some((x, y)) => format!("left:{x}px;top:{y}px;min-width:{min_width}px"),
        None => format!("left:0;top:0;min-width:{min_width}px"),
    };
    rsx! {
        div {
            class: "overlay-backdrop",
            onmousedown: move |e| {
                e.stop_propagation();
                handle.close();
            },
            oncontextmenu: move |e| {
                e.prevent_default();
                e.stop_propagation();
                handle.close();
            },
        }
        div {
            class: if pos().is_some() { "overlay-card scroll" } else { "overlay-card scroll unplaced" },
            style,
            onmounted: move |e: MountedEvent| async move {
                if let Some(rect) = client_rect(&e.data()).await {
                    pos.set(Some(place(anchor, (rect.w, rect.h), viewport(), placement, gap)));
                }
            },
            {(render.0)(handle)}
        }
    }
}

/// Trigger element + popup card: clicking the trigger element opens it.
#[component]
pub fn Popover(
    #[props(default)] options: PopoverOptions,
    content: Callback<PopoverHandle, Element>,
    children: Element,
) -> Element {
    let overlays = use_overlays();
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    rsx! {
        div {
            class: "popover-anchor",
            onmounted: move |e| mounted.set(Some(e.data())),
            onclick: move |_| async move {
                let el = mounted.peek().clone();
                let Some(el) = el else { return };
                if let Some(anchor) = client_rect(&el).await {
                    overlays.open_popover(anchor, options, move |h| content.call(h));
                }
            },
            {children}
        }
    }
}

#[component]
fn TooltipView(overlays: Overlays) -> Element {
    let tooltip = overlays.tooltip;
    let current = tooltip.read().clone();
    rsx! {
        if let Some(entry) = current {
            TooltipBox { key: "{entry.text}", entry }
        }
    }
}

#[component]
fn TooltipBox(entry: TooltipEntry) -> Element {
    let mut pos = use_signal(|| None::<(f64, f64)>);
    let anchor = entry.anchor;
    let style = pos().map_or_else(String::new, |(x, y)| format!("left:{x}px;top:{y}px"));
    rsx! {
        div {
            class: if pos().is_some() { "tooltip" } else { "tooltip unplaced" },
            style,
            onmounted: move |e: MountedEvent| async move {
                if let Some(rect) = client_rect(&e.data()).await {
                    pos.set(Some(place(anchor, (rect.w, rect.h), viewport(), Placement::Bottom, 6.0)));
                }
            },
            "{entry.text}"
        }
    }
}

/// Shows a text tooltip while the mouse hovers.
#[component]
pub fn Tooltip(#[props(into)] text: String, children: Element) -> Element {
    let overlays = use_overlays();
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let mut pending = use_signal(|| None::<Task>);
    use_drop(move || {
        if let Some(task) = pending.peek().as_ref() {
            task.cancel();
        }
        overlays.show_tooltip(None);
    });
    let mut text_signal = use_signal(|| text.clone());
    if *text_signal.peek() != text {
        text_signal.set(text);
    }
    rsx! {
        div {
            class: "tooltip-anchor",
            onmounted: move |e| mounted.set(Some(e.data())),
            onmouseenter: move |_| {
                let task = spawn(async move {
                    tokio::time::sleep(Duration::from_millis(TOOLTIP_DELAY_MS)).await;
                    let el = mounted.peek().clone();
                    let Some(el) = el else { return };
                    if let Some(anchor) = client_rect(&el).await {
                        overlays.show_tooltip(Some(TooltipEntry { anchor, text: text_signal.peek().clone() }));
                    }
                });
                pending.set(Some(task));
            },
            onmouseleave: move |_| {
                if let Some(task) = pending.take() {
                    task.cancel();
                }
                overlays.show_tooltip(None);
            },
            onmousedown: move |_| {
                if let Some(task) = pending.take() {
                    task.cancel();
                }
                overlays.show_tooltip(None);
            },
            {children}
        }
    }
}
