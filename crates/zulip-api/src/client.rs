use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use chrono::Local;
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER, USER_AGENT,
};
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::ClientConfig;
use crate::error::ApiError;
use crate::form::Form;
use crate::trace::{
    TraceRequest, TraceResponse, TraceSink, describe_body, format_headers, is_text_type, mask_form,
    mask_json, truncate_body,
};

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RATE_LIMIT_RETRIES: u32 = 3;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);

/// Zulip REST 客户端。可以廉价地 `clone`，克隆体共用连接池。
///
/// 登录前用 [`ZulipClient::anonymous`] 调用 [`server_settings`](Self::server_settings) 与
/// [`fetch_api_key`](Self::fetch_api_key)；其它接口需要 [`ZulipClient::new`] 创建的带认证的客户端。
#[derive(Clone)]
pub struct ZulipClient {
    inner: Arc<Inner>,
    cancel: Option<CancellationToken>,
}

struct Inner {
    realm: Url,
    credentials: Option<(String, String)>,
    http: reqwest::Client,
    config: Arc<ClientConfig>,
    shutdown: CancellationToken,
}

impl fmt::Debug for ZulipClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZulipClient")
            .field("realm", &self.inner.realm.as_str())
            .field("email", &self.email())
            .finish_non_exhaustive()
    }
}

/// [`ZulipClient`] 的创建选项。
pub struct ClientBuilder {
    realm: String,
    credentials: Option<(String, String)>,
    allow_invalid_cert: bool,
    config: Option<Arc<ClientConfig>>,
}

impl ClientBuilder {
    /// Basic 认证：邮箱与 API key。
    pub fn credentials(mut self, email: &str, api_key: &str) -> Self {
        self.credentials = Some((email.to_owned(), api_key.to_owned()));
        self
    }

    /// 接受无效的服务器证书（自签名等）。
    pub fn allow_invalid_cert(mut self, allow: bool) -> Self {
        self.allow_invalid_cert = allow;
        self
    }

    /// 使用独立的 User-Agent 与请求记录设置；默认使用 [`ClientConfig::global`]。
    pub fn config(mut self, config: Arc<ClientConfig>) -> Self {
        self.config = Some(config);
        self
    }

    pub fn build(self) -> Result<ZulipClient, ApiError> {
        let realm = normalize_realm(&self.realm)?;
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(self.allow_invalid_cert)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| ApiError::from_reqwest(e, "创建 HTTP 客户端"))?;
        Ok(ZulipClient {
            inner: Arc::new(Inner {
                realm,
                credentials: self.credentials,
                http,
                config: self.config.unwrap_or_else(ClientConfig::global),
                shutdown: CancellationToken::new(),
            }),
            cancel: None,
        })
    }
}

/// 解析组织地址：去掉查询和片段，路径以 `/` 结尾。
fn normalize_realm(realm: &str) -> Result<Url, ApiError> {
    let mut url = Url::parse(realm.trim())
        .map_err(|e| ApiError::InvalidInput(format!("组织地址无效：{e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.cannot_be_a_base() {
        return Err(ApiError::InvalidInput(format!("组织地址无效：{realm}")));
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

pub(crate) struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl ZulipClient {
    pub fn builder(realm: &str) -> ClientBuilder {
        ClientBuilder {
            realm: realm.to_owned(),
            credentials: None,
            allow_invalid_cert: false,
            config: None,
        }
    }

    pub fn new(
        realm: &str,
        email: &str,
        api_key: &str,
        allow_invalid_cert: bool,
    ) -> Result<Self, ApiError> {
        Self::builder(realm)
            .credentials(email, api_key)
            .allow_invalid_cert(allow_invalid_cert)
            .build()
    }

    /// 不带认证的客户端，用于登录前的接口。
    pub fn anonymous(realm: &str, allow_invalid_cert: bool) -> Result<Self, ApiError> {
        Self::builder(realm)
            .allow_invalid_cert(allow_invalid_cert)
            .build()
    }

    /// 组织地址，以 `/` 结尾。
    pub fn realm_url(&self) -> &Url {
        &self.inner.realm
    }

    pub fn email(&self) -> &str {
        self.inner
            .credentials
            .as_ref()
            .map_or("", |(email, _)| email)
    }

    /// 取消所有使用这个客户端（含克隆体）的进行中和之后的请求，它们返回 [`ApiError::Cancelled`]。
    pub fn close(&self) {
        self.inner.shutdown.cancel();
    }

    /// 返回共用连接池的克隆体，令牌取消时它的请求返回 [`ApiError::Cancelled`]。
    pub fn with_cancel(&self, token: CancellationToken) -> Self {
        Self {
            inner: self.inner.clone(),
            cancel: Some(token),
        }
    }

    /// 发送请求并返回 JSON 根对象。`timeout` 为 `None` 时不设超时。
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        form: Option<&Form>,
        timeout: Option<Duration>,
    ) -> Result<Value, ApiError> {
        let operation = format!("{method} {path}");
        let mut attempt = 0;
        loop {
            let mut builder = self.request_builder(&method, self.url_for(path, form, &method)?);
            if !matches!(method, Method::GET | Method::HEAD)
                && let Some(form) = form
            {
                builder = builder
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(form.to_query_string());
            }
            if let Some(timeout) = timeout {
                builder = builder.timeout(timeout);
            }
            let reply = self.exchange(builder, &operation).await?;
            if reply.status == StatusCode::TOO_MANY_REQUESTS && attempt < MAX_RATE_LIMIT_RETRIES {
                attempt += 1;
                self.guarded(async {
                    tokio::time::sleep(retry_after(&reply)).await;
                    Ok(())
                })
                .await?;
                continue;
            }
            return parse_result(reply.status, &reply.body);
        }
    }

    /// 默认超时的请求，结果解析为 `T`。
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        form: Option<&Form>,
    ) -> Result<T, ApiError> {
        decode(
            self.request(method, path, form, Some(DEFAULT_TIMEOUT))
                .await?,
        )
    }

    /// 默认超时的请求，只取响应中的一个字段并解析为 `T`。
    pub(crate) async fn call_field<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        form: Option<&Form>,
        field: &str,
    ) -> Result<T, ApiError> {
        take_field(
            self.request(method, path, form, Some(DEFAULT_TIMEOUT))
                .await?,
            field,
        )
    }

    /// 组织内的地址。`path` 可以带 scheme 或写成别的形式（来自消息内容、插件），
    /// 拼接结果不在本组织时返回错误，认证信息不会发往别的站点。
    pub(crate) fn url(&self, path: &str) -> Result<Url, ApiError> {
        let url = self
            .inner
            .realm
            .join(path.trim_start_matches('/'))
            .map_err(|e| ApiError::InvalidInput(format!("路径无效：{path}：{e}")))?;
        if !self.is_same_origin(&url) {
            return Err(ApiError::InvalidInput(format!("地址不属于本组织：{path}")));
        }
        Ok(url)
    }

    fn is_same_origin(&self, url: &Url) -> bool {
        let realm = &self.inner.realm;
        url.scheme() == realm.scheme()
            && url.host() == realm.host()
            && url.port_or_known_default() == realm.port_or_known_default()
    }

    fn url_for(&self, path: &str, form: Option<&Form>, method: &Method) -> Result<Url, ApiError> {
        let mut url = self.url(path)?;
        if let Some(form) =
            form.filter(|f| !f.is_empty() && matches!(*method, Method::GET | Method::HEAD))
        {
            url.set_query(Some(&form.to_query_string()));
        }
        Ok(url)
    }

    /// 带 User-Agent 的请求；只有本组织的地址才带认证。User-Agent 每个请求都取当前值。
    pub(crate) fn request_builder(&self, method: &Method, url: Url) -> RequestBuilder {
        let agent = self.inner.config.user_agent();
        let agent = HeaderValue::from_bytes(agent.as_bytes())
            .unwrap_or_else(|_| HeaderValue::from_static("YaZ"));
        let same_origin = self.is_same_origin(&url);
        let builder = self
            .inner
            .http
            .request(method.clone(), url)
            .header(USER_AGENT, agent);
        match &self.inner.credentials {
            Some((email, api_key)) if same_origin => builder.basic_auth(email, Some(api_key)),
            _ => builder,
        }
    }

    /// 发送请求并读完响应正文。
    pub(crate) async fn exchange(
        &self,
        builder: RequestBuilder,
        operation: &str,
    ) -> Result<Reply, ApiError> {
        let request = builder
            .build()
            .map_err(|e| ApiError::from_reqwest(e, operation))?;
        let trace = self.begin_trace(&request);
        let path = request.url().path().to_owned();
        let result = self
            .guarded(async {
                let response = self
                    .inner
                    .http
                    .execute(request)
                    .await
                    .map_err(|e| ApiError::from_reqwest(e, operation))?;
                let (status, headers) = (response.status(), response.headers().clone());
                let body = response
                    .bytes()
                    .await
                    .map_err(|e| ApiError::from_reqwest(e, operation))?;
                Ok(Reply {
                    status,
                    headers,
                    body,
                })
            })
            .await;
        if let Some(trace) = trace {
            match &result {
                Ok(reply) => trace.finish(
                    Some((reply.status, &reply.headers)),
                    reply_body(reply, &path),
                    None,
                ),
                Err(error) => trace.finish(None, String::new(), Some(error)),
            }
        }
        result
    }

    /// 发送请求，收到响应头即返回，正文由调用方读取。
    pub(crate) async fn open(
        &self,
        builder: RequestBuilder,
        operation: &str,
    ) -> Result<Response, ApiError> {
        let request = builder
            .build()
            .map_err(|e| ApiError::from_reqwest(e, operation))?;
        let trace = self.begin_trace(&request);
        let result = self
            .guarded(async {
                self.inner
                    .http
                    .execute(request)
                    .await
                    .map_err(|e| ApiError::from_reqwest(e, operation))
            })
            .await;
        if let Some(trace) = trace {
            match &result {
                Ok(response) => {
                    let body = describe_body(
                        header_str(response.headers(), &CONTENT_TYPE),
                        header_u64(response.headers(), &CONTENT_LENGTH),
                    );
                    trace.finish(Some((response.status(), response.headers())), body, None);
                }
                Err(error) => trace.finish(None, String::new(), Some(error)),
            }
        }
        result
    }

    /// 在客户端关闭或令牌取消时中断 `future`。
    pub(crate) async fn guarded<T>(
        &self,
        future: impl Future<Output = Result<T, ApiError>>,
    ) -> Result<T, ApiError> {
        tokio::select! {
            biased;
            () = self.inner.shutdown.cancelled() => Err(ApiError::Cancelled),
            () = async {
                match &self.cancel {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending().await,
                }
            } => Err(ApiError::Cancelled),
            result = future => result,
        }
    }

    fn begin_trace(&self, request: &reqwest::Request) -> Option<ActiveTrace> {
        let sink = self.inner.config.active_trace_sink()?;
        let content_type = header_str(request.headers(), &CONTENT_TYPE);
        let body = match request.body() {
            None => String::new(),
            Some(body) => match body.as_bytes() {
                Some(bytes) if is_text_type(content_type) => {
                    mask_form(&String::from_utf8_lossy(bytes))
                }
                Some(bytes) => describe_body(content_type, Some(bytes.len() as u64)),
                None => describe_body(content_type, header_u64(request.headers(), &CONTENT_LENGTH)),
            },
        };
        let id = sink.begin(TraceRequest {
            time: Local::now(),
            method: request.method().to_string(),
            url: request.url().to_string(),
            headers: format_headers(request.headers()),
            body,
        });
        Some(ActiveTrace {
            sink,
            id,
            started: Instant::now(),
        })
    }
}

struct ActiveTrace {
    sink: Arc<dyn TraceSink>,
    id: u64,
    started: Instant,
}

impl ActiveTrace {
    fn finish(
        self,
        response: Option<(StatusCode, &HeaderMap)>,
        body: String,
        error: Option<&ApiError>,
    ) {
        let (status, reason, headers) = match response {
            Some((status, headers)) => (
                Some(status.as_u16()),
                status.canonical_reason().unwrap_or_default().to_owned(),
                format_headers(headers),
            ),
            None => (None, String::new(), String::new()),
        };
        self.sink.end(
            self.id,
            TraceResponse {
                status,
                reason,
                headers,
                body,
                error: error.map(|e| match e {
                    ApiError::Cancelled | ApiError::Timeout(_) => "已取消或超时".to_owned(),
                    other => other.to_string(),
                }),
                duration: self.started.elapsed(),
            },
        );
    }
}

/// API 响应的文本正文（`api_key` 打码）；文件等其它响应只记录类型和大小。
fn reply_body(reply: &Reply, path: &str) -> String {
    let content_type = header_str(&reply.headers, &CONTENT_TYPE);
    if path.contains("/api/") && is_text_type(content_type) {
        truncate_body(mask_json(&String::from_utf8_lossy(&reply.body)))
    } else {
        describe_body(content_type, Some(reply.body.len() as u64))
    }
}

pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

pub(crate) fn header_u64(headers: &HeaderMap, name: &HeaderName) -> Option<u64> {
    header_str(headers, name)?.trim().parse().ok()
}

fn retry_after(reply: &Reply) -> Duration {
    if let Some(seconds) = header_u64(&reply.headers, &RETRY_AFTER) {
        return Duration::from_secs(seconds).min(MAX_RETRY_WAIT);
    }
    let body_seconds = serde_json::from_slice::<Value>(&reply.body)
        .ok()
        .and_then(|v| v.get("retry-after")?.as_f64());
    match body_seconds {
        Some(seconds) => Duration::from_secs_f64(seconds.clamp(0.0, MAX_RETRY_WAIT.as_secs_f64())),
        None => Duration::from_secs(1),
    }
}

/// 把响应解析为 JSON；HTTP 状态码非 2xx、`result` 为 `error` 或响应不是 JSON 时返回 [`ApiError::Api`]。
pub(crate) fn parse_result(status: StatusCode, body: &[u8]) -> Result<Value, ApiError> {
    let text = String::from_utf8_lossy(body);
    let code = status.as_u16();
    // 例如 nginx 在 502/504 时返回 HTML
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return Err(ApiError::api(
            code,
            &format!("HTTP_{code}"),
            text.chars().take(200).collect::<String>(),
            None,
        ));
    };
    let object = root.as_object();
    let is_error = object.is_none_or(|o| o.get("result").and_then(Value::as_str) == Some("error"));
    if !status.is_success() || is_error {
        let field = |name: &str| {
            object
                .and_then(|o| o.get(name))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (error_code, message) = (field("code"), field("msg"));
        return Err(ApiError::api(code, &error_code, message, Some(root)));
    }
    Ok(root)
}

pub(crate) fn decode<T: DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|e| ApiError::Decode(e.to_string()))
}

pub(crate) fn take_field<T: DeserializeOwned>(mut root: Value, field: &str) -> Result<T, ApiError> {
    match root.get_mut(field) {
        Some(value) => decode(value.take()),
        None => Err(ApiError::Decode(format!("响应缺少字段 {field}"))),
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::AUTHORIZATION;

    use super::*;

    fn client() -> ZulipClient {
        ZulipClient::new("https://z.test:8443/", "a@z.test", "KEY", false).unwrap()
    }

    #[test]
    fn url_stays_inside_the_realm() {
        let client = client();
        for (path, expected) in [
            (
                "/user_uploads/a.png",
                "https://z.test:8443/user_uploads/a.png",
            ),
            (
                "user_uploads/a.png",
                "https://z.test:8443/user_uploads/a.png",
            ),
            ("//evil.test/a.png", "https://z.test:8443/evil.test/a.png"),
            (r"/\evil.test/a.png", "https://z.test:8443/evil.test/a.png"),
            ("https://z.test:8443/a.png", "https://z.test:8443/a.png"),
            ("/https://z.test:8443/a.png", "https://z.test:8443/a.png"),
        ] {
            assert_eq!(client.url(path).unwrap().as_str(), expected, "{path}");
        }
    }

    #[test]
    fn url_rejects_other_origins() {
        let client = client();
        for path in [
            "https://evil.test/a.png",
            "/https://evil.test/a.png",
            "///https://evil.test/a.png",
            r"\\evil.test/a.png",
            "https://z.test@evil.test:8443/a.png",
            "https://z.test/a.png",
            "https://z.test:9443/a.png",
            "http://z.test:8443/a.png",
            "http:evil.test/a.png",
            "file:///c:/a.png",
        ] {
            assert!(
                matches!(client.url(path), Err(ApiError::InvalidInput(_))),
                "{path}"
            );
        }
    }

    #[test]
    fn credentials_are_attached_only_for_the_realm() {
        let client = client();
        let headers = |url: &str| {
            client
                .request_builder(&Method::GET, Url::parse(url).unwrap())
                .build()
                .unwrap()
                .headers()
                .clone()
        };
        assert!(headers("https://z.test:8443/a").contains_key(AUTHORIZATION));
        for url in [
            "https://evil.test:8443/a",
            "https://z.test/a",
            "http://z.test:8443/a",
        ] {
            assert!(!headers(url).contains_key(AUTHORIZATION), "{url}");
        }
    }
}
