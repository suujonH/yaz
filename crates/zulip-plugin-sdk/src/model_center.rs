//! 模型中心的客户端。其他插件在 plugin.json 的 dependencies 声明 [`PLUGIN_ID`]，设置项写作
//! `{"type": "resource", "plugin": PLUGIN_ID, "resource": "model"}`，值为选中的 provider 与模型。
//! 请求经宿主交给模型中心发送（使用者看不到 API Key）。都在工作线程上调用，阻塞到回复或超时。

use serde_json::{Value, json};

use crate::abi;
use crate::host::request_resource;
use crate::types::str_of;

pub const PLUGIN_ID: &str = "com.yaz.model-center";

/// 对话模型。请求 `{"messages": [{"role", "content"}]}`，回复 `{"content"}`。
pub const CHAT: &str = "model";
/// 判断模型（OpenRouter decisions）。请求 `{"state", "questions"}`，回复 `{"answers", "model"}`。
pub const DECISION: &str = "decision_model";
/// 向量模型。请求 `{"input": [文本]}`，回复 `{"embeddings": [[数字]]}`。
pub const EMBEDDING: &str = "embedding_model";
/// 图片模型。请求 `{"prompt", "size"?}`，回复 `{"images": [{"url"} 或 {"b64"}]}`。
pub const IMAGE: &str = "image_model";
/// 重排序模型。请求 `{"query", "documents": [文本], "top_n"?}`，回复 `{"results": [{"index", "score"}]}`。
pub const RERANK: &str = "rerank_model";
/// 语音合成模型。请求 `{"text", "voice"?}`，回复 `{"audio": base64, "content_type"}`。
pub const SPEECH: &str = "speech_model";
/// 语音识别模型。请求 `{"audio": base64, "filename"?}`，回复 `{"text"}`。
pub const TRANSCRIPTION: &str = "transcription_model";

const UNKNOWN_REPLY: &str = "模型中心返回的格式无法识别";

/// 设置项 `key` 中选中的模型。
pub fn selection(settings: &Value, key: &str) -> Option<Value> {
    let value = settings.get(key)?;
    (!str_of(value, "resource").is_empty()).then(|| value.clone())
}

/// 对话：系统提示词（可以为空）+ 一条用户消息，返回模型回复的文字。
pub fn ask(selection: &Value, system: &str, user: &str) -> Result<String, String> {
    let mut messages = Vec::new();
    if !system.trim().is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.push(json!({"role": "user", "content": user}));
    let reply = request(selection, &json!({"messages": messages}))?;
    reply["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| UNKNOWN_REPLY.to_owned())
}

/// 判断：返回 answers 对象（题目 id → 答案）。
pub fn decide(selection: &Value, state: &Value, questions: &Value) -> Result<Value, String> {
    let reply = request(selection, &json!({"state": state, "questions": questions}))?;
    match reply.get("answers") {
        Some(answers) if answers.is_object() => Ok(answers.clone()),
        _ => Err(UNKNOWN_REPLY.to_owned()),
    }
}

// 回复中的 error 也算失败。
fn request(selection: &Value, request: &Value) -> Result<Value, String> {
    let reply = request_resource(selection, request).map_err(|error| match error.0 {
        abi::ERR_NOT_FOUND => {
            "选择的模型不可用：请检查模型中心是否开启、模型是否仍在其中".to_owned()
        }
        abi::ERR_NETWORK => "等待模型中心回复超时".to_owned(),
        abi::ERR_PERMISSION => format!("plugin.json 的 dependencies 未声明 {PLUGIN_ID}"),
        code => format!("请求失败（{code}）"),
    })?;
    let value: Value = serde_json::from_str(&reply).map_err(|_| UNKNOWN_REPLY.to_owned())?;
    if !value.is_object() {
        return Err(UNKNOWN_REPLY.to_owned());
    }
    match str_of(&value, "error") {
        "" => Ok(value),
        error => Err(error.to_owned()),
    }
}
