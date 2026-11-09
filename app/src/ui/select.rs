use super::geometry::{Placement, client_rect};
use super::icon::{Icon, IconName};
use super::overlay::{PopoverOptions, use_overlays};
use dioxus::prelude::*;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

impl SelectOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// 下拉选择框。
#[component]
pub fn Select(
    options: Vec<SelectOption>,
    #[props(into)] value: String,
    #[props(default, into)] placeholder: String,
    #[props(default)] disabled: bool,
    onchange: Option<EventHandler<String>>,
) -> Element {
    let overlays = use_overlays();
    let mut open = use_signal(|| false);
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let current = options
        .iter()
        .find(|o| o.value == value)
        .map(|o| o.label.clone());
    let list = options.clone();
    let selected = value.clone();
    rsx! {
        button {
            class: if open() { "select open" } else { "select" },
            r#type: "button",
            disabled,
            onmounted: move |e| mounted.set(Some(e.data())),
            onclick: move |_| {
                let list = list.clone();
                let selected = selected.clone();
                async move {
                    let el = mounted.peek().clone();
                    let Some(el) = el else { return };
                    let Some(anchor) = client_rect(&el).await else { return };
                    open.set(true);
                    let options = PopoverOptions { placement: Placement::BottomStart, gap: 4.0, min_width: anchor.w };
                    let mut overlays = overlays;
                    overlays.open_popover_inner(
                        anchor,
                        options,
                        Rc::new(move |handle| {
                            let list = list.clone();
                            let selected = selected.clone();
                            rsx! {
                                div { style: "padding:4px", role: "listbox",
                                    for option in list {
                                        div {
                                            key: "{option.value}",
                                            class: if option.value == selected { "menu-row selected" } else { "menu-row" },
                                            role: "option",
                                            onclick: {
                                                let value = option.value.clone();
                                                move |_| {
                                                    handle.close();
                                                    if let Some(h) = onchange {
                                                        h.call(value.clone());
                                                    }
                                                }
                                            },
                                            span { class: "menu-label", "{option.label}" }
                                        }
                                    }
                                }
                            }
                        }),
                        Some(Rc::new(move || {
                            let mut open = open;
                            open.set(false);
                        })),
                    );
                }
            },
            match current {
                Some(label) => rsx! { span { class: "value", "{label}" } },
                None => rsx! { span { class: "value placeholder", "{placeholder}" } },
            }
            Icon { name: IconName::ChevronDown, size: 14.0 }
        }
    }
}
