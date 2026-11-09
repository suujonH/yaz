//! Stateful fake Zulip server for development and testing. Fixed account [`ME_EMAIL`] / any
//! password; API key is [`API_KEY`]. The behavior of other users is injected by tests through
//! methods of [`TestServer`].

#![recursion_limit = "512"]

mod api;
mod markdown;
mod seed;
mod state;

use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Multipart, Path, Request, State as AxumState};
use axum::http::{Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use base64::Engine;
use bytes::Bytes;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use api::{ApiFail, Params};
use state::{State, Upload, push_to_queue};

pub use seed::{AJIE, DAILY, DEV, GAME, LAOWANG, NOTICE, XIAOYU};
pub use state::Target;

pub const ME_ID: i64 = 1;
pub const ME_EMAIL: &str = "me@zulip.test";
pub const API_KEY: &str = "test-api-key-0123456789";
pub const BOT_ID: i64 = 99;

/// How often long polling returns a heartbeat when there are no events.
const HEARTBEAT: Duration = Duration::from_secs(30);

pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

struct Shared {
    state: Mutex<State>,
    notify: Notify,
    realm: Mutex<String>,
    /// Cancellation token of the current run: when stopping, makes pending long polls return
    /// immediately.
    stop: Mutex<CancellationToken>,
}

/// A running server. Stops on drop.
pub struct TestServer {
    shared: Arc<Shared>,
    addr: SocketAddr,
    task: Option<JoinHandle<()>>,
}

impl TestServer {
    /// Start on 127.0.0.1; when `port` is 0 the system assigns one.
    pub async fn start(port: u16) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(seed::build(now())),
            notify: Notify::new(),
            realm: Mutex::new(String::new()),
            stop: Mutex::new(CancellationToken::new()),
        });
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        *shared.realm.lock() = format!("http://{addr}");
        let task = spawn_serve(listener, shared.clone());
        Ok(Self {
            shared,
            addr,
            task: Some(task),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stop listening and drop connections; state and event queues are kept.
    pub async fn stop(&mut self) {
        self.shared.stop.lock().cancel();
        self.shared.notify.notify_waiters();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    /// Start listening again on the same port.
    pub async fn restart(&mut self) -> std::io::Result<()> {
        self.stop().await;
        *self.shared.stop.lock() = CancellationToken::new();
        let listener = TcpListener::bind(self.addr).await?;
        self.task = Some(spawn_serve(listener, self.shared.clone()));
        Ok(())
    }

    /// Invalidate all event queues; the client receives `BAD_EVENT_QUEUE_ID` the next time it
    /// fetches events.
    pub fn expire_queues(&self) {
        self.shared.state.lock().queues.clear();
        self.shared.notify.notify_waiters();
    }

    /// Send a message as another user; returns the message id.
    pub fn send_as(&self, sender_id: i64, target: Target, content: &str) -> i64 {
        let id = self
            .shared
            .state
            .lock()
            .send(sender_id, target, content, now(), None);
        self.shared.notify.notify_waiters();
        id
    }

    /// Push an arbitrary event (the `id` field is filled in by the server).
    pub fn push_event(&self, event: Value) {
        self.shared.state.lock().emit(event);
        self.shared.notify.notify_waiters();
    }

    /// Query the current flags, for test assertions.
    pub fn flags_of(&self, message_id: i64) -> Vec<String> {
        self.shared
            .state
            .lock()
            .messages
            .get(&message_id)
            .map(|m| m.flags.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the server holds the content of this message.
    pub fn content_of(&self, message_id: i64) -> Option<String> {
        self.shared
            .state
            .lock()
            .messages
            .get(&message_id)
            .map(|m| m.raw.clone())
    }

    pub fn latest_message_id(&self) -> i64 {
        let state = self.shared.state.lock();
        state.messages.keys().last().copied().unwrap_or_default()
    }

    /// Return a server error when fetching a single message, simulating a partial failure to load
    /// quotes.
    pub fn set_message_fetch_failing(&self, failing: bool) {
        self.shared.state.lock().fail_message_fetch = failing;
    }

    /// Make fetching this message always return a server error.
    pub fn fail_message(&self, id: i64) {
        self.shared.state.lock().failing_messages.insert(id);
    }

    /// Set the topic move time limit: messages older than this id cannot be moved.
    pub fn set_move_limit(&self, first_allowed: Option<i64>) {
        self.shared.state.lock().move_limit_first_id = first_allowed;
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shared.stop.lock().cancel();
        self.shared.notify.notify_waiters();
    }
}

fn spawn_serve(listener: TcpListener, shared: Arc<Shared>) -> JoinHandle<()> {
    let stop = shared.stop.lock().clone();
    let app = router(shared);
    tokio::spawn(async move {
        let serve = axum::serve(listener, app).with_graceful_shutdown(stop.cancelled_owned());
        if let Err(error) = serve.await {
            eprintln!("test-server 退出：{error}");
        }
    })
}

fn router(shared: Arc<Shared>) -> Router {
    let authed = Router::new()
        .route("/api/v1/events", get(get_events))
        .route(
            "/api/v1/user_uploads",
            post(upload).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/api/v1/users/me/avatar", post(upload_avatar))
        .route("/api/v1/{*path}", any(api_call))
        .route("/user_uploads/{*path}", get(download))
        .layer(middleware::from_fn(require_auth));
    Router::new()
        .route("/api/v1/server_settings", get(server_settings))
        .route("/api/v1/fetch_api_key", post(fetch_api_key))
        .route("/avatar/{file}", get(avatar))
        .route("/static/emoji.json", get(emoji_data))
        .route("/test/send_as", post(send_as_route))
        .route("/test/push_event", post(push_event_route))
        .route("/test/fail_message_fetch", post(fail_message_fetch_route))
        .merge(authed)
        .with_state(shared)
}

fn ok(mut body: Value) -> Response {
    body["result"] = json!("success");
    body["msg"] = json!("");
    axum::Json(body).into_response()
}

fn fail(error: ApiFail) -> Response {
    let mut body = error.extra;
    body["result"] = json!("error");
    body["msg"] = json!(error.msg);
    body["code"] = json!(error.code);
    (error.status, axum::Json(body)).into_response()
}

async fn require_auth(request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Basic "))
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .is_some_and(|b| b == format!("{ME_EMAIL}:{API_KEY}").as_bytes());
    if authorized {
        next.run(request).await
    } else {
        fail(ApiFail::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "Invalid API key",
        ))
    }
}

async fn server_settings(AxumState(shared): AxumState<Arc<Shared>>) -> Response {
    let realm = shared.realm.lock().clone();
    ok(api::server_settings(&realm))
}

async fn fetch_api_key(uri: Uri, body: Bytes) -> Response {
    match api::fetch_api_key(&Params::parse(uri.query(), &body)) {
        Ok(value) => ok(value),
        Err(error) => fail(error),
    }
}

async fn api_call(
    AxumState(shared): AxumState<Arc<Shared>>,
    method: Method,
    Path(path): Path<String>,
    uri: Uri,
    body: Bytes,
) -> Response {
    let params = Params::parse(uri.query(), &body);
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let realm = shared.realm.lock().clone();
    let result = api::dispatch(
        &mut shared.state.lock(),
        &method,
        &segments,
        &params,
        &realm,
    );
    shared.notify.notify_waiters();
    match result {
        Ok(value) => ok(value),
        Err(error) => fail(error),
    }
}

async fn get_events(AxumState(shared): AxumState<Arc<Shared>>, uri: Uri) -> Response {
    let params = Params::parse(uri.query(), &[]);
    let queue_id = params.str("queue_id").unwrap_or_default().to_owned();
    let last_event_id = params.i64("last_event_id").unwrap_or(-1);
    let stop = shared.stop.lock().clone();
    let deadline = tokio::time::Instant::now() + HEARTBEAT;
    loop {
        let mut notified = pin!(shared.notify.notified());
        notified.as_mut().enable();
        {
            let mut state = shared.state.lock();
            let Some(queue) = state.queues.get_mut(&queue_id) else {
                let mut error = ApiFail::new(
                    StatusCode::BAD_REQUEST,
                    "BAD_EVENT_QUEUE_ID",
                    format!("Bad event queue ID: {queue_id}"),
                );
                error.extra = json!({"queue_id": queue_id});
                return fail(error);
            };
            queue
                .events
                .retain(|e| e["id"].as_i64().unwrap_or(-1) > last_event_id);
            if !queue.events.is_empty() {
                return ok(json!({"events": queue.events}));
            }
            if tokio::time::Instant::now() >= deadline {
                push_to_queue(queue, json!({"type": "heartbeat"}));
                return ok(json!({"events": queue.events}));
            }
        }
        tokio::select! {
            () = notified => {}
            () = tokio::time::sleep_until(deadline) => {}
            () = stop.cancelled() => {
                return fail(ApiFail::new(StatusCode::SERVICE_UNAVAILABLE, "STOPPED", "服务器已停止"));
            }
        }
    }
}

async fn upload(AxumState(shared): AxumState<Arc<Shared>>, mut multipart: Multipart) -> Response {
    let (name, content_type, bytes) = match read_file_part(&mut multipart).await {
        Ok(part) => part,
        Err(error) => return fail(error),
    };
    let mut state = shared.state.lock();
    let id = state.alloc_id();
    let path = format!("1/{id}/{name}");
    let url = format!("/user_uploads/{path}");
    state.uploads.insert(
        path.clone(),
        Upload {
            bytes,
            content_type,
            name: name.clone(),
            attachment_id: id,
            created_at: now(),
            dangling_refs: Vec::new(),
        },
    );
    ok(json!({"url": url, "filename": name}))
}

async fn upload_avatar(
    AxumState(shared): AxumState<Arc<Shared>>,
    mut multipart: Multipart,
) -> Response {
    if let Err(error) = read_file_part(&mut multipart).await {
        return fail(error);
    }
    let mut state = shared.state.lock();
    if let Some(me) = state.users.iter_mut().find(|u| u.id == ME_ID) {
        me.avatar_version += 1;
    }
    let url = state.avatar_url(ME_ID);
    state.emit(json!({
        "type": "realm_user", "op": "update",
        "person": {"user_id": ME_ID, "avatar_url": url, "avatar_version": 2},
    }));
    drop(state);
    shared.notify.notify_waiters();
    ok(json!({"avatar_url": url}))
}

async fn read_file_part(multipart: &mut Multipart) -> Result<(String, String, Vec<u8>), ApiFail> {
    let bad = |msg: String| ApiFail::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", msg);
    let field = multipart
        .next_field()
        .await
        .map_err(|e| bad(e.to_string()))?
        .ok_or_else(|| bad("没有文件".to_owned()))?;
    let name = field.file_name().unwrap_or("file").to_owned();
    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_owned();
    let bytes = field.bytes().await.map_err(|e| bad(e.to_string()))?;
    Ok((name, content_type, bytes.to_vec()))
}

async fn download(AxumState(shared): AxumState<Arc<Shared>>, Path(path): Path<String>) -> Response {
    let state = shared.state.lock();
    match state.uploads.get(&path) {
        Some(file) => (
            [(header::CONTENT_TYPE, file.content_type.clone())],
            file.bytes.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// For manual testing: send a message as another user. Parameters `sender`, `content`; for a
/// channel message also `stream` and `topic`, otherwise a DM to self. With `?on=true`, fetching a
/// single message returns a server error; `on=false` restores it.
async fn fail_message_fetch_route(AxumState(shared): AxumState<Arc<Shared>>, uri: Uri) -> Response {
    let on = Params::parse(uri.query(), &[]).str("on") == Some("true");
    shared.state.lock().fail_message_fetch = on;
    ok(json!({}))
}

async fn send_as_route(
    AxumState(shared): AxumState<Arc<Shared>>,
    uri: Uri,
    body: Bytes,
) -> Response {
    let params = Params::parse(uri.query(), &body);
    let sender = params.i64("sender").unwrap_or(seed::AJIE);
    let target = match params.i64("stream") {
        Some(stream) => Target::Stream(stream, params.str("topic").unwrap_or_default().to_owned()),
        None => Target::Dm(vec![sender]),
    };
    let id = shared.state.lock().send(
        sender,
        target,
        params.str("content").unwrap_or_default(),
        now(),
        None,
    );
    shared.notify.notify_waiters();
    ok(json!({"id": id}))
}

/// Push an arbitrary event (typing notice, read change, etc.); the request body is the event's
/// JSON.
async fn push_event_route(AxumState(shared): AxumState<Arc<Shared>>, body: Bytes) -> Response {
    let Ok(event) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    shared.state.lock().emit(event);
    shared.notify.notify_waiters();
    ok(json!({}))
}

async fn avatar() -> Response {
    ([(header::CONTENT_TYPE, "image/png")], seed::TINY_PNG).into_response()
}

async fn emoji_data() -> Response {
    axum::Json(json!({"code_to_names": {
        "1f44d": ["+1", "thumbs_up", "like"],
        "2764": ["heart", "love"],
        "1f602": ["joy", "tears"],
        "1f62e": ["open_mouth"],
        "1f622": ["cry"],
        "1f64f": ["pray", "folded_hands"],
        "1f600": ["grinning", "happy"],
    }}))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get_json(url: &str, auth: bool) -> (u16, Value) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let url = url.strip_prefix("http://").unwrap();
        let (host, path) = url.split_once('/').unwrap();
        let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
        let credentials =
            base64::engine::general_purpose::STANDARD.encode(format!("{ME_EMAIL}:{API_KEY}"));
        let auth_line = if auth {
            format!("Authorization: Basic {credentials}\r\n")
        } else {
            String::new()
        };
        let request =
            format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\n{auth_line}Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        let status = text.split(' ').nth(1).unwrap().parse().unwrap();
        let body = text.split_once("\r\n\r\n").unwrap().1;
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn auth_is_required_and_settings_are_public() {
        let server = TestServer::start(0).await.unwrap();
        let (status, body) =
            get_json(&format!("{}/api/v1/server_settings", server.url()), false).await;
        assert_eq!(status, 200);
        assert_eq!(body["zulip_feature_level"], 445);
        let (status, body) = get_json(&format!("{}/api/v1/drafts", server.url()), false).await;
        assert_eq!(status, 401);
        assert_eq!(body["code"], "UNAUTHORIZED");
        let (status, body) = get_json(&format!("{}/api/v1/drafts", server.url()), true).await;
        assert_eq!(status, 200);
        assert_eq!(body["drafts"][0]["content"], "我想推荐一个……");
    }

    #[tokio::test]
    async fn unknown_queue_reports_bad_event_queue_id() {
        let server = TestServer::start(0).await.unwrap();
        let url = format!(
            "{}/api/v1/events?queue_id=nope&last_event_id=-1",
            server.url()
        );
        let (status, body) = get_json(&url, true).await;
        assert_eq!(status, 400);
        assert_eq!(body["code"], "BAD_EVENT_QUEUE_ID");
    }

    #[test]
    fn seed_has_varied_content() {
        let state = seed::build(1_700_000_000);
        let any = |needle: &str| state.messages.values().any(|m| m.html.contains(needle));
        assert!(any("<table>"));
        assert!(any("codehilite"));
        assert!(any("<blockquote>"));
        assert!(any("message_inline_image"));
        assert!(state.messages.values().any(|m| !m.submessages.is_empty()));
        assert!(state.messages.values().any(|m| m.flags.contains("starred")));
        assert!(state.messages.values().any(|m| !m.flags.contains("read")));
    }

    #[tokio::test]
    async fn long_poll_delivers_and_restart_keeps_state() {
        let mut server = TestServer::start(0).await.unwrap();
        let id = {
            let mut state = server.shared.state.lock();
            state.queues.insert("q".into(), state::Queue::default());
            drop(state);
            server.send_as(AJIE, Target::Stream(DEV, "接口设计".into()), "新消息")
        };
        let url = format!("{}/api/v1/events?queue_id=q&last_event_id=-1", server.url());
        let (status, body) = get_json(&url, true).await;
        assert_eq!(status, 200);
        assert_eq!(body["events"][0]["type"], "message");
        assert_eq!(body["events"][0]["message"]["id"], id);

        server.restart().await.unwrap();
        assert_eq!(server.content_of(id).as_deref(), Some("新消息"));
        let (status, _) = get_json(&url, true).await;
        assert_eq!(status, 200);
    }
}
