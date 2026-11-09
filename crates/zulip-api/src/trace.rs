//! 请求记录：开发者模式的“显示请求”。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Local};
use parking_lot::{Mutex, RwLock};
use reqwest::header::HeaderMap;

const MASK: &str = "******";

/// 记录的正文超过这个字符数时截断（register 的响应可能有几 MB）。
const MAX_BODY_CHARS: usize = 2 * 1024 * 1024;

/// 记录下来的请求。头部每行一个“名称: 值”；正文为文本，或无法记录时的说明（如 `[image/png，1024 字节，未记录内容]`）。
#[derive(Debug, Clone)]
pub struct TraceRequest {
    pub time: DateTime<Local>,
    pub method: String,
    pub url: String,
    pub headers: String,
    pub body: String,
}

/// 记录下来的响应；`status` 为 `None` 表示没有收到响应（`error` 为原因）。
#[derive(Debug, Clone)]
pub struct TraceResponse {
    pub status: Option<u16>,
    pub reason: String,
    pub headers: String,
    pub body: String,
    pub error: Option<String>,
    pub duration: Duration,
}

/// 请求记录的接收方。可能在任意线程上调用。
pub trait TraceSink: Send + Sync {
    fn is_enabled(&self) -> bool;

    /// 请求发出前调用，返回这条记录的 id。
    fn begin(&self, request: TraceRequest) -> u64;

    /// 收到响应或请求失败后调用。
    fn end(&self, id: u64, response: TraceResponse);
}

/// 请求记录中的一条。`response` 为 `None` 表示还在等待响应。
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub id: u64,
    pub request: TraceRequest,
    pub response: Option<TraceResponse>,
}

impl LogEntry {
    pub fn is_pending(&self) -> bool {
        self.response.is_none()
    }

    /// 没有收到响应，或状态码为 4xx、5xx。
    pub fn is_failed(&self) -> bool {
        self.response
            .as_ref()
            .is_some_and(|r| r.status.is_none_or(|s| s >= 400))
    }

    /// 网址、请求头、请求正文、响应头、响应正文、错误原因中是否包含 `query`（不区分大小写）。
    pub fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        let contains = |text: &str| text.to_lowercase().contains(&query);
        contains(&self.request.url)
            || contains(&self.request.headers)
            || contains(&self.request.body)
            || self.response.as_ref().is_some_and(|r| {
                contains(&r.headers)
                    || contains(&r.body)
                    || r.error.as_deref().is_some_and(contains)
            })
    }
}

type ChangeListener = Arc<dyn Fn() + Send + Sync>;

struct State {
    entries: VecDeque<Arc<LogEntry>>,
    next_id: u64,
    limit: usize,
}

/// 内存中的请求记录，超出保存条数时删除最早的记录。作为 [`TraceSink`] 交给 [`ClientConfig`](crate::ClientConfig)。
pub struct RequestLog {
    enabled: AtomicBool,
    state: Mutex<State>,
    on_change: RwLock<Option<ChangeListener>>,
}

impl RequestLog {
    /// 保存条数为此值时不限制。
    pub const UNLIMITED: usize = 0;

    pub fn new(enabled: bool, limit: usize) -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            state: Mutex::new(State {
                entries: VecDeque::new(),
                next_id: 0,
                limit,
            }),
            on_change: RwLock::new(None),
        })
    }

    /// 设置变化时调用。关闭后不再记录，已有的记录保留。
    pub fn configure(&self, enabled: bool, limit: usize) {
        self.enabled.store(enabled, Ordering::Relaxed);
        let trimmed = {
            let mut state = self.state.lock();
            state.limit = limit;
            Self::trim(&mut state)
        };
        if trimmed {
            self.notify();
        }
    }

    pub fn clear(&self) {
        self.state.lock().entries.clear();
        self.notify();
    }

    /// 按时间先后排列的记录。
    pub fn entries(&self) -> Vec<Arc<LogEntry>> {
        self.state.lock().entries.iter().cloned().collect()
    }

    /// 记录有变化（新增、收到响应、清除）时在发生变化的线程上调用，调用方自行转到界面线程。
    pub fn set_on_change(&self, listener: Option<ChangeListener>) {
        *self.on_change.write() = listener;
    }

    fn notify(&self) {
        let listener = self.on_change.read().clone();
        if let Some(listener) = listener {
            listener();
        }
    }

    fn trim(state: &mut State) -> bool {
        let mut trimmed = false;
        while state.limit != Self::UNLIMITED && state.entries.len() > state.limit {
            state.entries.pop_front();
            trimmed = true;
        }
        trimmed
    }
}

impl TraceSink for RequestLog {
    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn begin(&self, request: TraceRequest) -> u64 {
        let id = {
            let mut state = self.state.lock();
            state.next_id += 1;
            let id = state.next_id;
            state.entries.push_back(Arc::new(LogEntry {
                id,
                request,
                response: None,
            }));
            Self::trim(&mut state);
            id
        };
        self.notify();
        id
    }

    fn end(&self, id: u64, response: TraceResponse) {
        let updated = {
            let mut state = self.state.lock();
            // 已被清除或因超出条数被删除的记录不再更新
            state.entries.iter_mut().find(|e| e.id == id).map(|entry| {
                *entry = Arc::new(LogEntry {
                    id,
                    request: entry.request.clone(),
                    response: Some(response),
                });
            })
        };
        if updated.is_some() {
            self.notify();
        }
    }
}

/// 请求头，每行“名称: 值”；认证信息和 Cookie 打码。
pub(crate) fn format_headers(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let text = String::from_utf8_lossy(value.as_bytes());
            let shown = match name.as_str() {
                "authorization" | "proxy-authorization" => {
                    format!("{} {MASK}", text.split(' ').next().unwrap_or_default())
                }
                "cookie" | "set-cookie" => MASK.to_owned(),
                _ => text.into_owned(),
            };
            format!("{}: {shown}", canonical_header_name(name.as_str()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn canonical_header_name(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// 表单中的 `password`、`api_key` 打码。
pub(crate) fn mask_form(form: &str) -> String {
    form.split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _))
                if name.eq_ignore_ascii_case("password")
                    || name.eq_ignore_ascii_case("api_key") =>
            {
                format!("{name}={MASK}")
            }
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// JSON 中 `"api_key": "…"` 的值打码。
pub(crate) fn mask_json(json: &str) -> String {
    const KEY: &str = "\"api_key\"";
    let mut out = String::with_capacity(json.len());
    let mut rest = json;
    while let Some(found) = rest.find(KEY) {
        let after_key = found + KEY.len();
        let tail = &rest[after_key..];
        let value = tail
            .trim_start()
            .strip_prefix(':')
            .map(str::trim_start)
            .and_then(|t| t.strip_prefix('"'));
        match value.and_then(|v| v.find('"').map(|end| (v, end))) {
            Some((value, end)) => {
                out.push_str(&rest[..after_key + tail.len() - value.len()]);
                out.push_str(MASK);
                rest = &value[end..];
            }
            None => {
                out.push_str(&rest[..after_key]);
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 无法记录正文时的说明。
pub(crate) fn describe_body(content_type: Option<&str>, length: Option<u64>) -> String {
    let media_type = content_type
        .and_then(|t| t.split(';').next())
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let media_type = media_type.unwrap_or("未知类型");
    match length {
        Some(length) => format!("[{media_type}，{length} 字节，未记录内容]"),
        None => format!("[{media_type}，未记录内容]"),
    }
}

pub(crate) fn is_text_type(content_type: Option<&str>) -> bool {
    let Some(media_type) = content_type
        .and_then(|t| t.split(';').next())
        .map(|t| t.trim().to_ascii_lowercase())
    else {
        return false;
    };
    media_type.starts_with("text/")
        || media_type.ends_with("json")
        || media_type.ends_with("xml")
        || media_type == "application/x-www-form-urlencoded"
}

pub(crate) fn truncate_body(text: String) -> String {
    match text.char_indices().nth(MAX_BODY_CHARS) {
        None => text,
        Some((cut, _)) => {
            let total = text.chars().count();
            format!("{}\n[已截断，共 {total} 字符]", &text[..cut])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_form_secrets() {
        assert_eq!(
            mask_form("username=a%40b&password=p%26w&x=1"),
            "username=a%40b&password=******&x=1"
        );
        assert_eq!(mask_form("API_KEY=abc"), "API_KEY=******");
        assert_eq!(
            mask_form("passwords=1&q=password=2"),
            "passwords=1&q=password=2"
        );
    }

    #[test]
    fn masks_json_api_key() {
        assert_eq!(
            mask_json(
                r#"{"result":"success","api_key" : "abc123","email":"a@b","x":{"api_key":"k2"}}"#
            ),
            r#"{"result":"success","api_key" : "******","email":"a@b","x":{"api_key":"******"}}"#
        );
        assert_eq!(
            mask_json(r#"{"giphy_api_key":"public"}"#),
            r#"{"giphy_api_key":"public"}"#
        );
        assert_eq!(mask_json(r#"{"api_key":null}"#), r#"{"api_key":null}"#);
    }

    #[test]
    fn masks_sensitive_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic dXNlcjprZXk=".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        assert_eq!(
            format_headers(&headers),
            "Authorization: Basic ******\nContent-Type: application/json"
        );
    }

    #[test]
    fn describes_binary_bodies() {
        assert_eq!(
            describe_body(Some("image/png; x=1"), Some(1024)),
            "[image/png，1024 字节，未记录内容]"
        );
        assert_eq!(describe_body(None, None), "[未知类型，未记录内容]");
    }

    #[test]
    fn truncates_long_bodies() {
        let text = "好".repeat(MAX_BODY_CHARS + 5);
        let cut = truncate_body(text);
        assert!(cut.ends_with(&format!("[已截断，共 {} 字符]", MAX_BODY_CHARS + 5)));
        assert_eq!(truncate_body("short".to_owned()), "short");
    }

    fn request() -> TraceRequest {
        TraceRequest {
            time: Local::now(),
            method: "GET".into(),
            url: "https://z/api/v1/x".into(),
            headers: String::new(),
            body: String::new(),
        }
    }

    fn response(status: Option<u16>) -> TraceResponse {
        TraceResponse {
            status,
            reason: String::new(),
            headers: String::new(),
            body: "Hello".into(),
            error: None,
            duration: Duration::ZERO,
        }
    }

    #[test]
    fn log_trims_oldest_and_ignores_removed_entries() {
        let log = RequestLog::new(true, 2);
        let first = log.begin(request());
        let second = log.begin(request());
        log.begin(request());
        assert_eq!(
            log.entries().iter().map(|e| e.id).collect::<Vec<_>>(),
            [2, 3]
        );

        log.end(first, response(Some(200)));
        assert!(log.entries().iter().all(|e| e.is_pending()));
        log.end(second, response(Some(500)));
        let entries = log.entries();
        assert!(entries[0].is_failed() && entries[1].is_pending());

        log.configure(true, RequestLog::UNLIMITED);
        for _ in 0..5 {
            log.begin(request());
        }
        assert_eq!(log.entries().len(), 7);
        log.clear();
        assert!(log.entries().is_empty());
    }

    #[test]
    fn log_notifies_changes() {
        let log = RequestLog::new(false, 10);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = count.clone();
        log.set_on_change(Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        })));
        assert!(!log.is_enabled());
        let id = log.begin(request());
        log.end(id, response(None));
        log.clear();
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn entry_search_is_case_insensitive() {
        let entry = LogEntry {
            id: 1,
            request: request(),
            response: Some(response(Some(200))),
        };
        assert!(entry.matches("hello"));
        assert!(entry.matches("V1/X"));
        assert!(!entry.matches("missing"));
        assert!(!entry.is_failed());
        assert!(
            LogEntry {
                response: Some(response(None)),
                ..entry
            }
            .is_failed()
        );
    }
}
