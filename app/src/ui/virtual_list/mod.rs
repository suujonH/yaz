//! Virtual scrolling list: variable heights, keeps position when loading history upward, scroll to
//! an item, stick to bottom.
//!
//! The height table and render range live in Rust ([`state::ListState`]); `VirtualList` in
//! `assets/ui.js` measures within the same frame and corrects the scroll position.

mod state;

use super::next_dom_id;
use dioxus::prelude::*;
use serde::Deserialize;
use state::{ListState, ScrollReport};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollAlign {
    #[default]
    Start,
    Center,
    End,
}

impl ScrollAlign {
    const fn as_js(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Center => "center",
            Self::End => "end",
        }
    }
}

/// Control handle of the list; created by [`use_virtual_list`] and passed to [`VirtualList`].
#[derive(Clone)]
pub struct VirtualListHandle {
    id: Rc<str>,
    state: Rc<RefCell<ListState>>,
    tick: Signal<u64>,
    at_bottom: Signal<bool>,
    initial: Rc<RefCell<Option<String>>>,
}

/// Visible range after scrolling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListViewport {
    /// The item at the top of the viewport; `None` while the item is not rendered yet.
    pub first: Option<u64>,
    /// Distance from the viewport top to the top of that item.
    pub offset: f64,
    pub last: Option<u64>,
    pub at_bottom: bool,
}

impl PartialEq for VirtualListHandle {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.state, &other.state)
    }
}

impl VirtualListHandle {
    /// Scroll to an item. Does nothing if the item does not exist.
    pub fn scroll_to_key(&self, key: u64, align: ScrollAlign) {
        self.scroll_with_offset(key, align, 0.0);
    }

    /// Keep the item under the client y coordinate `y` in place while the content changes (until
    /// the user scrolls).
    pub fn hold_item_at(&self, y: f64) {
        document::eval(&format!("window.zulipUi.listHoldAt({:?},{y})", &*self.id));
    }

    /// Scroll to an item; with `Start` alignment the viewport top sits `offset` below the item top.
    pub fn scroll_with_offset(&self, key: u64, align: ScrollAlign, offset: f64) {
        if self.state.borrow_mut().jump_to(key) {
            self.bump();
            document::eval(&format!(
                "window.zulipUi.listScrollTo({:?},\"{key}\",\"{}\",{offset})",
                &*self.id,
                align.as_js()
            ));
        }
    }

    /// Replace the items synchronously (use when about to scroll to a new item right after); `keys`
    /// must still be passed to [`VirtualList`].
    pub fn sync_keys(&self, keys: &[u64]) {
        let mut state = self.state.borrow_mut();
        if state.keys() != keys {
            state.set_keys(keys);
        }
    }

    /// Be at a given item on first paint; call before [`VirtualList`] mounts.
    pub fn start_at(&self, keys: &[u64], key: u64, align: ScrollAlign, offset: f64) {
        let mut state = self.state.borrow_mut();
        state.set_keys(keys);
        if state.jump_to(key) {
            *self.initial.borrow_mut() = Some(format!(
                "{{key:\"{key}\",align:\"{}\",extra:{offset}}}",
                align.as_js()
            ));
        }
    }

    pub fn scroll_to_bottom(&self) {
        self.state.borrow_mut().jump_to_bottom();
        self.bump();
        document::eval(&format!(
            "window.zulipUi.listScrollToBottom({:?})",
            &*self.id
        ));
    }

    /// Whether at the bottom (reactive; can drive a "back to bottom" button).
    pub fn at_bottom(&self) -> bool {
        (self.at_bottom)()
    }

    fn bump(&self) {
        let mut tick = self.tick;
        tick += 1;
    }
}

/// List configuration. All of it is read once at creation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualListConfig {
    /// Estimated height of unmeasured items.
    pub estimate: f64,
    /// Height pre-rendered above and below the viewport.
    pub overscan: f64,
    /// When at the bottom, keep sticking to the bottom as content grows (message list).
    pub stick_to_bottom: bool,
    /// Initial position is at the bottom.
    pub start_at_bottom: bool,
}

impl Default for VirtualListConfig {
    fn default() -> Self {
        Self {
            estimate: 56.0,
            overscan: 800.0,
            stick_to_bottom: false,
            start_at_bottom: false,
        }
    }
}

pub fn use_virtual_list(config: VirtualListConfig) -> VirtualListHandle {
    let tick = use_signal(|| 0);
    let at_bottom = use_signal(|| config.start_at_bottom);
    use_hook(|| VirtualListHandle {
        id: next_dom_id().into(),
        state: Rc::new(RefCell::new(ListState::new(
            config.estimate,
            config.overscan,
            config.stick_to_bottom,
            config.start_at_bottom,
        ))),
        tick,
        at_bottom,
        initial: Rc::default(),
    })
}

#[derive(Deserialize)]
#[serde(tag = "t")]
enum ListMessage {
    #[serde(rename = "s")]
    Scroll {
        top: f64,
        view: f64,
        bottom: bool,
        anchor: Option<String>,
        off: f64,
    },
    #[serde(rename = "m")]
    Measure { sizes: Vec<(String, f64)> },
}

/// Virtual scrolling list.
///
/// - `keys`: a stable ID per item (message ID, hash of a conversation key, etc.); order is display
///   order. When content changes but the key does not, the list does not re-render the item, so
///   items should be components that read their own state.
/// - `render`: renders item content by index, wrapped in a `div.vl-item`. Items must not use
///   `margin` to push outward; use `padding`.
/// - `stick_to_bottom` and `start_at_bottom` are configured in [`VirtualListConfig`].
#[component]
pub fn VirtualList(
    handle: VirtualListHandle,
    keys: Rc<Vec<u64>>,
    render: Callback<usize, Element>,
    /// Near the top (load history). Fires again only after the list changes at the top.
    on_reach_top: Option<EventHandler<()>>,
    /// Near the bottom (load newer messages).
    on_reach_bottom: Option<EventHandler<()>>,
    /// Visible range after scrolling or item changes.
    on_viewport: Option<EventHandler<ListViewport>>,
    #[props(default, into)] class: String,
) -> Element {
    let state = handle.state.clone();
    let mut last_keys = use_hook(|| Rc::new(RefCell::new(None::<Rc<Vec<u64>>>)));
    let _ = (handle.tick)();

    {
        let mut slot = last_keys.borrow_mut();
        let changed = !slot.as_ref().is_some_and(|k| Rc::ptr_eq(k, &keys));
        if changed {
            if state.borrow().keys() != keys.as_slice() || slot.is_none() {
                state.borrow_mut().set_keys(&keys);
            }
            *slot = Some(keys.clone());
        }
    }
    let _ = &mut last_keys;

    let (range, top, bottom) = {
        let st = state.borrow();
        let (top, bottom) = st.spacers();
        (st.range(), top, bottom)
    };
    let id = handle.id.clone();
    let id_for_drop = handle.id.clone();
    use_drop(move || {
        document::eval(&format!("window.zulipUi.listDetach({:?})", &*id_for_drop));
    });

    // When the initial position is not the top, hide content until JS has positioned it
    let positioning = use_hook(|| {
        (handle.state.borrow().is_at_bottom() || handle.initial.borrow().is_some()).then_some("1")
    });
    let handle_for_mount = handle.clone();
    rsx! {
        div {
            id: "{id}",
            class: "vl scroll fade-scrollbar {class}",
            tabindex: "-1",
            "data-vl-positioning": positioning,
            onmounted: move |_| {
                let handle = handle_for_mount.clone();
                async move {
                    run_list_channel(handle, on_reach_top, on_reach_bottom, on_viewport).await;
                }
            },
            div { class: "vl-spacer", style: "height:{top}px" }
            for i in range {
                div {
                    key: "{keys[i]}",
                    class: "vl-item",
                    "data-key": "{keys[i]}",
                    {render.call(i)}
                }
            }
            div { class: "vl-spacer", style: "height:{bottom}px" }
        }
    }
}

/// Connect to JS and handle its messages until the component unmounts.
async fn run_list_channel(
    handle: VirtualListHandle,
    on_reach_top: Option<EventHandler<()>>,
    on_reach_bottom: Option<EventHandler<()>>,
    on_viewport: Option<EventHandler<ListViewport>>,
) {
    let mut at_bottom = handle.at_bottom;
    let mut tick = handle.tick;
    let (stick, start_bottom) = {
        let st = handle.state.borrow();
        (st.stick(), st.is_at_bottom())
    };
    let initial = handle
        .initial
        .borrow_mut()
        .take()
        .unwrap_or_else(|| "null".into());
    let js = format!(
        "await window.zulipUi.listAttach({:?}, (m) => dioxus.send(m), {stick}, {start_bottom}, {initial});",
        &*handle.id
    );
    let mut eval = document::eval(&js);
    while let Ok(message) = eval.recv::<ListMessage>().await {
        let changed = match message {
            ListMessage::Measure { sizes } => {
                let sizes: Vec<(u64, f64)> = sizes
                    .into_iter()
                    .filter_map(|(k, h)| k.parse().ok().map(|k| (k, h)))
                    .collect();
                handle.state.borrow_mut().measure(&sizes)
            }
            ListMessage::Scroll {
                top,
                view,
                bottom,
                anchor,
                off,
            } => {
                let anchor = anchor.and_then(|k| k.parse().ok()).map(|k: u64| (k, off));
                if *at_bottom.peek() != bottom {
                    at_bottom.set(bottom);
                }
                let changed = handle.state.borrow_mut().on_scroll(ScrollReport {
                    top,
                    view,
                    at_bottom: bottom,
                    anchor,
                });
                if let Some(h) = on_viewport {
                    let last = handle.state.borrow().last_visible();
                    h.call(ListViewport {
                        first: anchor.map(|a| a.0),
                        offset: off,
                        last,
                        at_bottom: bottom,
                    });
                }
                changed
            }
        };
        if changed {
            tick += 1;
        }
        let reached = handle.state.borrow_mut().take_reached();
        if reached.top
            && let Some(h) = on_reach_top
        {
            h.call(());
        }
        if reached.bottom
            && let Some(h) = on_reach_bottom
        {
            h.call(());
        }
    }
}
