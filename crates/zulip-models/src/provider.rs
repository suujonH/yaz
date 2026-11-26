//! One provider: a row of the providers setting (name, type, endpoint, api_key, models, headers,
//! body).

use zulip_plugin_sdk::abi::LOG_WARNING;
use zulip_plugin_sdk::model_center::{
    CHAT, DECISION, EMBEDDING, IMAGE, RERANK, SPEECH, TRANSCRIPTION,
};
use zulip_plugin_sdk::serde_json::{Map, Value};
use zulip_plugin_sdk::{log, serde_json, str_of};

pub const CHAT_API: &str = "chat";
pub const RESPONSES: &str = "responses";
pub const CLAUDE: &str = "claude";
pub const GEMINI: &str = "gemini";
pub const DECISIONS: &str = "decisions";
pub const EMBEDDINGS: &str = "embeddings";
pub const IMAGES: &str = "images";
pub const RERANK_API: &str = "rerank";
pub const SPEECH_API: &str = "speech";
pub const TRANSCRIPTION_API: &str = "transcription";

pub const ALL_TYPES: [&str; 10] = [
    CHAT_API,
    RESPONSES,
    CLAUDE,
    GEMINI,
    DECISIONS,
    EMBEDDINGS,
    IMAGES,
    RERANK_API,
    SPEECH_API,
    TRANSCRIPTION_API,
];

const OPENROUTER_DECISIONS_URL: &str = "https://openrouter.ai/api/alpha/decisions";
const OPENROUTER_DECISION_MODELS_URL: &str =
    "https://openrouter.ai/api/v1/models?output_modalities=decisions";

/// The resource type provided by the interface type.
pub fn resource_of(kind: &str) -> &'static str {
    match kind {
        DECISIONS => DECISION,
        EMBEDDINGS => EMBEDDING,
        IMAGES => IMAGE,
        RERANK_API => RERANK,
        SPEECH_API => SPEECH,
        TRANSCRIPTION_API => TRANSCRIPTION,
        _ => CHAT,
    }
}

// The interface path after the EndPoint (Gemini and Decisions are handled separately; empty).
fn path_of(kind: &str) -> &'static str {
    match kind {
        CHAT_API => "/chat/completions",
        RESPONSES => "/responses",
        CLAUDE => "/messages",
        EMBEDDINGS => "/embeddings",
        IMAGES => "/images/generations",
        RERANK_API => "/rerank",
        SPEECH_API => "/audio/speech",
        TRANSCRIPTION_API => "/audio/transcriptions",
        _ => "",
    }
}

#[derive(Clone, Debug, Default)]
pub struct Provider {
    pub name: String,
    pub kind: String,
    pub endpoint: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Map<String, Value>>,
}

impl Provider {
    /// When the interface type is not recognized it is Chat Completions; when the extra Body is not
    /// a JSON object it is ignored and logged.
    pub fn from_row(row: &Value) -> Self {
        let name = str_of(row, "name").trim().to_owned();
        let headers = str_of(row, "headers")
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                let key = key.trim();
                (!key.is_empty()).then(|| (key.to_owned(), value.trim().to_owned()))
            })
            .collect();
        let body_text = str_of(row, "body").trim();
        let body = if body_text.is_empty() {
            None
        } else {
            let parsed = serde_json::from_str::<Value>(body_text);
            match parsed.as_ref().ok().and_then(Value::as_object) {
                Some(object) => Some(object.clone()),
                None => {
                    log(
                        LOG_WARNING,
                        &format!("{name} 的额外 Body 无效，已忽略：不是 JSON 对象"),
                    );
                    None
                }
            }
        };
        let kind = str_of(row, "type");
        Self {
            name,
            kind: if ALL_TYPES.contains(&kind) {
                kind
            } else {
                CHAT_API
            }
            .to_owned(),
            endpoint: str_of(row, "endpoint").trim().to_owned(),
            api_key: str_of(row, "api_key").trim().to_owned(),
            models: row["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_owned)
                .collect(),
            headers,
            body,
        }
    }

    /// Name and EndPoint are filled in (the EndPoint of Decisions may be left empty).
    pub fn is_configured(&self) -> bool {
        !self.name.is_empty() && (!self.endpoint.is_empty() || self.kind == DECISIONS)
    }

    pub fn resource(&self) -> &'static str {
        resource_of(&self.kind)
    }

    // Strip the trailing / and interface path (a full URL may be entered, e.g.
    // .../chat/completions).
    fn base_url(&self) -> &str {
        let url = self.endpoint.trim_end_matches('/');
        let path = path_of(&self.kind);
        if !path.is_empty()
            && url.len() >= path.len()
            && url.is_char_boundary(url.len() - path.len())
            && url[url.len() - path.len()..].eq_ignore_ascii_case(path)
        {
            &url[..url.len() - path.len()]
        } else {
            url
        }
    }

    /// The URL to request `model`.
    pub fn url(&self, model: &str) -> String {
        match self.kind.as_str() {
            GEMINI => format!(
                "{}/models/{}:generateContent",
                self.base_url(),
                escape(model)
            ),
            DECISIONS => self.decisions_url(),
            kind => format!("{}{}", self.base_url(), path_of(kind)),
        }
    }

    // Filled in automatically when the EndPoint is empty, or it is OpenRouter but not a decisions
    // URL.
    fn decisions_url(&self) -> String {
        let is_decisions = self
            .endpoint
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/decisions");
        if self.endpoint.is_empty() || (self.is_openrouter() && !is_decisions) {
            OPENROUTER_DECISIONS_URL.to_owned()
        } else {
            self.endpoint.clone()
        }
    }

    fn is_openrouter(&self) -> bool {
        let lower = self.endpoint.to_ascii_lowercase();
        let Some(rest) = lower.split_once("://").map(|(_, rest)| rest) else {
            return false;
        };
        let host = rest.split(['/', ':', '?', '#']).next().unwrap_or_default();
        host == "openrouter.ai" || host.ends_with(".openrouter.ai")
    }

    /// Among Decisions, only OpenRouter has a model list (EndPoint empty or openrouter.ai).
    pub fn can_list_models(&self) -> bool {
        self.kind != DECISIONS || self.endpoint.is_empty() || self.is_openrouter()
    }

    pub fn models_url(&self) -> String {
        match self.kind.as_str() {
            GEMINI => format!("{}/models?pageSize=1000", self.base_url()),
            DECISIONS => OPENROUTER_DECISION_MODELS_URL.to_owned(),
            _ => format!("{}/models", self.base_url()),
        }
    }
}

// Percent-encoding (keeping unreserved characters).
fn escape(text: &str) -> String {
    let mut result = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use zulip_plugin_sdk::serde_json::json;

    use super::*;

    fn provider(kind: &str, endpoint: &str) -> Provider {
        Provider::from_row(&json!({"name": "p", "type": kind, "endpoint": endpoint}))
    }

    #[test]
    fn urls() {
        let chat = provider("chat", "https://a/v1/");
        assert_eq!(chat.url("m"), "https://a/v1/chat/completions");
        assert_eq!(chat.models_url(), "https://a/v1/models");
        let full = provider("chat", "https://a/v1/Chat/Completions");
        assert_eq!(full.url("m"), "https://a/v1/chat/completions");
        let gemini = provider("gemini", "https://g/v1beta");
        assert_eq!(
            gemini.url("a b"),
            "https://g/v1beta/models/a%20b:generateContent"
        );
        assert_eq!(gemini.models_url(), "https://g/v1beta/models?pageSize=1000");
    }

    #[test]
    fn decisions_endpoint_is_completed() {
        assert_eq!(provider("decisions", "").url("m"), OPENROUTER_DECISIONS_URL);
        let router = provider("decisions", "https://openrouter.ai/api/v1");
        assert_eq!(router.url("m"), OPENROUTER_DECISIONS_URL);
        assert!(router.can_list_models());
        let custom = provider("decisions", "https://x.example/d");
        assert_eq!(custom.url("m"), "https://x.example/d");
        assert!(!custom.can_list_models());
    }

    #[test]
    fn row_parsing() {
        let p = Provider::from_row(&json!({
            "name": " a ", "type": "nope", "endpoint": " e ", "api_key": " k ",
            "models": ["x", "", 1], "headers": "A: 1\n: 坏\n没有冒号\nB:2:3", "body": "{\"t\": 1}",
        }));
        assert_eq!(p.name, "a");
        assert_eq!(p.kind, "chat");
        assert_eq!(p.models, vec!["x"]);
        assert_eq!(
            p.headers,
            vec![
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "2:3".to_owned())
            ]
        );
        assert_eq!(p.body.unwrap()["t"], 1);
        assert!(
            Provider::from_row(&json!({"name": "a", "body": "[1]"}))
                .body
                .is_none()
        );
        assert!(!provider("chat", "").is_configured());
        assert!(provider("decisions", "").is_configured());
    }

    #[test]
    fn resources() {
        assert_eq!(resource_of("claude"), CHAT);
        assert_eq!(resource_of("speech"), SPEECH);
    }
}
