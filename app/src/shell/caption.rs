//! 标题栏：拖动区、最小化 / 最大化 / 关闭按钮，双击最大化。

use crate::ui::{Icon, IconName};
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::{use_window, use_wry_event_handler};
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// 拖动开始后 WebView 收不到 mouseup，也就没有 dblclick 事件，所以按两次按下的间隔判断双击。
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// 可拖动窗口的区域，双击切换最大化。子元素里的按钮要自行 `stop_propagation`（[`crate::ui`] 的按钮放在标题栏时用 `no-drag` 包裹）。
#[component]
pub fn DragRegion(#[props(default, into)] class: String, children: Element) -> Element {
    let window = use_window();
    let last_press = use_hook(|| Rc::new(Cell::new(None::<Instant>)));
    rsx! {
        div {
            class,
            onmousedown: move |e| {
                if e.trigger_button() != Some(MouseButton::Primary) {
                    return;
                }
                let now = Instant::now();
                if last_press.get().is_some_and(|t| now - t < DOUBLE_CLICK) {
                    last_press.set(None);
                    if window.is_resizable() {
                        window.toggle_maximized();
                    }
                } else {
                    last_press.set(Some(now));
                    window.drag();
                }
            },
            {children}
        }
    }
}

/// 窗口外壳已包含。
#[component]
pub fn CaptionBar(
    #[props(default, into)] title: String,
    #[props(default = true)] resizable: bool,
    on_close: Option<EventHandler<()>>,
    left: Option<Element>,
    right: Option<Element>,
) -> Element {
    let window = use_window();
    let mut maximized = use_signal(|| window.is_maximized());
    {
        let window = window.clone();
        use_wry_event_handler(move |event, _| {
            if let Event::WindowEvent {
                event: WindowEvent::Resized(_),
                window_id,
                ..
            } = event
                && *window_id == window.id()
            {
                let now = window.is_maximized();
                if *maximized.peek() != now {
                    maximized.set(now);
                }
            }
        });
    }
    let minimize = window.clone();
    let toggle = window.clone();
    let close = window.clone();
    rsx! {
        div { class: "caption-bar",
            DragRegion { class: "caption-drag",
                {left}
                if !title.is_empty() {
                    span { "{title}" }
                }
            }
            div { class: "caption-buttons",
                {right}
                button {
                    class: "caption-btn",
                    r#type: "button",
                    tabindex: "-1",
                    onclick: move |_| minimize.set_minimized(true),
                    Icon { name: IconName::Minus, size: 14.0 }
                }
                if resizable {
                    button {
                        class: "caption-btn",
                        r#type: "button",
                        tabindex: "-1",
                        onclick: move |_| toggle.toggle_maximized(),
                        if maximized() {
                            Icon { name: IconName::Restore, size: 12.0 }
                        } else {
                            Icon { name: IconName::Rectangle, size: 11.0 }
                        }
                    }
                }
                button {
                    class: "caption-btn close",
                    r#type: "button",
                    tabindex: "-1",
                    onclick: move |_| match on_close {
                        Some(handler) => handler.call(()),
                        None => close.close(),
                    },
                    Icon { name: IconName::Close, size: 14.0 }
                }
            }
        }
    }
}
