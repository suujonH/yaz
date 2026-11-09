//! Verify ZulipClient requests and error handling against a local axum fake server.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use parking_lot::Mutex;
use serde_json::{Value, json};
use zulip_api::*;

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Recorded {
    fn form(&self) -> HashMap<String, String> {
        url::form_urlencoded::parse(&self.body)
            .into_owned()
            .collect()
    }

    fn query_pairs(&self) -> HashMap<String, String> {
        url::form_urlencoded::parse(self.query.as_bytes())
            .into_owned()
            .collect()
    }

    /// Query parameters of GET, or form parameters of other methods.
    fn param(&self, name: &str) -> Option<String> {
        let mut params = if self.method == "GET" {
            self.query_pairs()
        } else {
            self.form()
        };
        params.remove(name)
    }

    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", String::as_str)
    }
}

fn form_json(request: &Recorded, name: &str) -> Value {
    serde_json::from_str(&request.form()[name]).unwrap()
}

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    delay: Duration,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self::bytes(serde_json::to_vec(&value).unwrap()).header("content-type", "application/json")
    }

    fn ok() -> Self {
        Self::json(json!({"result": "success", "msg": ""}))
    }

    fn bytes(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body,
            delay: Duration::ZERO,
        }
    }

    fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }

    fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

type Responder = Box<dyn Fn(&Recorded, usize) -> Reply + Send + Sync>;

struct Shared {
    requests: Mutex<Vec<Recorded>>,
    responder: Responder,
}

struct Fake {
    url: String,
    shared: Arc<Shared>,
}

impl Fake {
    async fn start(responder: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            requests: Mutex::new(Vec::new()),
            responder: Box::new(responder),
        });
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, shared }
    }

    fn client(&self) -> ZulipClient {
        ZulipClient::builder(&self.url)
            .credentials("bot@x.test", "KEY123")
            .config(ClientConfig::new())
            .build()
            .unwrap()
    }

    fn requests(&self) -> Vec<Recorded> {
        self.shared.requests.lock().clone()
    }

    fn last(&self) -> Recorded {
        self.requests().pop().expect("没有收到请求")
    }
}

async fn handle(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap()
        .to_vec();
    let headers = parts
        .headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_owned()))
        .collect();
    let recorded = Recorded {
        method: parts.method.to_string(),
        path: parts.uri.path().to_owned(),
        query: parts.uri.query().unwrap_or_default().to_owned(),
        headers,
        body,
    };
    let index = {
        let mut requests = shared.requests.lock();
        requests.push(recorded.clone());
        requests.len() - 1
    };
    let reply = (shared.responder)(&recorded, index);
    tokio::time::sleep(reply.delay).await;
    let mut response = Response::builder().status(StatusCode::from_u16(reply.status).unwrap());
    for (name, value) in reply.headers {
        response = response.header(name, value);
    }
    response.body(Body::from(reply.body)).unwrap()
}

fn success(extra: Value) -> Reply {
    let mut body = json!({"result": "success", "msg": ""});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    Reply::json(body)
}

#[tokio::test]
async fn authenticates_and_follows_current_user_agent() {
    let fake =
        Fake::start(|_, _| success(json!({"message": {"id": 5, "content": "<p>x</p>"}}))).await;
    let config = ClientConfig::new();
    let client = ZulipClient::builder(&fake.url)
        .credentials("bot@x.test", "KEY123")
        .config(config.clone())
        .build()
        .unwrap();

    let message = client.get_message(5, true).await.unwrap();
    assert_eq!(message.id, 5);
    let first = fake.last();
    assert_eq!(
        first.header("authorization"),
        "Basic Ym90QHgudGVzdDpLRVkxMjM="
    );
    assert!(first.header("user-agent").starts_with("YaZ/"));
    assert_eq!(first.path, "/api/v1/messages/5");
    assert_eq!(first.query_pairs()["allow_empty_topic_name"], "true");

    config.set_client_name("website");
    client.get_message(5, false).await.unwrap();
    let second = fake.last();
    assert_eq!(second.header("user-agent"), "website");
    assert_eq!(second.query_pairs()["apply_markdown"], "false");
}

#[tokio::test]
async fn anonymous_client_sends_no_credentials() {
    let fake = Fake::start(|req, _| match req.path.as_str() {
        "/api/v1/server_settings" => success(json!({"zulip_feature_level": 421, "zulip_version": "12.3", "realm_name": "测试", "email_auth_enabled": true})),
        _ => success(json!({"api_key": "SECRET", "email": "a@b", "user_id": 8})),
    })
    .await;
    let client = ZulipClient::anonymous(&fake.url, false).unwrap();

    let settings = client.server_settings().await.unwrap();
    assert_eq!(
        (
            settings.zulip_feature_level,
            settings.realm_name.as_str(),
            settings.email_auth_enabled
        ),
        (421, "测试", true)
    );
    assert_eq!(fake.last().header("authorization"), "");

    let key = client.fetch_api_key("a@b", "p&ss").await.unwrap();
    assert_eq!((key.api_key.as_str(), key.user_id), ("SECRET", 8));
    let request = fake.last();
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.header("content-type"),
        "application/x-www-form-urlencoded"
    );
    assert_eq!(request.form()["password"], "p&ss");
}

#[tokio::test]
async fn realm_url_is_normalized() {
    let client = ZulipClient::new("https://example.com/zulip?x=1#f", "a", "b", false).unwrap();
    assert_eq!(client.realm_url().as_str(), "https://example.com/zulip/");
    assert_eq!(client.email(), "a");
    assert!(matches!(
        ZulipClient::new("not a url", "a", "b", false),
        Err(ApiError::InvalidInput(_))
    ));
    assert!(matches!(
        ZulipClient::new("ftp://example.com/", "a", "b", false),
        Err(ApiError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn register_parses_queue() {
    let fake =
        Fake::start(|_, _| Reply::bytes(include_bytes!("fixtures/register.json").to_vec())).await;
    let reg = fake.client().register().await.unwrap();
    assert_eq!(reg.queue_id, "REDACTED");

    let request = fake.last();
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/api/v1/register")
    );
    let form = request.form();
    assert_eq!(form["apply_markdown"], "true");
    assert_eq!(form["slim_presence"], "true");
    let capabilities: Value = serde_json::from_str(&form["client_capabilities"]).unwrap();
    assert_eq!(capabilities["empty_topic_name"], true);
    assert_eq!(capabilities["user_avatar_url_field_optional"], false);
}

#[tokio::test]
async fn get_events_sends_query_and_returns_raw_events() {
    let fake = Fake::start(|_, _| {
        success(json!({"events": [{"id": 3, "type": "heartbeat"}, {"id": 4, "type": "x"}]}))
    })
    .await;
    let events = fake
        .client()
        .get_events("Q:1", 2, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["id"], 4);
    let request = fake.last();
    assert_eq!(request.method, "GET");
    assert_eq!(request.query_pairs()["queue_id"], "Q:1");
    assert_eq!(request.query_pairs()["last_event_id"], "2");
    assert!(request.body.is_empty());

    let none = Fake::start(|_, _| Reply::ok()).await;
    assert!(
        none.client()
            .get_events("Q", 0, Duration::from_secs(5))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn delete_queue_sends_form_body() {
    let fake = Fake::start(|_, _| Reply::ok()).await;
    fake.client().delete_queue("Q:1").await.unwrap();
    let request = fake.last();
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.form()["queue_id"], "Q:1");
}

#[tokio::test]
async fn api_errors_carry_code_and_status() {
    let fake = Fake::start(|req, _| match req.path.as_str() {
        "/api/v1/messages/1" => Reply::json(
            json!({"result": "error", "msg": "Bad event queue id", "code": "BAD_EVENT_QUEUE_ID"}),
        )
        .status(400),
        "/api/v1/messages/2" => Reply::json(
            json!({"result": "error", "msg": "Invalid API key", "code": "UNAUTHORIZED"}),
        )
        .status(401),
        "/api/v1/messages/3" => {
            Reply::json(json!({"result": "error", "msg": "no code"})).status(400)
        }
        "/api/v1/messages/4" => {
            Reply::json(json!({"result": "error", "msg": "on a 200", "code": "WEIRD"}))
        }
        "/api/v1/messages/5" => Reply::bytes("<html>".repeat(100).into_bytes()).status(502),
        _ => Reply::json(json!([1, 2])),
    })
    .await;
    let client = fake.client();

    let error = client.get_message(1, true).await.unwrap_err();
    assert!(error.is_bad_event_queue());
    assert!(
        matches!(&error, ApiError::Api { status: 400, message, .. } if message == "Bad event queue id")
    );

    let error = client.get_message(2, true).await.unwrap_err();
    assert!(error.is_auth_error() && !error.is_bad_event_queue());
    assert_eq!(error.code(), Some("UNAUTHORIZED"));

    assert_eq!(
        client.get_message(3, true).await.unwrap_err().code(),
        Some("BAD_REQUEST")
    );
    assert_eq!(
        client.get_message(4, true).await.unwrap_err().code(),
        Some("WEIRD")
    );

    match client.get_message(5, true).await.unwrap_err() {
        ApiError::Api {
            status,
            code,
            message,
            body,
        } => {
            assert_eq!((status, code.as_str()), (502, "HTTP_502"));
            assert_eq!(message.chars().count(), 200);
            assert!(body.is_none());
        }
        other => panic!("{other:?}"),
    }

    // Success status code but the root is not an object
    assert_eq!(
        client.get_message(6, true).await.unwrap_err().code(),
        Some("BAD_REQUEST")
    );
    // Missing expected field
    let fake = Fake::start(|_, _| Reply::ok()).await;
    assert!(matches!(
        fake.client().get_message(1, true).await,
        Err(ApiError::Decode(_))
    ));
}

#[tokio::test]
async fn rate_limited_requests_are_retried() {
    let fake = Fake::start(|_, index| {
        if index < 2 {
            Reply::json(json!({"result": "error", "msg": "slow down", "code": "RATE_LIMIT_HIT", "retry-after": 0.01})).status(429)
        } else {
            success(json!({"id": 9}))
        }
    })
    .await;
    let sent = fake
        .client()
        .send_message(&OutgoingMessage {
            user_ids: vec![9],
            content: "hi".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(sent.id, 9);
    assert_eq!(fake.requests().len(), 3);

    let always = Fake::start(|_, _| {
        Reply::json(json!({"result": "error", "msg": "slow", "code": "RATE_LIMIT_HIT"}))
            .status(429)
            .header("retry-after", "0")
    })
    .await;
    let error = always.client().delete_message(1).await.unwrap_err();
    assert!(matches!(error, ApiError::Api { status: 429, .. }));
    assert_eq!(always.requests().len(), 4);
}

#[tokio::test]
async fn slow_requests_time_out() {
    let fake = Fake::start(|_, _| Reply::ok().delay(Duration::from_secs(5))).await;
    let error = fake
        .client()
        .request(
            Method::GET,
            "api/v1/events",
            None,
            Some(Duration::from_millis(100)),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ApiError::Timeout(_)), "{error:?}");
}

#[tokio::test]
async fn cancelling_aborts_requests() {
    let fake = Fake::start(|_, _| Reply::ok().delay(Duration::from_secs(5))).await;
    let client = fake.client();

    let token = CancellationToken::new();
    let scoped = client.with_cancel(token.clone());
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        canceller.cancel();
    });
    let error = scoped
        .get_events("Q", 0, Duration::from_secs(30))
        .await
        .unwrap_err();
    assert!(error.is_cancelled());

    let closer = client.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        closer.close();
    });
    assert!(
        client
            .get_events("Q", 0, Duration::from_secs(30))
            .await
            .unwrap_err()
            .is_cancelled()
    );
    // New requests after close are cancelled immediately
    assert!(client.delete_message(1).await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn send_message_forms() {
    let fake = Fake::start(|_, _| success(json!({"id": 42, "scheduled_message_id": 11}))).await;
    let client = fake.client();

    let channel = OutgoingMessage {
        stream_id: Some(4),
        topic: Some("构建 失败".into()),
        content: "你好 & 再见".into(),
        queue_id: Some("Q:1".into()),
        local_id: Some("L1".into()),
        ..Default::default()
    };
    assert_eq!(client.send_message(&channel).await.unwrap().id, 42);
    let form = fake.last().form();
    assert_eq!(form["type"], "channel");
    assert_eq!(
        (
            form["to"].as_str(),
            form["topic"].as_str(),
            form["content"].as_str()
        ),
        ("4", "构建 失败", "你好 & 再见")
    );
    assert_eq!(
        (
            form["read_by_sender"].as_str(),
            form["queue_id"].as_str(),
            form["local_id"].as_str()
        ),
        ("true", "Q:1", "L1")
    );

    client
        .send_message(&OutgoingMessage {
            user_ids: vec![9, 10],
            content: "dm".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let form = fake.last().form();
    assert_eq!(
        (form["type"].as_str(), form["to"].as_str()),
        ("direct", "[9,10]")
    );
    assert!(!form.contains_key("queue_id") && !form.contains_key("topic"));

    let scheduled = OutgoingMessage {
        stream_id: Some(4),
        topic: Some("t".into()),
        content: "later".into(),
        ..Default::default()
    };
    assert_eq!(
        client
            .create_scheduled_message(&scheduled, 1_800_000_000)
            .await
            .unwrap(),
        11
    );
    let request = fake.last();
    assert_eq!(request.path, "/api/v1/scheduled_messages");
    assert_eq!(request.form()["type"], "stream");
    assert_eq!(request.form()["scheduled_delivery_timestamp"], "1800000000");
}

#[tokio::test]
async fn get_messages_query() {
    let fake = Fake::start(|_, _| success(json!({"messages": [{"id": 1, "type": "stream", "stream_id": 4, "subject": "t"}], "found_newest": true, "anchor": 10000000000000000_i64}))).await;
    let mut query = MessageQuery::new(topic_narrow(4, "构建失败"), "newest", 50, 0);
    query.anchor_date = Some("2026-01-01T00:00:00Z".into());
    let result = fake.client().get_messages(&query).await.unwrap();
    assert!(result.found_newest && result.messages[0].is_stream());

    let pairs = fake.last().query_pairs();
    assert_eq!(
        (
            pairs["anchor"].as_str(),
            pairs["num_before"].as_str(),
            pairs["num_after"].as_str()
        ),
        ("newest", "50", "0")
    );
    assert_eq!(
        pairs["narrow"],
        r#"[{"operator":"channel","operand":4,"negated":false},{"operator":"topic","operand":"构建失败","negated":false}]"#
    );
    assert_eq!(
        (
            pairs["include_anchor"].as_str(),
            pairs["client_gravatar"].as_str()
        ),
        ("true", "false")
    );
    assert_eq!(pairs["allow_empty_topic_name"], "true");
    assert_eq!(pairs["anchor_date"], "2026-01-01T00:00:00Z");
}

#[tokio::test]
async fn endpoints_use_expected_requests() {
    let fake = Fake::start(|_, _| {
        success(json!({
            "id": 7, "ids": [31], "subscribers": [3, 4], "topics": [{"name": "t", "max_id": 9}], "complete": false,
            "message_history": [{"topic": "t", "content": "a", "timestamp": 1}], "user_ids": [7],
            "alert_words": ["w"], "scheduled_message_id": 11, "reminder_id": 12, "saved_snippet_id": 13, "rendered": "<p>x</p>",
            "attachments": [{"id": 1, "name": "f", "path_id": "1/a/f", "size": 5, "create_time": 1_700_000_000, "message_ids": [4]}],
            "upload_space_used": 99, "drafts": [{"id": 1, "type": "stream", "to": [5], "topic": "t", "content": "c"}],
            "presences": {"9": {"active_timestamp": 5, "idle_timestamp": 6.0}}, "presence_last_update_id": 3,
            "processed_count": 2, "updated_count": 1,
        }))
    })
    .await;
    let client = fake.client();

    // (method, path, form/query parameters)
    macro_rules! check {
        ($method:expr, $path:expr, [$(($k:expr, $v:expr)),* $(,)?]) => {{
            let request = fake.last();
            assert_eq!(request.method, $method, "{}", $path);
            assert_eq!(request.path, $path);
            $(assert_eq!(request.param($k).as_deref(), Some($v), "{} {}", $path, $k);)*
        }};
    }

    assert_eq!(client.render_message("**x**").await.unwrap(), "<p>x</p>");
    check!("POST", "/api/v1/messages/render", [("content", "**x**")]);

    client
        .update_flags(&[1, 2], FlagOp::Add, MessageFlag::Read)
        .await
        .unwrap();
    check!(
        "POST",
        "/api/v1/messages/flags",
        [("messages", "[1,2]"), ("op", "add"), ("flag", "read")]
    );

    let narrow = FlagsNarrowUpdate {
        narrow: vec![NarrowTerm::is("unread")],
        anchor: "first_unread".into(),
        include_anchor: true,
        num_before: 0,
        num_after: 1000,
        op: FlagOp::Add,
        flag: MessageFlag::Read,
    };
    assert_eq!(
        client
            .update_flags_for_narrow(&narrow)
            .await
            .unwrap()
            .processed_count,
        2
    );
    check!(
        "POST",
        "/api/v1/messages/flags/narrow",
        [("anchor", "first_unread"), ("num_after", "1000")]
    );

    client
        .add_reaction(5, "smile", "1f642", "unicode_emoji")
        .await
        .unwrap();
    check!(
        "POST",
        "/api/v1/messages/5/reactions",
        [
            ("emoji_name", "smile"),
            ("emoji_code", "1f642"),
            ("reaction_type", "unicode_emoji")
        ]
    );
    client
        .remove_reaction(5, "smile", "1f642", "unicode_emoji")
        .await
        .unwrap();
    check!(
        "DELETE",
        "/api/v1/messages/5/reactions",
        [("emoji_name", "smile")]
    );

    client.send_submessage(5, "widget", "{}").await.unwrap();
    check!(
        "POST",
        "/api/v1/submessage",
        [
            ("message_id", "5"),
            ("msg_type", "widget"),
            ("content", "{}")
        ]
    );

    let update = MessageUpdate {
        topic: Some("新".into()),
        propagate_mode: Some("change_all".into()),
        ..Default::default()
    };
    client.update_message(5, &update).await.unwrap();
    check!(
        "PATCH",
        "/api/v1/messages/5",
        [("topic", "新"), ("propagate_mode", "change_all")]
    );
    assert!(!fake.last().form().contains_key("content"));
    client
        .update_message(
            5,
            &MessageUpdate {
                stream_id: Some(8),
                content: Some("c".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    check!(
        "PATCH",
        "/api/v1/messages/5",
        [("stream_id", "8"), ("content", "c")]
    );

    client.delete_message(5).await.unwrap();
    check!("DELETE", "/api/v1/messages/5", []);

    assert_eq!(client.get_message_history(5).await.unwrap()[0].topic, "t");
    check!(
        "GET",
        "/api/v1/messages/5/history",
        [("allow_empty_topic_name", "true")]
    );
    assert_eq!(client.get_read_receipts(5).await.unwrap(), [7]);
    check!("GET", "/api/v1/messages/5/read_receipts", []);

    client
        .send_typing("start", Some(4), Some("t"), &[])
        .await
        .unwrap();
    check!(
        "POST",
        "/api/v1/typing",
        [
            ("op", "start"),
            ("type", "stream"),
            ("stream_id", "4"),
            ("topic", "t")
        ]
    );
    client.send_typing("stop", None, None, &[9]).await.unwrap();
    check!(
        "POST",
        "/api/v1/typing",
        [("op", "stop"), ("type", "direct"), ("to", "[9]")]
    );

    assert_eq!(client.get_subscribers(4).await.unwrap(), [3, 4]);
    check!("GET", "/api/v1/streams/4/members", []);
    let topics = client.get_topics(4).await.unwrap();
    assert_eq!((topics[0].name.as_str(), topics[0].max_id), ("t", 9));
    check!(
        "GET",
        "/api/v1/users/me/4/topics",
        [("allow_empty_topic_name", "true")]
    );
    client.set_topic_visibility(4, "t", 3).await.unwrap();
    check!(
        "POST",
        "/api/v1/user_topics",
        [
            ("stream_id", "4"),
            ("topic", "t"),
            ("visibility_policy", "3")
        ]
    );

    let change = SubscriptionPropertyChange {
        stream_id: 4,
        property: "pin_to_top".into(),
        value: json!(true),
    };
    client
        .update_subscription_properties(&[change])
        .await
        .unwrap();
    check!("POST", "/api/v1/users/me/subscriptions/properties", []);
    assert_eq!(
        form_json(&fake.last(), "subscription_data"),
        json!([{"stream_id": 4, "property": "pin_to_top", "value": true}])
    );

    assert!(!client.delete_topic(4, "t").await.unwrap());
    check!(
        "POST",
        "/api/v1/streams/4/delete_topic",
        [("topic_name", "t")]
    );

    client.subscribe(&["dev"], Some(&[9])).await.unwrap();
    check!(
        "POST",
        "/api/v1/users/me/subscriptions",
        [
            ("subscriptions", r#"[{"name":"dev"}]"#),
            ("principals", "[9]")
        ]
    );
    client.unsubscribe(&["dev".to_owned()], None).await.unwrap();
    check!(
        "DELETE",
        "/api/v1/users/me/subscriptions",
        [("subscriptions", r#"["dev"]"#)]
    );
    assert!(!fake.last().form().contains_key("principals"));

    assert_eq!(
        client
            .create_channel("new", "d", true, &[8, 9])
            .await
            .unwrap(),
        7
    );
    check!(
        "POST",
        "/api/v1/channels/create",
        [
            ("name", "new"),
            ("description", "d"),
            ("invite_only", "true"),
            ("subscribers", "[8,9]")
        ]
    );
    client
        .update_channel(4, &Form::new().add("new_name", "x"))
        .await
        .unwrap();
    check!("PATCH", "/api/v1/streams/4", [("new_name", "x")]);

    assert_eq!(client.create_reminder(5, 1_800_000_000).await.unwrap(), 12);
    check!(
        "POST",
        "/api/v1/reminders",
        [
            ("message_id", "5"),
            ("scheduled_delivery_timestamp", "1800000000")
        ]
    );
    client.delete_reminder(12).await.unwrap();
    check!("DELETE", "/api/v1/reminders/12", []);
    client.delete_scheduled_message(11).await.unwrap();
    check!("DELETE", "/api/v1/scheduled_messages/11", []);
    assert_eq!(
        client.create_saved_snippet("标题", "内容").await.unwrap(),
        13
    );
    check!(
        "POST",
        "/api/v1/saved_snippets",
        [("title", "标题"), ("content", "内容")]
    );
    client.delete_saved_snippet(13).await.unwrap();
    check!("DELETE", "/api/v1/saved_snippets/13", []);

    client.send_invites("a@b.c,d@e.f", &[1, 2]).await.unwrap();
    check!(
        "POST",
        "/api/v1/invites",
        [
            ("invitee_emails", "a@b.c,d@e.f"),
            ("stream_ids", "[1,2]"),
            ("invite_as", "400"),
            ("include_realm_default_subscriptions", "false")
        ]
    );

    client
        .update_user(9, Some("新名字"), Some(200))
        .await
        .unwrap();
    check!(
        "PATCH",
        "/api/v1/users/9",
        [("full_name", "新名字"), ("role", "200")]
    );
    client.mute_user(9).await.unwrap();
    check!("POST", "/api/v1/users/me/muted_users/9", []);
    client.unmute_user(9).await.unwrap();
    check!("DELETE", "/api/v1/users/me/muted_users/9", []);

    let attachments = client.get_attachments().await.unwrap();
    assert_eq!(
        (
            attachments.attachments[0].message_ids.clone(),
            attachments.upload_space_used
        ),
        (vec![4], 99)
    );
    check!("GET", "/api/v1/attachments", []);
    client.delete_attachment(1).await.unwrap();
    check!("DELETE", "/api/v1/attachments/1", []);

    assert_eq!(client.get_drafts().await.unwrap()[0].to, [5]);
    check!("GET", "/api/v1/drafts", []);
    let draft = DraftDto {
        id: Some(1),
        kind: "stream".into(),
        to: vec![5],
        topic: "t".into(),
        content: "c".into(),
        timestamp: Some(123),
    };
    assert_eq!(client.create_draft(&draft).await.unwrap(), 31);
    check!("POST", "/api/v1/drafts", []);
    assert_eq!(
        form_json(&fake.last(), "drafts"),
        json!([{"type": "stream", "to": [5], "topic": "t", "content": "c", "timestamp": 123}])
    );
    client
        .edit_draft(
            1,
            &DraftDto {
                timestamp: None,
                ..draft
            },
        )
        .await
        .unwrap();
    check!("PATCH", "/api/v1/drafts/1", []);
    assert_eq!(
        form_json(&fake.last(), "draft"),
        json!({"type": "stream", "to": [5], "topic": "t", "content": "c"})
    );
    client.delete_draft(1).await.unwrap();
    check!("DELETE", "/api/v1/drafts/1", []);

    let presence = client.update_presence("active", 3, true).await.unwrap();
    assert_eq!(
        (
            presence.presences[&9].idle_timestamp,
            presence.presence_last_update_id
        ),
        (6, Some(3))
    );
    check!(
        "POST",
        "/api/v1/users/me/presence",
        [
            ("status", "active"),
            ("last_update_id", "3"),
            ("new_user_input", "true"),
            ("slim_presence", "true")
        ]
    );

    client
        .update_settings(&Form::new().add("enter_sends", true))
        .await
        .unwrap();
    check!("PATCH", "/api/v1/settings", [("enter_sends", "true")]);
    client.set_status("忙").await.unwrap();
    check!("POST", "/api/v1/users/me/status", [("status_text", "忙")]);
    assert_eq!(client.add_alert_words(&["w"]).await.unwrap(), ["w"]);
    check!(
        "POST",
        "/api/v1/users/me/alert_words",
        [("alert_words", r#"["w"]"#)]
    );
    client.remove_alert_words(&["w"]).await.unwrap();
    check!(
        "DELETE",
        "/api/v1/users/me/alert_words",
        [("alert_words", r#"["w"]"#)]
    );
    client
        .update_realm(&Form::new().add("name", "组织"))
        .await
        .unwrap();
    check!("PATCH", "/api/v1/realm", [("name", "组织")]);
}

#[tokio::test]
async fn upload_streams_file_and_reports_progress() {
    let fake = Fake::start(|_, _| {
        success(json!({"url": "/user_uploads/1/ab/x/文件.txt", "filename": "文件.txt"}))
    })
    .await;
    let data = vec![7u8; 200_000];
    let last = Arc::new(AtomicU64::new(0));
    let calls = Arc::new(AtomicU64::new(0));
    let (progress_last, progress_calls) = (last.clone(), calls.clone());

    let result = fake
        .client()
        .upload_file(
            Cursor::new(data.clone()),
            data.len() as u64,
            "文件.txt",
            "text/plain",
            move |sent| {
                assert!(sent > progress_last.swap(sent, Ordering::SeqCst));
                progress_calls.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .unwrap();
    assert_eq!(result.url, "/user_uploads/1/ab/x/文件.txt");
    assert_eq!(last.load(Ordering::SeqCst), 200_000);
    assert!(calls.load(Ordering::SeqCst) >= 2);

    let request = fake.last();
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/api/v1/user_uploads")
    );
    assert!(
        request
            .header("content-type")
            .starts_with("multipart/form-data; boundary=")
    );
    // Known length: chunked transfer is not used
    assert_eq!(
        request.header("content-length"),
        request.body.len().to_string()
    );
    let text = String::from_utf8_lossy(&request.body);
    assert!(text.contains(r#"name="file""#));
    assert!(text.contains(r#"filename="文件.txt""#));
    assert!(text.contains("Content-Type: text/plain"));
    assert!(
        request
            .body
            .windows(1000)
            .any(|w| w.iter().all(|&b| b == 7))
    );
}

#[tokio::test]
async fn upload_avatar_posts_file() {
    let fake = Fake::start(|_, _| success(json!({"avatar_url": "/x"}))).await;
    fake.client()
        .upload_avatar(vec![1, 2, 3], "a.png", "image/png")
        .await
        .unwrap();
    let request = fake.last();
    assert_eq!(request.path, "/api/v1/users/me/avatar");
    assert!(String::from_utf8_lossy(&request.body).contains(r#"filename="a.png""#));

    let error = fake
        .client()
        .upload_avatar(vec![], "a.png", "not a type")
        .await
        .unwrap_err();
    assert!(matches!(error, ApiError::InvalidInput(_)));
}

#[tokio::test]
async fn download_writes_file_and_reports_progress() {
    let payload: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
    let served = payload.clone();
    let fake = Fake::start(move |req, _| {
        if req.path.starts_with("/user_uploads/") {
            Reply::bytes(served.clone()).header("content-type", "application/octet-stream")
        } else {
            Reply::ok().status(404)
        }
    })
    .await;

    let mut target = Vec::new();
    let mut seen = Vec::new();
    fake.client()
        .download("/user_uploads/1/ab/x/f.bin", &mut target, |done, total| {
            seen.push((done, total))
        })
        .await
        .unwrap();
    assert_eq!(target, payload);
    assert_eq!(*seen.last().unwrap(), (150_000, Some(150_000)));
    assert!(seen.windows(2).all(|w| w[0].0 < w[1].0));
    let request = fake.last();
    assert_eq!(request.path, "/user_uploads/1/ab/x/f.bin");
    assert!(request.header("authorization").starts_with("Basic "));

    let error = fake
        .client()
        .download("/missing", &mut Vec::new(), |_, _| {})
        .await
        .unwrap_err();
    assert!(matches!(&error, ApiError::Api { status: 404, code, .. } if code == "HTTP_404"));
}

#[tokio::test]
async fn path_pointing_to_another_site_never_reaches_it() {
    let evil = Fake::start(|_, _| Reply::bytes(vec![1])).await;
    let realm = Fake::start(|_, _| Reply::bytes(vec![2])).await;
    let client = realm.client();
    let target = format!("{}a.png", evil.url);
    for path in [target.clone(), format!("/{target}")] {
        let error = client
            .download(&path, &mut Vec::new(), |_, _| {})
            .await
            .unwrap_err();
        assert!(matches!(error, ApiError::InvalidInput(_)), "{path}");
    }
    assert!(evil.shared.requests.lock().is_empty());
    assert!(realm.shared.requests.lock().is_empty());

    // An absolute address of this organization works, and carries authentication.
    let own = format!("{}a.png", realm.url);
    client
        .download(&own, &mut Vec::new(), |_, _| {})
        .await
        .unwrap();
    assert!(realm.last().header("authorization").starts_with("Basic "));
}

#[tokio::test]
async fn head_size_reads_content_length() {
    let fake = Fake::start(|req, _| {
        if req.method == "HEAD" {
            Reply::bytes(vec![]).header("content-length", "1234")
        } else {
            Reply::bytes(vec![0; 10])
        }
    })
    .await;
    assert_eq!(
        fake.client().head_size("/user_uploads/f").await.unwrap(),
        Some(1234)
    );
    assert_eq!(fake.requests().len(), 1);
    assert_eq!(fake.last().header("accept-encoding"), "identity");
}

#[tokio::test]
async fn head_size_falls_back_to_range_request() {
    let ranged = Fake::start(|req, _| {
        if req.method == "HEAD" {
            Reply::bytes(vec![]).status(405)
        } else {
            Reply::bytes(vec![0])
                .status(206)
                .header("content-range", "bytes 0-0/4321")
        }
    })
    .await;
    assert_eq!(ranged.client().head_size("/f").await.unwrap(), Some(4321));
    assert_eq!(ranged.last().header("range"), "bytes=0-0");

    let whole = Fake::start(|req, _| {
        if req.method == "HEAD" {
            Reply::bytes(vec![]).status(405)
        } else {
            Reply::bytes(vec![0; 77])
        }
    })
    .await;
    assert_eq!(whole.client().head_size("/f").await.unwrap(), Some(77));

    let unknown = Fake::start(|_, _| Reply::bytes(vec![]).status(404)).await;
    assert_eq!(unknown.client().head_size("/f").await.unwrap(), None);
}

#[tokio::test]
async fn head_size_is_none_when_unreachable_but_cancellable() {
    let client = ZulipClient::new("http://127.0.0.1:9/", "a", "b", false).unwrap();
    assert_eq!(client.head_size("/f").await.unwrap(), None);
    client.close();
    assert!(client.head_size("/f").await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn network_failure_is_reported() {
    let client = ZulipClient::new("http://127.0.0.1:9/", "a", "b", false).unwrap();
    assert!(matches!(
        client.delete_message(1).await,
        Err(ApiError::Network(_))
    ));
}

#[tokio::test]
async fn trace_records_requests_with_secrets_masked() {
    let fake = Fake::start(|req, _| match req.path.as_str() {
        "/api/v1/fetch_api_key" => {
            success(json!({"api_key": "SECRET-KEY", "email": "a@b", "user_id": 8}))
        }
        "/user_uploads/f" => Reply::bytes(vec![1; 10]).header("content-type", "image/png"),
        _ => Reply::json(json!({"result": "error", "msg": "nope", "code": "X"})).status(400),
    })
    .await;
    let log = RequestLog::new(true, 100);
    let config = ClientConfig::new();
    config.set_trace_sink(Some(log.clone()));
    let client = ZulipClient::builder(&fake.url)
        .credentials("bot@x.test", "KEY123")
        .config(config.clone())
        .build()
        .unwrap();

    client.fetch_api_key("u@x", "p@ss&word").await.unwrap();
    client.delete_message(1).await.unwrap_err();
    client
        .download("/user_uploads/f", &mut Vec::new(), |_, _| {})
        .await
        .unwrap();
    client
        .upload_file(
            Cursor::new(vec![0u8; 64]),
            64,
            "a.bin",
            "application/octet-stream",
            |_| {},
        )
        .await
        .unwrap_err();

    let entries = log.entries();
    assert_eq!(entries.len(), 4);
    assert!(entries.iter().all(|e| !e.is_pending()));

    let login = &entries[0];
    assert_eq!(login.request.method, "POST");
    assert!(login.request.url.ends_with("/api/v1/fetch_api_key"));
    assert_eq!(login.request.body, "username=u%40x&password=******");
    assert!(
        login
            .request
            .headers
            .contains("Authorization: Basic ******")
    );
    assert!(login.request.headers.contains("User-Agent: YaZ/"));
    let response = login.response.as_ref().unwrap();
    assert_eq!(
        (response.status, response.reason.as_str()),
        (Some(200), "OK")
    );
    assert!(
        response.body.contains(r#""api_key":"******""#) && !response.body.contains("SECRET-KEY")
    );
    assert!(response.headers.contains("Content-Type: application/json"));

    assert!(entries[1].is_failed());
    assert!(entries[1].response.as_ref().unwrap().body.contains("nope"));

    let download = entries[2].response.as_ref().unwrap();
    assert_eq!(download.body, "[image/png，10 字节，未记录内容]");

    let upload = &entries[3];
    assert!(
        upload.request.body.starts_with("[multipart/form-data，"),
        "{}",
        upload.request.body
    );

    assert!(
        !format!(
            "{:?}",
            entries
                .iter()
                .map(|e| &e.request.headers)
                .collect::<Vec<_>>()
        )
        .contains("KEY123")
    );

    // No more recording after recording is turned off
    log.configure(false, 100);
    client.delete_message(1).await.unwrap_err();
    assert_eq!(log.entries().len(), 4);
}

#[tokio::test]
async fn trace_records_network_failures() {
    let log = RequestLog::new(true, 10);
    let config = ClientConfig::new();
    config.set_trace_sink(Some(log.clone()));
    let client = ZulipClient::builder("http://127.0.0.1:9/")
        .credentials("a", "b")
        .config(config)
        .build()
        .unwrap();
    client.delete_message(1).await.unwrap_err();

    let entries = log.entries();
    let response = entries[0].response.as_ref().unwrap();
    assert!(response.status.is_none() && response.error.is_some());
    assert!(entries[0].is_failed());
}
