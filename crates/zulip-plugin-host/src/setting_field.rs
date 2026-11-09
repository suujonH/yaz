//! plugin.json 中的 settings：宿主据此让界面生成表单，值保存在宿主中，插件只能读取。
//! 值的 JSON 类型：Bool 为 true/false，List 为对象数组，Tags 为字符串数组，Resource 为对象（没有选择时为空对象），其余为字符串。

use serde_json::{Map, Value, json};
use zulip_plugin_sdk::str_of;

use crate::resources::{ResourceSelection, is_valid_type};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginSettingType {
    Text,
    Password,
    Multiline,
    Select,
    Bool,
    /// 可增删的列表，每一行由 `fields` 组成（不能嵌套列表）。
    List,
    /// 没有值，不保存：点击时以表单当前的值调用 on_settings_button。
    Button,
    /// 字符串列表；写了 `item_button` 时每一项旁边有一个按钮。
    Tags,
    /// 选择其他插件发布的资源。
    Resource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSettingOption {
    pub value: String,
    pub label: String,
}

/// 只在同一层的另一项取这些值之一时显示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleWhen {
    pub key: String,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginSettingField {
    pub key: String,
    pub label: String,
    pub kind: PluginSettingType,
    pub hint: String,
    pub placeholder: String,
    pub default: Option<Value>,
    pub options: Vec<PluginSettingOption>,
    pub fields: Vec<PluginSettingField>,
    pub item_button: String,
    pub resource_type: String,
    pub resource_plugin: String,
    pub visible_when: Option<VisibleWhen>,
}

fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl PluginSettingField {
    /// 该类型的空值。
    pub fn empty_value(&self) -> Value {
        match self.kind {
            PluginSettingType::Bool => json!(false),
            PluginSettingType::List | PluginSettingType::Tags => json!([]),
            PluginSettingType::Resource => json!({}),
            PluginSettingType::Select => {
                json!(self.options.first().map_or("", |o| o.value.as_str()))
            }
            _ => json!(""),
        }
    }

    pub fn default_value(&self) -> Value {
        self.default.clone().unwrap_or_else(|| self.empty_value())
    }

    /// 按类型取值：类型不对或没有时返回默认值。
    pub fn normalize(&self, value: Option<&Value>) -> Value {
        match self.kind {
            PluginSettingType::Bool => value
                .filter(|v| v.is_boolean())
                .cloned()
                .unwrap_or_else(|| self.default_value()),
            PluginSettingType::List => match value.and_then(Value::as_array) {
                Some(rows) => Value::Array(
                    rows.iter()
                        .filter_map(|row| {
                            row.as_object()
                                .map(|r| normalize_values(&self.fields, Some(r)))
                        })
                        .map(Value::Object)
                        .collect(),
                ),
                None => self.default_value(),
            },
            PluginSettingType::Tags => match value.and_then(Value::as_array) {
                Some(tags) => {
                    let mut result: Vec<String> = Vec::new();
                    for tag in tags.iter().filter_map(Value::as_str).map(str::trim) {
                        if !tag.is_empty() && !result.iter().any(|t| t == tag) {
                            result.push(tag.to_owned());
                        }
                    }
                    json!(result)
                }
                None => self.default_value(),
            },
            PluginSettingType::Resource => value
                .and_then(ResourceSelection::from_json)
                .filter(|s| s.kind == self.resource_type && s.plugin_id == self.resource_plugin)
                .map_or_else(|| self.empty_value(), |s| s.to_json()),
            PluginSettingType::Select => value
                .and_then(Value::as_str)
                .filter(|s| self.options.iter().any(|o| o.value == *s))
                .map_or_else(|| self.default_value(), |s| json!(s)),
            _ => value
                .filter(|v| v.is_string())
                .cloned()
                .unwrap_or_else(|| self.default_value()),
        }
    }

    /// 解析 settings 数组；格式错误的项忽略并通过 `warn` 报告。
    pub fn parse_list(
        array: &Value,
        warn: &mut dyn FnMut(String),
        allow_list: bool,
    ) -> Vec<PluginSettingField> {
        let mut result: Vec<PluginSettingField> = Vec::new();
        for item in array.as_array().into_iter().flatten() {
            match Self::parse(item, warn, allow_list) {
                Ok(field) if result.iter().any(|f| f.key == field.key) => {
                    warn(format!("忽略设置项：设置项 key 重复：{}", field.key));
                }
                Ok(field) => result.push(field),
                Err(error) => warn(format!("忽略设置项：{error}")),
            }
        }
        result
    }

    fn parse(
        item: &Value,
        warn: &mut dyn FnMut(String),
        allow_list: bool,
    ) -> Result<PluginSettingField, String> {
        if !item.is_object() {
            return Err("必须是对象".to_owned());
        }
        let key = str_of(item, "key");
        if !is_valid_key(key) {
            return Err(format!("key 不合法：{key}"));
        }
        let kind = match str_of(item, "type") {
            "" | "text" => PluginSettingType::Text,
            "password" => PluginSettingType::Password,
            "multiline" => PluginSettingType::Multiline,
            "select" => PluginSettingType::Select,
            "bool" => PluginSettingType::Bool,
            "list" if allow_list => PluginSettingType::List,
            "button" => PluginSettingType::Button,
            "tags" => PluginSettingType::Tags,
            "resource" => PluginSettingType::Resource,
            other => return Err(format!("{key} 的类型不支持：{other}")),
        };

        let mut options: Vec<PluginSettingOption> = Vec::new();
        if kind == PluginSettingType::Select {
            for o in item["options"].as_array().into_iter().flatten() {
                let value = str_of(o, "value");
                if !value.is_empty() && options.iter().all(|x| x.value != value) {
                    let label = str_of(o, "label");
                    options.push(PluginSettingOption {
                        value: value.to_owned(),
                        label: if label.is_empty() { value } else { label }.to_owned(),
                    });
                }
            }
            if options.is_empty() {
                return Err(format!("{key} 没有选项"));
            }
        }

        let is_resource = kind == PluginSettingType::Resource;
        let resource_type = str_of(item, "resource");
        let resource_plugin = str_of(item, "plugin");
        if is_resource && !is_valid_type(resource_type) {
            return Err(format!("{key} 的资源类型不合法：{resource_type}"));
        }
        if is_resource && resource_plugin.is_empty() {
            return Err(format!("{key} 没有填写发布资源的插件（plugin）"));
        }

        let fields = if kind == PluginSettingType::List {
            Self::parse_list(&item["fields"], warn, false)
        } else {
            Vec::new()
        };
        if kind == PluginSettingType::List && fields.is_empty() {
            return Err(format!("{key} 没有列表项"));
        }

        let visible_when = item["visible_when"]
            .as_object()
            .and_then(|o| o.iter().next())
            .and_then(|(key, value)| {
                let values = match value {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                    _ => return None,
                };
                Some(VisibleWhen {
                    key: key.clone(),
                    values,
                })
            });

        let label = str_of(item, "label");
        let mut field = PluginSettingField {
            key: key.to_owned(),
            label: if label.is_empty() { key } else { label }.to_owned(),
            kind,
            hint: str_of(item, "hint").to_owned(),
            placeholder: str_of(item, "placeholder").to_owned(),
            default: None,
            options,
            fields,
            item_button: if kind == PluginSettingType::Tags {
                str_of(item, "item_button").to_owned()
            } else {
                String::new()
            },
            resource_type: if is_resource { resource_type } else { "" }.to_owned(),
            resource_plugin: if is_resource { resource_plugin } else { "" }.to_owned(),
            visible_when,
        };
        // 默认值类型不对时忽略。
        if let Some(default) = item.get("default") {
            field.default = Some(field.normalize(Some(default)));
        }
        Ok(field)
    }

    /// 在同一层的值 `values` 下是否显示。
    pub fn is_visible(&self, values: &Value) -> bool {
        self.visible_when.as_ref().is_none_or(|when| {
            let current = match &values[when.key.as_str()] {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                _ => String::new(),
            };
            when.values.contains(&current)
        })
    }
}

/// 只保留清单中的项，类型不对的取默认值。
pub(crate) fn normalize_values(
    fields: &[PluginSettingField],
    values: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    fields
        .iter()
        .filter(|f| f.kind != PluginSettingType::Button)
        .map(|f| {
            let value = match values.and_then(|v| v.get(&f.key)) {
                Some(value) => f.normalize(Some(value)),
                None => f.default_value(),
            };
            (f.key.clone(), value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(json: Value) -> (Vec<PluginSettingField>, Vec<String>) {
        let mut warnings = Vec::new();
        let list = PluginSettingField::parse_list(&json, &mut |w| warnings.push(w), true);
        (list, warnings)
    }

    #[test]
    fn parse_skips_bad_items() {
        let (list, warnings) = fields(json!([
            {"key": "a", "label": "A"},
            {"key": "a"},
            {"key": "Bad"},
            {"key": "s", "type": "select"},
            {"key": "n", "type": "nope"},
            {"key": "l", "type": "list", "fields": [{"key": "x", "type": "list"}]},
            {"key": "r", "type": "resource", "resource": "model"},
        ]));
        assert_eq!(list.len(), 1);
        assert_eq!(warnings.len(), 7);
    }

    #[test]
    fn normalize_by_type() {
        let (list, _) = fields(json!([
            {"key": "b", "type": "bool", "default": true},
            {"key": "s", "type": "select", "options": [{"value": "x"}, {"value": "y", "label": "Y"}]},
            {"key": "t", "type": "tags"},
            {"key": "bad_default", "type": "bool", "default": "no"},
            {"key": "r", "type": "resource", "resource": "model", "plugin": "a.b.c"},
            {"key": "l", "type": "list", "fields": [{"key": "x", "default": "d"}]},
        ]));
        assert_eq!(list[0].normalize(Some(&json!("no"))), json!(true));
        assert_eq!(list[1].normalize(Some(&json!("z"))), json!("x"));
        assert_eq!(list[1].normalize(Some(&json!("y"))), json!("y"));
        assert_eq!(
            list[2].normalize(Some(&json!([" a ", "a", "", 1, "b"]))),
            json!(["a", "b"])
        );
        assert_eq!(list[3].default_value(), json!(false));
        let good = json!({"type": "model", "plugin": "a.b.c", "resource": "r", "item": "i"});
        assert_eq!(list[4].normalize(Some(&good)), good);
        let other = json!({"type": "model", "plugin": "x.y.z", "resource": "r"});
        assert_eq!(list[4].normalize(Some(&other)), json!({}));
        assert_eq!(
            list[5].normalize(Some(&json!([{"y": 1}, 2]))),
            json!([{"x": "d"}])
        );
    }

    #[test]
    fn visible_when() {
        let (list, _) = fields(json!([
            {"key": "a", "visible_when": {"mode": ["x", "y"]}},
            {"key": "b", "visible_when": {"on": "true"}},
        ]));
        assert!(list[0].is_visible(&json!({"mode": "y"})));
        assert!(!list[0].is_visible(&json!({"mode": "z"})));
        assert!(list[1].is_visible(&json!({"on": true})));
    }
}
