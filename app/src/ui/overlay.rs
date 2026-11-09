//! 窗口级弹层：对话框、弹出卡片、右键菜单、提示、工具提示。由 [`OverlayHost`] 渲染在窗口根部，
//! 不受页面 `overflow` 裁剪。页面用 [`use_overlays`] 打开。

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
/// 同时显示的提示条数，超过时最早的先消失。
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

/// 打开中的对话框的句柄。
#[derive(Clone, Copy, PartialEq)]
pub struct DialogHandle {
    id: u64,
    overlays: Overlays,
}

impl DialogHandle {
    /// 播放消失动画后移除。
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

/// 弹出卡片的选项。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopoverOptions {
    pub placement: Placement,
    pub gap: f64,
    /// 卡片的最小宽度（下拉框与锚点同宽）。
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

/// 弹层操作入口。`Copy`，可在事件处理和异步任务中使用。
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

/// 当前窗口的弹层入口；必须在 [`OverlayHost`] 内使用（窗口外壳已包含）。
pub fn use_overlays() -> Overlays {
    use_context::<Overlays>()
}

impl Overlays {
    fn alloc_id(mut self) -> u64 {
        let id = *self.next_id.peek() + 1;
        self.next_id.set(id);
        id
    }

    /// 打开模态对话框。`dismissable` 为 true 时 Esc 和点击遮罩关闭。
    /// `content` 里只放组件（不要直接用 hook）。
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

    /// 在锚点旁打开卡片：点击外部或 Esc 关闭。
    pub fn open_popover(
        mut self,
        anchor: Rect,
        options: PopoverOptions,
        content: impl Fn(PopoverHandle) -> Element + 'static,
    ) -> PopoverHandle {
        self.open_popover_inner(anchor, options, Rc::new(content), None)
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

    /// 在坐标处打开右键菜单（通常取 `evt.client_coordinates()`）。没有菜单项时不打开。
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

    /// 打开右键菜单，上方带一张独立的工具条。工具条回调的参数用来关闭菜单。
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

    /// 有菜单、卡片或对话框打开（窗口贴边隐藏据此判断）。
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

/// 提供 [`Overlays`] 并渲染弹层。窗口外壳已包含，不需要手动使用。
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

/// 比较时只看指针，内容闭包不变就不重新渲染。
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

/// 对话框的标准结构：标题栏（居中标题、右侧关闭）、内容、底部按钮（用 `DialogButtons`）。没有标题时只留关闭按钮。
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

/// 触发元素 + 弹出卡片：点击触发元素打开。
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

/// 鼠标停留时显示文字提示。
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
