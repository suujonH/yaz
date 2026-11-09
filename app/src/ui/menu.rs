//! 右键菜单与子菜单。

use super::geometry::{Placement, Rect, client_rect, place, place_menu_with_bar, viewport};
use super::icon::{Icon, IconName};
use super::overlay::{Overlays, use_overlays};
use dioxus::dioxus_core::Task;
use dioxus::prelude::*;
use std::rc::Rc;
use std::time::Duration;

const SUBMENU_GAP: f64 = 5.0;
/// 子菜单上移外框到第一项的距离，第一项与父菜单项在同一行。
const SUBMENU_LIFT: f64 = 5.0;
const SUBMENU_DELAY_MS: u64 = 120;

/// 菜单项。链式设置图标、禁用、勾选。
#[derive(Clone)]
pub struct MenuAction {
    label: String,
    icon: Option<IconName>,
    /// 一级菜单中没有图标时按名称取 [`default_icon`]。
    auto_icon: bool,
    hint: Option<String>,
    checked: Option<bool>,
    disabled: bool,
    danger: bool,
    on_select: Rc<dyn Fn()>,
}

#[derive(Clone)]
pub struct MenuSubmenu {
    label: String,
    icon: Option<IconName>,
    auto_icon: bool,
    disabled: bool,
    items: Rc<Vec<MenuEntry>>,
}

#[derive(Clone)]
pub enum MenuEntry {
    Action(MenuAction),
    Submenu(MenuSubmenu),
    Separator,
}

impl MenuEntry {
    pub fn item(label: impl Into<String>, on_select: impl Fn() + 'static) -> Self {
        Self::Action(MenuAction {
            label: label.into(),
            icon: None,
            auto_icon: true,
            hint: None,
            checked: None,
            disabled: false,
            danger: false,
            on_select: Rc::new(on_select),
        })
    }

    pub fn submenu(label: impl Into<String>, items: Vec<MenuEntry>) -> Self {
        Self::Submenu(MenuSubmenu {
            label: label.into(),
            icon: None,
            auto_icon: true,
            disabled: false,
            items: Rc::new(items),
        })
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Action(a) => &a.label,
            Self::Submenu(s) => &s.label,
            Self::Separator => "",
        }
    }

    /// 执行菜单项（菜单之外的入口用）；子菜单和分隔线没有动作。
    pub fn select(&self) {
        if let Self::Action(a) = self {
            (a.on_select)();
        }
    }

    /// 子菜单的各项。
    pub fn items(&self) -> Vec<MenuEntry> {
        match self {
            Self::Submenu(s) => s.items.as_ref().clone(),
            _ => Vec::new(),
        }
    }

    pub fn icon(mut self, name: IconName) -> Self {
        match &mut self {
            Self::Action(a) => a.icon = Some(name),
            Self::Submenu(s) => s.icon = Some(name),
            Self::Separator => {}
        }
        self
    }

    /// 不按名称自动取图标（图片查看器等不带图标的菜单）。
    pub fn plain(mut self) -> Self {
        match &mut self {
            Self::Action(a) => a.auto_icon = false,
            Self::Submenu(s) => s.auto_icon = false,
            Self::Separator => {}
        }
        self
    }

    /// 一级菜单显示的样子：没有图标的项按名称补上图标；勾选项的图标列留给对勾。
    fn with_default_icon(mut self) -> Self {
        match &mut self {
            Self::Action(a) if a.icon.is_none() && a.auto_icon && a.checked.is_none() => {
                a.icon = default_icon(&a.label);
            }
            Self::Submenu(s) if s.icon.is_none() && s.auto_icon => s.icon = default_icon(&s.label),
            _ => {}
        }
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        match &mut self {
            Self::Action(a) => a.disabled = disabled,
            Self::Submenu(s) => s.disabled = disabled,
            Self::Separator => {}
        }
        self
    }

    /// 勾选项：对勾显示在图标列，未勾选时也占位。
    pub fn checked(mut self, checked: bool) -> Self {
        if let Self::Action(a) = &mut self {
            a.checked = Some(checked);
        }
        self
    }

    pub fn danger(mut self) -> Self {
        if let Self::Action(a) = &mut self {
            a.danger = true;
        }
        self
    }

    /// 右侧的说明文字（快捷键等）。
    pub fn hint(mut self, text: impl Into<String>) -> Self {
        if let Self::Action(a) = &mut self {
            a.hint = Some(text.into());
        }
        self
    }
}

/// 一级菜单项的图标：按名称统一对照，所有菜单共用。对照表中没有的名称不显示图标，文字仍与其它项对齐。
fn default_icon(label: &str) -> Option<IconName> {
    use IconName as I;
    Some(match label {
        // 消息
        "剪切" => I::Cut,
        "复制" => I::Copy,
        "粘贴" => I::Paste,
        "全选" | "标为已读" => I::CheckCircle,
        "另存为" | "保存" => I::Download,
        "收藏" | "取消收藏" => I::Star,
        "引用" => I::Quote,
        "转发" => I::Share,
        "多选" => I::Check,
        "编辑" | "备注" | "重命名话题" | "重命名分组" => I::Edit,
        "更多" => I::More,
        "删除" | "从消息列表中移除" | "删除话题" | "删除分组" | "删除账号" => {
            I::Delete
        }
        // 文件
        "打开" => I::File,
        "打开所在文件夹" | "打开文件夹" | "频道文件" => I::Folder,
        "投票" => I::Poll,
        "定位到聊天" => I::Location,
        // 会话、话题、频道
        "置顶" | "取消置顶" => I::Backtop,
        "复制链接" | "复制频道链接" => I::Link,
        "标为未读" | "常用语" => I::ChatBubble,
        "设置免打扰" | "消息设置" => I::Notification,
        "取消免打扰" => I::NotificationOff,
        "打开独立聊天窗口" => I::Fullscreen,
        "退订频道" | "退出账号" => I::Logout,
        "频道颜色" => I::Palette,
        // 聊天成员、联系人
        "发送消息" | "发消息" => I::Chat,
        "@TA" => I::Mention,
        "查看资料" | "按成员" | "邀请成员" => I::User,
        "屏蔽此人" | "取消屏蔽" => I::Forbid,
        "移动联系人至" | "移动到其他分组" | "发起群聊" => I::Usergroup,
        // 聊天记录筛选
        "按日期…" | "定时发送…" => I::Time,
        "清除筛选" => I::Close,
        // “+”菜单
        "添加分组" | "新建话题" | "创建频道" => I::Add,
        "加入频道" => I::ViewList,
        // 账号
        "更多设置" => I::Setting,
        _ => return None,
    })
}

/// 只要有一项带图标或勾选位，所有项都留出图标列。
fn needs_icon_column(items: &[MenuEntry]) -> bool {
    items.iter().any(|entry| match entry {
        MenuEntry::Action(a) => a.icon.is_some() || a.checked.is_some(),
        MenuEntry::Submenu(s) => s.icon.is_some(),
        MenuEntry::Separator => false,
    })
}

/// 菜单上方独立工具条的内容。回调的参数用来关闭菜单。
pub(super) type MenuBar = Rc<dyn Fn(Callback<()>) -> Element>;

#[derive(Clone)]
pub(super) struct MenuState {
    pub anchor: Rect,
    pub items: Rc<Vec<MenuEntry>>,
    pub bar: Option<MenuBar>,
}

#[derive(Clone)]
struct Items(Rc<Vec<MenuEntry>>);

impl PartialEq for Items {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone)]
struct BarProp(MenuBar);

impl PartialEq for BarProp {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// 菜单定位时要一起摆放的工具条。
#[derive(Clone, PartialEq)]
struct BarSlot {
    size: (f64, f64),
    placed: Callback<(f64, f64)>,
}

fn fixed_style(pos: Option<(f64, f64)>) -> String {
    pos.map_or_else(
        || "left:0;top:0".to_string(),
        |(x, y)| format!("left:{x}px;top:{y}px"),
    )
}

#[component]
pub(super) fn MenuHost(
    state: Signal<Option<MenuState>, UnsyncStorage>,
    on_close: Callback<()>,
) -> Element {
    let current = state.read().clone();
    rsx! {
        if let Some(menu) = current {
            div {
                key: "{menu.anchor.x},{menu.anchor.y}",
                class: "overlay-backdrop",
                style: "z-index:300",
                onmousedown: move |e| {
                    e.stop_propagation();
                    on_close.call(());
                },
                oncontextmenu: move |e| {
                    e.prevent_default();
                    e.stop_propagation();
                    on_close.call(());
                },
            }
            if let Some(bar) = menu.bar {
                BarredMenu {
                    bar: BarProp(bar),
                    items: Items(menu.items),
                    anchor: menu.anchor,
                    on_close,
                }
            } else {
                MenuPanel {
                    items: Items(menu.items),
                    anchor: menu.anchor,
                    submenu: false,
                    on_close,
                }
            }
        }
    }
}

/// 工具条与菜单是两张独立的卡片。先量工具条，菜单定位时一起算出两者的位置。
#[component]
fn BarredMenu(bar: BarProp, items: Items, anchor: Rect, on_close: Callback<()>) -> Element {
    let mut size = use_signal(|| None::<(f64, f64)>);
    let mut pos = use_signal(|| None::<(f64, f64)>);
    let placed = use_callback(move |at| pos.set(Some(at)));
    rsx! {
        div {
            class: if pos().is_some() { "menu-bar" } else { "menu-bar unplaced" },
            style: fixed_style(pos()),
            oncontextmenu: move |e| {
                e.prevent_default();
                e.stop_propagation();
            },
            onmousedown: move |e| e.stop_propagation(),
            onmounted: move |e: MountedEvent| async move {
                if let Some(rect) = client_rect(&e.data()).await {
                    size.set(Some((rect.w, rect.h)));
                }
            },
            {(bar.0)(on_close)}
        }
        if let Some(size) = size() {
            MenuPanel {
                items,
                anchor,
                submenu: false,
                on_close,
                bar: BarSlot { size, placed },
            }
        }
    }
}

#[component]
fn MenuPanel(
    items: Items,
    anchor: Rect,
    submenu: bool,
    on_close: Callback<()>,
    bar: Option<BarSlot>,
) -> Element {
    let mut pos = use_signal(|| None::<(f64, f64)>);
    let mut open_sub = use_signal(|| None::<(usize, Rect, Rc<Vec<MenuEntry>>)>);
    let mut hover_task = use_signal(|| None::<Task>);
    // 子菜单不自动加图标
    let items = if submenu {
        items
    } else {
        Items(Rc::new(
            items
                .0
                .iter()
                .cloned()
                .map(MenuEntry::with_default_icon)
                .collect(),
        ))
    };
    let icon_column = needs_icon_column(&items.0);
    let style = fixed_style(pos());
    let open_index = open_sub.read().as_ref().map(|(i, _, _)| *i);
    let mut on_hover = move |target: Option<(usize, Rc<Vec<MenuEntry>>, Rc<MountedData>)>| {
        if let Some(task) = hover_task.take() {
            task.cancel();
        }
        let task = spawn(async move {
            tokio::time::sleep(Duration::from_millis(SUBMENU_DELAY_MS)).await;
            match target {
                Some((index, items, el)) => {
                    if let Some(rect) = client_rect(&el).await {
                        open_sub.set(Some((index, rect, items)));
                    }
                }
                None => open_sub.set(None),
            }
        });
        hover_task.set(Some(task));
    };
    rsx! {
        div {
            class: if pos().is_some() { "menu scroll" } else { "menu scroll unplaced" },
            style,
            role: "menu",
            oncontextmenu: move |e| {
                e.prevent_default();
                e.stop_propagation();
            },
            onmousedown: move |e| e.stop_propagation(),
            onmounted: move |e: MountedEvent| {
                let bar = bar.clone();
                async move {
                    let Some(rect) = client_rect(&e.data()).await else {
                        return;
                    };
                    let size = (rect.w, rect.h);
                    if let Some(bar) = bar {
                        let (menu_at, bar_at) = place_menu_with_bar(anchor, size, bar.size, viewport());
                        pos.set(Some(menu_at));
                        bar.placed.call(bar_at);
                        return;
                    }
                    let (anchor, placement, gap) = if submenu {
                        (Rect { y: anchor.y - SUBMENU_LIFT, ..anchor }, Placement::RightStart, SUBMENU_GAP)
                    } else {
                        (anchor, Placement::BottomStart, 0.0)
                    };
                    pos.set(Some(place(anchor, size, viewport(), placement, gap)));
                }
            },
            for (index, entry) in items.0.iter().cloned().enumerate() {
                match entry {
                    MenuEntry::Separator => rsx! { div { key: "{index}", class: "menu-separator" } },
                    MenuEntry::Action(action) => rsx! {
                        ActionRow { key: "{index}", action: ActionProp(action), icon_column, on_close, on_hover: move |_| on_hover(None) }
                    },
                    MenuEntry::Submenu(sub) => rsx! {
                        SubmenuRow {
                            key: "{index}",
                            sub: SubmenuProp(sub),
                            icon_column,
                            open: open_index == Some(index),
                            on_hover: move |(items, el)| on_hover(Some((index, items, el))),
                        }
                    },
                }
            }
        }
        if let Some((index, rect, sub_items)) = open_sub.read().clone() {
            MenuPanel { key: "{index}", items: Items(sub_items), anchor: rect, submenu: true, on_close }
        }
    }
}

#[derive(Clone)]
struct ActionProp(MenuAction);

impl PartialEq for ActionProp {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0.on_select, &other.0.on_select)
    }
}

#[derive(Clone)]
struct SubmenuProp(MenuSubmenu);

impl PartialEq for SubmenuProp {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0.items, &other.0.items)
    }
}

#[component]
fn ActionRow(
    action: ActionProp,
    icon_column: bool,
    on_close: Callback<()>,
    on_hover: Callback<()>,
) -> Element {
    let action = action.0;
    let mut class = String::from("menu-row");
    if action.disabled {
        class.push_str(" disabled");
    }
    if action.danger {
        class.push_str(" danger");
    }
    let disabled = action.disabled;
    let on_select = action.on_select.clone();
    rsx! {
        div {
            class,
            role: "menuitem",
            onmouseenter: move |_| on_hover.call(()),
            onclick: move |_| {
                if !disabled {
                    on_close.call(());
                    on_select();
                }
            },
            if icon_column {
                span { class: "menu-icon-slot",
                    if action.checked == Some(true) {
                        svg { class: "menu-check", view_box: "0 0 12 8", path { d: "M0.5,4 L4,7.5 L11.5,0.5" } }
                    } else if let Some(name) = action.icon {
                        Icon { name, size: 16.0, class: "menu-icon" }
                    }
                }
            }
            span { class: "menu-label", "{action.label}" }
            if let Some(hint) = action.hint {
                span { class: "menu-hint", "{hint}" }
            }
        }
    }
}

#[component]
fn SubmenuRow(
    sub: SubmenuProp,
    icon_column: bool,
    open: bool,
    on_hover: Callback<(Rc<Vec<MenuEntry>>, Rc<MountedData>)>,
) -> Element {
    let sub = sub.0;
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let mut class = String::from("menu-row");
    if sub.disabled {
        class.push_str(" disabled");
    }
    if open {
        class.push_str(" sub-open");
    }
    let disabled = sub.disabled;
    let items = sub.items.clone();
    rsx! {
        div {
            class,
            role: "menuitem",
            aria_haspopup: "menu",
            onmounted: move |e| mounted.set(Some(e.data())),
            onmouseenter: move |_| {
                let el = mounted.peek().clone();
                if let (false, Some(el)) = (disabled, el) {
                    on_hover.call((items.clone(), el));
                }
            },
            if icon_column {
                span { class: "menu-icon-slot",
                    if let Some(name) = sub.icon {
                        Icon { name, size: 16.0, class: "menu-icon" }
                    }
                }
            }
            span { class: "menu-label", "{sub.label}" }
            svg { class: "menu-arrow", view_box: "0 0 6 10", path { d: "M0.5,0.5 L5,5 L0.5,9.5" } }
        }
    }
}

/// 右键点击区域内时打开菜单。`items` 在打开时才调用。
#[component]
pub fn ContextMenuArea(
    items: Callback<(), Vec<MenuEntry>>,
    #[props(default, into)] class: String,
    children: Element,
) -> Element {
    let overlays = use_overlays();
    rsx! {
        div {
            class,
            oncontextmenu: move |e| open_row_menu(overlays, &e, Some(items)),
            {children}
        }
    }
}

/// 列表行、文件卡片的右键菜单：没有菜单时交给外层处理。
pub(super) fn open_row_menu(
    overlays: Overlays,
    event: &MouseEvent,
    menu: Option<Callback<(), Vec<MenuEntry>>>,
) {
    if let Some(items) = menu {
        event.prevent_default();
        event.stop_propagation();
        let at = event.client_coordinates();
        overlays.show_menu(at.x, at.y, items.call(()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_column_is_shared() {
        assert!(!needs_icon_column(&[
            MenuEntry::item("a", || {}),
            MenuEntry::Separator
        ]));
        assert!(needs_icon_column(&[
            MenuEntry::item("a", || {}),
            MenuEntry::item("b", || {}).icon(IconName::Copy)
        ]));
        assert!(needs_icon_column(&[
            MenuEntry::item("a", || {}).checked(false)
        ]));
    }

    fn icon_of(entry: MenuEntry) -> Option<IconName> {
        match entry.with_default_icon() {
            MenuEntry::Action(a) => a.icon,
            MenuEntry::Submenu(s) => s.icon,
            MenuEntry::Separator => None,
        }
    }

    #[test]
    fn top_level_items_get_icons_by_name() {
        assert_eq!(
            icon_of(MenuEntry::item("复制", || {})),
            Some(IconName::Copy)
        );
        assert_eq!(
            icon_of(MenuEntry::submenu("更多", Vec::new())),
            Some(IconName::More)
        );
        assert_eq!(icon_of(MenuEntry::item("没有对照", || {})), None);
        // 显式图标、勾选项、plain 不变
        assert_eq!(
            icon_of(MenuEntry::item("复制", || {}).icon(IconName::Cut)),
            Some(IconName::Cut)
        );
        assert_eq!(icon_of(MenuEntry::item("复制", || {}).checked(true)), None);
        assert_eq!(icon_of(MenuEntry::item("复制", || {}).plain()), None);
    }
}
