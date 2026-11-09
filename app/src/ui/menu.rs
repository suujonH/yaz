//! Context menus and submenus.

use super::geometry::{Placement, Rect, client_rect, place, place_menu_with_bar, viewport};
use super::icon::{Icon, IconName};
use super::overlay::{Overlays, use_overlays};
use dioxus::dioxus_core::Task;
use dioxus::prelude::*;
use std::rc::Rc;
use std::time::Duration;

const SUBMENU_GAP: f64 = 5.0;
/// Distance a submenu's frame is shifted up so its first item is on the same row as the parent menu
/// item.
const SUBMENU_LIFT: f64 = 5.0;
const SUBMENU_DELAY_MS: u64 = 120;

/// Menu item. Icon, disabled and checked state are set by chaining.
#[derive(Clone)]
pub struct MenuAction {
    label: String,
    icon: Option<IconName>,
    /// In a top-level menu, when there is no icon one is taken from [`default_icon`] by name.
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

    /// Runs the menu item (for entry points outside the menu); submenus and separators have no
    /// action.
    pub fn select(&self) {
        if let Self::Action(a) = self {
            (a.on_select)();
        }
    }

    /// The items of a submenu.
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

    /// Do not pick an icon automatically by name (menus without icons, such as the image viewer).
    pub fn plain(mut self) -> Self {
        match &mut self {
            Self::Action(a) => a.auto_icon = false,
            Self::Submenu(s) => s.auto_icon = false,
            Self::Separator => {}
        }
        self
    }

    /// How a top-level menu is displayed: items without an icon get one by name; the icon column of
    /// checked items is left for the checkmark.
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

    /// Checked item: the checkmark is shown in the icon column and still reserves space when
    /// unchecked.
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

    /// Description text on the right (shortcut, etc.).
    pub fn hint(mut self, text: impl Into<String>) -> Self {
        if let Self::Action(a) = &mut self {
            a.hint = Some(text.into());
        }
        self
    }
}

/// Icon of a top-level menu item: one lookup by name shared by all menus. Names not in the table
/// show no icon, and the text still aligns with other items.
fn default_icon(label: &str) -> Option<IconName> {
    use IconName as I;
    Some(match label {
        // Message
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
        // File
        "打开" => I::File,
        "打开所在文件夹" | "打开文件夹" | "频道文件" => I::Folder,
        "投票" => I::Poll,
        "定位到聊天" => I::Location,
        // Conversation, topic, channel
        "置顶" | "取消置顶" => I::Backtop,
        "复制链接" | "复制频道链接" => I::Link,
        "标为未读" | "常用语" => I::ChatBubble,
        "设置免打扰" | "消息设置" => I::Notification,
        "取消免打扰" => I::NotificationOff,
        "打开独立聊天窗口" => I::Fullscreen,
        "退订频道" | "退出账号" => I::Logout,
        "频道颜色" => I::Palette,
        // Chat members, contacts
        "发送消息" | "发消息" => I::Chat,
        "@TA" => I::Mention,
        "查看资料" | "按成员" | "邀请成员" => I::User,
        "屏蔽此人" | "取消屏蔽" => I::Forbid,
        "移动联系人至" | "移动到其他分组" | "发起群聊" => I::Usergroup,
        // Chat history filter
        "按日期…" | "定时发送…" => I::Time,
        "清除筛选" => I::Close,
        // The "+" menu
        "添加分组" | "新建话题" | "创建频道" => I::Add,
        "加入频道" => I::ViewList,
        // Account
        "更多设置" => I::Setting,
        _ => return None,
    })
}

/// As long as one item has an icon or a check slot, all items reserve the icon column.
fn needs_icon_column(items: &[MenuEntry]) -> bool {
    items.iter().any(|entry| match entry {
        MenuEntry::Action(a) => a.icon.is_some() || a.checked.is_some(),
        MenuEntry::Submenu(s) => s.icon.is_some(),
        MenuEntry::Separator => false,
    })
}

/// Content of the standalone toolbar above the menu. The callback's argument is used to close the
/// menu.
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

/// The toolbar that is placed together with the menu when the menu is positioned.
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

/// The toolbar and the menu are two independent cards. The toolbar is measured first, and the menu
/// positioning computes both positions together.
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
    // Submenus do not get icons automatically
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

/// Opens the menu on right-click inside the area. `items` is called only when opened.
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
            oncontextmenu: move |e| {
                open_row_menu(overlays, &e, Some(items));
            },
            {children}
        }
    }
}

/// Context menu of list rows and file cards: when there is no menu it is left to the outer handler.
pub(super) fn open_row_menu(
    overlays: Overlays,
    event: &MouseEvent,
    menu: Option<Callback<(), Vec<MenuEntry>>>,
) -> Option<usize> {
    let items = menu?;
    event.prevent_default();
    event.stop_propagation();
    let at = event.client_coordinates();
    overlays.show_menu(at.x, at.y, items.call(()));
    overlays.menu_token()
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
        // Explicit icons, checked items and plain stay unchanged
        assert_eq!(
            icon_of(MenuEntry::item("复制", || {}).icon(IconName::Cut)),
            Some(IconName::Cut)
        );
        assert_eq!(icon_of(MenuEntry::item("复制", || {}).checked(true)), None);
        assert_eq!(icon_of(MenuEntry::item("复制", || {}).plain()), None);
    }
}
