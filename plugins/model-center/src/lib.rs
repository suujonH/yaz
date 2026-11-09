//! 模型中心：在设置中添加 provider（接口类型、EndPoint、API Key），获取模型列表并添加模型、测试模型；
//! 按接口类型把添加的模型发布为对应的资源，依赖本插件的其他插件在设置中选择后，
//! 通过宿主把请求交给本插件发送（其他插件看不到 API Key）。

mod client;
mod provider;

use zulip_plugin_sdk::abi::{LOG_ERROR, LOG_INFO, LOG_WARNING};
use zulip_plugin_sdk::serde_json::{Value, json};
use zulip_plugin_sdk::{
    Plugin, Result, SettingsButton, export_plugin, log, post, publish_resources, reply_resource,
    settings, settings_feedback, settings_item_result, settings_text, str_of,
};

use crate::provider::{ALL_TYPES, DECISIONS, Provider, resource_of};

const FETCH_BUTTON: &str = "fetch";
const MODELS_FIELD: &str = "models";

struct ModelCenter;

impl Plugin for ModelCenter {
    fn init(&self) -> Result<()> {
        publish(&settings());
        Ok(())
    }

    fn on_settings_changed(&self) {
        publish(&settings());
    }

    // 设置中的“获取模型列表”与每个模型旁的“测试”：values 为这一行当前的值（尚未保存）。
    fn on_settings_button(&self, button: &SettingsButton) {
        let item = button.item.clone().unwrap_or_default();
        let is_fetch = button.key == FETCH_BUTTON;
        if !is_fetch && (button.key != MODELS_FIELD || item.is_empty()) {
            return;
        }
        let provider = Provider::from_row(&button.values);
        let row = button.row;
        // 测试的结果显示在模型旁的按钮上，获取模型列表的显示在按钮下方。
        let fail = |text: &str| {
            let result = if is_fetch {
                settings_text(FETCH_BUTTON, row, text)
            } else {
                settings_item_result(MODELS_FIELD, row, &item, Some(text))
            };
            report(result);
        };
        if provider.endpoint.is_empty() && provider.kind != DECISIONS {
            fail("请先填写 EndPoint");
            return;
        }
        let queued = if is_fetch {
            if !provider.can_list_models() {
                fail("只有 OpenRouter 提供判断模型的列表，请手动输入模型名称");
                return;
            }
            report(settings_text(FETCH_BUTTON, row, "正在获取模型列表…"));
            post(move || fetch_models(&provider, row))
        } else {
            let model = item.clone();
            post(move || test_model(&provider, &model, row))
        };
        if !queued {
            fail("请求过多，请稍后再试");
        }
    }

    // 其他插件的请求：{"type": 资源类型, "resource": provider 名称, "item": 模型, "request": {...}, "from": 插件 id}。
    fn on_resource_request(&self, request_id: i64, request: &Value) {
        let kind = str_of(request, "type");
        let name = str_of(request, "resource");
        let provider = providers(&settings())
            .into_iter()
            .find(|p| p.resource() == kind && p.name == name);
        let Some(provider) = provider else {
            let error = format!("模型中心中没有 {name}");
            report(reply_resource(request_id, &json!({ "error": error })));
            return;
        };
        let body = request["request"].clone();
        if !body.is_object() {
            report(reply_resource(
                request_id,
                &json!({"error": "请求格式错误"}),
            ));
            return;
        }
        let model = str_of(request, "item").to_owned();
        let from = str_of(request, "from").to_owned();
        if !post(move || answer(request_id, &provider, &model, &body, &from)) {
            report(reply_resource(
                request_id,
                &json!({"error": "模型中心的请求过多，请稍后再试"}),
            ));
        }
    }
}

fn report(result: Result<()>) {
    if let Err(error) = result {
        log(LOG_WARNING, &format!("调用宿主失败：{error}"));
    }
}

// 有效的 provider（名称与 EndPoint 已填写；同一资源类型中重名的只取第一个）。
fn providers(settings: &Value) -> Vec<Provider> {
    let mut result: Vec<Provider> = Vec::new();
    for row in settings["providers"].as_array().into_iter().flatten() {
        let provider = Provider::from_row(row);
        if provider.is_configured()
            && !result
                .iter()
                .any(|p| p.resource() == provider.resource() && p.name == provider.name)
        {
            result.push(provider);
        }
    }
    result
}

// 按资源类型发布添加了模型的 provider；没有的类型发布空列表（清除）。
fn publish(settings: &Value) {
    let providers = providers(settings);
    let mut resources: Vec<&str> = ALL_TYPES.iter().map(|t| resource_of(t)).collect();
    resources.dedup();
    for resource in resources {
        let list: Vec<Value> = providers
            .iter()
            .filter(|p| p.resource() == resource && !p.models.is_empty())
            .map(|p| json!({"id": p.name, "name": p.name, "items": p.models}))
            .collect();
        match publish_resources(resource, &json!(list)) {
            Err(error) => log(LOG_WARNING, &format!("发布 {resource} 失败：{error}")),
            Ok(()) if !list.is_empty() => log(
                LOG_INFO,
                &format!("已提供 {} 个 provider 的 {resource}", list.len()),
            ),
            Ok(()) => {}
        }
    }
}

fn fetch_models(provider: &Provider, row: Option<i64>) {
    let models = match client::list_models(provider) {
        Ok(models) => models,
        Err(error) => {
            report(settings_text(
                FETCH_BUTTON,
                row,
                &format!("获取失败：{}", error.message),
            ));
            return;
        }
    };
    let mut feedback = json!({"key": MODELS_FIELD, "options": models});
    if let Some(row) = row {
        feedback["row"] = json!(row);
    }
    report(settings_feedback(&feedback));
    let text = if models.is_empty() {
        "没有获取到模型，可以在下方手动输入模型名称".to_owned()
    } else {
        format!("获取到 {} 个模型", models.len())
    };
    report(settings_text(FETCH_BUTTON, row, &text));
}

// 按接口类型发送测试请求，失败的原因含 HTTP 状态码。
fn test_model(provider: &Provider, model: &str, row: Option<i64>) {
    let result = client::handle(provider, model, &client::test_request(&provider.kind));
    let error = result.err().map(|e| match e.status {
        Some(status) => format!("HTTP {status}：{}", e.message),
        None => e.message,
    });
    report(settings_item_result(
        MODELS_FIELD,
        row,
        model,
        error.as_deref(),
    ));
}

fn answer(request_id: i64, provider: &Provider, model: &str, request: &Value, from: &str) {
    let name = &provider.name;
    match client::handle(provider, model, request) {
        Ok(reply) => {
            report(reply_resource(request_id, &reply));
            log(LOG_INFO, &format!("{from} 使用了 {name} / {model}"));
        }
        Err(error) => {
            let mut failure = json!({"error": error.to_string()});
            if let Some(status) = error.status {
                failure["status"] = json!(status);
            }
            report(reply_resource(request_id, &failure));
            log(
                LOG_ERROR,
                &format!("{from} 使用 {name} / {model} 失败：{error}"),
            );
        }
    }
}

export_plugin!(ModelCenter, "模型中心");
