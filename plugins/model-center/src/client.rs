//! 按接口类型发送请求（同步，在工作线程上调用）：接收 `zulip_plugin_sdk::model_center` 约定的请求，
//! 转换为对应接口的格式，再把服务器的回复转换为约定的回复。

use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ureq::Agent;
use ureq::http::Request;
use ureq::tls::{RootCerts, TlsConfig};
use zulip_plugin_sdk::model_center::{DECISION, EMBEDDING, IMAGE, RERANK, SPEECH, TRANSCRIPTION};
use zulip_plugin_sdk::serde_json::{Map, Value, json};
use zulip_plugin_sdk::{serde_json, str_of};

use crate::provider::{
    CHAT_API, CLAUDE, DECISIONS, EMBEDDINGS, GEMINI, IMAGES, Provider, RERANK_API, RESPONSES,
    SPEECH_API, TRANSCRIPTION_API, resource_of,
};

// 短于宿主等待回复的 120 秒，超时时其他插件能收到原因。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(100);
const MAX_RESPONSE: u64 = 256 * 1024 * 1024;
const BOUNDARY: &str = "----zulip-plugin-model-center";

/// 请求失败的原因；`status` 为服务器返回的 HTTP 状态码（没有收到回复时为 `None`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelError {
    pub message: String,
    pub status: Option<u16>,
}

impl ModelError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: None,
        }
    }

    fn bad_request() -> Self {
        Self::new("请求格式错误")
    }

    fn unknown_reply(what: &str, json: &Value) -> Self {
        Self::new(format!(
            "无法识别{what}的回复：{}",
            truncate(&json.to_string())
        ))
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(f, "{}（HTTP {status}）", self.message),
            None => f.write_str(&self.message),
        }
    }
}

type Reply = Result<Value, ModelError>;

/// 处理一个请求，返回约定格式的回复。
pub fn handle(provider: &Provider, model: &str, request: &Value) -> Reply {
    match provider.kind.as_str() {
        CHAT_API => converse(provider, model, request, chat_body, chat_text),
        RESPONSES => converse(provider, model, request, responses_body, responses_text),
        CLAUDE => converse(provider, model, request, claude_body, claude_text),
        GEMINI => converse(provider, model, request, gemini_body, gemini_text),
        DECISIONS => decide(provider, model, request),
        EMBEDDINGS => embed(provider, model, request),
        IMAGES => generate_image(provider, model, request),
        RERANK_API => rerank(provider, model, request),
        SPEECH_API => speak(provider, model, request),
        TRANSCRIPTION_API => transcribe(provider, model, request),
        _ => Err(ModelError::bad_request()),
    }
}

/// 测试用的请求：对话类发送“你好”，其他类型各发送一个最简单的请求。
pub fn test_request(kind: &str) -> Value {
    let resource = resource_of(kind);
    if resource == DECISION {
        json!({
            "state": {"latest_messages": [{"from": "other", "text": "Could you send me the report by Friday?"}]},
            "questions": {"test": {
                "type": "noul",
                "instructions": "In latest_messages, does the other person ask the user to do something?",
                "criteria": {"true": "They ask the user to do something.", "false": "They ask for nothing."},
            }},
        })
    } else if resource == EMBEDDING {
        json!({"input": ["你好"]})
    } else if resource == IMAGE {
        json!({"prompt": "一只坐在窗台上的橘猫"})
    } else if resource == RERANK {
        json!({"query": "水果", "documents": ["苹果是一种水果", "今天天气很好"]})
    } else if resource == SPEECH {
        json!({"text": "你好"})
    } else if resource == TRANSCRIPTION {
        json!({"audio": STANDARD.encode(test_wav()), "filename": "test.wav"})
    } else {
        json!({"messages": [{"role": "user", "content": "你好"}]})
    }
}

/// 获取模型列表（按名称排序）。
pub fn list_models(provider: &Provider) -> Result<Vec<String>, ModelError> {
    let json = send_json(provider, "GET", &provider.models_url(), None, Vec::new())?;
    let mut ids = parse_model_list(&provider.kind, &json).ok_or_else(|| {
        ModelError::new(format!("无法识别模型列表：{}", truncate(&json.to_string())))
    })?;
    ids.retain(|id| !id.is_empty());
    ids.sort();
    ids.dedup();
    Ok(ids)
}

// OpenAI 兼容与 Claude 为 data[].id；Gemini 为 models[].name（“models/gemini-…”）；
// Decisions 只取 architecture.output_modalities 含 decisions 的（只有它们能调用 decisions 接口）。
fn parse_model_list(kind: &str, json: &Value) -> Option<Vec<String>> {
    if let Some(data) = json["data"].as_array() {
        return Some(
            data.iter()
                .filter(|m| {
                    kind != DECISIONS
                        || m["architecture"]["output_modalities"]
                            .as_array()
                            .is_some_and(|o| o.iter().any(|x| x == "decisions"))
                })
                .map(|m| str_of(m, "id").to_owned())
                .collect(),
        );
    }
    json["models"].as_array().map(|models| {
        models
            .iter()
            .map(|m| {
                let name = str_of(m, "name");
                name.strip_prefix("models/").unwrap_or(name).to_owned()
            })
            .collect()
    })
}

// ---------- 对话 ----------

fn converse(
    provider: &Provider,
    model: &str,
    request: &Value,
    body: fn(&str, &[Value]) -> Value,
    text: fn(&Value) -> Option<String>,
) -> Reply {
    let Some(messages) = request["messages"].as_array() else {
        return Err(ModelError::bad_request());
    };
    let json = post_json(provider, &provider.url(model), body(model, messages))?;
    text(&json)
        .map(|content| json!({ "content": content }))
        .ok_or_else(|| ModelError::unknown_reply("模型", &json))
}

fn chat_body(model: &str, messages: &[Value]) -> Value {
    let mut body = json!({ "messages": messages });
    if !model.is_empty() {
        body["model"] = json!(model);
    }
    body
}

fn chat_text(json: &Value) -> Option<String> {
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_owned)
}

fn responses_body(model: &str, messages: &[Value]) -> Value {
    json!({"model": model, "input": messages})
}

fn concat_parts<'a>(parts: impl Iterator<Item = &'a Value>, kind: Option<&str>) -> Option<String> {
    let texts: Vec<&str> = parts
        .filter(|p| kind.is_none_or(|k| str_of(p, "type") == k))
        .map(|p| str_of(p, "text"))
        .collect();
    (!texts.is_empty()).then(|| texts.concat())
}

// output[] 中 message 的 output_text。
fn responses_text(json: &Value) -> Option<String> {
    let direct = str_of(json, "output_text");
    if !direct.is_empty() {
        return Some(direct.to_owned());
    }
    let parts = json["output"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|o| o["content"].as_array().into_iter().flatten());
    concat_parts(parts, Some("output_text"))
}

fn system_text(messages: &[Value]) -> String {
    messages
        .iter()
        .filter(|m| str_of(m, "role") == "system")
        .map(|m| str_of(m, "content"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

// system 消息放到 system 字段；max_tokens 是必填项（可以用额外 Body 覆盖）。
fn claude_body(model: &str, messages: &[Value]) -> Value {
    let mut body = json!({"model": model, "max_tokens": 4096});
    let system = system_text(messages);
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    let items: Vec<Value> = messages
        .iter()
        .filter(|m| str_of(m, "role") != "system")
        .map(|m| {
            let role = if str_of(m, "role") == "assistant" {
                "assistant"
            } else {
                "user"
            };
            json!({"role": role, "content": str_of(m, "content")})
        })
        .collect();
    body["messages"] = json!(items);
    body
}

fn claude_text(json: &Value) -> Option<String> {
    concat_parts(json["content"].as_array()?.iter(), Some("text"))
}

// system 消息放到 systemInstruction；assistant 对应 model。
fn gemini_body(_model: &str, messages: &[Value]) -> Value {
    let mut body = json!({});
    let system = system_text(messages);
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }
    let contents: Vec<Value> = messages
        .iter()
        .filter(|m| str_of(m, "role") != "system")
        .map(|m| {
            let role = if str_of(m, "role") == "assistant" {
                "model"
            } else {
                "user"
            };
            json!({"role": role, "parts": [{"text": str_of(m, "content")}]})
        })
        .collect();
    body["contents"] = json!(contents);
    body
}

fn gemini_text(json: &Value) -> Option<String> {
    concat_parts(
        json["candidates"][0]["content"]["parts"].as_array()?.iter(),
        None,
    )
}

// ---------- 其他类型 ----------

// OpenRouter decisions：POST {"model", "state", "questions"}。
fn decide(provider: &Provider, model: &str, request: &Value) -> Reply {
    let (state, questions) = (&request["state"], &request["questions"]);
    if !state.is_object() || !questions.is_object() {
        return Err(ModelError::bad_request());
    }
    let body = json!({"model": model, "state": state, "questions": questions});
    let json = post_json(provider, &provider.url(model), body)?;
    if json["answers"].is_object() {
        Ok(json!({"answers": json["answers"], "model": json["model"]}))
    } else {
        Err(ModelError::unknown_reply("分析模型", &json))
    }
}

// POST /embeddings {"model", "input"} → data[].embedding（按 index 排序）。
fn embed(provider: &Provider, model: &str, request: &Value) -> Reply {
    let input = request["input"].as_array().filter(|i| !i.is_empty());
    let Some(input) = input else {
        return Err(ModelError::bad_request());
    };
    let body = json!({"model": model, "input": input});
    let json = post_json(provider, &provider.url(model), body)?;
    let Some(data) = json["data"].as_array() else {
        return Err(ModelError::unknown_reply("向量模型", &json));
    };
    let mut items: Vec<&Value> = data.iter().filter(|d| d.is_object()).collect();
    items.sort_by_key(|d| d["index"].as_i64().unwrap_or(0));
    let embeddings: Vec<&Value> = items.iter().map(|d| &d["embedding"]).collect();
    Ok(json!({ "embeddings": embeddings }))
}

// POST /images/generations {"model", "prompt", "n", "size"?} → data[].url 或 data[].b64_json。
fn generate_image(provider: &Provider, model: &str, request: &Value) -> Reply {
    let prompt = str_of(request, "prompt");
    if prompt.is_empty() {
        return Err(ModelError::bad_request());
    }
    let mut body = json!({"model": model, "prompt": prompt, "n": 1});
    let size = str_of(request, "size");
    if !size.is_empty() {
        body["size"] = json!(size);
    }
    let json = post_json(provider, &provider.url(model), body)?;
    let Some(data) = json["data"].as_array() else {
        return Err(ModelError::unknown_reply("图片模型", &json));
    };
    let images: Vec<Value> = data
        .iter()
        .filter(|d| d.is_object())
        .map(|d| match str_of(d, "url") {
            "" => json!({"b64": str_of(d, "b64_json")}),
            url => json!({ "url": url }),
        })
        .collect();
    Ok(json!({ "images": images }))
}

// POST /rerank {"model", "query", "documents", "top_n"?} → results[]{index, relevance_score}。
fn rerank(provider: &Provider, model: &str, request: &Value) -> Reply {
    let query = str_of(request, "query");
    let documents = &request["documents"];
    if query.is_empty() || !documents.is_array() {
        return Err(ModelError::bad_request());
    }
    let mut body = json!({"model": model, "query": query, "documents": documents});
    if request["top_n"].is_number() {
        body["top_n"] = request["top_n"].clone();
    }
    let json = post_json(provider, &provider.url(model), body)?;
    let Some(results) = json["results"].as_array() else {
        return Err(ModelError::unknown_reply("重排序模型", &json));
    };
    let mut items: Vec<(i64, f64)> = results
        .iter()
        .filter(|r| r.is_object())
        .map(|r| {
            let score = r["relevance_score"]
                .as_f64()
                .or(r["score"].as_f64())
                .unwrap_or(0.0);
            (r["index"].as_i64().unwrap_or(-1), score)
        })
        .collect();
    items.sort_by(|a, b| b.1.total_cmp(&a.1));
    let results: Vec<Value> = items
        .into_iter()
        .map(|(index, score)| json!({"index": index, "score": score}))
        .collect();
    Ok(json!({ "results": results }))
}

// POST /audio/speech {"model", "input", "voice"} → 音频数据。
fn speak(provider: &Provider, model: &str, request: &Value) -> Reply {
    let text = str_of(request, "text");
    if text.is_empty() {
        return Err(ModelError::bad_request());
    }
    let voice = match str_of(request, "voice") {
        "" => "alloy",
        voice => voice,
    };
    let body = merge(
        json!({"model": model, "input": text, "voice": voice}),
        provider,
    );
    let (audio, content_type) = send(
        provider,
        "POST",
        &provider.url(model),
        Some("application/json"),
        body.to_string().into_bytes(),
    )?;
    Ok(json!({"audio": STANDARD.encode(audio), "content_type": content_type}))
}

// POST /audio/transcriptions（multipart：file、model）→ {"text"}。
fn transcribe(provider: &Provider, model: &str, request: &Value) -> Reply {
    let audio = STANDARD
        .decode(str_of(request, "audio"))
        .ok()
        .filter(|a| !a.is_empty())
        .ok_or_else(ModelError::bad_request)?;
    let file_name = match str_of(request, "filename") {
        "" => "audio.wav",
        name => name,
    };
    let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
    let json = send_json(
        provider,
        "POST",
        &provider.url(model),
        Some(&content_type),
        multipart_body(&audio, file_name, model),
    )?;
    match json["text"].as_str() {
        Some(text) => Ok(json!({ "text": text })),
        None => Err(ModelError::unknown_reply("语音识别模型", &json)),
    }
}

fn multipart_body(audio: &[u8], file_name: &str, model: &str) -> Vec<u8> {
    let file_name = file_name.replace(['"', '\r', '\n'], "_");
    let mut body = Vec::with_capacity(audio.len() + 256);
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(audio);
    body.extend_from_slice(
        format!(
            "\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n--{BOUNDARY}--\r\n"
        )
        .as_bytes(),
    );
    body
}

// 1 秒、16 kHz、16 位单声道的 440 Hz 提示音（WAV）。
fn test_wav() -> Vec<u8> {
    const RATE: u32 = 16000;
    let mut wav = Vec::with_capacity(44 + RATE as usize * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + RATE * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    for i in 0..RATE {
        let phase = 2.0 * std::f64::consts::PI * 440.0 * f64::from(i) / f64::from(RATE);
        wav.extend_from_slice(&((phase.sin() * 3000.0) as i16).to_le_bytes());
    }
    wav
}

// ---------- HTTP ----------

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into()
    })
}

// POST JSON（合并额外 Body）并解析回复。
fn post_json(provider: &Provider, url: &str, body: Value) -> Reply {
    let body = merge(body, provider).to_string().into_bytes();
    send_json(provider, "POST", url, Some("application/json"), body)
}

// 额外 Body 合并到请求体（同名字段覆盖）。
fn merge(mut body: Value, provider: &Provider) -> Value {
    if let (Some(extra), Some(object)) = (&provider.body, body.as_object_mut()) {
        let extra: &Map<String, Value> = extra;
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    body
}

fn send_json(
    provider: &Provider,
    method: &str,
    url: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> Reply {
    let (data, _) = send(provider, method, url, content_type, body)?;
    let text = String::from_utf8_lossy(&data);
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| ModelError::new(format!("无法识别服务器的回复：{}", truncate(&text))))
}

// API Key：Claude 为 x-api-key（并补上 anthropic-version），Gemini 为 x-goog-api-key，其他为 Authorization: Bearer。
fn auth_headers(provider: &Provider) -> Vec<(&'static str, String)> {
    let mut headers = Vec::new();
    if provider.kind == CLAUDE
        && !provider
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-version"))
    {
        headers.push(("anthropic-version", "2023-06-01".to_owned()));
    }
    if !provider.api_key.is_empty() {
        headers.push(match provider.kind.as_str() {
            CLAUDE => ("x-api-key", provider.api_key.clone()),
            GEMINI => ("x-goog-api-key", provider.api_key.clone()),
            _ => ("Authorization", format!("Bearer {}", provider.api_key)),
        });
    }
    headers
}

// 发送并返回回复的内容与类型；服务器返回错误时含 HTTP 状态码和服务器给出的原因。
fn send(
    provider: &Provider,
    method: &str,
    url: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> Result<(Vec<u8>, String), ModelError> {
    let mut request = Request::builder().method(method).uri(url);
    if let Some(content_type) = content_type {
        request = request.header("Content-Type", content_type);
    }
    for (name, value) in auth_headers(provider) {
        request = request.header(name, value);
    }
    for (name, value) in &provider.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let failed = |e: &dyn fmt::Display| ModelError::new(format!("请求失败：{e}"));
    let request = request.body(body).map_err(|e| failed(&e))?;
    let mut response = agent().run(request).map_err(|e| failed(&e))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap_or_default()
        .trim()
        .to_owned();
    let data = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE)
        .read_to_vec()
        .map_err(|e| failed(&e))?;
    if !status.is_success() {
        return Err(ModelError {
            message: error_message(&String::from_utf8_lossy(&data)),
            status: Some(status.as_u16()),
        });
    }
    Ok((data, content_type))
}

// 错误回复中的原因：{"error": {"message"}}、{"error": "…"}、{"message"}，否则为原文。
fn error_message(text: &str) -> String {
    if let Ok(json) = serde_json::from_str::<Value>(text) {
        let nested = match &json["error"] {
            error if error.is_object() => str_of(error, "message"),
            _ => str_of(&json, "error"),
        };
        let message = if nested.is_empty() {
            str_of(&json, "message")
        } else {
            nested
        };
        if !message.is_empty() {
            return truncate(message);
        }
    }
    match text.trim() {
        "" => "（服务器没有说明原因）".to_owned(),
        text => truncate(text),
    }
}

fn truncate(text: &str) -> String {
    if text.chars().count() > 300 {
        format!("{}…", text.chars().take(300).collect::<String>())
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    fn messages() -> Vec<Value> {
        vec![
            json!({"role": "system", "content": "规则"}),
            json!({"role": "user", "content": "问"}),
            json!({"role": "assistant", "content": "答"}),
        ]
    }

    #[test]
    fn request_bodies() {
        let claude = claude_body("m", &messages());
        assert_eq!(claude["system"], "规则");
        assert_eq!(claude["max_tokens"], 4096);
        assert_eq!(claude["messages"].as_array().unwrap().len(), 2);

        let gemini = gemini_body("m", &messages());
        assert_eq!(gemini["systemInstruction"]["parts"][0]["text"], "规则");
        assert_eq!(gemini["contents"][1]["role"], "model");

        assert!(chat_body("", &messages()).get("model").is_none());
        assert_eq!(chat_body("m", &messages())["model"], "m");
    }

    #[test]
    fn reply_texts() {
        assert_eq!(
            chat_text(&json!({"choices": [{"message": {"content": "好"}}]})).as_deref(),
            Some("好")
        );
        assert_eq!(chat_text(&json!({})), None);
        assert_eq!(
            responses_text(&json!({"output": [
                {"type": "reasoning"},
                {"content": [{"type": "output_text", "text": "a"}, {"type": "other", "text": "x"}, {"type": "output_text", "text": "b"}]},
            ]}))
            .as_deref(),
            Some("ab")
        );
        assert_eq!(
            claude_text(&json!({"content": [{"type": "text", "text": "c"}]})).as_deref(),
            Some("c")
        );
        assert_eq!(
            gemini_text(
                &json!({"candidates": [{"content": {"parts": [{"text": "d"}, {"text": "e"}]}}]})
            )
            .as_deref(),
            Some("de")
        );
    }

    #[test]
    fn model_lists() {
        let decisions = json!({"data": [
            {"id": "a", "architecture": {"output_modalities": ["decisions"]}},
            {"id": "b", "architecture": {"output_modalities": ["text"]}},
        ]});
        assert_eq!(parse_model_list(DECISIONS, &decisions).unwrap(), vec!["a"]);
        assert_eq!(parse_model_list(CHAT_API, &decisions).unwrap().len(), 2);
        let gemini = json!({"models": [{"name": "models/g"}]});
        assert_eq!(parse_model_list(GEMINI, &gemini).unwrap(), vec!["g"]);
        assert_eq!(parse_model_list(CHAT_API, &json!({"x": 1})), None);
    }

    #[test]
    fn error_messages() {
        assert_eq!(error_message(r#"{"error": {"message": "坏"}}"#), "坏");
        assert_eq!(error_message(r#"{"error": "坏"}"#), "坏");
        assert_eq!(error_message(r#"{"message": "坏"}"#), "坏");
        assert_eq!(error_message(" 文字 "), "文字");
        assert_eq!(error_message(""), "（服务器没有说明原因）");
        assert_eq!(truncate(&"字".repeat(301)).chars().count(), 301);
    }

    #[test]
    fn test_wav_has_declared_length() {
        let wav = test_wav();
        assert_eq!(wav.len(), 44 + 32000);
        assert_eq!(&wav[..4], b"RIFF");
    }

    // 只回复一次的本地服务器；返回地址和收到的请求。
    fn serve_once(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                received.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&received);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&received).into_owned()
        });
        (url, handle)
    }

    #[test]
    fn sends_chat_over_http() {
        let (url, server) = serve_once(
            "200 OK",
            r#"{"choices": [{"message": {"content": "你好"}}]}"#,
        );
        let provider = Provider::from_row(&json!({
            "name": "p", "type": "chat", "endpoint": url, "api_key": "k",
            "headers": "X-Extra: 1", "body": "{\"temperature\": 0}",
        }));
        let reply = handle(
            &provider,
            "m",
            &json!({"messages": [{"role": "user", "content": "问"}]}),
        )
        .unwrap();
        assert_eq!(reply["content"], "你好");
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /chat/completions"));
        assert!(request.contains("uthorization: Bearer k"));
        assert!(request.to_ascii_lowercase().contains("x-extra: 1"));
        assert!(request.contains("\"temperature\":0"));
    }

    #[test]
    fn reports_http_errors() {
        let (url, server) = serve_once("401 Unauthorized", r#"{"error": {"message": "密钥错误"}}"#);
        let provider = Provider::from_row(&json!({"name": "p", "endpoint": url}));
        let error = handle(&provider, "m", &json!({"messages": []})).unwrap_err();
        assert_eq!(error.status, Some(401));
        assert_eq!(error.to_string(), "密钥错误（HTTP 401）");
        server.join().unwrap();
        assert_eq!(
            handle(&provider, "m", &json!({})).unwrap_err().message,
            "请求格式错误"
        );
    }
}
