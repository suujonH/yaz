//! Simple controls: button, checkbox, switch, slider, tabs, badge.

use super::icon::{Icon, IconName};
use dioxus::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    #[default]
    Secondary,
    Danger,
    Flat,
}

impl ButtonKind {
    const fn class(self) -> &'static str {
        match self {
            Self::Primary => "btn btn-primary",
            Self::Secondary => "btn btn-secondary",
            Self::Danger => "btn btn-danger",
            Self::Flat => "btn btn-flat",
        }
    }
}

#[component]
pub fn Button(
    #[props(default)] kind: ButtonKind,
    #[props(default)] disabled: bool,
    icon: Option<IconName>,
    #[props(default, into)] class: String,
    onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    rsx! {
        button {
            class: "{kind.class()} {class}",
            r#type: "button",
            disabled,
            onclick: move |e| {
                if let Some(h) = onclick {
                    h.call(e);
                }
            },
            if let Some(name) = icon {
                Icon { name, size: 14.0 }
            }
            {children}
        }
    }
}

/// Flat button with only an icon.
#[component]
pub fn IconButton(
    icon: IconName,
    #[props(default = 28.0)] size: f64,
    #[props(default = 16.0)] icon_size: f64,
    #[props(default)] disabled: bool,
    #[props(default, into)] class: String,
    /// Image address, sometimes shown instead of `icon` (a plugin's icon file, etc.).
    #[props(default)]
    image: Option<String>,
    onclick: Option<EventHandler<MouseEvent>>,
) -> Element {
    rsx! {
        button {
            class: "icon-btn {class}",
            r#type: "button",
            style: "width:{size}px;height:{size}px",
            disabled,
            onclick: move |e| {
                if let Some(h) = onclick {
                    h.call(e);
                }
            },
            if let Some(src) = image {
                img { src, width: "{icon_size}", height: "{icon_size}", alt: "" }
            } else {
                Icon { name: icon, size: icon_size }
            }
        }
    }
}

/// Checkbox (`radio` gives the radio style).
#[component]
pub fn Checkbox(
    checked: bool,
    #[props(default)] disabled: bool,
    #[props(default)] radio: bool,
    onchange: Option<EventHandler<bool>>,
    children: Element,
) -> Element {
    let state = if checked { " checked" } else { "" };
    let dim = if disabled { " disabled" } else { "" };
    rsx! {
        button {
            class: "check{state}{dim}",
            r#type: "button",
            role: if radio { "radio" } else { "checkbox" },
            aria_checked: "{checked}",
            onclick: move |_| {
                if !disabled
                    && let Some(h) = onchange {
                        h.call(!checked);
                    }
            },
            span { class: if radio { "check-box radio" } else { "check-box" },
                if checked && !radio {
                    svg { class: "menu-check", view_box: "0 0 12 8", path { d: "M0.5,4 L4,7.5 L11.5,0.5" } }
                }
            }
            {children}
        }
    }
}

#[component]
pub fn Toggle(
    checked: bool,
    #[props(default)] disabled: bool,
    onchange: Option<EventHandler<bool>>,
    children: Element,
) -> Element {
    let state = if checked { " checked" } else { "" };
    let dim = if disabled { " disabled" } else { "" };
    rsx! {
        button {
            class: "toggle{state}{dim}",
            r#type: "button",
            role: "switch",
            aria_checked: "{checked}",
            onclick: move |_| {
                if !disabled
                    && let Some(h) = onchange {
                        h.call(!checked);
                    }
            },
            span { class: "toggle-track", span { class: "toggle-thumb" } }
            {children}
        }
    }
}

#[component]
pub fn Slider(
    value: f64,
    #[props(default = 0.0)] min: f64,
    #[props(default = 100.0)] max: f64,
    #[props(default = 1.0)] step: f64,
    #[props(default)] disabled: bool,
    oninput: Option<EventHandler<f64>>,
) -> Element {
    let pct = if max > min {
        (value - min) / (max - min) * 100.0
    } else {
        0.0
    };
    rsx! {
        input {
            class: "slider",
            r#type: "range",
            min: "{min}",
            max: "{max}",
            step: "{step}",
            value: "{value}",
            disabled,
            style: "--pct:{pct}%",
            oninput: move |e| {
                if let (Some(h), Ok(v)) = (oninput, e.value().parse::<f64>()) {
                    h.call(v);
                }
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabItem {
    pub id: String,
    pub label: String,
    pub icon: Option<IconName>,
}

impl TabItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// Tab title. The caller switches the content by `value`. `pill` is the gray capsule style (filter
/// bar), `segmented` is an equal-width segmented switch, and the default is the underline style.
#[component]
pub fn Tabs(
    items: Vec<TabItem>,
    #[props(into)] value: String,
    #[props(default)] pill: bool,
    #[props(default)] segmented: bool,
    onchange: Option<EventHandler<String>>,
) -> Element {
    let count = items.len();
    let index = items.iter().position(|item| item.id == value);
    rsx! {
        div {
            class: if segmented { "tabs segmented" } else if pill { "tabs pill" } else { "tabs" },
            role: "tablist",
            // Segmented: the white block behind the selected tab slides to the newly selected one
            if segmented && let Some(index) = index {
                span {
                    class: "tabs-indicator",
                    style: "--tab-count:{count};--tab-index:{index}",
                }
            }
            for item in items {
                button {
                    key: "{item.id}",
                    class: if item.id == value { "tab active" } else { "tab" },
                    r#type: "button",
                    role: "tab",
                    onclick: {
                        let id = item.id.clone();
                        move |_| {
                            if let Some(h) = onchange {
                                h.call(id.clone());
                            }
                        }
                    },
                    if let Some(name) = item.icon {
                        Icon { name, size: 14.0 }
                    }
                    "{item.label}"
                }
            }
        }
    }
}

/// Text shown by the unread count badge.
fn badge_text(count: u32) -> String {
    if count > 99 {
        "99+".into()
    } else {
        count.to_string()
    }
}

/// Red unread badge. Not shown when `count` is 0 and it is not `dot`; `muted` is the gray style for
/// muted conversations.
#[component]
pub fn Badge(
    #[props(default)] count: u32,
    #[props(default)] muted: bool,
    #[props(default)] dot: bool,
) -> Element {
    if count == 0 && !dot {
        return rsx! {};
    }
    let class = match (muted, dot) {
        (true, true) => "badge muted dot",
        (true, false) => "badge muted",
        (false, true) => "badge dot",
        (false, false) => "badge",
    };
    rsx! {
        span { class,
            if !dot {
                "{badge_text(count)}"
            }
        }
    }
}

/// Organization role badge next to a member: by default the blue "administrator" badge, and `owner` gives the
/// orange "owner" badge.
#[component]
pub fn RoleBadge(#[props(default)] owner: bool, children: Element) -> Element {
    rsx! {
        span { class: if owner { "role-badge owner" } else { "role-badge" }, {children} }
    }
}

#[cfg(test)]
mod tests {
    use super::badge_text;

    #[test]
    fn badge_caps_at_99() {
        assert_eq!(badge_text(5), "5");
        assert_eq!(badge_text(99), "99");
        assert_eq!(badge_text(100), "99+");
    }
}
