//! 会话钩子：通知型事件、统一发送出口、当前会话。

mod common;

use std::sync::Arc;

use common::{eventually, new_hub, password_request};
use parking_lot::Mutex;
use serde_json::json;
use tempfile::TempDir;
use test_server::{AJIE, DEV, ME_ID, Target, TestServer, XIAOYU};
use zulip_core::ConversationKey;
use zulip_core::model::ChatMessage;
use zulip_session::{
    ConnectionState, HookEvent, HookFuture, Hub, OutgoingKind, Session, SessionError, SessionHooks,
};

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<HookEvent>>,
    kinds: Mutex<Vec<OutgoingKind>>,
}

impl Recorder {
    fn has(&self, check: impl Fn(&HookEvent) -> bool) -> bool {
        self.events.lock().iter().any(check)
    }
}

impl SessionHooks for Recorder {
    fn session_started(&self, _session: &Arc<Session>) {}

    fn session_ending(&self, _session: &Arc<Session>) -> HookFuture<()> {
        Box::pin(async {})
    }

    fn filter_incoming(&self, _session: &Arc<Session>, _message: &ChatMessage) -> HookFuture<bool> {
        Box::pin(async { false })
    }

    fn on_message(&self, _session: &Arc<Session>, _message: &ChatMessage) {}

    fn on_event(&self, _session: &Arc<Session>, event: HookEvent) {
        self.events.lock().push(event);
    }

    fn filter_outgoing(
        &self,
        _session: &Arc<Session>,
        _key: &ConversationKey,
        content: &str,
        kind: OutgoingKind,
    ) -> HookFuture<Result<String, String>> {
        self.kinds.lock().push(kind);
        let result = if content.contains("拦截") {
            Err("不许发".to_owned())
        } else {
            Ok(content.replace("旧词", "新词"))
        };
        Box::pin(async move { result })
    }
}

struct Fixture {
    server: TestServer,
    _hub: Arc<Hub>,
    session: Arc<Session>,
    hooks: Arc<Recorder>,
    _dir: TempDir,
}

async fn fixture() -> Fixture {
    let server = TestServer::start(0).await.unwrap();
    let dir = TempDir::new().unwrap();
    let hub = new_hub(&dir);
    let hooks = Arc::new(Recorder::default());
    hub.set_hooks(hooks.clone());
    let session = hub
        .login_with_password(password_request(&server))
        .await
        .unwrap();
    Fixture {
        server,
        _hub: hub,
        session,
        hooks,
        _dir: dir,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn send_exit_filters_every_kind() {
    let f = fixture().await;
    let key = ConversationKey::dm([XIAOYU], ME_ID);
    let other = ConversationKey::dm([AJIE], ME_ID);

    let id = f.session.send_message(&key, "旧词").await.unwrap();
    assert_eq!(f.server.content_of(id).as_deref(), Some("新词"));

    let error = f.session.send_message(&key, "拦截").await.unwrap_err();
    assert!(matches!(&error, SessionError::Invalid(text) if text == "不许发"));

    let id = f
        .session
        .send_as(&key, "旧词 文件", OutgoingKind::File)
        .await
        .unwrap();
    assert_eq!(f.server.content_of(id).as_deref(), Some("新词 文件"));

    f.hooks.kinds.lock().clear();
    let original = f
        .server
        .send_as(XIAOYU, Target::Dm(vec![ME_ID]), "旧词转发");
    f.session.load_newest(&key).await.unwrap();
    let before = f.server.latest_message_id();
    f.session
        .forward_each(&[original], std::slice::from_ref(&other))
        .await
        .unwrap();
    let forwarded = f.server.latest_message_id();
    assert!(forwarded > before);
    assert_eq!(f.server.content_of(forwarded).as_deref(), Some("新词转发"));
    f.session
        .forward_merged(&[original], std::slice::from_ref(&other))
        .await
        .unwrap();
    assert_eq!(
        *f.hooks.kinds.lock(),
        vec![OutgoingKind::Forward, OutgoingKind::Forward]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn local_draft_changes_notify_hooks() {
    let f = fixture().await;
    f.session
        .set_zulip_setting("enable_drafts_synchronization", json!(false))
        .await
        .unwrap();
    let key = ConversationKey::dm([XIAOYU], ME_ID);
    let count = || {
        let drafts = |e: &&HookEvent| matches!(e, HookEvent::DraftChanged(k) if *k == key);
        f.hooks.events.lock().iter().filter(drafts).count()
    };

    f.session.save_draft(&key, "写了一半").await.unwrap();
    assert_eq!(count(), 1);
    f.session.save_draft(&key, "写了一半").await.unwrap();
    assert_eq!(count(), 1);
    f.session.save_draft(&key, "").await.unwrap();
    assert_eq!(count(), 2);
    f.session.save_draft(&key, "").await.unwrap();
    assert_eq!(count(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn current_conversation_follows_windows() {
    let f = fixture().await;
    let key = ConversationKey::dm([XIAOYU], ME_ID);
    let topic = ConversationKey::topic(DEV, "发布计划");
    assert_eq!(f.session.current_conversation(), None);

    f.session.open_conversation(&key);
    assert_eq!(f.session.current_conversation(), Some(key.clone()));
    f.session.open_conversation(&topic);
    f.session.set_actively_viewing(&key, true);
    assert_eq!(f.session.current_conversation(), Some(key.clone()));
    let opened = |event: &HookEvent| matches!(event, HookEvent::ConversationOpened(_));
    assert_eq!(
        f.hooks.events.lock().iter().filter(|e| opened(e)).count(),
        3
    );

    f.session.close_conversation(&key);
    assert_eq!(f.session.current_conversation(), None);
    f.session.close_conversation(&topic);
    assert_eq!(f.session.current_conversation(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn notices_cover_server_events() {
    let mut f = fixture().await;
    let session = f.session.clone();
    let key = ConversationKey::dm([XIAOYU], ME_ID);
    let hooks = f.hooks.clone();

    let id = session.send_message(&key, "原文").await.unwrap();
    session.load_newest(&key).await.unwrap();
    session.edit_message(id, "改过").await.unwrap();
    eventually("编辑", || {
        hooks.has(|e| {
            matches!(e, HookEvent::MessageEdited { id: edited, key: Some(k), edit_timestamp: Some(_), .. }
                if *edited == id && *k == key)
        })
    })
    .await;

    let emoji = session.emoji_catalog();
    let mut thumbs = emoji.get("+1").cloned();
    for _ in 0..100 {
        if thumbs.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        thumbs = session.emoji_catalog().get("+1").cloned();
    }
    let thumbs = thumbs.expect("表情目录");
    session.toggle_reaction(id, &thumbs).await.unwrap();
    eventually("加回应", || {
        hooks.has(|e| {
            matches!(e, HookEvent::ReactionChanged { id: r, added: true, reaction, .. }
                if *r == id && reaction.emoji_name == "+1")
        })
    })
    .await;
    session.toggle_reaction(id, &thumbs).await.unwrap();
    eventually("取消回应", || {
        hooks.has(|e| matches!(e, HookEvent::ReactionChanged { added: false, .. }))
    })
    .await;

    let incoming = f.server.send_as(XIAOYU, Target::Dm(vec![ME_ID]), "x");
    session.mark_read(&[incoming]).await.unwrap();
    eventually("已读", || {
        hooks.has(|e| matches!(e, HookEvent::ReadChanged { ids, read: true, all: false } if ids == &[incoming]))
    })
    .await;

    session
        .delete(zulip_session::DeleteTarget::Message(id))
        .await
        .unwrap();
    eventually("删除", || {
        hooks.has(|e| matches!(e, HookEvent::MessagesDeleted { ids, .. } if ids == &[id]))
    })
    .await;

    let topic = ConversationKey::topic(DEV, "发布计划");
    session.toggle_topic_resolved(&topic).await.unwrap();
    eventually("话题改名", || {
        hooks.has(|e| {
            matches!(e, HookEvent::TopicMoved { old_stream, old_topic, new_stream, new_topic, .. }
                if *old_stream == DEV && *new_stream == DEV && old_topic == "发布计划"
                    && new_topic == "✔ 发布计划")
        })
    })
    .await;

    f.server.push_event(json!({
        "type": "stream", "op": "update", "stream_id": DEV, "property": "description",
        "value": "新介绍", "rendered_description": "<p>新介绍</p>",
    }));
    eventually("频道", || {
        hooks.has(|e| matches!(e, HookEvent::ChannelsChanged { stream_ids, op } if stream_ids == &[DEV] && op == "update"))
    })
    .await;

    f.server
        .push_event(json!({"type": "user_status", "user_id": AJIE, "status_text": "忙"}));
    eventually("用户", || {
        hooks.has(|e| *e == HookEvent::UserChanged(AJIE))
    })
    .await;

    f.server.push_event(json!({
        "type": "presence", "presences": {
            AJIE.to_string(): {"active_timestamp": chrono::Utc::now().timestamp(), "idle_timestamp": 0}
        },
    }));
    eventually("在线状态", || {
        hooks.has(|e| matches!(e, HookEvent::PresenceChanged(ids) if ids == &[AJIE]))
    })
    .await;

    f.server.stop().await;
    eventually("断开", || {
        hooks.has(|e| *e == HookEvent::Connection(ConnectionState::Reconnecting))
    })
    .await;
    hooks.events.lock().clear();
    f.server.restart().await.unwrap();
    eventually("恢复", || {
        hooks.has(|e| *e == HookEvent::Connection(ConnectionState::Connected))
    })
    .await;
}
