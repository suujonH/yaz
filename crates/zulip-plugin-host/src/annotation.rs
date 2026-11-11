//! Content a plugin attaches to a message (set_annotation): valid only for this run; the host does
//! not persist it. Results that must be kept long-term are saved by the plugin itself and
//! re-attached in on_messages_shown.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;
use zulip_plugin_sdk::{abi, str_of};

use crate::event::PluginEvent;
use crate::host::PluginHost;
use crate::host_api::HOST_CATEGORY;
use crate::launch;
use crate::markup::{markdown_html, plain_text_html};
use crate::permission;
use crate::plugin::NativePlugin;

pub const MAX_TEXT_LENGTH: usize = 4000;
pub const MAX_REPLACE_LENGTH: usize = 20000;
pub const MAX_ACTIONS: usize = 5;
pub const MAX_LABEL_LENGTH: usize = 40;
pub const MAX_TITLE_LENGTH: usize = 200;
pub const MAX_TAGS: usize = 6;
pub const MAX_TAG_LENGTH: usize = 20;
pub const MAX_FIELDS: usize = 12;
pub const MAX_REPLIES: usize = 5;
pub const MAX_TOOLS: usize = 3;
pub const MAX_FIELD_ITEMS: usize = 8;
/// A tool with this icon is the card's close button (the X in the header).
pub const CLOSE_ICON: &str = "close";

/// Color of a tag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TagTone {
    #[default]
    Neutral,
    Success,
    Warning,
    Danger,
    /// Blue.
    Info,
    /// Purple.
    Accent,
}

impl TagTone {
    /// Unknown names are neutral.
    pub fn parse(name: &str) -> Self {
        match name {
            "success" => Self::Success,
            "warning" => Self::Warning,
            "danger" => Self::Danger,
            "info" => Self::Info,
            "accent" => Self::Accent,
            _ => Self::Neutral,
        }
    }

    /// CSS class suffix.
    pub fn class(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Danger => "danger",
            Self::Info => "info",
            Self::Accent => "accent",
        }
    }
}

/// A short colored label at the top of the card (e.g. a risk level).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationTag {
    pub text: String,
    pub tone: TagTone,
    /// Shown on hover.
    pub tip: String,
}

/// An icon button in the card header: clicking calls the plugin's on_message_menu with `menu` and
/// the message the card is attached to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationTool {
    pub menu: i32,
    /// Built-in icon name (the same names as composer_buttons).
    pub icon: String,
    pub tooltip: String,
}

/// A labeled line of the card: plain text, or `items` shown as chips colored by `tone` (the text
/// comes first when both are given).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationField {
    pub label: String,
    pub value: String,
    pub items: Vec<String>,
    pub tone: TagTone,
}

/// Button style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActionStyle {
    Primary,
    #[default]
    Secondary,
}

/// A button in the attached content: when `fill` is non-empty, clicking fills the composer (without
/// sending); otherwise it opens `target`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginAction {
    pub label: String,
    pub target: String,
    pub args: String,
    pub fill: String,
    pub style: ActionStyle,
}

impl PluginAction {
    pub fn is_fill(&self) -> bool {
        !self.fill.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginAnnotation {
    pub plugin_id: String,
    pub tags: Vec<AnnotationTag>,
    /// Headline in bold.
    pub title: String,
    pub fields: Vec<AnnotationField>,
    pub text: String,
    /// Heading above the candidate replies.
    pub replies_title: String,
    /// Candidate texts shown in full; clicking one fills it into the composer (all are fill actions).
    pub replies: Vec<PluginAction>,
    pub actions: Vec<PluginAction>,
    /// Icon buttons in the header (only shown when there is a card).
    pub tools: Vec<AnnotationTool>,
    /// Zulip Markdown that temporarily replaces the bubble content (the client renders it through
    /// the server like a message).
    pub replace: String,
}

impl PluginAnnotation {
    /// Parses the JSON passed by the plugin; returns the reason on a format error or limit
    /// violation, and `Ok(None)` when there is no content.
    pub fn parse(plugin_id: &str, json: &str) -> Result<Option<Self>, String> {
        let root: Value = serde_json::from_str(json).map_err(|e| format!("JSON 格式错误：{e}"))?;
        if !root.is_object() {
            return Err("必须是 JSON 对象".to_owned());
        }
        let text = str_of(&root, "text").trim();
        if text.chars().count() > MAX_TEXT_LENGTH {
            return Err(format!("text 超过 {MAX_TEXT_LENGTH} 个字符"));
        }
        let replace = str_of(&root, "replace").trim();
        if replace.chars().count() > MAX_REPLACE_LENGTH {
            return Err(format!("replace 超过 {MAX_REPLACE_LENGTH} 个字符"));
        }
        let mut actions = Vec::new();
        for a in root["actions"].as_array().into_iter().flatten() {
            actions.extend(parse_action(a)?);
        }
        if actions.len() > MAX_ACTIONS {
            return Err(format!("按钮超过 {MAX_ACTIONS} 个"));
        }
        let mut replies = Vec::new();
        for r in root["replies"].as_array().into_iter().flatten() {
            let fill = str_of(r, "fill").trim();
            if fill.chars().count() > MAX_TEXT_LENGTH {
                return Err(format!("fill 超过 {MAX_TEXT_LENGTH} 个字符"));
            }
            if !fill.is_empty() {
                replies.push(PluginAction {
                    label: truncate(str_of(r, "label"), MAX_LABEL_LENGTH),
                    target: String::new(),
                    args: String::new(),
                    fill: fill.to_owned(),
                    style: ActionStyle::Primary,
                });
            }
        }
        if replies.len() > MAX_REPLIES {
            return Err(format!("候选回复超过 {MAX_REPLIES} 条"));
        }
        let mut tags = Vec::new();
        for t in root["tags"].as_array().into_iter().flatten() {
            let text = truncate(str_of(t, "text"), MAX_TAG_LENGTH);
            if text.is_empty() {
                continue;
            }
            let tone = TagTone::parse(str_of(t, "tone"));
            let tip = truncate(str_of(t, "tip"), MAX_TITLE_LENGTH);
            tags.push(AnnotationTag { text, tone, tip });
        }
        if tags.len() > MAX_TAGS {
            return Err(format!("标签超过 {MAX_TAGS} 个"));
        }
        let mut fields = Vec::new();
        for f in root["fields"].as_array().into_iter().flatten() {
            let value = str_of(f, "value").trim();
            if value.chars().count() > MAX_TEXT_LENGTH {
                return Err(format!("value 超过 {MAX_TEXT_LENGTH} 个字符"));
            }
            let items: Vec<String> = f["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|item| truncate(item, MAX_TAG_LENGTH))
                .filter(|item| !item.is_empty())
                .collect();
            if items.len() > MAX_FIELD_ITEMS {
                return Err(format!("items 超过 {MAX_FIELD_ITEMS} 个"));
            }
            if !value.is_empty() || !items.is_empty() {
                fields.push(AnnotationField {
                    label: truncate(str_of(f, "label"), MAX_TAG_LENGTH),
                    value: value.to_owned(),
                    items,
                    tone: TagTone::parse(str_of(f, "tone")),
                });
            }
        }
        if fields.len() > MAX_FIELDS {
            return Err(format!("字段超过 {MAX_FIELDS} 个"));
        }
        let mut tools = Vec::new();
        for t in root["tools"].as_array().into_iter().flatten() {
            let Some(menu) = t["menu"].as_i64().and_then(|m| i32::try_from(m).ok()) else {
                continue;
            };
            tools.push(AnnotationTool {
                menu,
                icon: truncate(str_of(t, "icon"), MAX_TAG_LENGTH),
                tooltip: truncate(str_of(t, "tooltip"), MAX_LABEL_LENGTH),
            });
        }
        if tools.len() > MAX_TOOLS {
            return Err(format!("标题按钮超过 {MAX_TOOLS} 个"));
        }
        let annotation = Self {
            plugin_id: plugin_id.to_owned(),
            tags,
            title: truncate(str_of(&root, "title"), MAX_TITLE_LENGTH),
            fields,
            text: text.to_owned(),
            replies_title: truncate(str_of(&root, "replies_title"), MAX_LABEL_LENGTH),
            replies,
            actions,
            tools,
            replace: replace.to_owned(),
        };
        if !annotation.has_card() && annotation.replace.is_empty() {
            return Ok(None);
        }
        Ok(Some(annotation))
    }

    /// There is content to show below the bubble (none when only the bubble content is replaced).
    pub fn has_card(&self) -> bool {
        !self.text.is_empty()
            || !self.actions.is_empty()
            || !self.title.is_empty()
            || !self.tags.is_empty()
            || !self.fields.is_empty()
            || !self.replies.is_empty()
    }
}

/// Trimmed and cut to `max` characters.
fn truncate(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

// A button; None when it is not an object or has neither fill nor target.
fn parse_action(a: &Value) -> Result<Option<PluginAction>, String> {
    if !a.is_object() {
        return Ok(None);
    }
    let fill = str_of(a, "fill").trim();
    let target = str_of(a, "target").trim();
    if fill.chars().count() > MAX_TEXT_LENGTH {
        return Err(format!("fill 超过 {MAX_TEXT_LENGTH} 个字符"));
    }
    if fill.is_empty() && target.is_empty() {
        return Ok(None);
    }
    let label = match str_of(a, "label").trim() {
        "" if !fill.is_empty() => "填入输入框",
        "" => "打开",
        label => label,
    };
    let label = label.chars().take(MAX_LABEL_LENGTH).collect();
    let style = match str_of(a, "style") {
        "primary" => ActionStyle::Primary,
        _ => ActionStyle::Secondary,
    };
    Ok(Some(if fill.is_empty() {
        PluginAction {
            label,
            target: target.to_owned(),
            args: str_of(a, "args").trim().to_owned(),
            fill: String::new(),
            style,
        }
    } else {
        PluginAction {
            label,
            target: String::new(),
            args: String::new(),
            fill: fill.to_owned(),
            style,
        }
    }))
}

#[derive(Default)]
struct Inner {
    by_message: HashMap<(i64, i64), Vec<PluginAnnotation>>,
    original_shown: HashSet<(i64, i64)>,
}

/// Attached content plus its show-original state. Methods return whether the state
/// changed.
#[derive(Default)]
pub(crate) struct AnnotationStore {
    inner: Mutex<Inner>,
}

impl AnnotationStore {
    /// In the order they were set.
    pub fn get(&self, account_id: i64, message_id: i64) -> Vec<PluginAnnotation> {
        self.inner
            .lock()
            .by_message
            .get(&(account_id, message_id))
            .cloned()
            .unwrap_or_default()
    }

    /// Sets or clears a plugin's attached content on this message.
    pub fn set(
        &self,
        account_id: i64,
        message_id: i64,
        plugin_id: &str,
        annotation: Option<PluginAnnotation>,
    ) {
        let mut inner = self.inner.lock();
        let inner = &mut *inner;
        let list = inner
            .by_message
            .entry((account_id, message_id))
            .or_default();
        list.retain(|a| a.plugin_id != plugin_id);
        if let Some(annotation) = annotation {
            // New replacement content (e.g. a re-translation) is shown directly.
            if !annotation.replace.is_empty() {
                inner.original_shown.remove(&(account_id, message_id));
            }
            list.push(annotation);
        }
        if list.is_empty() {
            inner.by_message.remove(&(account_id, message_id));
        }
    }

    /// Removes a plugin's card (what is shown below the bubble) from a message, keeping its
    /// replacement of the bubble content. Returns whether there was a card.
    pub fn remove_card(&self, account_id: i64, message_id: i64, plugin_id: &str) -> bool {
        let mut inner = self.inner.lock();
        let Some(list) = inner.by_message.get_mut(&(account_id, message_id)) else {
            return false;
        };
        let Some(index) = list
            .iter()
            .position(|a| a.plugin_id == plugin_id && a.has_card())
        else {
            return false;
        };
        if list[index].replace.is_empty() {
            list.remove(index);
        } else {
            let replace = std::mem::take(&mut list[index].replace);
            list[index] = PluginAnnotation {
                plugin_id: plugin_id.to_owned(),
                tags: Vec::new(),
                title: String::new(),
                fields: Vec::new(),
                text: String::new(),
                replies_title: String::new(),
                replies: Vec::new(),
                actions: Vec::new(),
                tools: Vec::new(),
                replace,
            };
        }
        if list.is_empty() {
            inner.by_message.remove(&(account_id, message_id));
        }
        true
    }

    pub fn is_original_shown(&self, account_id: i64, message_id: i64) -> bool {
        self.inner
            .lock()
            .original_shown
            .contains(&(account_id, message_id))
    }

    pub fn set_original_shown(&self, account_id: i64, message_id: i64, shown: bool) -> bool {
        let mut inner = self.inner.lock();
        if shown {
            inner.original_shown.insert((account_id, message_id))
        } else {
            inner.original_shown.remove(&(account_id, message_id))
        }
    }
}

/// A button in the attached content.
#[derive(Clone, Debug)]
pub struct ActionView {
    pub plugin: Arc<NativePlugin>,
    pub action: PluginAction,
    pub label: String,
    pub style: ActionStyle,
    /// Text to fill in, or the full target (program with arguments).
    pub detail: String,
    pub tooltip: String,
    /// The plugin is still enabled and declares the corresponding permission.
    pub can_run: bool,
}

/// What the caller should do after a button is clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionOutcome {
    /// Fill the text into the composer (without sending).
    Fill(String),
    /// already open, or cannot be run.
    Done,
}

/// Content attached below the bubble by a plugin.
#[derive(Clone, Debug)]
pub struct AnnotationCard {
    pub plugin_id: String,
    pub plugin_name: String,
    pub tags: Vec<AnnotationTag>,
    pub title: String,
    pub fields: Vec<AnnotationField>,
    pub text: String,
    /// Safe HTML rendered from `text` as restricted Markdown.
    pub text_html: String,
    pub replies_title: String,
    pub replies: Vec<ActionView>,
    pub actions: Vec<ActionView>,
    /// Icon buttons in the header, without the close button.
    pub tools: Vec<AnnotationTool>,
    /// The plugin's own close button (icon `close`), shown as the X in the header; without one the
    /// X removes the card for this run (`close_annotation`).
    pub close: Option<AnnotationTool>,
}

/// The plugin replaces the bubble content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedContent {
    pub plugin_id: String,
    pub plugin_name: String,
    /// The plugin's icon buttons, shown next to the toggle when it has no card (otherwise they are in
    /// the card header).
    pub tools: Vec<AnnotationTool>,
    /// The replacement's Zulip Markdown; the client renders it through the server.
    pub markdown: String,
    /// Plain-text HTML of `markdown`, shown until the server's rendering is available or when it
    /// fails.
    pub html: String,
    /// The user chose to show the original text.
    pub original_shown: bool,
}

/// The attached content to show on a message (only from enabled plugins).
#[derive(Clone, Debug, Default)]
pub struct MessageAnnotations {
    pub rendered: Option<RenderedContent>,
    pub cards: Vec<AnnotationCard>,
}

impl PluginHost {
    pub(crate) fn set_annotation(
        &self,
        account_id: i64,
        message_id: i64,
        plugin_id: &str,
        annotation: Option<PluginAnnotation>,
    ) {
        self.annotations
            .set(account_id, message_id, plugin_id, annotation);
        self.emit(PluginEvent::AnnotationChanged {
            account_id,
            message_id,
        });
    }

    /// The plugin has the launch permission and the target is in its allowlist (it can be opened
    /// without a user click).
    pub fn is_whitelisted(plugin: &NativePlugin, target: &str) -> bool {
        plugin.has_permission(permission::LAUNCH)
            && plugin
                .manifest()
                .is_some_and(|m| m.launch.iter().any(|r| r.matches(target)))
    }

    /// The attached content to show on a message: only enabled plugins are shown; the first replace
    /// from a plugin with the render_message permission replaces the bubble content.
    pub fn message_annotations(&self, account_id: i64, message_id: i64) -> MessageAnnotations {
        let mut result = MessageAnnotations::default();
        for annotation in self.annotations.get(account_id, message_id) {
            let Some(plugin) = self
                .plugin(&annotation.plugin_id)
                .filter(|p| p.is_enabled())
            else {
                continue;
            };
            if !annotation.replace.is_empty()
                && result.rendered.is_none()
                && plugin.has_permission(permission::RENDER_MESSAGE)
            {
                result.rendered = Some(RenderedContent {
                    plugin_id: annotation.plugin_id.clone(),
                    plugin_name: plugin.name(),
                    tools: if annotation.has_card() {
                        Vec::new()
                    } else {
                        annotation.tools.clone()
                    },
                    markdown: annotation.replace.clone(),
                    html: plain_text_html(&annotation.replace),
                    original_shown: self.annotations.is_original_shown(account_id, message_id),
                });
            }
            if annotation.has_card() {
                let name = plugin.name();
                let (close, tools): (Vec<_>, Vec<_>) = annotation
                    .tools
                    .iter()
                    .cloned()
                    .partition(|t| t.icon == CLOSE_ICON);
                result.cards.push(AnnotationCard {
                    plugin_id: annotation.plugin_id.clone(),
                    plugin_name: name,
                    tags: annotation.tags.clone(),
                    title: annotation.title.clone(),
                    fields: annotation.fields.clone(),
                    text_html: markdown_html(&annotation.text),
                    text: annotation.text.clone(),
                    replies_title: annotation.replies_title.clone(),
                    tools,
                    close: close.into_iter().next(),
                    replies: annotation
                        .replies
                        .iter()
                        .map(|a| action_view(&plugin, a))
                        .collect(),
                    actions: annotation
                        .actions
                        .iter()
                        .map(|a| action_view(&plugin, a))
                        .collect(),
                });
            }
        }
        result
    }

    /// The X in a card header when the plugin has no close button: removes the card for this run
    /// (the plugin may attach it again, e.g. when the message is shown again).
    pub fn close_annotation(&self, account_id: i64, message_id: i64, plugin_id: &str) {
        if self
            .annotations
            .remove_card(account_id, message_id, plugin_id)
        {
            self.emit(PluginEvent::AnnotationChanged {
                account_id,
                message_id,
            });
        }
    }

    /// When a plugin has replaced the bubble content, the user chooses to show the original or the
    /// translation.
    pub fn set_original_shown(&self, account_id: i64, message_id: i64, shown: bool) {
        if self
            .annotations
            .set_original_shown(account_id, message_id, shown)
        {
            self.emit(PluginEvent::AnnotationChanged {
                account_id,
                message_id,
            });
        }
    }

    /// Clicks a button in the attached content (the user's click counts as confirmation; no
    /// allowlist needed). Returns hint text on failure.
    pub fn run_action(&self, view: &ActionView) -> Result<ActionOutcome, String> {
        if !view.can_run {
            return Ok(ActionOutcome::Done);
        }
        if view.action.is_fill() {
            return Ok(ActionOutcome::Fill(view.action.fill.clone()));
        }
        match launch::launch(&view.action.target, &view.action.args) {
            Ok(()) => {
                view.plugin.add_log(
                    abi::LOG_INFO,
                    HOST_CATEGORY,
                    &format!("用户点击打开 {}", view.detail),
                );
                Ok(ActionOutcome::Done)
            }
            Err(error) => {
                log::warn!("打开插件按钮失败：{}：{error}", view.detail);
                Err(format!("无法打开：{}", view.action.target))
            }
        }
    }
}

fn action_view(plugin: &Arc<NativePlugin>, action: &PluginAction) -> ActionView {
    let detail = if action.is_fill() {
        action.fill.clone()
    } else {
        launch::describe(&action.target, &action.args)
    };
    let tooltip = if action.is_fill() {
        format!("点击填入输入框（不会发送）：{detail}")
    } else if PluginHost::is_whitelisted(plugin, &action.target) {
        format!("白名单内：{detail}")
    } else {
        format!("不在白名单中，点击后打开：{detail}")
    };
    let needed = if action.is_fill() {
        permission::FILL_COMPOSER
    } else {
        permission::LAUNCH
    };
    ActionView {
        plugin: plugin.clone(),
        action: action.clone(),
        label: action.label.clone(),
        style: action.style,
        detail,
        tooltip,
        can_run: plugin.is_enabled() && plugin.has_permission(needed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_limits() {
        let a = PluginAnnotation::parse(
            "p",
            r#"{"text": " 你好 ", "actions": [{"target": "https://x"}, {"fill": "y"}, {"label": "空"}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(a.text, "你好");
        assert_eq!(a.actions.len(), 2);
        assert_eq!(a.actions[0].label, "打开");
        assert_eq!(a.actions[1].label, "填入输入框");
        assert!(a.actions[1].is_fill());
        assert_eq!(a.actions[0].style, ActionStyle::Secondary);
        let styled = PluginAnnotation::parse(
            "p",
            r#"{"actions": [{"fill": "x", "style": "primary"}, {"fill": "y", "style": "未知"}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(styled.actions[0].style, ActionStyle::Primary);
        assert_eq!(styled.actions[1].style, ActionStyle::Secondary);

        assert_eq!(PluginAnnotation::parse("p", "{}"), Ok(None));
        assert!(PluginAnnotation::parse("p", "[]").is_err());
        assert!(PluginAnnotation::parse("p", "{坏").is_err());
        let long = serde_json::json!({"text": "字".repeat(MAX_TEXT_LENGTH + 1)}).to_string();
        assert!(PluginAnnotation::parse("p", &long).is_err());
        let many = serde_json::json!({"actions": vec![serde_json::json!({"fill": "x"}); 6]});
        assert!(PluginAnnotation::parse("p", &many.to_string()).is_err());
    }

    #[test]
    fn parses_structured_card() {
        let a = PluginAnnotation::parse(
            "p",
            r##"{"title": " 想约时间 ", "tags": [{"text": "风险 低", "tone": "success"}, {"text": " "}, {"text": "工作", "tone": "?"}],
                "fields": [{"label": "建议", "value": " 直接答应 "}, {"label": "空", "value": ""},
                           {"label": "信号", "items": ["催促", " ", 1], "tone": "accent"}],
                "replies_title": "候选回复", "replies": [{"label": "#1", "fill": "好"}, {"label": "#2", "fill": " "}]}"##,
        )
        .unwrap()
        .unwrap();
        assert!(a.has_card());
        assert_eq!(a.title, "想约时间");
        assert_eq!(a.tags.len(), 2);
        assert_eq!(a.tags[0].tone, TagTone::Success);
        assert_eq!(a.tags[1].tone, TagTone::Neutral);
        assert_eq!(
            a.fields,
            vec![
                AnnotationField {
                    label: "建议".into(),
                    value: "直接答应".into(),
                    items: Vec::new(),
                    tone: TagTone::Neutral,
                },
                AnnotationField {
                    label: "信号".into(),
                    value: String::new(),
                    items: vec!["催促".into()],
                    tone: TagTone::Accent,
                },
            ]
        );
        assert_eq!(a.replies.len(), 1);
        assert!(a.replies[0].is_fill());
        assert_eq!(a.replies[0].label, "#1");
        let tools = PluginAnnotation::parse(
            "p",
            r#"{"text": "x", "tools": [{"menu": 2, "icon": "refresh", "tooltip": "重新获取"}, {"icon": "delete"}]}"#,
        )
        .unwrap()
        .unwrap()
        .tools;
        assert_eq!(
            tools,
            vec![AnnotationTool {
                menu: 2,
                icon: "refresh".into(),
                tooltip: "重新获取".into(),
            }]
        );
        let many = serde_json::json!({"replies": vec![serde_json::json!({"fill": "x"}); 6]});
        assert!(PluginAnnotation::parse("p", &many.to_string()).is_err());
    }

    #[test]
    fn store_replaces_per_plugin() {
        let store = AnnotationStore::default();
        let annotation = |plugin: &str, text: &str, replace: &str| PluginAnnotation {
            plugin_id: plugin.into(),
            tags: Vec::new(),
            title: String::new(),
            fields: Vec::new(),
            text: text.into(),
            replies_title: String::new(),
            replies: Vec::new(),
            actions: Vec::new(),
            tools: Vec::new(),
            replace: replace.into(),
        };
        store.set(1, 2, "a", Some(annotation("a", "x", "")));
        store.set(1, 2, "b", Some(annotation("b", "y", "")));
        store.set(1, 2, "a", Some(annotation("a", "z", "")));
        let list = store.get(1, 2);
        assert_eq!(list[0].plugin_id, "b");
        assert_eq!(list[1].text, "z");

        assert!(store.set_original_shown(1, 2, true));
        assert!(!store.set_original_shown(1, 2, true));
        store.set(1, 2, "a", Some(annotation("a", "", "译文")));
        assert!(!store.is_original_shown(1, 2));

        store.set(1, 2, "a", None);
        store.set(1, 2, "b", None);
        assert!(store.get(1, 2).is_empty());
    }
}
