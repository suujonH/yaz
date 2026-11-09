//! 列表类窗口共用的外框：左侧导航 + 右侧内容、导航项、标题栏、多选。

use crate::services::services;
use crate::state::use_app_state;
use crate::ui::*;
use dioxus::prelude::*;

/// 窗口内用 `AccountSwitcher` 选择账号：窗口的会话跟着换。返回值每次切换加一，
/// 用它作为内容的 `key`，内容重新挂载后读新账号的数据。
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

/// 给信号赋值；窗口已关闭（信号已释放）时什么也不做。后台任务在等待之后写页面状态时用。
pub fn set_if_alive<T: 'static>(mut signal: Signal<T>, value: T) {
    if let Ok(mut slot) = signal.try_write() {
        *slot = value;
    }
}

/// 左侧导航加右侧内容。
#[component]
pub fn SplitView(nav: Element, children: Element) -> Element {
    rsx! {
        div { class: "split",
            div { class: "split-nav scroll", {nav} }
            div { class: "split-main", {children} }
        }
    }
}

/// 导航项：图标、名称、右侧数量或红点。
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

/// 内容区上方的标题栏：左边标题，右边是筛选、排序等操作。
#[component]
pub fn PaneHeader(#[props(into)] title: String, children: Element) -> Element {
    rsx! {
        div { class: "pane-head",
            span { class: "pane-title", "{title}" }
            div { class: "pane-actions", {children} }
        }
    }
}

/// 多选状态：开启后列表行显示勾选框。
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

    /// 关闭多选时清空已选。
    pub fn set_active(mut self, active: bool) {
        self.mode.set(active);
        if !active {
            self.ids.write().clear();
        }
    }

    /// 行的勾选状态；不在多选时为 `None`。
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

    /// 去掉已不存在的项（取消收藏、删除之后）。
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

/// 有已选项时从底部滑出的操作栏；`children` 放操作按钮。
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
