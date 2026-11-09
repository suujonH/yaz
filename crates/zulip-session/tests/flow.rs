mod common;

use common::{eventually, fixture, new_hub, password_request, wait_for};
use tempfile::TempDir;
use test_server::{AJIE, DEV, GAME, LAOWANG, ME_EMAIL, ME_ID, Target, TestServer, XIAOYU};
use zulip_core::ConversationKey;
use zulip_core::model::MessageState;
use zulip_session::{AccountStatus, ConnectionState, Hub, SessionError, SessionEvent};

#[tokio::test]
async fn login_and_register_load_the_account() {
    let mut f = fixture().await;
    let s = &f.session;
    assert_eq!(s.self_id(), ME_ID);
    assert_eq!(s.display_name(AJIE), "阿杰");
    assert!(s.subscribed_channels().iter().any(|c| c.name == "开发"));

    let accounts = f.hub.accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].account.email, ME_EMAIL);
    assert_eq!(accounts[0].status, AccountStatus::Online);
    assert_eq!(f.hub.active_account(), Some(s.account_id()));

    // 种子未读
    let dm = ConversationKey::dm([LAOWANG], ME_ID);
    assert_eq!(s.unread_count(&dm), 1);
    assert_eq!(s.unread_count(&ConversationKey::topic(DEV, "发布计划")), 2);
    let rows = s.message_rows();
    assert!(
        rows.iter()
            .any(|r| r.key() == Some(&dm) && r.unread_count == 1)
    );
    let draft = s.draft(&ConversationKey::topic(GAME, "新游推荐")).unwrap();
    assert_eq!(draft.content, "我想推荐一个……");

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn zuliprc_login_reuses_the_account_and_saved_key_works_after_restart() {
    let mut server = TestServer::start(0).await.unwrap();
    let dir = TempDir::new().unwrap();
    let hub = new_hub(&dir);
    let rc = format!(
        "[api]\nemail={ME_EMAIL}\nkey={}\nsite={}\n",
        test_server::API_KEY,
        server.url()
    );
    let session = hub.login_with_zuliprc(&rc, false).await.unwrap();
    let id = session.account_id();
    hub.shutdown().await;
    drop(hub);

    let hub = new_hub(&dir);
    hub.restore_startup_accounts().await.unwrap();
    assert_eq!(hub.active_session().unwrap().account_id(), id);
    hub.shutdown().await;
    server.stop().await;
}

#[tokio::test]
async fn wrong_server_reports_a_friendly_error() {
    let dir = TempDir::new().unwrap();
    let hub: std::sync::Arc<Hub> = new_hub(&dir);
    let mut server = TestServer::start(0).await.unwrap();
    let mut request = password_request(&server);
    request.realm = "不是地址 /".to_owned();
    let Err(error) = hub.login_with_password(request).await else {
        panic!("应当失败");
    };
    assert_eq!(error.to_string(), "服务器地址无效");
    server.stop().await;
}

#[tokio::test]
async fn incoming_message_updates_unread_and_conversation_list() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::topic(GAME, "吐槽");
    let before = s.unread_count(&key);
    s.load_newest(&key).await.unwrap();
    let mut rx = s.subscribe();

    let id = f
        .server
        .send_as(AJIE, Target::Stream(GAME, "吐槽".into()), "这游戏太肝了");
    wait_for(&mut rx, "新消息", |e| {
        *e == SessionEvent::MessageAdded {
            key: key.clone(),
            id,
        }
    })
    .await;
    assert_eq!(s.unread_count(&key), before + 1);
    let timeline = s.timeline(&key);
    let last = timeline.messages.last().unwrap();
    assert_eq!(last.id, id);
    assert!(last.content_html.contains("这游戏太肝了"));
    let row = s
        .message_rows()
        .into_iter()
        .find(|r| r.key() == Some(&key))
        .unwrap();
    assert!(row.preview.contains("这游戏太肝了"));

    // 正在看这个会话时自动已读
    s.set_actively_viewing(&key, true);
    let id = f
        .server
        .send_as(XIAOYU, Target::Stream(GAME, "吐槽".into()), "确实");
    wait_for(
        &mut rx,
        "第二条",
        |e| matches!(e, SessionEvent::MessageAdded { id: got, .. } if *got == id),
    )
    .await;
    let server = &f.server;
    for _ in 0..100 {
        if server.flags_of(id).iter().any(|flag| flag == "read") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(server.flags_of(id).iter().any(|flag| flag == "read"));

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn send_echoes_locally_then_confirms() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::dm([AJIE], ME_ID);
    s.load_newest(&key).await.unwrap();
    let mut rx = s.subscribe();

    let id = s.send_message(&key, "收到  \n").await.unwrap();
    assert_eq!(f.server.content_of(id).as_deref(), Some("收到"));
    let added = wait_for(&mut rx, "回显", |e| {
        matches!(e, SessionEvent::MessageAdded { .. })
    })
    .await;
    assert!(matches!(added, SessionEvent::MessageAdded { id, .. } if id < 0));
    wait_for(
        &mut rx,
        "确认",
        |e| matches!(e, SessionEvent::MessageConfirmed { id: got, .. } if *got == id),
    )
    .await;
    let timeline = s.timeline(&key);
    assert_eq!(timeline.messages.last().unwrap().id, id);
    assert!(timeline.messages.iter().all(|m| m.id > 0));
    // 自己发的消息不增加未读
    assert_eq!(s.unread_count(&key), 0);

    assert!(matches!(
        s.send_message(&key, "   ").await,
        Err(SessionError::Invalid(_))
    ));

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn failed_send_stays_failed_until_retry() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::topic(DEV, "接口设计");
    s.load_newest(&key).await.unwrap();
    let mut rx = s.subscribe();

    f.server.stop().await;
    assert!(s.send_message(&key, "断网时发").await.is_err());
    let local = s
        .timeline(&key)
        .messages
        .iter()
        .find(|m| m.id < 0)
        .cloned()
        .unwrap();
    assert!(matches!(local.state, MessageState::Failed(_)));
    let local_id = local.local_echo().unwrap().local_id.clone();

    f.server.restart().await.unwrap();
    let id = s.retry_send(&local_id).await.unwrap();
    assert_eq!(f.server.content_of(id).as_deref(), Some("断网时发"));
    wait_for(
        &mut rx,
        "确认",
        |e| matches!(e, SessionEvent::MessageConfirmed { id: got, .. } if *got == id),
    )
    .await;

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn read_state_round_trip() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::topic(DEV, "发布计划");
    let opened = s.open_conversation(&key);
    assert_eq!(opened.unread.ids.len(), 2);
    assert_eq!(opened.first_unread_id, opened.unread.ids.first().copied());
    assert_eq!(opened.mention_queue.len(), 1);
    let mut rx = s.subscribe();

    s.mark_conversation_read(&key).await.unwrap();
    assert_eq!(s.unread_count(&key), 0);
    for id in &opened.unread.ids {
        assert!(f.server.flags_of(*id).iter().any(|flag| flag == "read"));
    }

    s.mark_unread_from(&key, opened.unread.ids[0])
        .await
        .unwrap();
    wait_for(
        &mut rx,
        "恢复未读",
        |e| matches!(e, SessionEvent::UnreadChanged(keys) if keys.contains(&key)),
    )
    .await;
    eventually("未读恢复", || s.unread_count(&key) == 2).await;

    s.mark_read(&opened.unread.ids[..1]).await.unwrap();
    assert_eq!(s.unread_count(&key), 1);

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn reconnects_after_server_outage_without_losing_events() {
    let mut f = fixture().await;
    let s = &f.session;
    let mut rx = s.subscribe();

    f.server.stop().await;
    wait_for(&mut rx, "断线", |e| {
        *e == SessionEvent::Connection(ConnectionState::Reconnecting)
    })
    .await;
    assert_eq!(s.connection(), ConnectionState::Reconnecting);

    f.server.restart().await.unwrap();
    wait_for(&mut rx, "恢复", |e| {
        *e == SessionEvent::Connection(ConnectionState::Connected)
    })
    .await;

    let key = ConversationKey::dm([XIAOYU], ME_ID);
    let id = f
        .server
        .send_as(XIAOYU, Target::Dm(vec![XIAOYU, ME_ID]), "重连后");
    wait_for(
        &mut rx,
        "重连后的消息",
        |e| matches!(e, SessionEvent::UnreadChanged(keys) if keys.contains(&key)),
    )
    .await;
    assert_eq!(s.unread_count(&key), 1);
    assert!(s.message(id).is_none() || s.message(id).unwrap().id == id);

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn expired_queue_triggers_resync() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::topic(GAME, "吐槽");
    s.load_newest(&key).await.unwrap();
    let mut rx = s.subscribe();

    f.server.expire_queues();
    // 长轮询要结束才会发现失效：发一条消息唤醒它
    f.server
        .send_as(AJIE, Target::Stream(GAME, "吐槽".into()), "唤醒");
    wait_for(&mut rx, "重新同步", |e| *e == SessionEvent::Resynced).await;

    let id = f
        .server
        .send_as(XIAOYU, Target::Stream(GAME, "吐槽".into()), "同步后");
    wait_for(
        &mut rx,
        "同步后的消息",
        |e| matches!(e, SessionEvent::MessageAdded { id: got, .. } if *got == id),
    )
    .await;
    assert_eq!(s.timeline(&key).messages.last().unwrap().id, id);

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn logout_and_delete_remove_the_account() {
    let mut f = fixture().await;
    let id = f.session.account_id();
    f.hub.logout(id, false).await;
    assert!(f.hub.accounts().unwrap().is_empty());
    assert!(f.hub.session(id).is_none());
    assert_eq!(f.hub.active_account(), None);

    let again = f
        .hub
        .login_with_password(password_request(&f.server))
        .await
        .unwrap();
    assert_eq!(again.account_id(), id);
    f.hub.delete_account(id, true).await.unwrap();
    assert!(f.hub.accounts().unwrap().is_empty());
    f.server.stop().await;
}
