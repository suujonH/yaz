//! Client for the model center. Other plugins declare [`PLUGIN_ID`] in the dependencies of
//! plugin.json and write the setting item as `{"type": "resource", "plugin": PLUGIN_ID, "resource":
//! "model"}`; the value is the selected provider and model. Requests are handed to the model center
//! via the host (the caller never sees the API key). All calls are made on a worker thread and
//! block until a reply or timeout.

use serde_json::{Value, json};

use crate::abi;
use crate::host::request_resource;
use crate::types::str_of;

pub const PLUGIN_ID: &str = "com.yaz.model-center";

/// Chat model. Request `{"messages": [{"role", "content"}]}`, reply `{"content"}`.
pub const CHAT: &str = "model";
/// Decision model (OpenRouter decisions). Request `{"state", "questions"}`, reply `{"answers",
/// "model"}`.
pub const DECISION: &str = "decision_model";
/// Embedding model. Request `{"input": [text]}`, reply `{"embeddings": [[number]]}`.
pub const EMBEDDING: &str = "embedding_model";
/// Image model. Request `{"prompt", "size"?}`, reply `{"images": [{"url"} or {"b64"}]}`.
pub const IMAGE: &str = "image_model";
/// Rerank model. Request `{"query", "documents": [text], "top_n"?}`, reply `{"results": [{"index",
/// "score"}]}`.
pub const RERANK: &str = "rerank_model";
/// Speech synthesis model. Request `{"text", "voice"?}`, reply `{"audio": base64, "content_type"}`.
pub const SPEECH: &str = "speech_model";
/// Speech recognition model. Request `{"audio": base64, "filename"?}`, reply `{"text"}`.
pub const TRANSCRIPTION: &str = "transcription_model";

const UNKNOWN_REPLY: &str = "模型中心返回的格式无法识别";

/// The model selected in setting item `key`.
pub fn selection(settings: &Value, key: &str) -> Option<Value> {
    let value = settings.get(key)?;
    (!str_of(value, "resource").is_empty()).then(|| value.clone())
}

/// Chat: a system prompt (may be empty) plus one user message; returns the text of the model's
/// reply.
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

/// Decision: returns the answers object (question id -> answer).
pub fn decide(selection: &Value, state: &Value, questions: &Value) -> Result<Value, String> {
    let reply = request(selection, &json!({"state": state, "questions": questions}))?;
    match reply.get("answers") {
        Some(answers) if answers.is_object() => Ok(answers.clone()),
        _ => Err(UNKNOWN_REPLY.to_owned()),
    }
}

// An error in the reply also counts as a failure.
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
