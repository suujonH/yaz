//! Developer mode: show setting sources, show unavailable features, request log (list and detail
//! windows).

use super::field::*;
use crate::request_log::{request_log, subscribe_request_log};
use crate::shell::{WindowShell, WindowSpec, open_with_args, use_window_args};
use crate::ui::*;
use dioxus::prelude::*;
use percent_encoding::percent_decode_str;
use serde_json::Value;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use zulip_api::LogEntry;

/// Refresh interval of the request log: requests arriving in batches are coalesced.
const REFRESH: Duration = Duration::from_millis(100);

/// Snapshot of the request log, updated when the log changes.
fn use_request_entries() -> Signal<Vec<Arc<LogEntry>>> {
    let mut entries = use_signal(|| request_log().entries());
    use_future(move || async move {
        let mut rx = subscribe_request_log();
        while rx.changed().await.is_ok() {
            tokio::time::sleep(REFRESH).await;
            rx.borrow_and_update();
            entries.set(request_log().entries());
        }
    });
    entries
}

/// Path and query parameters after the organization address.
fn name_of(entry: &LogEntry) -> String {
    let url = &entry.request.url;
    url.split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_owned()))
        .unwrap_or_else(|| url.clone())
}

fn status_text(entry: &LogEntry) -> String {
    match &entry.response {
        None => "等待中".to_owned(),
        Some(r) => r
            .status
            .map_or_else(|| "失败".to_owned(), |s| s.to_string()),
    }
}

fn status_line(entry: &LogEntry) -> String {
    match &entry.response {
        None => "等待中".to_owned(),
        Some(r) => match r.status {
            Some(status) => format!("{status} {}", r.reason).trim_end().to_owned(),
            None => format!("失败：{}", r.error.as_deref().unwrap_or_default()),
        },
    }
}

fn time_text(entry: &LogEntry) -> String {
    entry.request.time.format("%H:%M:%S%.3f").to_string()
}

fn duration_text(entry: &LogEntry) -> String {
    entry
        .response
        .as_ref()
        .map_or_else(String::new, |r| format!("{} ms", r.duration.as_millis()))
}

/// Shows JSON indented; returned as-is when it is not JSON.
fn pretty(text: &str) -> String {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return text.to_owned();
    }
    serde_json::from_str::<Value>(text)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| text.to_owned())
}

fn decode(text: &str) -> String {
    percent_decode_str(&text.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

fn append_pairs(out: &mut String, title: &str, encoded: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(title);
    out.push('\n');
    for pair in encoded.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.push_str(&format!(
            "  {}: {}\n",
            decode(name),
            pair_value(&decode(value))
        ));
    }
}

/// A parameter value: short JSON is shown compactly on one line, long JSON is indented and aligned
/// with the parameter name.
fn pair_value(value: &str) -> String {
    const INLINE_LIMIT: usize = 80;
    let formatted = pretty(value);
    if !formatted.contains('\n') {
        return formatted;
    }
    match serde_json::from_str::<Value>(value).map(|v| v.to_string()) {
        Ok(compact) if compact.chars().count() <= INLINE_LIMIT => compact,
        _ => formatted.replace('\n', "\n  "),
    }
}

/// Payload: query parameters and form data are decoded item by item; any other body is shown as-is.
fn payload_text(entry: &LogEntry) -> String {
    let request = &entry.request;
    let mut out = String::new();
    if let Some((_, query)) = request.url.split_once('?') {
        append_pairs(&mut out, "查询参数", query);
    }
    if !request.body.is_empty() {
        if request
            .headers
            .to_lowercase()
            .contains("application/x-www-form-urlencoded")
        {
            append_pairs(&mut out, "表单数据", &request.body);
        } else {
            out.push_str(&format!("请求正文\n{}\n", request.body));
        }
    }
    if out.is_empty() {
        "（没有载荷）".to_owned()
    } else {
        out.trim_end_matches('\n').to_owned()
    }
}

fn limit_options() -> Vec<(i32, String)> {
    vec![
        (100, "100 条".to_owned()),
        (500, "500 条".to_owned()),
        (1000, "1000 条".to_owned()),
        (0, "无上限".to_owned()),
    ]
}

/// One entry in the request list, compared by pointer.
#[derive(Clone)]
struct LogRef(Arc<LogEntry>);

impl PartialEq for LogRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[component]
fn RequestRow(entry: LogRef) -> Element {
    let LogRef(entry) = entry;
    let id = entry.id;
    let time = time_text(&entry);
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            leading: rsx! {
                div { class: "settings-request-lead",
                    span { class: "text-sub", "{time}" }
                    span { class: "text-sub", "{entry.request.method}" }
                    span { class: if entry.is_failed() { "text-sub settings-request-failed" } else { "text-sub" }, "{status_text(&entry)}" }
                }
            },
            title: name_of(&entry),
            meta: duration_text(&entry),
            onclick: move |_| open_request_detail(id),
        }
    }
}

#[component]
fn Requests() -> Element {
    let entries = use_request_entries();
    let mut query = use_signal(String::new);
    let mut newest_first = use_signal(|| true);
    let shown = use_memo(move || {
        let q = query.read().trim().to_owned();
        let mut shown: Vec<LogRef> = entries
            .read()
            .iter()
            .filter(|e| q.is_empty() || e.matches(&q))
            .cloned()
            .map(LogRef)
            .collect();
        if newest_first() {
            shown.reverse();
        }
        Rc::new(shown)
    });
    // When the response arrives the same record's content changes, so the key must change with it
    let keys = use_memo(move || {
        Rc::new(
            shown
                .read()
                .iter()
                .map(|e| e.0.id * 2 + u64::from(!e.0.is_pending()))
                .collect::<Vec<u64>>(),
        )
    });
    let list = use_virtual_list(VirtualListConfig {
        estimate: 40.0,
        ..VirtualListConfig::default()
    });
    let overlays = use_overlays();
    rsx! {
        div { class: "settings-title-row",
            div { class: "setting-section", "请求" }
            Button {
                onclick: move |_| {
                    request_log().clear();
                    overlays.toast("已清除");
                },
                "清除"
            }
        }
        TextInput {
            value: query(),
            placeholder: "搜索网址、请求头、请求或响应内容",
            clearable: true,
            oninput: move |v| query.set(v),
        }
        SettingCard {
            div { class: "settings-request-head text-sub",
                Button {
                    kind: ButtonKind::Flat,
                    onclick: move |_| newest_first.toggle(),
                    if newest_first() { "时间 ↓" } else { "时间 ↑" }
                }
                span { "方法" }
                span { "状态" }
                span { "名称" }
                span { "耗时" }
            }
            div { class: "settings-requests",
                if keys.read().is_empty() {
                    div { class: "picker-empty", "没有请求记录" }
                } else {
                    VirtualList {
                        handle: list,
                        keys: keys(),
                        render: move |i: usize| {
                            let entry = shown.peek()[i].clone();
                            rsx! { RequestRow { entry } }
                        },
                    }
                }
            }
        }
    }
}

#[component]
pub fn Developer() -> Element {
    let source = field(Source::App(
        |s| s.show_setting_source(),
        |s, v| s.set_show_setting_source(v),
    ));
    let unavailable = field(Source::App(
        |s| s.show_unavailable(),
        |s, v| s.set_show_unavailable(v),
    ));
    let log_requests = field(Source::App(
        |s| s.log_requests(),
        |s, v| s.set_log_requests(v),
    ));
    let limit = field(Source::App(
        |s| s.request_log_limit() as i32,
        |s, v| s.set_request_log_limit(v.max(0) as usize),
    ));
    rsx! {
        SettingsPage { title: "开发者模式",
            Group { title: None::<String>,
                ToggleRow { label: "显示设置来源", bind: source }
                ToggleRow { label: "显示不可用的功能", bind: unavailable }
                ToggleRow { label: "显示请求", bind: log_requests }
                ChoiceRow { label: "保存的条数", bind: limit, options: limit_options() }
            }
            Requests {}
        }
    }
}

/// Request details: general info, headers, payload, response.
#[component]
fn RequestDetail(id: u64) -> Element {
    let entries = use_request_entries();
    let mut tab = use_signal(|| "headers".to_owned());
    let Some(entry) = entries.read().iter().find(|e| e.id == id).cloned() else {
        return rsx! { div { class: "picker-empty", "记录已不存在" } };
    };
    let response = entry.response.as_ref();
    let time = time_text(&entry);
    let body = match tab().as_str() {
        "payload" => payload_text(&entry),
        "response" => response.map_or_else(String::new, |r| pretty(&r.body)),
        _ => format!(
            "响应标头\n{}\n\n请求标头\n{}",
            response.map_or("", |r| r.headers.as_str()),
            entry.request.headers
        ),
    };
    rsx! {
        div { class: "settings-detail",
            div { class: "settings-detail-info",
                span { class: "text-sub", "请求网址" }
                span { class: "settings-mono selectable", "{entry.request.url}" }
                span { class: "text-sub", "请求方法" }
                span { class: "settings-mono selectable", "{entry.request.method}" }
                span { class: "text-sub", "状态" }
                span { class: "settings-mono selectable", "{status_line(&entry)}" }
                span { class: "text-sub", "时间" }
                span { class: "settings-mono selectable", "{time}" }
                span { class: "text-sub", "耗时" }
                span { class: "settings-mono selectable", "{duration_text(&entry)}" }
            }
            Tabs {
                items: vec![
                    TabItem::new("headers", "标头"),
                    TabItem::new("payload", "载荷"),
                    TabItem::new("response", "响应"),
                ],
                value: tab(),
                onchange: move |v| tab.set(v),
            }
            pre { class: "settings-mono selectable scroll settings-detail-body", "{body}" }
        }
    }
}

#[component]
fn RequestDetailWindow() -> Element {
    let id = use_window_args::<u64>();
    rsx! {
        WindowShell { title: "请求详情", RequestDetail { id: id() } }
    }
}

/// Opens the request detail window; if already open, switches to this record.
fn open_request_detail(id: u64) {
    open_with_args(
        WindowSpec::new("request-detail", "请求详情", (640.0, 600.0)).min_size((420.0, 320.0)),
        RequestDetailWindow,
        id,
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;
    use zulip_api::TraceRequest;

    fn entry(url: &str, headers: &str, body: &str) -> LogEntry {
        LogEntry {
            id: 1,
            request: TraceRequest {
                time: Local::now(),
                method: "POST".into(),
                url: url.into(),
                headers: headers.into(),
                body: body.into(),
            },
            response: None,
        }
    }

    #[test]
    fn name_is_path_and_query() {
        let e = entry("https://z.example/api/v1/messages?anchor=newest", "", "");
        assert_eq!(name_of(&e), "/api/v1/messages?anchor=newest");
        assert_eq!(name_of(&entry("relative", "", "")), "relative");
    }

    #[test]
    fn payload_decodes_query_and_form() {
        let e = entry(
            "https://z/api?narrow=%5B1%5D",
            "Content-Type: application/x-www-form-urlencoded",
            "content=%E4%BD%A0+%E5%A5%BD&type=stream",
        );
        assert_eq!(
            payload_text(&e),
            "查询参数\n  narrow: [1]\n\n表单数据\n  content: 你 好\n  type: stream"
        );
        assert_eq!(
            payload_text(&entry("https://z/api", "", "")),
            "（没有载荷）"
        );
    }

    #[test]
    fn json_is_indented_and_text_is_kept() {
        assert_eq!(pretty("{\"a\":1}"), "{\n  \"a\": 1\n}");
        assert_eq!(pretty("plain"), "plain");
        assert_eq!(pretty("{broken"), "{broken");
    }
}
