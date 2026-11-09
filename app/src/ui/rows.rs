//! Reusable blocks: list rows, profile cards.

use super::controls::Checkbox;
use super::layout::HighlightText;
use super::menu::{MenuEntry, open_row_menu};
use super::overlay::use_overlays;
use dioxus::prelude::*;

/// Row height: `Compact` for pickers and member lists; `Normal` for contacts and search results;
/// `Large` for the conversation list.
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

/// The row of every list: avatar or icon on the left, title + note at the top right, subtitle +
/// badge at the bottom right. Hover, selected and pinned backgrounds and the context menu are
/// unified here.
#[component]
pub fn ListRow(
    #[props(default)] density: RowDensity,
    leading: Option<Element>,
    #[props(default, into)] title: String,
    /// Custom content (a table row) that replaces the title and subtitle.
    cells: Option<Element>,
    /// Query words highlighted in the title (search results).
    #[props(into)]
    query: Option<String>,
    /// Note at the top right (time, etc.).
    #[props(into)]
    meta: Option<String>,
    /// Content before the subtitle (red hint, draft icon).
    subtitle_prefix: Option<Element>,
    #[props(into)] subtitle: Option<String>,
    /// Content on the right or bottom right (badge, button).
    trailing: Option<Element>,
    #[props(default)] selected: bool,
    #[props(default)] pinned: bool,
    /// Checked state when multi-selecting, shown at the far left.
    checked: Option<bool>,
    onclick: Option<EventHandler<MouseEvent>>,
    ondoubleclick: Option<EventHandler<MouseEvent>>,
    /// The pointer entered (true) or left (false) the row.
    onhover: Option<EventHandler<bool>>,
    /// Context menu, called only when opened.
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
    // Pressed with any button (`:active` covers only the left one), and the context menu this row
    // opened is showing: lists color these states
    let mut pressed = use_signal(|| false);
    let mut opened_menu = use_signal(|| None::<usize>);
    if pressed() {
        class.push_str(" pressed");
    }
    if opened_menu().is_some() && opened_menu() == overlays.menu_token() {
        class.push_str(" menu-open");
    }
    rsx! {
        div {
            class,
            onmousedown: move |_| pressed.set(true),
            onmouseup: move |_| pressed.set(false),
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
            oncontextmenu: move |e| {
                pressed.set(false);
                opened_menu.set(open_row_menu(overlays, &e, menu));
            },
            onmouseenter: move |_| {
                if let Some(h) = onhover {
                    h.call(true);
                }
            },
            onmouseleave: move |_| {
                pressed.set(false);
                if let Some(h) = onhover {
                    h.call(false);
                }
            },
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

/// Container of a profile card (user, channel, topic): avatar, name, badge, subtitle, then profile
/// rows and actions below. Used inside a `Popover`.
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

/// A profile row of a card: gray label on the left, content on the right. With `onclick` the
/// whole row is clickable (an editable field).
#[component]
pub fn ProfileField(
    #[props(into)] label: String,
    onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    rsx! {
        div {
            class: if onclick.is_some() { "profile-field clickable" } else { "profile-field" },
            onclick: move |e| {
                if let Some(h) = onclick {
                    h.call(e);
                }
            },
            span { class: "profile-label", "{label}" }
            div { class: "profile-value selectable", {children} }
        }
    }
}
