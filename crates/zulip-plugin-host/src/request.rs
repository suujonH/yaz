//! 插件传入的查询和受限请求的校验。

use serde_json::Value;
use zulip_api::{Form, Method};
use zulip_core::ConversationKey;
use zulip_plugin_sdk::str_of;
use zulip_session::{ContentFilter, MessageSearch, QueryScope};

const API_PREFIX: &str = "/api/v1/";
// 拿到或重置 API key 等于接管账号。
const DENIED_API_PATHS: [&str; 3] = [
    "fetch_api_key",
    "dev_fetch_api_key",
    "users/me/api_key/regenerate",
];
const MAX_PAGE_SIZE: u64 = 200;
const DEFAULT_PAGE_SIZE: u64 = 50;

/// `/api/v1/` 之下的路径才可以访问，不能带查询串，也不能用任何方式跳出前缀；获取、重置 API key 的接口除外。
pub(crate) fn is_allowed_api_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(API_PREFIX) else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains(['?', '#', '\\', '%'])
        && rest
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
        && !DENIED_API_PATHS
            .iter()
            .any(|denied| rest.trim_end_matches('/') == *denied)
}

pub(crate) fn api_method(method: &str) -> Option<Method> {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Some(Method::GET),
        "POST" => Some(Method::POST),
        "PUT" => Some(Method::PUT),
        "PATCH" => Some(Method::PATCH),
        "DELETE" => Some(Method::DELETE),
        _ => None,
    }
}

/// 请求参数：JSON 对象，字符串原样发送，其它值按 JSON 编码（Zulip 的约定）。
pub(crate) fn api_form(params: Option<&str>) -> Option<Form> {
    let Some(params) = params.filter(|p| !p.trim().is_empty()) else {
        return Some(Form::new());
    };
    let value: Value = serde_json::from_str(params).ok()?;
    if value.is_null() {
        return Some(Form::new());
    }
    let mut form = Form::new();
    for (name, value) in value.as_object()? {
        form = match value {
            Value::String(text) => form.add(name, text),
            other => form.add(name, other),
        };
    }
    Some(form)
}

/// 规范化后的 `/user_uploads/...`；其它路径（包括编码的跳出前缀）为 `None`。
pub(crate) fn upload_path(path: &str) -> Option<String> {
    let path = path.trim();
    let normalized = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    let rest = normalized.strip_prefix("/user_uploads/")?;
    let lower = rest.to_ascii_lowercase();
    (!rest.is_empty()
        && !rest.contains(['?', '#', '\\'])
        && !lower.contains("%2e")
        && !lower.contains("%2f")
        && !lower.contains("%5c")
        && rest
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".."))
    .then_some(normalized)
}

/// 搜索查询。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SearchQuery {
    pub search: MessageSearch,
    pub page_size: u32,
    pub cursor: Option<i64>,
}

fn content_filter(name: &str) -> Option<ContentFilter> {
    match name {
        "" | "all" => Some(ContentFilter::All),
        "plain" => Some(ContentFilter::PlainChat),
        "media" => Some(ContentFilter::Media),
        "files" => Some(ContentFilter::Files),
        "attachments" => Some(ContentFilter::Attachments),
        "links" => Some(ContentFilter::Links),
        _ => None,
    }
}

/// 格式见头文件 search_messages。`self_id` 用于私聊的会话键。
pub(crate) fn parse_search(json: &str, self_id: i64) -> Option<SearchQuery> {
    let root: Value = serde_json::from_str(json).ok()?;
    if !root.is_object() {
        return None;
    }
    let conv = &root["conv"];
    let scope = if conv.is_object() {
        QueryScope::Conversation(conversation_key(conv, self_id)?)
    } else if root["starred"].as_bool() == Some(true) {
        QueryScope::Starred
    } else {
        QueryScope::Everywhere
    };
    Some(SearchQuery {
        search: MessageSearch {
            scope,
            filter: content_filter(str_of(&root, "filter"))?,
            text: str_of(&root, "text").to_owned(),
            sender: root["sender"].as_i64().filter(|id| *id > 0),
            until: root["until"].as_str().map(str::to_owned),
        },
        page_size: root["page_size"]
            .as_u64()
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE) as u32,
        cursor: root["cursor"].as_i64().filter(|c| *c > 0),
    })
}

fn conversation_key(conv: &Value, self_id: i64) -> Option<ConversationKey> {
    match str_of(conv, "type") {
        "channel" => {
            let topic = str_of(conv, "topic");
            let stream = conv["stream"].as_i64().filter(|id| *id > 0)?;
            (!topic.is_empty()).then(|| ConversationKey::topic(stream, topic))
        }
        "direct" => {
            let users: Vec<i64> = conv["users"]
                .as_array()?
                .iter()
                .filter_map(Value::as_i64)
                .collect();
            Some(if users.is_empty() {
                ConversationKey::dm([self_id], self_id)
            } else {
                ConversationKey::dm(users, self_id)
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn api_path_stays_under_prefix() {
        for good in [
            "/api/v1/messages",
            "/api/v1/messages/12/history",
            "/api/v1/users/me",
        ] {
            assert!(is_allowed_api_path(good), "{good}");
        }
        for bad in [
            "",
            "/",
            "/api/v1/",
            "/api/v2/messages",
            "api/v1/messages",
            "https://evil.test/api/v1/messages",
            "//evil.test/api/v1/messages",
            "/user_uploads/a",
            "/api/v1/../../user_uploads/a",
            "/api/v1/./messages",
            "/api/v1/%2e%2e/%2e%2e/secret",
            "/api/v1/..\\..\\secret",
            "/api/v1//evil",
            "/api/v1/messages?anchor=1",
            "/api/v1/messages#x",
            "/api/v1/fetch_api_key",
            "/api/v1/dev_fetch_api_key",
            "/api/v1/users/me/api_key/regenerate",
        ] {
            assert!(!is_allowed_api_path(bad), "{bad}");
        }
    }

    #[test]
    fn api_method_and_form() {
        assert_eq!(api_method("get"), Some(Method::GET));
        assert_eq!(api_method("CONNECT"), None);
        assert!(api_form(None).unwrap().is_empty());
        assert!(api_form(Some("null")).unwrap().is_empty());
        assert!(api_form(Some("[1]")).is_none());
        assert!(api_form(Some("{坏")).is_none());
        let form = api_form(Some(r#"{"a": "文字", "b": 1, "c": [1, 2], "d": true}"#)).unwrap();
        let pairs: Vec<(&str, &str)> = form
            .pairs()
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("a", "文字"), ("b", "1"), ("c", "[1,2]"), ("d", "true")]
        );
    }

    #[test]
    fn upload_path_is_restricted() {
        assert_eq!(
            upload_path("/user_uploads/2/ab/%E6%96%87.png").as_deref(),
            Some("/user_uploads/2/ab/%E6%96%87.png")
        );
        assert_eq!(
            upload_path("user_uploads/2/a.txt").as_deref(),
            Some("/user_uploads/2/a.txt")
        );
        for bad in [
            "/user_uploads/",
            "/api/v1/messages",
            "https://x/user_uploads/a",
            "/user_uploads/../api/v1/x",
            "/user_uploads/%2E%2E/x",
            "/user_uploads/a%2fb",
            "/user_uploads/a?x=1",
            "/user_uploads/a\\b",
            "/user_uploads//a",
        ] {
            assert_eq!(upload_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn parses_search() {
        let q = parse_search(
            &json!({"conv": {"type": "channel", "stream": 2, "topic": "发布"}, "text": "x",
                    "filter": "files", "sender": 3, "page_size": 1000, "cursor": 9})
            .to_string(),
            1,
        )
        .unwrap();
        assert_eq!(
            q.search.scope,
            QueryScope::Conversation(ConversationKey::topic(2, "发布"))
        );
        assert_eq!(q.search.filter, ContentFilter::Files);
        assert_eq!((q.page_size, q.cursor), (200, Some(9)));

        let q = parse_search(r#"{"starred": true}"#, 1).unwrap();
        assert_eq!(q.search.scope, QueryScope::Starred);
        assert_eq!((q.page_size, q.cursor), (50, None));

        let q = parse_search(r#"{"conv": {"type": "direct", "users": []}}"#, 1).unwrap();
        assert_eq!(
            q.search.scope,
            QueryScope::Conversation(ConversationKey::dm([1], 1))
        );

        assert!(parse_search("[]", 1).is_none());
        assert!(parse_search(r#"{"filter": "未知"}"#, 1).is_none());
        assert!(parse_search(r#"{"conv": {"type": "channel", "stream": 2}}"#, 1).is_none());
    }
}
