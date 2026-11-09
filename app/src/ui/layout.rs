//! 展开 / 滑出容器、设置卡片、文件卡片、高亮文字等小组件。

use super::icon::{Icon, IconName};
use super::menu::{MenuEntry, open_row_menu};
use super::overlay::use_overlays;
use dioxus::prelude::*;
use zulip_core::text::highlight;

/// 展开 / 收起容器：高度过渡，收起后内容隐藏（不能获得焦点）。
#[component]
pub fn ExpandPanel(open: bool, children: Element) -> Element {
    rsx! {
        div { class: if open { "expand open" } else { "expand" },
            div { {children} }
        }
    }
}

/// 带标题的折叠区。
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

/// 底部滑出栏（多选操作栏）：打开时立即占据高度并滑入，关闭时滑出并收缩。
#[component]
pub fn SlideBar(open: bool, children: Element) -> Element {
    rsx! {
        div { class: if open { "slide-bar open" } else { "slide-bar" },
            div { {children} }
        }
    }
}

/// 圆形揭示：打开时内容从左下角圆形展开，关闭时立即隐藏。
#[component]
pub fn CircleReveal(open: bool, children: Element) -> Element {
    rsx! {
        div { class: if open { "circle-reveal open" } else { "circle-reveal" }, {children} }
    }
}

/// 沿圆角矩形描边一圈的动画（贴表情），盖在相对定位的父元素上。`play` 变为 true 时播放。
#[component]
pub fn TraceBorder(play: bool, #[props(default = 8.0)] radius: f64) -> Element {
    rsx! {
        svg { class: if play { "trace-border play" } else { "trace-border" },
            rect { width: "100%", height: "100%", rx: "{radius}", ry: "{radius}", path_length: "1" }
        }
    }
}

/// 设置页的白色圆角卡片。
#[component]
pub fn SettingCard(children: Element) -> Element {
    rsx! {
        div { class: "card", {children} }
    }
}

/// 设置项：左边文字、右边控件。
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

/// 聊天中的文件卡片：文件名、类型图标。
#[component]
pub fn FileCard(
    #[props(into)] name: String,
    #[props(default = IconName::File)] icon: IconName,
    /// 文件大小等说明。
    #[props(into)]
    detail: Option<String>,
    /// 上传或下载进度（0 到 1），结束后传 `None`。
    progress: Option<f64>,
    onclick: Option<EventHandler<MouseEvent>>,
    /// 右键菜单，打开时才调用。
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
            oncontextmenu: move |e| open_row_menu(overlays, &e, menu),
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

/// 标出查询词命中部分的文字。
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
