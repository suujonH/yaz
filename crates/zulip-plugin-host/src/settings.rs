//! 插件设置的值：保存在程序级设置 `plugin_settings:<id>` 中（JSON 对象），密码类的值用 DPAPI 加密。
//! 无法解密的密文（换了 Windows 账户）读取时为空；保存时如果还是空，原样保留密文。

use std::cell::RefCell;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};
use zulip_plugin_sdk::str_of;
use zulip_store::SettingRepo;
use zulip_store::{protect_key, unprotect_key};

use crate::manifest::PluginManifest;
use crate::setting_field::{PluginSettingField, PluginSettingType, normalize_values};

const ENCRYPTED_PREFIX: &str = "dpapi:";

fn storage_key(plugin_id: &str) -> String {
    format!("plugin_settings:{plugin_id}")
}

fn saved_values(settings: &SettingRepo, manifest: &PluginManifest) -> Option<Value> {
    settings
        .get(&storage_key(&manifest.id))
        .and_then(|json| serde_json::from_str::<Value>(&json).ok())
}

/// 读取（解密、补全默认值）。没有保存过时为全部默认值。第二项是无法解密的密码项的 key。
pub(crate) fn load(
    settings: &SettingRepo,
    manifest: &PluginManifest,
) -> (Map<String, Value>, Vec<String>) {
    let saved = saved_values(settings, manifest);
    let mut result = normalize_values(
        &manifest.settings,
        saved.as_ref().and_then(Value::as_object),
    );
    let failed = RefCell::new(Vec::new());
    transform(&manifest.settings, &mut result, &|key, stored| {
        decrypt(stored).unwrap_or_else(|| {
            log::warn!("无法解密插件设置：{}：{key}", manifest.id);
            failed.borrow_mut().push(key.to_owned());
            String::new()
        })
    });
    (result, failed.into_inner())
}

/// 保存（按清单整理后加密）。无法加密时不写入，返回原因。
pub(crate) fn save(
    settings: &SettingRepo,
    manifest: &PluginManifest,
    values: &Map<String, Value>,
) -> Result<(), String> {
    let saved = saved_values(settings, manifest);
    let mut stored = normalize_values(&manifest.settings, Some(values));
    protect(
        &manifest.settings,
        &mut stored,
        saved.as_ref().and_then(Value::as_object),
    )?;
    settings.set(
        &storage_key(&manifest.id),
        &Value::Object(stored).to_string(),
    );
    Ok(())
}

// 对所有密码项（含列表中的）的非空值做转换。
fn transform(
    fields: &[PluginSettingField],
    values: &mut Map<String, Value>,
    convert: &dyn Fn(&str, &str) -> String,
) {
    for field in fields {
        let Some(value) = values.get_mut(&field.key) else {
            continue;
        };
        match (field.kind, value) {
            (PluginSettingType::Password, Value::String(text)) if !text.is_empty() => {
                *text = convert(&field.key, text);
            }
            (PluginSettingType::List, Value::Array(rows)) => {
                for row in rows.iter_mut().filter_map(Value::as_object_mut) {
                    transform(&field.fields, row, convert);
                }
            }
            _ => {}
        }
    }
}

// 加密所有密码项；`saved` 是存储中的旧值，空值遇到无法解密的旧密文时保留。
// 列表只有行数未变时才按下标对应旧行：增删过行后下标对应不到同一行，保留会把密文放到别的行上。
fn protect(
    fields: &[PluginSettingField],
    values: &mut Map<String, Value>,
    saved: Option<&Map<String, Value>>,
) -> Result<(), String> {
    for field in fields {
        let Some(value) = values.get_mut(&field.key) else {
            continue;
        };
        let old = saved.and_then(|s| s.get(&field.key));
        match (field.kind, value) {
            (PluginSettingType::Password, Value::String(text)) => {
                if !text.is_empty() {
                    *text = encrypt(text)?;
                } else if let Some(kept) = old
                    .and_then(Value::as_str)
                    .filter(|stored| decrypt(stored).is_none())
                {
                    kept.clone_into(text);
                }
            }
            (PluginSettingType::List, Value::Array(rows)) => {
                let row_count = rows.len();
                let old_rows = old
                    .and_then(Value::as_array)
                    .filter(|old_rows| old_rows.len() == row_count);
                for (index, row) in rows.iter_mut().enumerate() {
                    if let Some(row) = row.as_object_mut() {
                        let old_row = old_rows
                            .and_then(|r| r.get(index))
                            .and_then(Value::as_object);
                        protect(&field.fields, row, old_row)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn encrypt(plain: &str) -> Result<String, String> {
    let data = protect_key(plain).map_err(|error| error.to_string())?;
    Ok(format!("{ENCRYPTED_PREFIX}{}", STANDARD.encode(data)))
}

// 无法解密时为 `None`；不带前缀的是明文。
fn decrypt(stored: &str) -> Option<String> {
    let Some(encoded) = stored.strip_prefix(ENCRYPTED_PREFIX) else {
        return Some(stored.to_owned());
    };
    STANDARD
        .decode(encoded)
        .ok()
        .and_then(|data| unprotect_key(&data).ok())
}

/// 插件通过 set_settings_feedback 回复设置窗口。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsFeedback {
    pub key: String,
    pub row: Option<i64>,
    pub text: Option<String>,
    /// tags 设置项的候选。
    pub options: Option<Vec<String>>,
    /// tags 某一项的按钮（item_button）的结果对应的项。
    pub item: Option<String>,
    /// item 按钮的结果；没有 status 时为 `None`。
    pub ok: Option<bool>,
}

impl SettingsFeedback {
    /// 格式错误或没有 key 时为 `None`。
    pub fn parse(json: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(json).ok()?;
        let key = str_of(&value, "key");
        if key.is_empty() {
            return None;
        }
        let options = value["options"].as_array().map(|items| {
            let mut result: Vec<String> = Vec::new();
            for option in items.iter().filter_map(Value::as_str).map(str::trim) {
                if !option.is_empty() && !result.iter().any(|o| o == option) {
                    result.push(option.to_owned());
                }
            }
            result
        });
        Some(Self {
            key: key.to_owned(),
            row: value["row"].as_i64(),
            text: value["text"].as_str().map(str::to_owned),
            options,
            item: value["item"]
                .as_str()
                .filter(|i| !i.is_empty())
                .map(str::to_owned),
            ok: match str_of(&value, "status") {
                "ok" => Some(true),
                "error" => Some(false),
                _ => None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn feedback_parse() {
        let f = SettingsFeedback::parse(
            r#"{"key": "m", "row": 2, "options": [" a ", "a", ""], "item": "x", "status": "error", "text": "坏"}"#,
        )
        .unwrap();
        assert_eq!(f.row, Some(2));
        assert_eq!(f.options, Some(vec!["a".to_owned()]));
        assert_eq!(f.ok, Some(false));
        assert_eq!(SettingsFeedback::parse("{}"), None);
        assert_eq!(SettingsFeedback::parse("坏"), None);
    }

    #[test]
    fn transform_reaches_list_rows() {
        let mut warnings = Vec::new();
        let fields = PluginSettingField::parse_list(
            &json!([
                {"key": "p", "type": "password"},
                {"key": "l", "type": "list", "fields": [{"key": "k", "type": "password"}, {"key": "n"}]},
            ]),
            &mut |w| warnings.push(w),
            true,
        );
        let mut values = json!({"p": "a", "l": [{"k": "b", "n": "c"}]})
            .as_object()
            .cloned()
            .unwrap();
        let encrypted_values = {
            protect(&fields, &mut values, None).unwrap();
            values.clone()
        };
        assert!(
            encrypted_values["p"]
                .as_str()
                .unwrap()
                .starts_with(ENCRYPTED_PREFIX)
        );
        assert_eq!(encrypted_values["l"][0]["n"], "c");
        transform(&fields, &mut values, &|_, stored| decrypt(stored).unwrap());
        assert_eq!(
            Value::Object(values),
            json!({"p": "a", "l": [{"k": "b", "n": "c"}]})
        );
    }

    #[test]
    fn empty_password_keeps_undecryptable_ciphertext() {
        let mut warnings = Vec::new();
        let fields = PluginSettingField::parse_list(
            &json!([
                {"key": "bad", "type": "password"},
                {"key": "good", "type": "password"},
                {"key": "l", "type": "list", "fields": [{"key": "k", "type": "password"}]},
            ]),
            &mut |w| warnings.push(w),
            true,
        );
        let broken = format!("{ENCRYPTED_PREFIX}AAAA");
        let good = encrypt("旧").unwrap();
        let saved = json!({"bad": broken, "good": good, "l": [{"k": broken}]});
        let mut values = json!({"bad": "", "good": "", "l": [{"k": ""}]})
            .as_object()
            .cloned()
            .unwrap();
        protect(&fields, &mut values, saved.as_object()).unwrap();
        assert_eq!(values["bad"], broken.as_str());
        // 能解密的密码清空就是清空。
        assert_eq!(values["good"], "");
        assert_eq!(values["l"][0]["k"], broken.as_str());
        // 输入了新值时覆盖。
        let mut values = json!({"bad": "新", "good": "", "l": []})
            .as_object()
            .cloned()
            .unwrap();
        protect(&fields, &mut values, saved.as_object()).unwrap();
        assert_eq!(decrypt(values["bad"].as_str().unwrap()).unwrap(), "新");
    }

    #[test]
    fn list_keeps_undecryptable_ciphertext_only_when_row_count_is_unchanged() {
        let mut warnings = Vec::new();
        let fields = PluginSettingField::parse_list(
            &json!([
                {"key": "top", "type": "password"},
                {"key": "l", "type": "list", "fields": [{"key": "k", "type": "password"}, {"key": "n"}]},
            ]),
            &mut |w| warnings.push(w),
            true,
        );
        let broken = format!("{ENCRYPTED_PREFIX}AAAA");
        let saved = json!({
            "top": broken,
            "l": [{"k": broken, "n": "a"}, {"k": broken, "n": "b"}],
        });
        let protected = |values: Value| {
            let mut values = values.as_object().cloned().unwrap();
            protect(&fields, &mut values, saved.as_object()).unwrap();
            values
        };

        // 行数未变：按下标保留。
        let values = protected(json!({
            "top": "",
            "l": [{"k": "", "n": "a"}, {"k": "", "n": "b"}],
        }));
        assert_eq!(values["l"][0]["k"], broken.as_str());
        assert_eq!(values["l"][1]["k"], broken.as_str());

        // 删除第一行：剩下的一行不能继承任何旧密文；顶层密码项不受影响。
        let values = protected(json!({"top": "", "l": [{"k": "", "n": "b"}]}));
        assert_eq!(values["top"], broken.as_str());
        assert_eq!(values["l"][0]["k"], "");

        // 增加一行时同样不保留，用户输入的新值照常加密。
        let values = protected(json!({
            "top": "",
            "l": [{"k": "", "n": "a"}, {"k": "", "n": "b"}, {"k": "新", "n": "c"}],
        }));
        assert_eq!(values["l"][0]["k"], "");
        assert_eq!(values["l"][1]["k"], "");
        assert_eq!(
            decrypt(values["l"][2]["k"].as_str().unwrap()).unwrap(),
            "新"
        );
    }
}
