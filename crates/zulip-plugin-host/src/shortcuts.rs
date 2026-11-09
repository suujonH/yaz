//! 插件的快捷操作：message_menus 绑定到对单条消息的鼠标手势，menus 绑定到键盘快捷键（只在程序窗口中有效）。
//! 绑定保存在程序设置中：`{"键": "手势"}`，空字符串表示不绑定；没有保存过的菜单项使用 plugin.json 中的默认值。

use std::collections::BTreeMap;
use std::sync::Arc;

use zulip_core::model::ChatMessage;
use zulip_session::Session;
use zulip_store::SettingRepo;

use crate::event::PluginEvent;
use crate::host::PluginHost;
use crate::manifest::{MenuContext, PluginMenu};
use crate::plugin::NativePlugin;

const SETTING_KEY: &str = "plugin_shortcuts";

// 宿主已经使用的手势：选中消息 / 选择文字、打开资料卡、右键菜单、Shift 扩展选择。
const RESERVED: [&str; 5] = [
    "left_click",
    "left_double",
    "right_click",
    "right_double",
    "shift+left_click",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// 按下的按键、次数与修饰键对应的手势；按住 Win 键或不是单击 / 双击时为 `None`。
pub fn mouse_gesture(button: MouseButton, clicks: u32, modifiers: Modifiers) -> Option<String> {
    if !(1..=2).contains(&clicks) || modifiers.meta {
        return None;
    }
    let name = match button {
        MouseButton::Left => "left",
        MouseButton::Middle => "middle",
        MouseButton::Right => "right",
    };
    let kind = if clicks == 1 { "click" } else { "double" };
    Some(format!("{}{name}_{kind}", mouse_prefix(modifiers)))
}

fn mouse_prefix(m: Modifiers) -> String {
    let mut prefix = String::new();
    for (on, text) in [(m.ctrl, "ctrl+"), (m.alt, "alt+"), (m.shift, "shift+")] {
        if on {
            prefix.push_str(text);
        }
    }
    prefix
}

fn parse_mouse(gesture: &str) -> Option<(Modifiers, &str, &str)> {
    let gesture = match gesture {
        "ctrl_click" => "ctrl+left_click",
        "alt_click" => "alt+left_click",
        other => other,
    };
    let (mods, last) = match gesture.rfind('+') {
        Some(i) => (&gesture[..i], &gesture[i + 1..]),
        None => ("", gesture),
    };
    let (button, kind) = last.split_once('_')?;
    if !matches!(button, "left" | "middle" | "right") || !matches!(kind, "click" | "double") {
        return None;
    }
    let mut modifiers = Modifiers::default();
    if !mods.is_empty() {
        for m in mods.split('+') {
            let slot = match m {
                "ctrl" => &mut modifiers.ctrl,
                "alt" => &mut modifiers.alt,
                "shift" => &mut modifiers.shift,
                _ => return None,
            };
            if std::mem::replace(slot, true) {
                return None;
            }
        }
    }
    Some((modifiers, button, kind))
}

/// 规范化鼠标手势（ctrl_click、alt_click 等同于 ctrl+left_click、alt+left_click）；格式错误时为 `None`。
pub fn normalize_mouse(gesture: &str) -> Option<String> {
    let (modifiers, button, kind) = parse_mouse(gesture)?;
    Some(format!("{}{button}_{kind}", mouse_prefix(modifiers)))
}

/// 可以绑定：格式正确且不是宿主已经使用的手势。
pub fn is_allowed_mouse(gesture: &str) -> bool {
    normalize_mouse(gesture).is_some_and(|g| !RESERVED.contains(&g.as_str()))
}

/// 显示用，例如“Ctrl+左键双击”；空字符串为“无”。
pub fn mouse_label(gesture: &str) -> String {
    let Some((m, button, kind)) = parse_mouse(gesture) else {
        return "无".to_owned();
    };
    let mut label = String::new();
    for (on, text) in [(m.ctrl, "Ctrl+"), (m.alt, "Alt+"), (m.shift, "Shift+")] {
        if on {
            label.push_str(text);
        }
    }
    label.push_str(match button {
        "left" => "左键",
        "middle" => "中键",
        _ => "右键",
    });
    label.push_str(if kind == "click" { "单击" } else { "双击" });
    label
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('F')
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
}

/// 按下的键对应的快捷键，例如 `Ctrl+Shift+T`；`key` 为键名（字母大写）。
/// 只按了修饰键，或没有 Ctrl / Alt 的普通键（F1～F24 除外）时为 `None`。
pub fn key_gesture(key: &str, modifiers: Modifiers) -> Option<String> {
    let key = if key.chars().count() == 1 {
        key.to_uppercase()
    } else {
        key.to_owned()
    };
    let modifier_key = [
        "Ctrl", "Control", "Alt", "Shift", "Meta", "Win", "Super", "None", "",
    ]
    .iter()
    .any(|m| key.eq_ignore_ascii_case(m));
    if modifier_key || (!modifiers.ctrl && !modifiers.alt && !is_function_key(&key)) {
        return None;
    }
    let mut gesture = String::new();
    for (on, text) in [
        (modifiers.ctrl, "Ctrl+"),
        (modifiers.alt, "Alt+"),
        (modifiers.shift, "Shift+"),
    ] {
        if on {
            gesture.push_str(text);
        }
    }
    gesture.push_str(&key);
    Some(gesture)
}

/// 规范化键盘快捷键；格式错误或不能绑定时为 `None`。
pub fn normalize_key(gesture: &str) -> Option<String> {
    let mut parts: Vec<&str> = gesture.split('+').map(str::trim).collect();
    let key = parts.pop()?;
    let mut modifiers = Modifiers::default();
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "alt" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            _ => return None,
        }
    }
    if !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    key_gesture(key, modifiers)
}

/// 保存绑定用的键：消息菜单为“插件 id:菜单 id”，插件菜单为“插件 id:menu:菜单 id”。
pub fn key_of(plugin_id: &str, menu_id: i32, message_menu: bool) -> String {
    if message_menu {
        format!("{plugin_id}:{menu_id}")
    } else {
        format!("{plugin_id}:menu:{menu_id}")
    }
}

/// 菜单中可以绑定快捷操作的项（子菜单展开）。
pub fn bindable(menus: &[PluginMenu]) -> Vec<&PluginMenu> {
    menus
        .iter()
        .flat_map(|m| {
            if m.is_group() {
                bindable(&m.children)
            } else if m.shortcut.is_some() {
                vec![m]
            } else {
                Vec::new()
            }
        })
        .collect()
}

/// [`bindable`] 中在这个上下文里显示的项：子菜单不显示时其中的项也不算。
fn visible_bindable<'a>(menus: &'a [PluginMenu], ctx: &MenuContext) -> Vec<&'a PluginMenu> {
    menus
        .iter()
        .filter(|m| m.visible_for(ctx))
        .flat_map(|m| {
            if m.is_group() {
                visible_bindable(&m.children, ctx)
            } else if m.shortcut.is_some() {
                vec![m]
            } else {
                Vec::new()
            }
        })
        .collect()
}

pub type Bindings = BTreeMap<String, String>;

pub(crate) fn load(settings: &SettingRepo) -> Bindings {
    settings
        .get(SETTING_KEY)
        .and_then(|json| {
            let value = serde_json::from_str::<serde_json::Value>(&json).ok()?;
            let object = value.as_object()?.clone();
            Some(
                object
                    .into_iter()
                    .filter_map(|(k, v)| Some((k, v.as_str()?.to_owned())))
                    .collect(),
            )
        })
        .unwrap_or_default()
}

pub(crate) fn save(settings: &SettingRepo, bindings: &Bindings) {
    let json = serde_json::to_string(bindings).unwrap_or_default();
    settings.set(SETTING_KEY, &json);
}

/// 菜单项当前绑定的手势 / 快捷键（规范化后；没有绑定时为空字符串）。
pub(crate) fn effective(
    bindings: &Bindings,
    plugin_id: &str,
    menu: &PluginMenu,
    message_menu: bool,
) -> String {
    let value = bindings
        .get(&key_of(plugin_id, menu.id, message_menu))
        .map(String::as_str)
        .or(menu.shortcut.as_deref())
        .unwrap_or_default();
    if value.is_empty() {
        return String::new();
    }
    let normalized = if message_menu {
        is_allowed_mouse(value)
            .then(|| normalize_mouse(value))
            .flatten()
    } else {
        normalize_key(value)
    };
    normalized.unwrap_or_default()
}

/// 快捷操作的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutRow {
    pub key: String,
    /// “插件名 · 菜单项”。
    pub title: String,
    /// 鼠标手势（消息菜单）；否则为键盘快捷键（插件菜单）。
    pub is_mouse: bool,
    /// 当前绑定（空字符串为不绑定）。
    pub gesture: String,
}

impl ShortcutRow {
    /// 显示用的绑定。
    pub fn label(&self) -> String {
        if self.is_mouse {
            mouse_label(&self.gesture)
        } else if self.gesture.is_empty() {
            "无".to_owned()
        } else {
            self.gesture.clone()
        }
    }
}

impl PluginHost {
    /// 已开启的插件可以绑定鼠标手势的消息菜单项。
    pub fn message_shortcuts(&self) -> Vec<ShortcutRow> {
        self.shortcut_rows(true)
    }

    /// 已开启的插件可以绑定键盘快捷键的插件菜单项。
    pub fn key_shortcuts(&self) -> Vec<ShortcutRow> {
        self.shortcut_rows(false)
    }

    fn shortcut_rows(&self, is_mouse: bool) -> Vec<ShortcutRow> {
        let bindings = self.shortcuts.lock().clone();
        self.plugins()
            .iter()
            .flat_map(|p| {
                let menus = if is_mouse {
                    p.active_message_menus()
                } else {
                    p.active_menus()
                };
                let id = p.id();
                let name = p.name();
                bindable(menus)
                    .into_iter()
                    .map(|m| ShortcutRow {
                        key: key_of(&id, m.id, is_mouse),
                        title: format!("{name} · {}", m.name),
                        is_mouse,
                        gesture: effective(&bindings, &id, m, is_mouse),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// 录制了新的手势 / 快捷键：宿主已经使用的鼠标手势不能绑定；同一个手势原来绑定的其他项改为不绑定。
    /// 返回要提示用户的文字。
    pub fn set_shortcut(&self, row: &ShortcutRow, gesture: &str) -> Vec<String> {
        if !gesture.is_empty() && row.is_mouse && !is_allowed_mouse(gesture) {
            return vec![format!(
                "{}已用于选中消息、打开资料卡或右键菜单，请加上 Ctrl / Alt / Shift",
                mouse_label(gesture)
            )];
        }
        let mut toasts = Vec::new();
        let others = self.shortcut_rows(row.is_mouse);
        let mut bindings = self.shortcuts.lock().clone();
        if !gesture.is_empty() {
            for other in others
                .iter()
                .filter(|o| o.key != row.key && o.gesture == gesture)
            {
                bindings.insert(other.key.clone(), String::new());
                toasts.push(format!("已取消“{}”的绑定", other.title));
            }
        }
        bindings.insert(row.key.clone(), gesture.to_owned());
        save(&SettingRepo::app(&self.db), &bindings);
        *self.shortcuts.lock() = bindings;
        self.emit(PluginEvent::PluginsChanged);
        toasts
    }

    // 绑定到手势的菜单项：按插件顺序取第一个；`ctx` 不为空时只看这个上下文中显示的项。
    fn find_shortcut(
        &self,
        gesture: &str,
        is_mouse: bool,
        ctx: Option<&MenuContext>,
    ) -> Option<(Arc<NativePlugin>, i32)> {
        let bindings = self.shortcuts.lock().clone();
        self.plugins().into_iter().find_map(|p| {
            let menus = if is_mouse {
                p.active_message_menus()
            } else {
                p.active_menus()
            };
            let id = p.id();
            let candidates = match ctx {
                Some(ctx) => visible_bindable(menus, ctx),
                None => bindable(menus),
            };
            let menu_id = candidates
                .into_iter()
                .find(|m| effective(&bindings, &id, m, is_mouse) == gesture)
                .map(|m| m.id);
            menu_id.map(|menu_id| (p.clone(), menu_id))
        })
    }

    /// 鼠标手势绑定了消息菜单项。
    pub fn has_mouse_shortcut(&self, gesture: &str) -> bool {
        self.find_shortcut(gesture, true, None).is_some()
    }

    /// 对消息使用手势：有绑定的菜单项且在 `ctx` 中显示时调用它并返回 true。
    pub fn invoke_mouse_shortcut(
        &self,
        gesture: &str,
        message: &ChatMessage,
        session: &Arc<Session>,
        ctx: &MenuContext,
    ) -> bool {
        match self.find_shortcut(gesture, true, Some(ctx)) {
            Some((plugin, menu_id)) => {
                self.invoke_message_menu(&plugin, menu_id, message, session);
                true
            }
            None => false,
        }
    }

    /// 按下键盘快捷键：绑定了插件菜单项时调用它并返回 true。
    pub fn invoke_key_shortcut(&self, gesture: &str) -> bool {
        match self.find_shortcut(gesture, false, None) {
            Some((plugin, menu_id)) => {
                self.invoke_menu(&plugin, menu_id);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{MenuConv, MenuWhen};

    #[test]
    fn hidden_menus_are_not_bindable() {
        let item = |id, conv: Option<MenuConv>| PluginMenu {
            id,
            name: "菜单".to_owned(),
            children: Vec::new(),
            shortcut: Some("ctrl+left_click".to_owned()),
            when: conv.map(|conv| MenuWhen {
                conv: Some(conv),
                ..MenuWhen::default()
            }),
        };
        let group = PluginMenu {
            id: 0,
            name: "子菜单".to_owned(),
            children: vec![item(3, None)],
            shortcut: None,
            when: Some(MenuWhen {
                conv: Some(MenuConv::Direct),
                ..MenuWhen::default()
            }),
        };
        let menus = vec![
            item(1, Some(MenuConv::Direct)),
            item(2, Some(MenuConv::Channel)),
            group,
        ];
        let ids = |conv| {
            let ctx = MenuContext {
                conv,
                ..MenuContext::default()
            };
            visible_bindable(&menus, &ctx)
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(MenuConv::Direct), [1, 3]);
        assert_eq!(ids(MenuConv::Channel), [2]);
    }

    #[test]
    fn mouse_gestures() {
        assert_eq!(
            normalize_mouse("ctrl_click").as_deref(),
            Some("ctrl+left_click")
        );
        assert_eq!(
            normalize_mouse("shift+ctrl+middle_double").as_deref(),
            Some("ctrl+shift+middle_double")
        );
        assert_eq!(normalize_mouse("ctrl+ctrl+left_click"), None);
        assert_eq!(normalize_mouse("win+left_click"), None);
        assert_eq!(normalize_mouse("left_triple"), None);
        assert!(!is_allowed_mouse("left_click"));
        assert!(!is_allowed_mouse("shift+left_click"));
        assert!(is_allowed_mouse("middle_click"));
        assert_eq!(mouse_label("ctrl+left_double"), "Ctrl+左键双击");
        assert_eq!(mouse_label(""), "无");
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            mouse_gesture(MouseButton::Left, 1, ctrl).as_deref(),
            Some("ctrl+left_click")
        );
        assert_eq!(mouse_gesture(MouseButton::Left, 3, ctrl), None);
    }

    #[test]
    fn key_gestures() {
        assert_eq!(
            normalize_key("shift+ctrl+t").as_deref(),
            Some("Ctrl+Shift+T")
        );
        assert_eq!(normalize_key("F5").as_deref(), Some("F5"));
        assert_eq!(normalize_key("T"), None);
        assert_eq!(normalize_key("Shift+T"), None);
        assert_eq!(normalize_key("Ctrl+Shift"), None);
        assert_eq!(normalize_key("F25"), None);
        assert_eq!(normalize_key("Win+T"), None);
    }

    #[test]
    fn effective_prefers_saved_binding() {
        let menu = PluginMenu {
            when: None,
            id: 1,
            name: "翻译".into(),
            children: Vec::new(),
            shortcut: Some("middle_click".into()),
        };
        let mut bindings = Bindings::new();
        assert_eq!(effective(&bindings, "a.b.c", &menu, true), "middle_click");
        bindings.insert("a.b.c:1".into(), String::new());
        assert_eq!(effective(&bindings, "a.b.c", &menu, true), "");
        bindings.insert("a.b.c:1".into(), "left_click".into());
        assert_eq!(effective(&bindings, "a.b.c", &menu, true), "");
    }
}
