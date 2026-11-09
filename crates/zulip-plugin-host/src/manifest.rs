//! 插件目录中的 plugin.json。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use zulip_plugin_sdk::str_of;

use crate::launch::LaunchRule;
use crate::panel::{PluginPanel, PluginTargetKind};
use crate::permission;
use crate::setting_field::{PluginSettingField, PluginSettingType};
use crate::shortcuts;

/// 只监控、不能拦截的优先级。
pub const MONITOR_PRIORITY: i64 = 10000;
pub const MAX_PRIORITY: i64 = 40000;

/// message_menus 子菜单最多嵌套的层数。
const MAX_MENU_DEPTH: usize = 2;

pub const ON_MESSAGE: &str = "on_message";
pub const FILTER_INCOMING: &str = "filter_incoming";
pub const FILTER_OUTGOING: &str = "filter_outgoing";

pub const ON_MESSAGE_EDITED: &str = "on_message_edited";
pub const ON_MESSAGE_DELETED: &str = "on_message_deleted";
pub const ON_REACTION_CHANGED: &str = "on_reaction_changed";
pub const ON_READ_STATE_CHANGED: &str = "on_read_state_changed";
pub const ON_TOPIC_MOVED: &str = "on_topic_moved";
pub const ON_CHANNEL_CHANGED: &str = "on_channel_changed";
pub const ON_USER_CHANGED: &str = "on_user_changed";
pub const ON_PRESENCE_CHANGED: &str = "on_presence_changed";
pub const ON_CONVERSATION_OPENED: &str = "on_conversation_opened";
pub const ON_CONNECTION_CHANGED: &str = "on_connection_changed";
pub const ON_TYPING_CHANGED: &str = "on_typing_changed";
pub const ON_SCHEDULED_CHANGED: &str = "on_scheduled_changed";
pub const ON_REMINDERS_CHANGED: &str = "on_reminders_changed";
pub const ON_DRAFT_CHANGED: &str = "on_draft_changed";
pub const ON_USER_TOPIC_CHANGED: &str = "on_user_topic_changed";
pub const ON_MUTED_USERS_CHANGED: &str = "on_muted_users_changed";
pub const ON_REALM_CHANGED: &str = "on_realm_changed";

/// 通知型事件：导出函数名为 `zulip_plugin_<事件>`。（事件，权限，确认框中的说明）
pub(crate) const NOTIFICATIONS: [(&str, &str, &str); 17] = [
    (ON_MESSAGE_EDITED, permission::READ_MESSAGE, "消息被编辑"),
    (ON_MESSAGE_DELETED, permission::READ_MESSAGE, "消息被删除"),
    (
        ON_REACTION_CHANGED,
        permission::READ_MESSAGE,
        "表情回应变化",
    ),
    (
        ON_READ_STATE_CHANGED,
        permission::READ_MESSAGE,
        "已读状态变化",
    ),
    (ON_TOPIC_MOVED, permission::READ_MESSAGE, "话题改名或移动"),
    (ON_CHANNEL_CHANGED, permission::READ_CONTACTS, "频道变化"),
    (ON_USER_CHANGED, permission::READ_CONTACTS, "用户资料变化"),
    (
        ON_PRESENCE_CHANGED,
        permission::READ_CONTACTS,
        "在线状态变化",
    ),
    (
        ON_CONVERSATION_OPENED,
        permission::READ_MESSAGE,
        "切换到的会话",
    ),
    (
        ON_CONNECTION_CHANGED,
        permission::READ_MESSAGE,
        "连接断开与恢复",
    ),
    (ON_TYPING_CHANGED, permission::READ_MESSAGE, "对方正在输入"),
    (
        ON_SCHEDULED_CHANGED,
        permission::READ_MESSAGE,
        "定时消息变化",
    ),
    (
        ON_REMINDERS_CHANGED,
        permission::READ_MESSAGE,
        "消息提醒变化",
    ),
    (ON_DRAFT_CHANGED, permission::READ_MESSAGE, "草稿变化"),
    (
        ON_USER_TOPIC_CHANGED,
        permission::READ_MESSAGE,
        "话题关注状态变化",
    ),
    (
        ON_MUTED_USERS_CHANGED,
        permission::READ_CONTACTS,
        "静音用户变化",
    ),
    (ON_REALM_CHANGED, permission::READ_CONTACTS, "组织设置变化"),
];

/// 接收事件需要的权限。
pub fn event_permission(event: &str) -> Option<&'static str> {
    match event {
        ON_MESSAGE | FILTER_INCOMING => Some(permission::READ_MESSAGE),
        FILTER_OUTGOING => Some(permission::MODIFY_MESSAGE),
        _ => NOTIFICATIONS
            .iter()
            .find(|(name, ..)| *name == event)
            .map(|(_, permission, _)| *permission),
    }
}

/// 开启确认中事件的说明。
pub(crate) fn event_title(event: &str) -> &str {
    match event {
        ON_MESSAGE => "接收新消息",
        FILTER_INCOMING => "拦截收到的消息",
        FILTER_OUTGOING => "发送前检查消息（含文件和转发）",
        _ => NOTIFICATIONS
            .iter()
            .find(|(name, ..)| *name == event)
            .map_or(event, |(_, _, title)| *title),
    }
}

/// composer_buttons 可以使用的内置图标。
pub const BUILTIN_ICONS: [&str; 14] = [
    "add", "chat", "check", "edit", "file", "folder", "history", "image", "lock", "more", "search",
    "smile", "time", "user",
];
const DEFAULT_ICON: &str = "more";

/// 工具栏按钮的图标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerIcon {
    /// [`BUILTIN_ICONS`] 之一。
    Builtin(String),
    /// 插件目录内的 SVG 文件（相对路径）。
    Svg(String),
}

/// plugin.json 的 composer_buttons 中的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginComposerButton {
    pub id: i32,
    pub icon: ComposerIcon,
    pub tooltip: String,
}

/// 校验失败的原因（直接显示给用户）。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ManifestError(pub String);

/// 插件菜单项；`children` 不为空时是子菜单（点击子菜单本身不调用插件）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginMenu {
    pub id: i32,
    pub name: String,
    pub children: Vec<PluginMenu>,
    /// `None` 表示不可绑定快捷操作，空字符串表示可绑定、没有默认值，否则为默认值（已规范化）。
    pub shortcut: Option<String>,
    /// 显示条件（message_menus、conversation_menus）；`None` 为总是显示。
    pub when: Option<MenuWhen>,
}

impl PluginMenu {
    pub fn is_group(&self) -> bool {
        !self.children.is_empty()
    }

    /// 在这个上下文中是否显示。子菜单还要求至少一个子项显示。
    pub fn visible_for(&self, ctx: &MenuContext) -> bool {
        self.when.as_ref().is_none_or(|when| when.matches(ctx))
            && (!self.is_group() || self.children.iter().any(|c| c.visible_for(ctx)))
    }
}

/// 菜单条件中的会话种类。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuConv {
    #[default]
    Channel,
    Direct,
}

/// plugin.json 中菜单项的 when；没有写的条件不限制。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuWhen {
    pub conv: Option<MenuConv>,
    pub own: Option<bool>,
    pub has_uploads: Option<bool>,
    pub can_edit: Option<bool>,
}

impl MenuWhen {
    fn matches(&self, ctx: &MenuContext) -> bool {
        self.conv.is_none_or(|conv| conv == ctx.conv)
            && self.own.is_none_or(|own| own == ctx.own)
            && self
                .has_uploads
                .is_none_or(|has_uploads| has_uploads == ctx.has_uploads)
            && self
                .can_edit
                .is_none_or(|can_edit| can_edit == ctx.can_edit)
    }
}

/// 右键菜单的上下文。会话菜单没有消息，`own`、`has_uploads`、`can_edit` 都为 false。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuContext {
    pub conv: MenuConv,
    pub own: bool,
    pub has_uploads: bool,
    pub can_edit: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub api_version: u32,
    pub library: String,
    pub permissions: BTreeSet<String>,
    /// 声明的事件及其优先级（数字越小越先执行）。
    pub events: BTreeMap<String, i64>,
    pub menus: Vec<PluginMenu>,
    pub message_menus: Vec<PluginMenu>,
    pub launch: Vec<LaunchRule>,
    pub settings: Vec<PluginSettingField>,
    pub dependencies: Vec<String>,
    pub panels: Vec<PluginPanel>,
    pub conversation_menus: Vec<PluginMenu>,
    pub user_menus: Vec<PluginMenu>,
    pub channel_menus: Vec<PluginMenu>,
    pub composer_buttons: Vec<PluginComposerButton>,
}

impl PluginManifest {
    /// 解析并校验。`directory_name` 为插件所在目录名，必须与 id 相同；
    /// 不认识的权限和事件忽略，并通过 `warn` 报告。
    pub fn parse(
        json: &str,
        directory_name: &str,
        host_api_version: u32,
        warn: &mut dyn FnMut(String),
    ) -> Result<Self, ManifestError> {
        let root: Value = serde_json::from_str(&strip_jsonc(json))
            .map_err(|e| ManifestError(format!("plugin.json 格式错误：{e}")))?;
        if !root.is_object() {
            return Err(ManifestError("plugin.json 必须是一个对象".to_owned()));
        }

        let id = required_string(&root, "id")?;
        if !is_valid_id(id) {
            return Err(ManifestError(format!(
                "id 不合法：{id}（只能包含小写字母、数字、点、下划线、减号，3～64 个字符）"
            )));
        }
        if id != directory_name {
            return Err(ManifestError(format!(
                "id（{id}）与插件目录名（{directory_name}）不一致"
            )));
        }

        let api_version = root["api_version"]
            .as_i64()
            .ok_or_else(|| ManifestError("缺少 api_version".to_owned()))?;
        if api_version < 1 || api_version > i64::from(host_api_version) {
            return Err(ManifestError(format!(
                "不支持的 api_version：{api_version}（宿主版本 {host_api_version}）"
            )));
        }

        let library = required_string(&root, "library")?;
        if library.contains(['/', '\\', ':']) || library == "." || library == ".." {
            return Err(ManifestError(format!("library 只能是文件名：{library}")));
        }

        let mut permissions = BTreeSet::new();
        for p in strings(&root["permissions"]) {
            if permission::find(p).is_some() {
                permissions.insert(p.to_owned());
            } else {
                warn(format!("忽略未知权限：{p}"));
            }
        }

        let mut events = BTreeMap::new();
        if let Some(object) = root["events"].as_object() {
            for (name, value) in object {
                if event_permission(name).is_none() {
                    warn(format!("忽略未知事件：{name}"));
                    continue;
                }
                let priority = value
                    .as_i64()
                    .filter(|p| (MONITOR_PRIORITY..=MAX_PRIORITY).contains(p))
                    .ok_or_else(|| {
                        ManifestError(format!(
                            "事件 {name} 的优先级必须是 {MONITOR_PRIORITY}～{MAX_PRIORITY} 的整数"
                        ))
                    })?;
                events.insert(name.clone(), priority);
            }
        }

        let menus = parse_menu_list(&root["menus"], warn, 0, true, false);
        let message_menus =
            parse_menu_list(&root["message_menus"], warn, MAX_MENU_DEPTH, false, true);
        if !message_menus.is_empty() && !permissions.contains(permission::READ_MESSAGE) {
            warn("声明了 message_menus 但没有声明 read_message 权限，消息菜单不会显示".to_owned());
        }

        let conversation_menus = parse_menu_list(
            &root["conversation_menus"],
            warn,
            MAX_MENU_DEPTH,
            false,
            true,
        );
        let user_menus = parse_menu_list(&root["user_menus"], warn, MAX_MENU_DEPTH, false, false);
        let channel_menus =
            parse_menu_list(&root["channel_menus"], warn, MAX_MENU_DEPTH, false, false);
        if !conversation_menus.is_empty() && !permissions.contains(permission::READ_MESSAGE) {
            warn("声明了 conversation_menus 但没有声明 read_message 权限，菜单不会显示".to_owned());
        }
        if (!user_menus.is_empty() || !channel_menus.is_empty())
            && !permissions.contains(permission::READ_CONTACTS)
        {
            warn(
                "声明了 user_menus 或 channel_menus 但没有声明 read_contacts 权限，菜单不会显示"
                    .to_owned(),
            );
        }
        let composer_buttons = parse_composer_buttons(&root["composer_buttons"], warn);
        if !composer_buttons.is_empty() && !permissions.contains(permission::READ_MESSAGE) {
            warn("声明了 composer_buttons 但没有声明 read_message 权限，按钮不会显示".to_owned());
        }

        let mut launch = Vec::new();
        for pattern in strings(&root["launch"]) {
            match LaunchRule::parse(pattern) {
                Ok(rule) => launch.push(rule),
                Err(error) => warn(format!("忽略白名单项：{error}")),
            }
        }
        if !launch.is_empty() && !permissions.contains(permission::LAUNCH) {
            warn("声明了 launch 白名单但没有声明 launch 权限，白名单不会生效".to_owned());
        }

        let mut dependencies: Vec<String> = Vec::new();
        for d in strings(&root["dependencies"]) {
            if !is_valid_id(d) || d == id {
                warn(format!("忽略依赖：{d}"));
            } else if !dependencies.iter().any(|x| x == d) {
                dependencies.push(d.to_owned());
            }
        }

        let mut panels: Vec<PluginPanel> = Vec::new();
        for p in root["panels"].as_array().into_iter().flatten() {
            let kind = PluginTargetKind::parse(str_of(p, "kind"));
            let label = str_of(p, "label");
            match kind {
                Some(kind) if !label.is_empty() && panels.iter().all(|x| x.kind != kind) => {
                    panels.push(PluginPanel {
                        kind,
                        label: label.to_owned(),
                    });
                }
                _ => warn("忽略格式错误或重复的面板".to_owned()),
            }
        }

        let settings = PluginSettingField::parse_list(&root["settings"], warn, true);
        let resource_plugins: BTreeSet<&str> = settings
            .iter()
            .filter(|f| f.kind == PluginSettingType::Resource)
            .map(|f| f.resource_plugin.as_str())
            .collect();
        for plugin in resource_plugins {
            if !dependencies.iter().any(|d| d == plugin) {
                warn(format!(
                    "声明了 resource 设置项但 dependencies 中没有 {plugin}，无法使用选择的资源"
                ));
            }
        }

        Ok(Self {
            id: id.to_owned(),
            name: required_string(&root, "name")?.to_owned(),
            version: str_of(&root, "version").to_owned(),
            author: str_of(&root, "author").to_owned(),
            description: str_of(&root, "description").to_owned(),
            api_version: api_version as u32,
            library: library.to_owned(),
            permissions,
            events,
            menus,
            message_menus,
            launch,
            settings,
            dependencies,
            panels,
            conversation_menus,
            user_menus,
            channel_menus,
            composer_buttons,
        })
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.contains(permission)
    }
}

fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (3..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn required_string<'a>(root: &'a Value, name: &str) -> Result<&'a str, ManifestError> {
    match str_of(root, name) {
        "" => Err(ManifestError(format!("缺少 {name}"))),
        text => Ok(text),
    }
}

fn strings(value: &Value) -> impl Iterator<Item = &str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn parse_when(menu: &Value, name: &str, warn: &mut dyn FnMut(String)) -> Option<MenuWhen> {
    let value = menu.get("when")?;
    let Some(object) = value.as_object() else {
        warn(format!("忽略菜单 {name} 格式错误的 when"));
        return None;
    };
    let mut when = MenuWhen::default();
    for (key, condition) in object {
        let flag = condition.as_bool();
        let known = match key.as_str() {
            "conv" => {
                when.conv = match condition.as_str() {
                    Some("channel") => Some(MenuConv::Channel),
                    Some("direct") => Some(MenuConv::Direct),
                    _ => None,
                };
                when.conv.is_some()
            }
            "own" => {
                when.own = flag;
                flag.is_some()
            }
            "has_uploads" => {
                when.has_uploads = flag;
                flag.is_some()
            }
            "can_edit" => {
                when.can_edit = flag;
                flag.is_some()
            }
            _ => false,
        };
        if !known {
            warn(format!("忽略菜单 {name} 的 when 中未知的条件：{key}"));
        }
    }
    Some(when)
}

// depth：还允许嵌套的层数。keyboard：shortcut 为键盘快捷键（menus），否则为鼠标手势（其余菜单）。
// conditional：支持 when。
fn parse_menu_list(
    menus: &Value,
    warn: &mut dyn FnMut(String),
    depth: usize,
    keyboard: bool,
    conditional: bool,
) -> Vec<PluginMenu> {
    let mut result = Vec::new();
    for m in menus.as_array().into_iter().flatten() {
        let name = str_of(m, "name");
        if name.is_empty() {
            warn("忽略格式错误的菜单项".to_owned());
            continue;
        }
        let when = if conditional {
            parse_when(m, name, warn)
        } else {
            if m.get("when").is_some() {
                warn(format!("忽略菜单 {name} 的 when（这类菜单不支持）"));
            }
            None
        };
        if let Some(children) = m.get("children") {
            if depth == 0 {
                warn(format!("忽略子菜单（不支持或嵌套过深）：{name}"));
                continue;
            }
            let items = parse_menu_list(children, warn, depth - 1, keyboard, conditional);
            if !items.is_empty() {
                result.push(PluginMenu {
                    id: 0,
                    name: name.to_owned(),
                    children: items,
                    shortcut: None,
                    when,
                });
            }
        } else if let Some(id) = m["id"].as_i64().and_then(|i| i32::try_from(i).ok()) {
            result.push(PluginMenu {
                id,
                name: name.to_owned(),
                children: Vec::new(),
                shortcut: parse_shortcut(m, name, warn, keyboard),
                when,
            });
        } else {
            warn("忽略格式错误的菜单项".to_owned());
        }
    }
    result
}

fn parse_composer_buttons(
    buttons: &Value,
    warn: &mut dyn FnMut(String),
) -> Vec<PluginComposerButton> {
    let mut result: Vec<PluginComposerButton> = Vec::new();
    for b in buttons.as_array().into_iter().flatten() {
        let id = b["id"].as_i64().and_then(|i| i32::try_from(i).ok());
        let tooltip = str_of(b, "tooltip");
        let Some(id) = id.filter(|id| !tooltip.is_empty() && result.iter().all(|x| x.id != *id))
        else {
            warn("忽略格式错误或重复的工具栏按钮".to_owned());
            continue;
        };
        result.push(PluginComposerButton {
            id,
            icon: parse_icon(str_of(b, "icon"), warn),
            tooltip: tooltip.to_owned(),
        });
    }
    result
}

fn parse_icon(icon: &str, warn: &mut dyn FnMut(String)) -> ComposerIcon {
    if BUILTIN_ICONS.contains(&icon) {
        return ComposerIcon::Builtin(icon.to_owned());
    }
    if is_relative_svg(icon) {
        return ComposerIcon::Svg(icon.to_owned());
    }
    warn(format!("工具栏按钮的图标不可用：{icon}"));
    ComposerIcon::Builtin(DEFAULT_ICON.to_owned())
}

// 只能是插件目录内的 SVG。
fn is_relative_svg(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".svg")
        && !path.contains([':', '\\'])
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

// 格式错误或不能绑定的默认值当作没有默认值。
fn parse_shortcut(
    menu: &Value,
    name: &str,
    warn: &mut dyn FnMut(String),
    keyboard: bool,
) -> Option<String> {
    let value = menu.get("shortcut")?;
    let gesture = value.as_str().unwrap_or_default();
    if gesture.is_empty() {
        return Some(String::new());
    }
    let normalized = if keyboard {
        shortcuts::normalize_key(gesture)
    } else if shortcuts::is_allowed_mouse(gesture) {
        shortcuts::normalize_mouse(gesture)
    } else {
        None
    };
    Some(normalized.unwrap_or_else(|| {
        warn(format!("忽略菜单 {name} 不能使用的快捷操作：{gesture}"));
        String::new()
    }))
}

// plugin.json 允许注释和结尾逗号。
fn strip_jsonc(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut chars = json.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                out.extend(chars.next());
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = ' ';
                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
            }
            ('}' | ']', _) => {
                let end = out.trim_end().len();
                if out[..end].ends_with(',') {
                    out.truncate(end - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ID: &str = "com.example.test";

    fn parse(value: Value) -> Result<(PluginManifest, Vec<String>), ManifestError> {
        let mut warnings = Vec::new();
        let manifest = PluginManifest::parse(&value.to_string(), ID, 1, &mut |w| warnings.push(w))?;
        Ok((manifest, warnings))
    }

    fn base() -> Value {
        json!({"id": ID, "name": "测试", "api_version": 1, "library": "test.dll"})
    }

    fn with(key: &str, value: Value) -> Value {
        let mut root = base();
        root[key] = value;
        root
    }

    #[test]
    fn parses_full_manifest() {
        let json = r#"{
            // 注释
            "id": "com.example.test", "name": "测试", "version": "1.2", "author": "A",
            "description": "说明 // 不是注释", "api_version": 1, "library": "test.dll",
            "permissions": ["read_message", "launch", "nope"],
            "events": {"on_message": 20000, "unknown": 1},
            /* 块注释 */
            "menus": [{"id": 1, "name": "打开", "shortcut": "ctrl+shift+t"}],
            "message_menus": [{"name": "组", "children": [{"id": 2, "name": "翻译", "shortcut": "ctrl_click"}]}],
            "launch": ["todo:", "https:"],
            "dependencies": ["com.example.dep", "com.example.dep", "com.example.test"],
            "panels": [{"kind": "user", "label": "身份"}, {"kind": "user", "label": "重复"}],
            "settings": [{"key": "a"},],
        }"#;
        let mut warnings = Vec::new();
        let m = PluginManifest::parse(json, ID, 1, &mut |w| warnings.push(w)).unwrap();
        assert_eq!(m.description, "说明 // 不是注释");
        assert_eq!(m.permissions.len(), 2);
        assert_eq!(m.events[ON_MESSAGE], 20000);
        assert_eq!(m.menus[0].shortcut.as_deref(), Some("Ctrl+Shift+T"));
        assert!(m.message_menus[0].is_group());
        assert_eq!(
            m.message_menus[0].children[0].shortcut.as_deref(),
            Some("ctrl+left_click")
        );
        assert_eq!(m.launch.len(), 1);
        assert_eq!(m.dependencies, vec!["com.example.dep"]);
        assert_eq!(m.panels.len(), 1);
        assert_eq!(m.settings.len(), 1);
        for expected in [
            "未知权限：nope",
            "未知事件：unknown",
            "白名单项",
            "依赖：com.example.test",
            "面板",
        ] {
            assert!(warnings.iter().any(|w| w.contains(expected)), "{expected}");
        }
    }

    #[test]
    fn rejects_invalid() {
        let error = |value: Value| parse(value).err().map(|e| e.0).unwrap_or_default();
        assert!(error(with("id", json!("UPPER"))).contains("id 不合法"));
        assert!(error(with("id", json!("com.other.test"))).contains("目录名"));
        assert!(error(with("api_version", json!(2))).contains("api_version"));
        assert!(error(with("api_version", json!(0))).contains("api_version"));
        assert!(error(with("library", json!("../a.dll"))).contains("文件名"));
        assert!(error(with("name", json!(""))).contains("缺少 name"));
        assert!(error(with("events", json!({"on_message": 5}))).contains("优先级"));
        assert!(error(json!([])).contains("对象"));
    }

    #[test]
    fn warns_about_mismatches() {
        let (m, warnings) = parse(with(
            "message_menus",
            json!([
                {"id": 1, "name": "a"},
                {"id": 2, "name": "b", "shortcut": "left_click"},
                {"name": "没有 id"},
            ]),
        ))
        .unwrap();
        assert_eq!(m.message_menus.len(), 2);
        assert_eq!(m.message_menus[0].shortcut, None);
        assert_eq!(m.message_menus[1].shortcut.as_deref(), Some(""));
        assert_eq!(warnings.len(), 3);

        let (_, warnings) = parse(with(
            "settings",
            json!([{"key": "m", "type": "resource", "resource": "model", "plugin": "a.b.c"}]),
        ))
        .unwrap();
        assert!(warnings[0].contains("dependencies"));
    }

    #[test]
    fn parses_notification_events() {
        let (m, warnings) = parse(with(
            "events",
            json!({"on_message_edited": 20000, "on_presence_changed": 10000, "on_x": 20000}),
        ))
        .unwrap();
        assert_eq!(m.events.len(), 2);
        assert!(warnings[0].contains("未知事件：on_x"));
        assert_eq!(
            event_permission(ON_PRESENCE_CHANGED),
            Some(permission::READ_CONTACTS)
        );
        assert_eq!(event_title(ON_TOPIC_MOVED), "话题改名或移动");
        assert!(parse(with("events", json!({"on_topic_moved": 1}))).is_err());
    }

    #[test]
    fn parses_context_menus_and_composer_buttons() {
        let mut root = base();
        root["permissions"] = json!(["read_message", "read_contacts"]);
        root["user_menus"] = json!([{"name": "组", "children": [{"id": 3, "name": "看"}]}]);
        root["channel_menus"] = json!([{"id": 4, "name": "看频道"}]);
        root["conversation_menus"] = json!([{"id": 5, "name": "看会话"}]);
        root["composer_buttons"] = json!([
            {"id": 1, "icon": "edit", "tooltip": "内置"},
            {"id": 2, "icon": "icons/a.SVG", "tooltip": "文件"},
            {"id": 2, "icon": "edit", "tooltip": "重复"},
            {"id": 6, "icon": "../a.svg", "tooltip": "跳出"},
            {"id": 7, "icon": "C:\\a.svg", "tooltip": "绝对"},
            {"id": 8, "icon": "edit"},
        ]);
        let (m, warnings) = parse(root).unwrap();
        assert_eq!(m.user_menus[0].children[0].id, 3);
        assert_eq!(m.channel_menus[0].id, 4);
        assert_eq!(m.conversation_menus[0].id, 5);
        let icons: Vec<&ComposerIcon> = m.composer_buttons.iter().map(|b| &b.icon).collect();
        assert_eq!(
            icons,
            vec![
                &ComposerIcon::Builtin("edit".into()),
                &ComposerIcon::Svg("icons/a.SVG".into()),
                &ComposerIcon::Builtin("more".into()),
                &ComposerIcon::Builtin("more".into()),
            ]
        );
        assert_eq!(warnings.len(), 4);

        let (_, warnings) = parse(with("user_menus", json!([{"id": 1, "name": "看"}]))).unwrap();
        assert!(warnings[0].contains("read_contacts"));
    }

    #[test]
    fn parses_menu_conditions() {
        let (m, warnings) = parse(with(
            "message_menus",
            json!([
                {"id": 1, "name": "a", "when": {"conv": "direct", "own": true, "has_uploads": false, "can_edit": true}},
                {"id": 2, "name": "b", "when": {"conv": "x", "own": "是", "nope": 1}},
                {"id": 3, "name": "c", "when": 1},
                {"id": 4, "name": "d"},
            ]),
        ))
        .unwrap();
        assert_eq!(
            m.message_menus[0].when,
            Some(MenuWhen {
                conv: Some(MenuConv::Direct),
                own: Some(true),
                has_uploads: Some(false),
                can_edit: Some(true),
            })
        );
        assert_eq!(m.message_menus[1].when, Some(MenuWhen::default()));
        assert_eq!(m.message_menus[2].when, None);
        assert_eq!(m.message_menus[3].when, None);
        for expected in [
            "未知的条件：conv",
            "未知的条件：own",
            "未知的条件：nope",
            "格式错误的 when",
        ] {
            assert!(warnings.iter().any(|w| w.contains(expected)), "{expected}");
        }

        let (m, warnings) = parse(with(
            "user_menus",
            json!([{"id": 1, "name": "a", "when": {"own": true}}]),
        ))
        .unwrap();
        assert_eq!(m.user_menus[0].when, None);
        assert!(warnings.iter().any(|w| w.contains("不支持")));
    }

    #[test]
    fn menu_visibility() {
        let item = |when: Option<MenuWhen>| PluginMenu {
            id: 1,
            name: "a".to_owned(),
            children: Vec::new(),
            shortcut: None,
            when,
        };
        let ctx = MenuContext {
            conv: MenuConv::Direct,
            own: true,
            has_uploads: false,
            can_edit: true,
        };
        assert!(item(None).visible_for(&ctx));
        assert!(item(Some(MenuWhen::default())).visible_for(&ctx));
        let cases = [
            (Some(MenuConv::Direct), None, None, None, true),
            (Some(MenuConv::Channel), None, None, None, false),
            (None, Some(true), None, None, true),
            (None, Some(false), None, None, false),
            (None, None, Some(false), None, true),
            (None, None, Some(true), None, false),
            (None, None, None, Some(true), true),
            (None, None, None, Some(false), false),
            (Some(MenuConv::Direct), Some(true), Some(true), None, false),
        ];
        for (conv, own, has_uploads, can_edit, expected) in cases {
            let when = MenuWhen {
                conv,
                own,
                has_uploads,
                can_edit,
            };
            assert_eq!(
                item(Some(when.clone())).visible_for(&ctx),
                expected,
                "{when:?}"
            );
        }

        // 子菜单：所有子项都不显示时不显示。
        let hidden = MenuWhen {
            own: Some(false),
            ..MenuWhen::default()
        };
        let group = |children| PluginMenu {
            id: 0,
            name: "组".to_owned(),
            children,
            shortcut: None,
            when: None,
        };
        assert!(!group(vec![item(Some(hidden.clone()))]).visible_for(&ctx));
        assert!(group(vec![item(Some(hidden)), item(None)]).visible_for(&ctx));
    }

    #[test]
    fn menus_do_not_nest() {
        let (m, warnings) = parse(with(
            "menus",
            json!([{"name": "组", "children": [{"id": 1, "name": "x"}]}]),
        ))
        .unwrap();
        assert!(m.menus.is_empty());
        assert!(warnings[0].contains("子菜单"));
    }
}
