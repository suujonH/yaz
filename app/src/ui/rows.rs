//! 复用块：列表行、名片卡片。

use super::controls::Checkbox;
use super::layout::HighlightText;
use super::menu::{MenuEntry, open_row_menu};
use super::overlay::use_overlays;
use dioxus::prelude::*;

/// 行高：`Compact` 选择器、成员列表；`Normal` 联系人、搜索结果；`Large` 会话列表。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowDensity {
    Compact,
    #[default]
    Normal,
    Large,
}

impl RowDensity {
    const fn class(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Normal => "normal",
            Self::Large => "large",
        }
    }
}

/// 所有列表的行：左侧头像或图标，标题 + 右上说明，副标题 + 右下徽标。
/// 悬停、选中、置顶的背景与右键菜单在这里统一。
#[component]
pub fn ListRow(
    #[props(default)] density: RowDensity,
    leading: Option<Element>,
    #[props(default, into)] title: String,
    /// 自定义的内容（表格的一行），代替标题和副标题。
    cells: Option<Element>,
    /// 标题中高亮的查询词（搜索结果）。
    #[props(into)]
    query: Option<String>,
    /// 右上角的说明（时间等）。
    #[props(into)]
    meta: Option<String>,
    /// 副标题之前的内容（红色提示、草稿图标）。
    subtitle_prefix: Option<Element>,
    #[props(into)] subtitle: Option<String>,
    /// 右侧或右下的内容（徽标、按钮）。
    trailing: Option<Element>,
    #[props(default)] selected: bool,
    #[props(default)] pinned: bool,
    /// 多选时的勾选状态，显示在最左侧。
    checked: Option<bool>,
    onclick: Option<EventHandler<MouseEvent>>,
    ondoubleclick: Option<EventHandler<MouseEvent>>,
    /// 右键菜单，打开时才调用。
    menu: Option<Callback<(), Vec<MenuEntry>>>,
) -> Element {
    let overlays = use_overlays();
    let mut class = format!("list-row {}", density.class());
    if selected {
        class.push_str(" selected");
    }
    if pinned {
        class.push_str(" pinned");
    }
    if onclick.is_some() {
        class.push_str(" clickable");
    }
    rsx! {
        div {
            class,
            onclick: move |e| {
                if let Some(h) = onclick {
                    h.call(e);
                }
            },
            ondoubleclick: move |e| {
                if let Some(h) = ondoubleclick {
                    h.call(e);
                }
            },
            oncontextmenu: move |e| open_row_menu(overlays, &e, menu),
            if let Some(checked) = checked {
                Checkbox { checked }
            }
            {leading}
            div { class: "list-row-text",
                if let Some(cells) = cells {
                    div { class: "list-row-line",
                        div { class: "list-row-title", {cells} }
                        {trailing.clone()}
                    }
                } else {
                div { class: "list-row-line",
                    div { class: "text-title list-row-title", HighlightText { text: title, query } }
                    if let Some(meta) = meta {
                        span { class: "text-caption list-row-meta", "{meta}" }
                    }
                    if subtitle.is_none() && subtitle_prefix.is_none() {
                        {trailing.clone()}
                    }
                }
                if subtitle.is_some() || subtitle_prefix.is_some() {
                    div { class: "list-row-line",
                        {subtitle_prefix}
                        div { class: "text-sub list-row-title", "{subtitle.clone().unwrap_or_default()}" }
                        {trailing}
                    }
                }
                }
            }
        }
    }
}

/// 名片卡片（用户、频道、话题）的容器：头像、名称、徽标、副标题，下面是资料行和操作。放进 `Popover` 使用。
#[component]
pub fn ProfileCard(
    avatar: Element,
    #[props(into)] name: String,
    badge: Option<Element>,
    subtitle: Option<Element>,
    actions: Option<Element>,
    #[props(default = 320.0)] width: f64,
    children: Element,
) -> Element {
    rsx! {
        div { class: "profile-card", style: "width:{width}px",
            div { class: "profile-head",
                {avatar}
                div { class: "profile-names",
                    div { class: "profile-name-line",
                        span { class: "profile-name", "{name}" }
                        {badge}
                    }
                    {subtitle}
                }
            }
            {children}
            if let Some(actions) = actions {
                div { class: "profile-actions", {actions} }
            }
        }
    }
}

/// 名片的资料行：左侧灰色标签，右侧内容。
#[component]
pub fn ProfileField(#[props(into)] label: String, children: Element) -> Element {
    rsx! {
        div { class: "profile-field",
            span { class: "profile-label", "{label}" }
            div { class: "profile-value selectable", {children} }
        }
    }
}
