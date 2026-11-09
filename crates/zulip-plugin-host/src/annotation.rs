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
    pub text: String,
    pub actions: Vec<PluginAction>,
    /// Plain text that temporarily replaces the bubble content.
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
            if !a.is_object() {
                continue;
            }
            let fill = str_of(a, "fill").trim();
            let target = str_of(a, "target").trim();
            if fill.chars().count() > MAX_TEXT_LENGTH {
                return Err(format!("fill 超过 {MAX_TEXT_LENGTH} 个字符"));
            }
            if fill.is_empty() && target.is_empty() {
                continue;
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
            actions.push(if fill.is_empty() {
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
            });
        }
        if actions.len() > MAX_ACTIONS {
            return Err(format!("按钮超过 {MAX_ACTIONS} 个"));
        }
        if text.is_empty() && actions.is_empty() && replace.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            plugin_id: plugin_id.to_owned(),
            text: text.to_owned(),
            actions,
            replace: replace.to_owned(),
        }))
    }

    /// There is content to show below the bubble (none when only the bubble content is replaced).
    pub fn has_card(&self) -> bool {
        !self.text.is_empty() || !self.actions.is_empty()
    }
}

#[derive(Default)]
struct Inner {
    by_message: HashMap<(i64, i64), Vec<PluginAnnotation>>,
    collapsed: HashSet<(i64, i64, String)>,
    original_shown: HashSet<(i64, i64)>,
}

/// Attached content plus its collapsed / show-original state. Methods return whether the state
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

    pub fn is_collapsed(&self, account_id: i64, message_id: i64, plugin_id: &str) -> bool {
        self.inner
            .lock()
            .collapsed
            .contains(&(account_id, message_id, plugin_id.to_owned()))
    }

    pub fn set_collapsed(
        &self,
        account_id: i64,
        message_id: i64,
        plugin_id: &str,
        collapsed: bool,
    ) -> bool {
        let key = (account_id, message_id, plugin_id.to_owned());
        let mut inner = self.inner.lock();
        if collapsed {
            inner.collapsed.insert(key)
        } else {
            inner.collapsed.remove(&key)
        }
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
    pub text: String,
    /// Safe HTML rendered from `text` as restricted Markdown.
    pub text_html: String,
    pub is_collapsed: bool,
    pub toggle_tooltip: String,
    pub actions: Vec<ActionView>,
}

/// The plugin replaces the bubble content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedContent {
    pub plugin_name: String,
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
                    plugin_name: plugin.name(),
                    html: plain_text_html(&annotation.replace),
                    original_shown: self.annotations.is_original_shown(account_id, message_id),
                });
            }
            if annotation.has_card() {
                let name = plugin.name();
                let is_collapsed =
                    self.annotations
                        .is_collapsed(account_id, message_id, &annotation.plugin_id);
                result.cards.push(AnnotationCard {
                    toggle_tooltip: if is_collapsed {
                        format!("展开 {name} 附加的内容")
                    } else {
                        format!("折叠 {name} 附加的内容（不修改消息）")
                    },
                    plugin_id: annotation.plugin_id.clone(),
                    plugin_name: name,
                    text_html: markdown_html(&annotation.text),
                    text: annotation.text.clone(),
                    is_collapsed,
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

    pub fn set_annotation_collapsed(
        &self,
        account_id: i64,
        message_id: i64,
        plugin_id: &str,
        collapsed: bool,
    ) {
        if self
            .annotations
            .set_collapsed(account_id, message_id, plugin_id, collapsed)
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
    fn store_replaces_per_plugin() {
        let store = AnnotationStore::default();
        let annotation = |plugin: &str, text: &str, replace: &str| PluginAnnotation {
            plugin_id: plugin.into(),
            text: text.into(),
            actions: Vec::new(),
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
