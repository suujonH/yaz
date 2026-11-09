//! Plugin quick actions: message_menus are bound to mouse gestures on a single message, and menus
//! are bound to keyboard shortcuts (effective only in the program window). Bindings are stored in
//! the program settings: `{"key": "gesture"}`; an empty string means not bound; menu items that
//! were never saved use the default in plugin.json.

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

// Gestures already used by the host: select message / select text, open profile card, context menu,
// Shift extend selection.
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

/// The gesture for the pressed button, click count and modifiers; `None` when the Win key is held
/// or it is not a single / double click.
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

/// Normalizes a mouse gesture (ctrl_click, alt_click etc. are equivalent to ctrl+left_click,
/// alt+left_click); `None` on a format error.
pub fn normalize_mouse(gesture: &str) -> Option<String> {
    let (modifiers, button, kind) = parse_mouse(gesture)?;
    Some(format!("{}{button}_{kind}", mouse_prefix(modifiers)))
}

/// Bindable: correctly formatted and not a gesture already used by the host.
pub fn is_allowed_mouse(gesture: &str) -> bool {
    normalize_mouse(gesture).is_some_and(|g| !RESERVED.contains(&g.as_str()))
}

/// For display, e.g. the label for Ctrl + left-button double-click; an empty string is shown as the
/// label for "none".
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

/// The shortcut for the pressed key, e.g. `Ctrl+Shift+T`; `key` is the key name (letters
/// uppercase). `None` when only modifier keys were pressed, or a plain key without Ctrl / Alt
/// (except F1 to F24).
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

/// Normalizes a keyboard shortcut; `None` on a format error or when it cannot be bound.
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

/// Key for storing a binding: "plugin id:menu id" for message menus, "plugin id:menu:menu id" for
/// plugin menus.
pub fn key_of(plugin_id: &str, menu_id: i32, message_menu: bool) -> String {
    if message_menu {
        format!("{plugin_id}:{menu_id}")
    } else {
        format!("{plugin_id}:menu:{menu_id}")
    }
}

/// Items in menus that can have a quick action bound (submenus expanded).
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

/// Items of [`bindable`] that are shown in this context: items inside a submenu that is not shown
/// do not count either.
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

/// The gesture / shortcut currently bound to a menu item (normalized; an empty string when not
/// bound).
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

/// A row of quick actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutRow {
    pub key: String,
    /// "Plugin name <middle dot> menu item".
    pub title: String,
    /// A mouse gesture (message menu); otherwise a keyboard shortcut (plugin menu).
    pub is_mouse: bool,
    /// The current binding (an empty string means not bound).
    pub gesture: String,
}

impl ShortcutRow {
    /// The binding for display.
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
    /// Message menu items of enabled plugins that can have a mouse gesture bound.
    pub fn message_shortcuts(&self) -> Vec<ShortcutRow> {
        self.shortcut_rows(true)
    }

    /// Plugin menu items of enabled plugins that can have a keyboard shortcut bound.
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

    /// A new gesture / shortcut was recorded: a mouse gesture already used by the host cannot be
    /// bound; other items previously bound to the same gesture become unbound. Returns the text to
    /// show to the user.
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

    // Menu item bound to the gesture: take the first in plugin order; when `ctx` is non-null, only
    // consider items shown in this context.
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

    /// A mouse gesture is bound to a message menu item.
    pub fn has_mouse_shortcut(&self, gesture: &str) -> bool {
        self.find_shortcut(gesture, true, None).is_some()
    }

    /// Uses a gesture on a message: if a menu item is bound and shown in `ctx`, calls it and
    /// returns true.
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

    /// A keyboard shortcut was pressed: if a plugin menu item is bound, calls it and returns true.
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
