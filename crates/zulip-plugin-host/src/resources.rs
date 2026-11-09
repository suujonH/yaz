//! 插件间共享的资源。宿主只是通道：资源类型与请求、回复的格式由发布资源的插件约定。
//! 资源只保存在内存中（插件每次加载、设置变化时重新发布）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Value, json};
use zulip_plugin_sdk::{abi, str_of};

use crate::event::PluginEvent;
use crate::host::PluginHost;
use crate::plugin::NativePlugin;
use crate::setting_field::PluginSettingField;

/// 等待发布资源的插件回复的时间。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 插件发布的一项资源：`items` 为其中可选的项（例如模型名称）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginResource {
    pub plugin_id: String,
    pub id: String,
    pub name: String,
    pub items: Vec<String>,
}

/// 资源类型只能由小写字母、数字和下划线组成（1～64 个字符）。
pub fn is_valid_type(kind: &str) -> bool {
    (1..=64).contains(&kind.len())
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 解析 publish_resources 的 JSON。id 为空、重复的资源忽略，name 为空时取 id，items 去掉空项和重复项。
/// 格式错误时为 `None`。
pub fn parse(plugin_id: &str, json: &str) -> Option<Vec<PluginResource>> {
    let value: Value = serde_json::from_str(json).ok()?;
    let mut result: Vec<PluginResource> = Vec::new();
    for object in value.as_array()?.iter().filter(|v| v.is_object()) {
        let id = str_of(object, "id").trim();
        if id.is_empty() || result.iter().any(|r| r.id == id) {
            continue;
        }
        let mut items: Vec<String> = Vec::new();
        for item in object["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
        {
            if !item.is_empty() && !items.iter().any(|i| i == item) {
                items.push(item.to_owned());
            }
        }
        let name = str_of(object, "name").trim();
        result.push(PluginResource {
            plugin_id: plugin_id.to_owned(),
            id: id.to_owned(),
            name: if name.is_empty() { id } else { name }.to_owned(),
            items,
        });
    }
    Some(result)
}

/// 设置中选中的资源（resource 类型设置项的值）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceSelection {
    pub kind: String,
    pub plugin_id: String,
    pub resource_id: String,
    pub item: String,
}

impl ResourceSelection {
    /// 类型不合法、插件或资源为空时为 `None`。
    pub fn from_json(value: &Value) -> Option<Self> {
        let selection = Self {
            kind: str_of(value, "type").to_owned(),
            plugin_id: str_of(value, "plugin").to_owned(),
            resource_id: str_of(value, "resource").to_owned(),
            item: str_of(value, "item").to_owned(),
        };
        (is_valid_type(&selection.kind)
            && !selection.plugin_id.is_empty()
            && !selection.resource_id.is_empty())
        .then_some(selection)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "type": self.kind,
            "plugin": self.plugin_id,
            "resource": self.resource_id,
            "item": self.item,
        })
    }
}

#[derive(Default)]
pub(crate) struct Resources {
    published: Mutex<HashMap<(String, String), Vec<PluginResource>>>,
    pending: Mutex<HashMap<i64, (String, SyncSender<String>)>>,
    next_request: AtomicI64,
}

impl Resources {
    /// 替换插件发布的某类资源；空列表表示清除。
    pub fn publish(&self, plugin_id: &str, kind: &str, resources: Vec<PluginResource>) {
        let key = (plugin_id.to_owned(), kind.to_owned());
        let mut published = self.published.lock();
        if resources.is_empty() {
            published.remove(&key);
        } else {
            published.insert(key, resources);
        }
    }

    pub fn published(&self, plugin_id: &str, kind: &str) -> Vec<PluginResource> {
        self.published
            .lock()
            .get(&(plugin_id.to_owned(), kind.to_owned()))
            .cloned()
            .unwrap_or_default()
    }

    /// 选择的资源和项仍然存在。
    pub fn contains(&self, selection: &ResourceSelection) -> bool {
        self.published(&selection.plugin_id, &selection.kind)
            .iter()
            .any(|r| {
                r.id == selection.resource_id
                    && (selection.item.is_empty() || r.items.contains(&selection.item))
            })
    }

    /// 登记一个等待中的请求。
    pub fn begin_request(&self, provider: &str) -> (i64, Receiver<String>) {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = sync_channel(1);
        self.pending.lock().insert(id, (provider.to_owned(), tx));
        (id, rx)
    }

    pub fn end_request(&self, id: i64) {
        self.pending.lock().remove(&id);
    }

    /// 发布资源的插件回复；请求不存在、已超时或不是发给该插件的返回 false。
    pub fn reply(&self, provider: &str, id: i64, json: String) -> bool {
        let mut pending = self.pending.lock();
        if pending.get(&id).is_none_or(|(owner, _)| owner != provider) {
            return false;
        }
        pending
            .remove(&id)
            .is_some_and(|(_, tx)| tx.try_send(json).is_ok())
    }
}

/// resource 设置项的一个选项；`selection` 为 `None` 表示不使用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceOption {
    pub selection: Option<ResourceSelection>,
    pub label: String,
    pub available: bool,
}

impl PluginHost {
    pub(crate) fn publish_resources(
        &self,
        plugin_id: &str,
        kind: &str,
        resources: Vec<PluginResource>,
    ) {
        self.resources.publish(plugin_id, kind, resources);
        self.emit(PluginEvent::ResourcesChanged);
    }

    /// 插件发布的某类资源；发布者不可用时为空。
    pub fn available_resources(&self, plugin_id: &str, kind: &str) -> Vec<PluginResource> {
        if self
            .plugin(plugin_id)
            .is_some_and(|p| p.can_provide_resources())
        {
            self.resources.published(plugin_id, kind)
        } else {
            Vec::new()
        }
    }

    /// resource 设置项的选项：第一项为“不使用”。已保存的选择不在可选项中时仍保留，标为不可用。
    pub fn resource_options(
        &self,
        field: &PluginSettingField,
        saved: Option<&ResourceSelection>,
    ) -> Vec<ResourceOption> {
        let mut options = vec![ResourceOption {
            selection: None,
            label: "不使用".to_owned(),
            available: true,
        }];
        for resource in self.available_resources(&field.resource_plugin, &field.resource_type) {
            for item in &resource.items {
                options.push(ResourceOption {
                    selection: Some(ResourceSelection {
                        kind: field.resource_type.clone(),
                        plugin_id: resource.plugin_id.clone(),
                        resource_id: resource.id.clone(),
                        item: item.clone(),
                    }),
                    label: format!("{} / {item}", resource.name),
                    available: true,
                });
            }
        }
        if let Some(saved) = saved
            && !options.iter().any(|o| o.selection.as_ref() == Some(saved))
        {
            options.push(ResourceOption {
                selection: Some(saved.clone()),
                label: format!("{} / {}（不可用）", saved.resource_id, saved.item),
                available: false,
            });
        }
        options
    }

    /// 依赖的插件没有发布该类资源时显示的文字。
    pub fn no_resources_text(&self, field: &PluginSettingField) -> String {
        let name = self
            .plugin(&field.resource_plugin)
            .map_or_else(|| field.resource_plugin.clone(), |p| p.name());
        format!("没有可选项：请先开启插件“{name}”并在其中添加")
    }

    /// 把请求转交给发布资源的插件并等待回复（阻塞，不能在插件线程上调用）。错误为 ABI 错误码。
    pub(crate) fn request_resource(
        &self,
        consumer: &NativePlugin,
        target: &ResourceSelection,
        request: &Value,
    ) -> Result<String, i32> {
        let Some(provider) = self
            .plugin(&target.plugin_id)
            .filter(|p| p.can_provide_resources())
        else {
            return Err(abi::ERR_NOT_FOUND);
        };
        if !self.resources.contains(target) {
            return Err(abi::ERR_NOT_FOUND);
        }
        let (id, reply) = self.resources.begin_request(&target.plugin_id);
        let payload = json!({
            "type": target.kind,
            "resource": target.resource_id,
            "item": target.item,
            "request": request,
            "from": consumer.id(),
        })
        .to_string();
        self.post(&provider, move |p| p.call_resource_request(id, &payload));
        let result = reply
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| abi::ERR_NETWORK);
        self.resources.end_request(id);
        result
    }

    pub(crate) fn reply_resource(&self, provider: &NativePlugin, id: i64, json: String) -> bool {
        self.resources.reply(&provider.id(), id, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normalizes() {
        let list = parse(
            "p",
            r#"[{"id": " a ", "items": ["x", " x", "", 1]}, {"id": "a"}, {"id": ""}, 3, {"id": "b", "name": "乙"}]"#,
        )
        .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "a");
        assert_eq!(list[0].items, vec!["x"]);
        assert_eq!(list[1].name, "乙");
        assert!(parse("p", "{}").is_none());
        assert!(parse("p", "坏").is_none());
    }

    #[test]
    fn selection_validates() {
        let good = json!({"type": "model", "plugin": "p", "resource": "r", "item": "i"});
        assert_eq!(ResourceSelection::from_json(&good).unwrap().to_json(), good);
        assert!(
            ResourceSelection::from_json(&json!({"type": "Bad!", "plugin": "p", "resource": "r"}))
                .is_none()
        );
        assert!(ResourceSelection::from_json(&json!({"type": "model", "plugin": "p"})).is_none());
    }

    #[test]
    fn request_reply_checks_provider() {
        let resources = Resources::default();
        let (id, rx) = resources.begin_request("provider");
        assert!(!resources.reply("other", id, "x".into()));
        assert!(resources.reply("provider", id, "好".into()));
        assert_eq!(rx.recv().unwrap(), "好");
        assert!(!resources.reply("provider", id, "x".into()));
    }

    #[test]
    fn publish_and_contains() {
        let resources = Resources::default();
        let list = parse("p", r#"[{"id": "r", "items": ["i"]}]"#).unwrap();
        resources.publish("p", "model", list);
        let selection = |item: &str| ResourceSelection {
            kind: "model".into(),
            plugin_id: "p".into(),
            resource_id: "r".into(),
            item: item.into(),
        };
        assert!(resources.contains(&selection("i")));
        assert!(resources.contains(&selection("")));
        assert!(!resources.contains(&selection("j")));
        resources.publish("p", "model", Vec::new());
        assert!(!resources.contains(&selection("i")));
    }
}
