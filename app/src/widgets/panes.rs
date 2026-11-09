//! Shared frame of list-style windows: left navigation + right content, nav items, title bar,
//! multi-select.

use crate::services::services;
use crate::state::use_app_state;
use crate::ui::*;
use dioxus::prelude::*;

/// Pick the account inside the window with `AccountSwitcher`: the window's conversations follow.
/// The return value is incremented on every switch; use it as the content's `key` so the content
/// remounts and reads the new account's data.
pub fn use_window_account(selected: Signal<i64>) -> Signal<u32> {
    let state = use_app_state();
    let mut epoch = use_signal(|| 0u32);
    use_effect(move || {
        let id = selected();
        if *state.account.peek() != Some(id) {
            let (mut account, mut session) = (state.account, state.session);
            account.set(Some(id));
            session.set(services().hub.session(id));
            epoch += 1;
        }
    });
    epoch
}

/// Assign to a signal; does nothing if the window is closed (signal dropped). Used when a
/// background task writes page state after an await.
pub fn set_if_alive<T: 'static>(mut signal: Signal<T>, value: T) {
    if let Ok(mut slot) = signal.try_write() {
        *slot = value;
    }
}

/// Left navigation plus right content.
#[component]
pub fn SplitView(nav: Element, children: Element) -> Element {
    rsx! {
        div { class: "split",
            div { class: "split-nav scroll", {nav} }
            div { class: "split-main", {children} }
        }
    }
}

/// Nav item: icon, name, count or red dot on the right.
#[component]
pub fn NavRow(
    icon: Option<IconName>,
    #[props(into)] label: String,
    #[props(into)] count: Option<String>,
    #[props(default)] dot: bool,
    #[props(default)] selected: bool,
    onclick: EventHandler<MouseEvent>,
    menu: Option<Callback<(), Vec<MenuEntry>>>,
) -> Element {
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            leading: icon.map(|name| rsx! { Icon { name, size: 16.0 } }),
            title: label,
            trailing: rsx! {
                if let Some(count) = count {
                    span { class: "text-caption", "{count}" }
                }
                Badge { dot }
            },
            selected,
            onclick: move |e| onclick.call(e),
            menu: menu,
        }
    }
}

/// Title bar above the content area: title on the left, actions such as filter and sort on the
/// right.
#[component]
pub fn PaneHeader(#[props(into)] title: String, children: Element) -> Element {
    rsx! {
        div { class: "pane-head",
            span { class: "pane-title", "{title}" }
            div { class: "pane-actions", {children} }
        }
    }
}

/// Multi-select state: when on, list rows show checkboxes.
#[derive(Clone, Copy, PartialEq)]
pub struct Selection {
    mode: Signal<bool>,
    ids: Signal<Vec<i64>>,
}

pub fn use_selection() -> Selection {
    Selection {
        mode: use_signal(|| false),
        ids: use_signal(Vec::new),
    }
}

impl Selection {
    pub fn is_active(self) -> bool {
        (self.mode)()
    }

    /// Clear the selection when multi-select is turned off.
    pub fn set_active(mut self, active: bool) {
        self.mode.set(active);
        if !active {
            self.ids.write().clear();
        }
    }

    /// Check state of a row; `None` when not in multi-select.
    pub fn checked(self, id: i64) -> Option<bool> {
        self.is_active().then(|| self.ids.read().contains(&id))
    }

    pub fn toggle(mut self, id: i64) {
        let mut ids = self.ids.write();
        if let Some(at) = ids.iter().position(|x| *x == id) {
            ids.remove(at);
        } else {
            ids.push(id);
        }
    }

    pub fn ids(self) -> Vec<i64> {
        self.ids.read().clone()
    }

    pub fn count(self) -> usize {
        self.ids.read().len()
    }

    /// Drop items that no longer exist (after unfavoriting or deleting).
    pub fn retain(mut self, keep: impl Fn(i64) -> bool) {
        self.ids.write().retain(|id| keep(*id));
    }

    pub fn clear(mut self) {
        self.ids.write().clear();
    }

    pub fn replace(mut self, ids: Vec<i64>) {
        *self.ids.write() = ids;
    }
}

/// Action bar that slides up from the bottom when items are selected; `children` holds the action
/// buttons.
#[component]
pub fn SelectionBar(count: usize, on_close: EventHandler<()>, children: Element) -> Element {
    rsx! {
        SlideBar { open: count > 0,
            div { class: "selection-bar",
                span { class: "text-sub", "已选 {count} 项" }
                {children}
                IconButton { icon: IconName::Close, onclick: move |_| on_close.call(()) }
            }
        }
    }
}
