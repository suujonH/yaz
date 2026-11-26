//! Model center: add providers in settings (interface type, EndPoint, API Key), fetch the model
//! list and add or test models; publishes the added models as the matching resources by interface
//! type. Translation and plugins that depend on the model center (id [`ID`]) choose a model in
//! their settings and hand requests to it through the host; they never see the API Key.

use serde_json::{Value, json};
use zulip_models::{ALL_TYPES, DECISIONS, Provider, resource_of};
use zulip_plugin_host::{BuiltinContext, BuiltinPlugin};
use zulip_plugin_sdk::abi::{LOG_ERROR, LOG_INFO, LOG_WARNING};
use zulip_plugin_sdk::str_of;

/// The id plugins already depend on (it was a plugin before it was built in).
pub const ID: &str = zulip_plugin_sdk::model_center::PLUGIN_ID;
pub const MANIFEST: &str = include_str!("model_center.json");

const FETCH_BUTTON: &str = "fetch";
const MODELS_FIELD: &str = "models";

pub struct ModelCenter;

impl BuiltinPlugin for ModelCenter {
    fn init(&self, ctx: &BuiltinContext) {
        publish(ctx);
    }

    fn on_settings_changed(&self, ctx: &BuiltinContext) {
        publish(ctx);
    }

    // "Fetch model list" in settings and the "Test" next to each model: values is this row's
    // current value (not yet saved).
    fn on_settings_button(&self, ctx: &BuiltinContext, button: &Value) {
        let key = str_of(button, "key");
        let item = str_of(button, "item").to_owned();
        let is_fetch = key == FETCH_BUTTON;
        if !is_fetch && (key != MODELS_FIELD || item.is_empty()) {
            return;
        }
        let provider = Provider::from_row(&button["values"]);
        let row = button["row"].as_i64();
        // The test result is shown on the button next to the model; the result of fetching the
        // model list is shown below the button.
        let fail = |text: &str| {
            if is_fetch {
                ctx.settings_text(FETCH_BUTTON, row, text);
            } else {
                item_result(ctx, row, &item, Some(text));
            }
        };
        if provider.endpoint.is_empty() && provider.kind != DECISIONS {
            fail("请先填写 EndPoint");
            return;
        }
        let worker = ctx.clone();
        if is_fetch {
            if !provider.can_list_models() {
                fail("只有 OpenRouter 提供判断模型的列表，请手动输入模型名称");
                return;
            }
            ctx.settings_text(FETCH_BUTTON, row, "正在获取模型列表…");
            ctx.spawn(move || fetch_models(&worker, &provider, row));
        } else {
            ctx.spawn(move || test_model(&worker, &provider, &item, row));
        }
    }

    // Requests from translation and plugins: {"type": resource type, "resource": provider name,
    // "item": model, "request": {...}, "from": plugin id}.
    fn on_resource_request(&self, ctx: &BuiltinContext, request: &Value) -> Value {
        let kind = str_of(request, "type");
        let name = str_of(request, "resource");
        let provider = providers(&ctx.settings())
            .into_iter()
            .find(|p| p.resource() == kind && p.name == name);
        let Some(provider) = provider else {
            return json!({"error": format!("模型中心中没有 {name}")});
        };
        let body = &request["request"];
        if !body.is_object() {
            return json!({"error": "请求格式错误"});
        }
        let model = str_of(request, "item");
        let from = str_of(request, "from");
        let name = &provider.name;
        match zulip_models::handle(&provider, model, body) {
            Ok(reply) => {
                ctx.log(LOG_INFO, &format!("{from} 使用了 {name} / {model}"));
                reply
            }
            Err(error) => {
                ctx.log(
                    LOG_ERROR,
                    &format!("{from} 使用 {name} / {model} 失败：{error}"),
                );
                let mut failure = json!({"error": error.to_string()});
                if let Some(status) = error.status {
                    failure["status"] = json!(status);
                }
                failure
            }
        }
    }
}

fn item_result(ctx: &BuiltinContext, row: Option<i64>, item: &str, error: Option<&str>) {
    let status = if error.is_none() { "ok" } else { "error" };
    let mut feedback = json!({"key": MODELS_FIELD, "item": item, "status": status});
    if let Some(error) = error {
        feedback["text"] = json!(error);
    }
    if let Some(row) = row {
        feedback["row"] = json!(row);
    }
    ctx.settings_feedback(&feedback);
}

// Valid providers (name and EndPoint filled in; among duplicate names within the same resource type
// only the first is taken).
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

// Publishes providers that have added models by resource type; types with none publish an empty
// list (clearing).
fn publish(ctx: &BuiltinContext) {
    let providers = providers(&ctx.settings());
    let mut resources: Vec<&str> = ALL_TYPES.iter().map(|t| resource_of(t)).collect();
    resources.sort_unstable();
    resources.dedup();
    for resource in resources {
        let list: Vec<Value> = providers
            .iter()
            .filter(|p| p.resource() == resource && !p.models.is_empty())
            .map(|p| json!({"id": p.name, "name": p.name, "items": p.models}))
            .collect();
        if !list.is_empty() {
            ctx.log(
                LOG_INFO,
                &format!("已提供 {} 个 provider 的 {resource}", list.len()),
            );
        }
        ctx.publish_resources(resource, &json!(list));
    }
}

fn fetch_models(ctx: &BuiltinContext, provider: &Provider, row: Option<i64>) {
    let models = match zulip_models::list_models(provider) {
        Ok(models) => models,
        Err(error) => {
            ctx.settings_text(FETCH_BUTTON, row, &format!("获取失败：{}", error.message));
            return;
        }
    };
    let mut feedback = json!({"key": MODELS_FIELD, "options": models});
    if let Some(row) = row {
        feedback["row"] = json!(row);
    }
    ctx.settings_feedback(&feedback);
    let text = if models.is_empty() {
        "没有获取到模型，可以在下方手动输入模型名称".to_owned()
    } else {
        format!("获取到 {} 个模型", models.len())
    };
    ctx.settings_text(FETCH_BUTTON, row, &text);
}

// Sends the test request by interface type; the failure reason includes the HTTP status code.
fn test_model(ctx: &BuiltinContext, provider: &Provider, model: &str, row: Option<i64>) {
    let result = zulip_models::handle(provider, model, &zulip_models::test_request(&provider.kind));
    let error = result.err().map(|e| match e.status {
        Some(status) => format!("HTTP {status}：{}", e.message),
        None => e.message,
    });
    if let Some(error) = &error {
        ctx.log(
            LOG_WARNING,
            &format!("测试 {} / {model} 失败：{error}", provider.name),
        );
    }
    item_result(ctx, row, model, error.as_deref());
}
