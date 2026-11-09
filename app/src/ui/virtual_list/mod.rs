//! 虚拟滚动列表：支持变高、向上加载历史时保持位置、滚动到某项、粘底。
//!
//! 高度表和渲染范围在 Rust（[`state::ListState`]）；`assets/ui.js` 的 `VirtualList` 负责同帧测量和滚动位置修正。

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

/// 列表的控制句柄，由 [`use_virtual_list`] 创建后传给 [`VirtualList`]。
#[derive(Clone)]
pub struct VirtualListHandle {
    id: Rc<str>,
    state: Rc<RefCell<ListState>>,
    tick: Signal<u64>,
    at_bottom: Signal<bool>,
    initial: Rc<RefCell<Option<String>>>,
}

/// 滚动后的可见范围。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListViewport {
    /// 视口顶端所在的条目；条目还没渲染出来时为 `None`。
    pub first: Option<u64>,
    /// 视口顶端距该条目顶端的距离。
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
    /// 滚动到条目。条目不存在时不动作。
    pub fn scroll_to_key(&self, key: u64, align: ScrollAlign) {
        self.scroll_with_offset(key, align, 0.0);
    }

    /// 滚动到条目，`Start` 对齐时视口顶端位于条目顶端之下 `offset`。
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

    /// 同步更换条目（之后马上要滚动到新条目时用）；`keys` 仍要传给 [`VirtualList`]。
    pub fn sync_keys(&self, keys: &[u64]) {
        let mut state = self.state.borrow_mut();
        if state.keys() != keys {
            state.set_keys(keys);
        }
    }

    /// 首次绘制时就位于某条目，[`VirtualList`] 挂载前调用。
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

    /// 是否在底部（响应式，可用来显示“回到底部”按钮）。
    pub fn at_bottom(&self) -> bool {
        (self.at_bottom)()
    }

    fn bump(&self) {
        let mut tick = self.tick;
        tick += 1;
    }
}

/// 列表配置。全部只在创建时读取一次。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualListConfig {
    /// 未测量条目的估计高度。
    pub estimate: f64,
    /// 视口上下各预渲染的高度。
    pub overscan: f64,
    /// 在底部时，内容增长继续贴住底部（消息列表）。
    pub stick_to_bottom: bool,
    /// 初始位置在底部。
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

/// 虚拟滚动列表。
///
/// - `keys`：每项的稳定 ID（消息 ID、会话键的哈希等），顺序即显示顺序。内容变化但键不变时列表不重新渲染条目，
///   所以条目应是读取自身状态的组件。
/// - `render`：按下标渲染条目内容，外层有 `div.vl-item`。条目不要用 `margin` 撑开外部，用 `padding`。
/// - `stick_to_bottom`、`start_at_bottom` 在 [`VirtualListConfig`] 中配置。
#[component]
pub fn VirtualList(
    handle: VirtualListHandle,
    keys: Rc<Vec<u64>>,
    render: Callback<usize, Element>,
    /// 接近顶部（加载历史）。列表在顶部有变化后才会再次触发。
    on_reach_top: Option<EventHandler<()>>,
    /// 接近底部（加载更新的消息）。
    on_reach_bottom: Option<EventHandler<()>>,
    /// 滚动或条目变化后的可见范围。
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

    // 初始位置不在顶部时，JS 定位完成前不显示内容
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

/// 连接 JS 并处理它的消息，直到组件卸载。
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
