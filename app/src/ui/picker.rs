//! Object picker: search box + list (users, channels, topics, forward targets); with multi-select
//! the right side shows the selection.

use super::avatar::Avatar;
use super::controls::IconButton;
use super::flows::{Answer, DialogButtons};
use super::icon::IconName;
use super::input::TextInput;
use super::overlay::{Overlays, ToastKind, use_overlays};
use super::rows::{ListRow, RowDensity};
use super::virtual_list::{VirtualList, VirtualListConfig, use_virtual_list};
use dioxus::prelude::*;
use std::rc::Rc;
use zulip_core::contains_ignore_case;

const ROW_ESTIMATE: f64 = 40.0;

/// Picker shell: title bar, search box, result area, optional right column and bottom. Use it
/// directly when the results need customization (channel list with "Join" buttons, server search);
/// otherwise use [`Overlays::pick`].
#[component]
pub fn PickerShell(
    #[props(into)] title: String,
    #[props(default = 480.0)] width: f64,
    #[props(default = 460.0)] height: f64,
    #[props(into)] query: String,
    #[props(default = "搜索".to_string(), into)] placeholder: String,
    on_query: EventHandler<String>,
    on_close: EventHandler<()>,
    /// Text shown when there are no results; when `None` the result area is shown.
    #[props(into)]
    empty_text: Option<String>,
    side: Option<Element>,
    footer: Option<Element>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "picker", style: "width:{width}px;height:{height}px",
            div { class: "dialog-head",
                div { class: "dialog-title", "{title}" }
                div { class: "dialog-close",
                    IconButton { icon: IconName::Close, size: 30.0, icon_size: 14.0, onclick: move |_| on_close.call(()) }
                }
            }
            div { class: "picker-main",
                div { class: "picker-results",
                    TextInput { value: query, placeholder, autofocus: true, clearable: true, oninput: move |v| on_query.call(v) }
                    div { class: "picker-list",
                        if let Some(text) = empty_text {
                            div { class: "picker-empty", "{text}" }
                        } else {
                            {children}
                        }
                    }
                }
                if let Some(side) = side {
                    div { class: "picker-side", {side} }
                }
            }
            if let Some(footer) = footer {
                div { class: "dialog-foot", {footer} }
            }
        }
    }
}

/// A selectable item.
#[derive(Clone, Debug, PartialEq)]
pub struct PickerItem {
    pub id: u64,
    pub title: String,
    pub subtitle: Option<String>,
    pub avatar_url: Option<String>,
    pub avatar_fill: Option<String>,
}

impl PickerItem {
    pub fn new(id: u64, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            subtitle: None,
            avatar_url: None,
            avatar_fill: None,
        }
    }

    pub fn subtitle(mut self, text: impl Into<String>) -> Self {
        self.subtitle = Some(text.into());
        self
    }

    pub fn avatar(mut self, url: impl Into<String>) -> Self {
        self.avatar_url = Some(url.into());
        self
    }
}

/// Flow configuration for choosing a target.
#[derive(Clone, Debug, PartialEq)]
pub struct Picker {
    title: String,
    placeholder: String,
    empty_text: String,
    items: Rc<Vec<PickerItem>>,
    multi: bool,
    initial: Vec<u64>,
    max_selected: Option<usize>,
    confirm_text: String,
}

impl Picker {
    /// Single select: clicking selects and closes.
    pub fn single(title: impl Into<String>, items: Vec<PickerItem>) -> Self {
        Self {
            title: title.into(),
            placeholder: "搜索".into(),
            empty_text: "未找到相关结果".into(),
            items: Rc::new(items),
            multi: false,
            initial: Vec::new(),
            max_selected: None,
            confirm_text: "确定".into(),
        }
    }

    /// Multi-select: the right column shows the selection.
    pub fn multi(title: impl Into<String>, items: Vec<PickerItem>, initial: Vec<u64>) -> Self {
        Self {
            multi: true,
            initial,
            ..Self::single(title, items)
        }
    }

    pub fn max_selected(mut self, max: usize) -> Self {
        self.max_selected = Some(max);
        self
    }

    pub fn confirm_text(mut self, text: impl Into<String>) -> Self {
        self.confirm_text = text.into();
        self
    }
}

/// Items whose name or description contains the query (case-insensitive).
fn filter_items(items: &[PickerItem], query: &str) -> Vec<usize> {
    let query = query.trim();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            contains_ignore_case(&item.title, query)
                || item
                    .subtitle
                    .as_deref()
                    .is_some_and(|s| contains_ignore_case(s, query))
        })
        .map(|(i, _)| i)
        .collect()
}

impl Overlays {
    /// Chooses a target; returns the ID of the chosen item, `None` on cancel.
    pub async fn pick(self, picker: Picker) -> Option<Vec<u64>> {
        self.ask_dialog(move |answer| {
            rsx! { PickerDialog { picker: picker.clone(), answer } }
        })
        .await
    }
}

#[component]
fn PickerDialog(picker: Picker, answer: Answer<Vec<u64>>) -> Element {
    let overlays = use_overlays();
    let mut query = use_signal(String::new);
    let mut selected = use_signal(|| picker.initial.clone());
    let items = picker.items.clone();
    let source = items.clone();
    let visible = use_memo(move || Rc::new(filter_items(&source, &query.read())));
    let keys = use_memo({
        let items = items.clone();
        move || {
            Rc::new(
                visible
                    .read()
                    .iter()
                    .map(|&i| items[i].id)
                    .collect::<Vec<u64>>(),
            )
        }
    });
    let list = use_virtual_list(VirtualListConfig {
        estimate: ROW_ESTIMATE,
        ..VirtualListConfig::default()
    });
    let multi = picker.multi;
    let max_selected = picker.max_selected;
    let pick_answer = answer.clone();
    let on_pick = use_callback(move |id: u64| {
        if !multi {
            pick_answer.send(vec![id]);
            return;
        }
        let exists = selected.peek().contains(&id);
        if exists {
            selected.write().retain(|x| *x != id);
        } else if max_selected.is_some_and(|max| selected.peek().len() >= max) {
            overlays.toast_kind(
                ToastKind::Error,
                format!("最多选择 {} 个", max_selected.unwrap_or(0)),
            );
        } else {
            selected.write().push(id);
        }
    });
    let (ok, cancel, close) = (answer.clone(), answer.clone(), answer);
    let chosen: Vec<PickerItem> = selected
        .read()
        .iter()
        .filter_map(|id| items.iter().find(|i| i.id == *id).cloned())
        .collect();
    let row_items = items.clone();
    rsx! {
        PickerShell {
            title: picker.title.clone(),
            width: if multi { 560.0 } else { 400.0 },
            query: query(),
            placeholder: picker.placeholder.clone(),
            on_query: move |v| query.set(v),
            on_close: move |_| close.cancel(),
            empty_text: visible.read().is_empty().then(|| picker.empty_text.clone()),
            side: multi.then(|| rsx! {
                div { class: "picker-count", "已选择 {chosen.len()} 项" }
                div { class: "picker-list scroll",
                    for item in chosen.iter().cloned() {
                        PickerRow {
                            key: "{item.id}",
                            item: item.clone(),
                            trailing: rsx! {
                                IconButton {
                                    icon: IconName::Close,
                                    size: 24.0,
                                    icon_size: 12.0,
                                    onclick: move |_| selected.write().retain(|x| *x != item.id),
                                }
                            },
                        }
                    }
                }
            }),
            footer: multi.then(|| rsx! {
                DialogButtons {
                    confirm_text: picker.confirm_text.clone(),
                    on_confirm: move |_| ok.send(selected()),
                    on_cancel: move |_| cancel.cancel(),
                }
            }),
            VirtualList {
                handle: list,
                keys: keys(),
                render: move |i: usize| {
                    let item = row_items[visible.peek()[i]].clone();
                    rsx! {
                        PickerRow {
                            item,
                            checks: if multi { Some(selected) } else { None },
                            on_pick,
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn PickerRow(
    item: PickerItem,
    checks: Option<Signal<Vec<u64>>>,
    on_pick: Option<Callback<u64>>,
    trailing: Option<Element>,
) -> Element {
    let id = item.id;
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            checked: checks.map(|c| c.read().contains(&id)),
            leading: rsx! {
                Avatar {
                    text: item.title.clone(),
                    owner: "{id}",
                    src: item.avatar_url.clone(),
                    fill: item.avatar_fill.clone(),
                    size: 28.0,
                }
            },
            title: item.title.clone(),
            subtitle: item.subtitle.clone(),
            trailing,
            onclick: move |_| {
                if let Some(h) = on_pick {
                    h.call(id);
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_title_and_subtitle_ignoring_case() {
        let items = vec![
            PickerItem::new(1, "Alice").subtitle("a@x.com"),
            PickerItem::new(2, "鲍勃").subtitle("BOB@x.com"),
            PickerItem::new(3, "Carol"),
        ];
        assert_eq!(filter_items(&items, ""), vec![0, 1, 2]);
        assert_eq!(filter_items(&items, " ALI "), vec![0]);
        assert_eq!(filter_items(&items, "bob"), vec![1]);
        assert_eq!(filter_items(&items, "鲍"), vec![1]);
        assert!(filter_items(&items, "zzz").is_empty());
    }
}
