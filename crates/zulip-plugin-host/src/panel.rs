//! Plugin groups in the user profile card / topic profile panel (panels in plugin.json). The
//! settings and data in them are all provided by the plugin through on_panel and saved by the
//! plugin itself.

use serde_json::{Map, Value, json};
use zulip_plugin_sdk::str_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginTargetKind {
    User,
    Topic,
}

impl PluginTargetKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Topic => "topic",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "user" => Some(Self::User),
            "topic" => Some(Self::Topic),
            _ => None,
        }
    }
}

/// Target of a panel. `name` is only used for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginTarget {
    User {
        account_id: i64,
        user_id: i64,
        name: String,
    },
    Topic {
        account_id: i64,
        stream_id: i64,
        topic: String,
        name: String,
    },
}

impl PluginTarget {
    pub fn kind(&self) -> PluginTargetKind {
        match self {
            Self::User { .. } => PluginTargetKind::User,
            Self::Topic { .. } => PluginTargetKind::Topic,
        }
    }

    pub fn account_id(&self) -> i64 {
        match self {
            Self::User { account_id, .. } | Self::Topic { account_id, .. } => *account_id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::User { name, .. } | Self::Topic { name, .. } => name,
        }
    }

    /// Parsed by kind; `None` when the account, user / channel or topic is missing.
    pub fn from_json(value: &Value, kind: PluginTargetKind) -> Option<Self> {
        let positive = |key: &str| value[key].as_i64().filter(|n| *n > 0);
        let account_id = positive("account")?;
        let name = str_of(value, "name").to_owned();
        match kind {
            PluginTargetKind::User => Some(Self::User {
                account_id,
                user_id: positive("user")?,
                name,
            }),
            PluginTargetKind::Topic => {
                let topic = str_of(value, "topic");
                (!topic.is_empty()).then_some(())?;
                Some(Self::Topic {
                    account_id,
                    stream_id: positive("stream")?,
                    topic: topic.to_owned(),
                    name,
                })
            }
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::User {
                account_id,
                user_id,
                name,
            } => json!({"account": account_id, "user": user_id, "name": name}),
            Self::Topic {
                account_id,
                stream_id,
                topic,
                name,
            } => json!({"account": account_id, "stream": stream_id, "topic": topic, "name": name}),
        }
    }

    /// The same user / topic (names are not compared; topic names are case-insensitive).
    pub fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::User {
                    account_id: a,
                    user_id: u,
                    ..
                },
                Self::User {
                    account_id: b,
                    user_id: v,
                    ..
                },
            ) => a == b && u == v,
            (
                Self::Topic {
                    account_id: a,
                    stream_id: s,
                    topic: t,
                    ..
                },
                Self::Topic {
                    account_id: b,
                    stream_id: r,
                    topic: u,
                    ..
                },
            ) => a == b && s == r && t.to_lowercase() == u.to_lowercase(),
            _ => false,
        }
    }
}

/// An entry in panels of plugin.json.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginPanel {
    pub kind: PluginTargetKind,
    pub label: String,
}

/// An option of a dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelFieldValue {
    Select {
        options: Vec<PanelOption>,
        selected: String,
    },
    Bool(bool),
    /// Single-line text; the plugin is notified with set after a change.
    Text {
        value: String,
        placeholder: String,
    },
    /// The plugin is notified with click when clicked.
    Button,
    /// Read-only text.
    Info(String),
}

/// An item in a panel (get / set reply of on_panel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelField {
    pub key: String,
    pub label: String,
    pub value: PanelFieldValue,
}

const MAX_FIELDS: usize = 20;
const MAX_OPTIONS: usize = 200;

/// Kind of show_dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    /// OK / Cancel. `fields` is empty.
    Confirm,
    /// A single text input: `fields` has exactly one [`PanelFieldValue::Text`].
    Input,
    /// Form: `fields` has at least one item.
    Form,
}

/// A dialog the plugin asks to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogSpec {
    pub kind: DialogKind,
    pub title: String,
    pub text: String,
    pub fields: Vec<PanelField>,
}

impl DialogSpec {
    /// `None` on a format error, unknown kind, or a form without fields. input takes the first text
    /// field, and if there is none, adds an empty text field with key value.
    pub fn parse(json: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(json).ok()?;
        let kind = match str_of(&value, "kind") {
            "confirm" => DialogKind::Confirm,
            "input" => DialogKind::Input,
            "form" => DialogKind::Form,
            _ => return None,
        };
        let parsed = parse_fields(&value);
        let fields = match kind {
            DialogKind::Confirm => Vec::new(),
            DialogKind::Input => {
                let text = parsed
                    .into_iter()
                    .find(|f| matches!(f.value, PanelFieldValue::Text { .. }));
                vec![text.unwrap_or_else(|| PanelField {
                    key: "value".to_owned(),
                    label: "内容".to_owned(),
                    value: PanelFieldValue::Text {
                        value: String::new(),
                        placeholder: String::new(),
                    },
                })]
            }
            DialogKind::Form if parsed.is_empty() => return None,
            DialogKind::Form => parsed,
        };
        Some(Self {
            kind,
            title: str_of(&value, "title").to_owned(),
            text: str_of(&value, "text").to_owned(),
            fields,
        })
    }
}

/// Result of a dialog: `ok` means the user clicked OK, and `values` maps field keys to values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DialogResult {
    pub ok: bool,
    pub values: Map<String, Value>,
}

impl DialogResult {
    pub(crate) fn to_json(&self) -> Value {
        json!({"ok": self.ok, "values": self.values})
    }
}

/// Parses fields in the reply; malformed items are ignored (a dropdown must have at least one
/// option).
pub fn parse_fields(reply: &Value) -> Vec<PanelField> {
    let mut fields: Vec<PanelField> = Vec::new();
    for f in reply["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_FIELDS)
    {
        let key = str_of(f, "key");
        let label = str_of(f, "label");
        if key.is_empty() || label.is_empty() || fields.iter().any(|x| x.key == key) {
            continue;
        }
        let value = match str_of(f, "type") {
            "bool" => PanelFieldValue::Bool(f["value"].as_bool().unwrap_or(false)),
            "text" => PanelFieldValue::Text {
                value: str_of(f, "value").to_owned(),
                placeholder: str_of(f, "placeholder").to_owned(),
            },
            "button" => PanelFieldValue::Button,
            "info" => PanelFieldValue::Info(str_of(f, "value").to_owned()),
            "select" => {
                let mut options: Vec<PanelOption> = Vec::new();
                for o in f["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(MAX_OPTIONS)
                {
                    let value = str_of(o, "value");
                    let label = str_of(o, "label");
                    if !label.is_empty() && options.iter().all(|x| x.value != value) {
                        options.push(PanelOption {
                            value: value.to_owned(),
                            label: label.to_owned(),
                        });
                    }
                }
                if options.is_empty() {
                    continue;
                }
                PanelFieldValue::Select {
                    options,
                    selected: str_of(f, "value").to_owned(),
                }
            }
            _ => continue,
        };
        fields.push(PanelField {
            key: key.to_owned(),
            label: label.to_owned(),
            value,
        });
    }
    fields
}

/// A row of the panel list in the plugin settings (list reply of on_panel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelEntry {
    pub target: PluginTarget,
    pub text: String,
}

pub fn parse_entries(reply: &Value, kind: PluginTargetKind) -> Vec<PanelEntry> {
    reply["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some(PanelEntry {
                target: PluginTarget::from_json(&e["target"], kind)?,
                text: str_of(e, "text").to_owned(),
            })
        })
        .collect()
}

/// Why [`PluginHost::panel`](crate::PluginHost::panel) did not wait for the plugin to finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PanelError {
    /// No return within 2 seconds, or the plugin thread has stopped.
    #[error("插件没有响应")]
    Timeout,
    /// The plugin is not enabled, does not implement on_panel, or does not declare a panel.
    #[error("插件未开启")]
    Inactive,
}

/// Request of on_panel.
#[derive(Clone, Debug)]
pub enum PanelRequest {
    /// Get the settings when the panel is opened.
    Get(PluginTarget),
    /// The user changed a setting; reply with the new settings.
    Set {
        target: PluginTarget,
        key: String,
        value: Value,
    },
    /// The user clicked a button item; reply with the new settings.
    Click { target: PluginTarget, key: String },
    /// A list in the plugin settings.
    List(PluginTargetKind),
    /// Remove from the list.
    Remove(PluginTarget),
}

impl PanelRequest {
    pub(crate) fn account_id(&self) -> Option<i64> {
        match self {
            Self::Get(t)
            | Self::Remove(t)
            | Self::Set { target: t, .. }
            | Self::Click { target: t, .. } => Some(t.account_id()),
            Self::List(_) => None,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut request = Map::new();
        let (action, kind, target) = match self {
            Self::Get(t) => ("get", t.kind(), Some(t)),
            Self::Set { target, .. } => ("set", target.kind(), Some(target)),
            Self::Click { target, .. } => ("click", target.kind(), Some(target)),
            Self::List(kind) => ("list", *kind, None),
            Self::Remove(t) => ("remove", t.kind(), Some(t)),
        };
        request.insert("action".into(), json!(action));
        request.insert("kind".into(), json!(kind.name()));
        if let Some(target) = target {
            request.insert("target".into(), target.to_json());
        }
        if let Self::Set { key, value, .. } = self {
            request.insert("key".into(), json!(key));
            request.insert("value".into(), value.clone());
        }
        if let Self::Click { key, .. } = self {
            request.insert("key".into(), json!(key));
        }
        Value::Object(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_round_trip() {
        let topic = PluginTarget::Topic {
            account_id: 1,
            stream_id: 2,
            topic: "发布".into(),
            name: "开发 / 发布".into(),
        };
        let parsed = PluginTarget::from_json(&topic.to_json(), PluginTargetKind::Topic);
        assert_eq!(parsed.as_ref(), Some(&topic));
        assert!(PluginTarget::from_json(&json!({"account": 1}), PluginTargetKind::User).is_none());
        let other = PluginTarget::Topic {
            account_id: 1,
            stream_id: 2,
            topic: "发布".into(),
            name: String::new(),
        };
        assert!(topic.same_as(&other));
    }

    #[test]
    fn parses_fields_and_entries() {
        let fields = parse_fields(&json!({"fields": [
            {"key": "a", "label": "A", "type": "bool", "value": true},
            {"key": "a", "label": "重复", "type": "bool"},
            {"key": "b", "label": "B", "type": "select", "value": "x",
             "options": [{"value": "x", "label": "X"}, {"value": "x", "label": "Y"}]},
            {"key": "c", "label": "C", "type": "select", "options": []},
            {"key": "d", "label": "D", "type": "未知"},
            {"key": "e", "label": "E", "type": "text", "value": "v", "placeholder": "p"},
            {"key": "f", "label": "F", "type": "button"},
            {"key": "g", "label": "G", "type": "info", "value": "说明"},
        ]}));
        assert_eq!(fields.len(), 5);
        assert_eq!(
            fields[2].value,
            PanelFieldValue::Text {
                value: "v".into(),
                placeholder: "p".into()
            }
        );
        assert_eq!(fields[3].value, PanelFieldValue::Button);
        assert_eq!(fields[4].value, PanelFieldValue::Info("说明".into()));
        assert_eq!(fields[0].value, PanelFieldValue::Bool(true));
        let PanelFieldValue::Select { options, .. } = &fields[1].value else {
            panic!("应为下拉框");
        };
        assert_eq!(options.len(), 1);

        let entries = parse_entries(
            &json!({"entries": [
                {"target": {"account": 1, "user": 2, "name": "小雨"}, "text": "朋友"},
                {"target": {"account": 1}, "text": "缺少用户"},
            ]}),
            PluginTargetKind::User,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "朋友");
    }

    #[test]
    fn request_json() {
        let list = PanelRequest::List(PluginTargetKind::Topic).to_json();
        assert_eq!(list, json!({"action": "list", "kind": "topic"}));
        let set = PanelRequest::Set {
            target: PluginTarget::User {
                account_id: 1,
                user_id: 2,
                name: "x".into(),
            },
            key: "k".into(),
            value: json!(true),
        }
        .to_json();
        assert_eq!(set["key"], "k");
        assert_eq!(set["target"]["user"], 2);
        let click = PanelRequest::Click {
            target: PluginTarget::User {
                account_id: 1,
                user_id: 2,
                name: "x".into(),
            },
            key: "k".into(),
        }
        .to_json();
        assert_eq!(
            (click["action"].as_str(), click["key"].as_str()),
            (Some("click"), Some("k"))
        );
    }

    #[test]
    fn dialog_spec() {
        let spec =
            DialogSpec::parse(r#"{"kind": "confirm", "title": "T", "text": "说明"}"#).unwrap();
        assert_eq!(spec.kind, DialogKind::Confirm);
        assert_eq!((spec.title.as_str(), spec.text.as_str()), ("T", "说明"));
        assert!(spec.fields.is_empty());

        let spec = DialogSpec::parse(r#"{"kind": "input"}"#).unwrap();
        assert_eq!(spec.fields[0].key, "value");
        let spec = DialogSpec::parse(
            r#"{"kind": "input", "fields": [{"key": "b", "label": "B", "type": "bool"}, {"key": "t", "label": "T", "type": "text"}]}"#,
        )
        .unwrap();
        assert_eq!(spec.fields.len(), 1);
        assert_eq!(spec.fields[0].key, "t");

        let spec = DialogSpec::parse(
            r#"{"kind": "form", "fields": [{"key": "b", "label": "B", "type": "bool", "value": true}]}"#,
        )
        .unwrap();
        assert_eq!(spec.fields[0].value, PanelFieldValue::Bool(true));
        assert_eq!(DialogSpec::parse(r#"{"kind": "form"}"#), None);
        assert_eq!(DialogSpec::parse(r#"{"kind": "x"}"#), None);
        assert_eq!(DialogSpec::parse("坏"), None);
    }
}
