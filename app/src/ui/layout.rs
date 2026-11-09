//! Small components: expand / slide-out containers, settings cards, file cards, highlighted text,
//! etc.

use super::icon::{Icon, IconName};
use super::menu::{MenuEntry, open_row_menu};
use super::overlay::use_overlays;
use dioxus::prelude::*;
use zulip_core::text::highlight;

/// Expand / collapse container: height transition; content is hidden after collapsing (cannot take
/// focus).
#[component]
pub fn ExpandPanel(open: bool, children: Element) -> Element {
    rsx! {
        div { class: if open { "expand open" } else { "expand" },
            div { {children} }
        }
    }
}

/// Collapsible section with a title.
#[component]
pub fn Expander(
    #[props(into)] title: String,
    #[props(default)] initially_open: bool,
    children: Element,
) -> Element {
    let mut open = use_signal(|| initially_open);
    rsx! {
        button {
            class: if open() { "expander-head open" } else { "expander-head" },
            r#type: "button",
            aria_expanded: "{open}",
            onclick: move |_| open.toggle(),
            Icon { name: IconName::ChevronRight, size: 14.0, class: "chevron" }
            "{title}"
        }
        ExpandPanel { open: open(), {children} }
    }
}

/// Bottom slide-out bar (multi-select action bar): takes up height and slides in immediately on
/// open; slides out and shrinks on close.
#[component]
pub fn SlideBar(open: bool, children: Element) -> Element {
    rsx! {
        div { class: if open { "slide-bar open" } else { "slide-bar" },
            div { {children} }
        }
    }
}

/// Circular reveal: on open the content expands in a circle from the bottom left; on close it hides
/// immediately.
#[component]
pub fn CircleReveal(open: bool, children: Element) -> Element {
    rsx! {
        div { class: if open { "circle-reveal open" } else { "circle-reveal" }, {children} }
    }
}

/// Animation tracing a rounded rectangle outline (sticking an emoji), overlaid on a
/// relatively-positioned parent. Plays when `play` becomes true.
#[component]
pub fn TraceBorder(play: bool, #[props(default = 8.0)] radius: f64) -> Element {
    rsx! {
        svg { class: if play { "trace-border play" } else { "trace-border" },
            rect { width: "100%", height: "100%", rx: "{radius}", ry: "{radius}", path_length: "1" }
        }
    }
}

/// White rounded card of a settings page.
#[component]
pub fn SettingCard(children: Element) -> Element {
    rsx! {
        div { class: "card", {children} }
    }
}

/// Setting item: text on the left, control on the right.
#[component]
pub fn SettingRow(
    #[props(into)] label: String,
    #[props(into)] description: Option<String>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "setting-row",
            div { class: "setting-text",
                div { class: "setting-label", "{label}" }
                if let Some(text) = description {
                    div { class: "setting-desc", "{text}" }
                }
            }
            {children}
        }
    }
}

/// File card in chat: file name, type icon.
#[component]
pub fn FileCard(
    #[props(into)] name: String,
    #[props(default = IconName::File)] icon: IconName,
    /// Description such as file size.
    #[props(into)]
    detail: Option<String>,
    /// Upload or download progress (0 to 1); pass `None` when finished.
    progress: Option<f64>,
    onclick: Option<EventHandler<MouseEvent>>,
    /// Context menu, called only when opened.
    menu: Option<Callback<(), Vec<MenuEntry>>>,
) -> Element {
    let overlays = use_overlays();
    rsx! {
        div {
            class: "file-card",
            onclick: move |e| {
                if let Some(h) = onclick {
                    h.call(e);
                }
            },
            oncontextmenu: move |e| {
                open_row_menu(overlays, &e, menu);
            },
            div { class: "file-card-text",
                span { class: "file-card-name", "{name}" }
                if let Some(detail) = detail {
                    span { class: "text-caption", "{detail}" }
                }
                if let Some(progress) = progress {
                    div { class: "progress",
                        div { class: "progress-bar", style: "width:{progress.clamp(0.0, 1.0) * 100.0}%" }
                    }
                }
            }
            Icon { name: icon, size: 32.0 }
        }
    }
}

/// Text with the parts matching the query highlighted.
#[component]
pub fn HighlightText(#[props(into)] text: String, #[props(into)] query: Option<String>) -> Element {
    let parts: Vec<(String, bool)> = highlight::split(&text, query.as_deref())
        .into_iter()
        .map(|(s, hit)| (s.to_string(), hit))
        .collect();
    rsx! {
        for (part, hit) in parts {
            if hit {
                mark { class: "hit", "{part}" }
            } else {
                "{part}"
            }
        }
    }
}
