mod common;

use common::{eventually, fixture, wait_for};
use serde_json::json;
use test_server::{AJIE, DEV, GAME, LAOWANG, ME_ID, Target, XIAOYU};
use zulip_core::ConversationKey;
use zulip_core::html::UploadLink;
use zulip_core::model::RemarkTarget;
use zulip_session::{
    ContentFilter, DeleteTarget, MessagePager, MessageSearch, OpenAction, QueryScope, RenameTarget,
    SessionError, SessionEvent, TopicDestination, WidgetAction, group_by_date,
};

#[tokio::test]
async fn pagination_and_locate_share_the_timeline() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::topic(DEV, "长话题");
    for i in 0..120 {
        f.server.send_as(
            AJIE,
            Target::Stream(DEV, "长话题".into()),
            &format!("消息{i}"),
        );
    }
    s.load_newest(&key).await.unwrap();
    let first = s.timeline(&key);
    assert_eq!(first.messages.len(), 50);
    assert!(first.found_newest && !first.found_oldest);
    s.load_older(&key).await.unwrap();
    let second = s.timeline(&key);
    assert_eq!(second.messages.len(), 100);
    assert!(second.messages.windows(2).all(|w| w[0].id < w[1].id));

    // 定位到窗口外的消息
    let target = second.messages[0].id - 5;
    let located = s.locate_message(None, target).await.unwrap();
    assert_eq!((located.key, located.message_id), (key, target));
    assert!(s.message(target).is_some());

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn refresh_conversations_emits_list_change() {
    let mut f = fixture().await;
    let s = &f.session;
    let mut rx = s.subscribe();

    s.refresh_conversations();
    wait_for(&mut rx, "列表变化", |e| {
        *e == SessionEvent::ConversationsChanged
    })
    .await;

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn unified_query_serves_favorites_history_and_search() {
    let mut f = fixture().await;
    let s = &f.session;

    // 收藏
    let mut pager = MessagePager::new(MessageSearch {
        scope: QueryScope::Starred,
        ..MessageSearch::default()
    });
    let starred = s.next_page(&mut pager).await.unwrap();
    assert_eq!(starred.len(), 2);
    assert!(starred[0].message.id > starred[1].message.id);
    assert!(pager.is_done());

    // 聊天记录：按日期分组，分页直到结束
    let key = ConversationKey::topic(DEV, "构建失败");
    let mut pager = MessagePager::with_page_size(
        MessageSearch {
            scope: QueryScope::Conversation(key.clone()),
            filter: ContentFilter::Media,
            ..MessageSearch::default()
        },
        1,
    );
    let mut media = Vec::new();
    while !pager.is_done() {
        media.extend(s.next_page(&mut pager).await.unwrap());
    }
    assert_eq!(media.len(), 1);
    assert_eq!(group_by_date(&media)[0].items.len(), 1);
    assert_eq!(media[0].uploads[0].file_name, "截图.png");

    // 文件
    let mut pager = MessagePager::new(MessageSearch {
        filter: ContentFilter::Files,
        ..MessageSearch::default()
    });
    let items = s.next_page(&mut pager).await.unwrap();
    let files = s.file_entries(&items);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].upload.file_name, "周报.txt");
    assert!(files[0].source.starts_with("来自 阿杰 | "));

    assert_eq!(s.rank_file_senders().await.unwrap()[0], AJIE);

    // 全局搜索按会话分组
    f.server
        .send_as(XIAOYU, Target::Stream(GAME, "吐槽".into()), "火锅真好吃");
    let groups = s.search_message_groups("火锅").await.unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].key, ConversationKey::topic(GAME, "吐槽"));
    assert_eq!(groups[0].count_text, "1条相关聊天记录");

    assert_eq!(s.search_contacts("阿杰")[0].user_id, AJIE);
    assert!(!s.search_channels("开发").is_empty());

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn delete_has_one_shape_for_every_target() {
    let mut f = fixture().await;
    let s = &f.session;
    let mut rx = s.subscribe();

    // 消息
    let key = ConversationKey::topic(GAME, "吐槽");
    s.load_newest(&key).await.unwrap();
    let id = s.send_message(&key, "要删除的").await.unwrap();
    wait_for(
        &mut rx,
        "确认",
        |e| matches!(e, SessionEvent::MessageConfirmed { id: got, .. } if *got == id),
    )
    .await;
    s.delete(DeleteTarget::Message(id)).await.unwrap();
    wait_for(&mut rx, "删除事件", |e| {
        *e == SessionEvent::MessageUpdated {
            key: key.clone(),
            id,
        }
    })
    .await;
    assert!(s.message(id).unwrap().is_deleted());

    // 话题
    let topic = ConversationKey::topic(DEV, "代码评审");
    s.delete(DeleteTarget::Topic(topic.clone())).await.unwrap();
    assert!(!s.message_rows().iter().any(|r| r.key() == Some(&topic)));

    // 草稿
    let draft = ConversationKey::topic(GAME, "新游推荐");
    s.delete(DeleteTarget::Draft(draft.clone())).await.unwrap();
    assert!(s.draft(&draft).is_none());

    // 分组
    let group = s.add_friend_group("同事").await.unwrap();
    s.delete(DeleteTarget::FriendGroup(group.id)).await.unwrap();
    assert!(s.friend_groups().iter().all(|g| g.id != group.id));

    // 服务器上的对象：通过事件更新
    let snippet = s.create_snippet("标题", "内容").await.unwrap();
    wait_for(&mut rx, "片段", |e| *e == SessionEvent::SnippetsChanged).await;
    s.delete(DeleteTarget::Snippet(snippet)).await.unwrap();
    eventually("片段删除", || {
        s.snippets().iter().all(|x| x.id != snippet)
    })
    .await;

    // 失败时同一种错误
    let failure = s.delete(DeleteTarget::Message(987_654)).await;
    assert!(matches!(failure, Err(SessionError::Api(_))));

    // 从列表移除
    let entry = ConversationKey::dm([LAOWANG], ME_ID);
    s.delete(DeleteTarget::ConversationEntry(entry.clone()))
        .await
        .unwrap();
    assert!(!s.message_rows().iter().any(|r| r.key() == Some(&entry)));

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn rename_has_one_shape_for_every_target() {
    let mut f = fixture().await;
    let s = &f.session;
    let mut rx = s.subscribe();

    let old = ConversationKey::topic(DEV, "接口设计");
    let new = ConversationKey::topic(DEV, "接口方案");
    s.load_newest(&old).await.unwrap();
    s.rename(RenameTarget::Topic(old.clone()), "接口方案")
        .await
        .unwrap();
    wait_for(&mut rx, "改名", |e| {
        *e == SessionEvent::ConversationRekeyed {
            old: old.clone(),
            new: new.clone(),
        }
    })
    .await;
    s.load_newest(&new).await.unwrap();
    assert!(!s.timeline(&new).messages.is_empty());

    s.rename(RenameTarget::Channel(GAME), "游戏区")
        .await
        .unwrap();
    eventually("频道改名", || s.channel(GAME).unwrap().name == "游戏区").await;

    let group = s.add_friend_group("同事").await.unwrap();
    s.rename(RenameTarget::FriendGroup(group.id), "好友")
        .await
        .unwrap();
    assert_eq!(s.friend_groups()[0].name, "好友");

    s.rename(RenameTarget::Remark(RemarkTarget::User(AJIE)), "杰哥")
        .await
        .unwrap();
    assert_eq!(s.display_name(AJIE), "杰哥");
    assert_eq!(s.name_with_original(AJIE), "杰哥(阿杰)");
    s.rename(RenameTarget::Remark(RemarkTarget::User(AJIE)), "  ")
        .await
        .unwrap();
    assert_eq!(s.display_name(AJIE), "阿杰");

    let dm = ConversationKey::dm([XIAOYU, LAOWANG], ME_ID);
    s.rename(
        RenameTarget::Remark(RemarkTarget::Conversation(dm.clone())),
        "小群",
    )
    .await
    .unwrap();
    assert_eq!(s.display_title(&dm), "小群");

    assert!(matches!(
        s.rename(RenameTarget::FriendGroup(group.id), "  ").await,
        Err(SessionError::Invalid(_))
    ));

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn topic_move_time_limit_is_a_typed_result() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::topic(DEV, "发布计划");
    let to = TopicDestination {
        stream_id: GAME,
        topic: "挪过来".into(),
    };
    let mut rx = s.subscribe();

    let first = f
        .server
        .send_as(AJIE, Target::Stream(DEV, "发布计划".into()), "最近的一条");
    wait_for(
        &mut rx,
        "消息",
        |e| matches!(e, SessionEvent::MessageAdded { id, .. } if *id == first),
    )
    .await;
    f.server.set_move_limit(Some(first));
    let Err(SessionError::TopicMoveLimited {
        allowed,
        total,
        first_allowed_id,
    }) = s.move_topic(&key, &to).await
    else {
        panic!("应当是部分超限");
    };
    assert_eq!((allowed, first_allowed_id), (1, first));
    assert!(total > 1);

    s.move_topic_from(&key, first_allowed_id, &to)
        .await
        .unwrap();
    let moved = ConversationKey::topic(GAME, "挪过来");
    wait_for(&mut rx, "移动事件", |e| {
        matches!(e, SessionEvent::ConversationsChanged)
    })
    .await;
    s.load_newest(&moved).await.unwrap();
    assert_eq!(s.timeline(&moved).messages[0].id, first);
    f.server.set_move_limit(None);

    // 已解决
    let build = ConversationKey::topic(DEV, "构建失败");
    s.toggle_topic_resolved(&build).await.unwrap();
    eventually("已解决", || {
        s.message_rows()
            .iter()
            .any(|r| r.key() == Some(&ConversationKey::topic(DEV, "✔ 构建失败")))
    })
    .await;

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn message_operations_follow_server_events() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::dm([AJIE], ME_ID);
    s.load_newest(&key).await.unwrap();
    let mut rx = s.subscribe();
    let id = s.send_message(&key, "原文").await.unwrap();
    wait_for(
        &mut rx,
        "确认",
        |e| matches!(e, SessionEvent::MessageConfirmed { id: got, .. } if *got == id),
    )
    .await;
    assert!(s.message_actions(id).can_edit);
    assert_eq!(s.raw_content(id).await.unwrap(), "原文");

    s.edit_message(id, "改过").await.unwrap();
    wait_for(
        &mut rx,
        "编辑",
        |e| matches!(e, SessionEvent::MessageUpdated { id: got, .. } if *got == id),
    )
    .await;
    assert!(s.message(id).unwrap().content_html.contains("改过"));
    assert!(s.message_actions(id).can_show_edit_history);
    let previous = s.previous_contents(id).await.unwrap();
    assert_eq!(previous.len(), 1);
    assert!(previous[0].contains("原文"));

    s.set_starred(&[id], true).await.unwrap();
    wait_for(&mut rx, "星标", |e| {
        *e == SessionEvent::StarredChanged { id, starred: true }
    })
    .await;

    let thumbs = s.emoji_catalog().get("+1").cloned();
    if let Some(emoji) = thumbs {
        s.toggle_reaction(id, &emoji).await.unwrap();
        wait_for(
            &mut rx,
            "回应",
            |e| matches!(e, SessionEvent::MessageUpdated { id: got, .. } if *got == id),
        )
        .await;
        assert_eq!(s.message(id).unwrap().reactions.len(), 1);
        s.toggle_reaction(id, &emoji).await.unwrap();
        eventually("取消回应", || {
            s.message(id).unwrap().reactions.is_empty()
        })
        .await;
    }

    let target = ConversationKey::dm([XIAOYU], ME_ID);
    s.forward_each(&[id], std::slice::from_ref(&target))
        .await
        .unwrap();
    assert_eq!(
        f.server.content_of(f.server.latest_message_id()).as_deref(),
        Some("改过")
    );
    s.forward_merged(&[id], std::slice::from_ref(&target))
        .await
        .unwrap();
    let merged = f.server.content_of(f.server.latest_message_id()).unwrap();
    assert!(merged.contains("改过"));

    assert!(s.quote_message(id).await.unwrap().contains("改过"));

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn poll_vote_round_trip() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::topic(DEV, "接口设计");
    s.load_newest(&key).await.unwrap();
    let mut rx = s.subscribe();
    let id = s.send_message(&key, "/poll 投什么\n甲\n乙").await.unwrap();
    wait_for(
        &mut rx,
        "确认",
        |e| matches!(e, SessionEvent::MessageConfirmed { id: got, .. } if *got == id),
    )
    .await;
    s.widget_action(
        id,
        WidgetAction::AddPollOption {
            option: "丙".into(),
        },
    )
    .await
    .unwrap();
    eventually("新选项到达", || {
        s.message(id).is_some_and(|m| m.submessages.len() >= 2)
    })
    .await;
    assert!(matches!(
        s.widget_action(
            id,
            WidgetAction::AddPollOption {
                option: "丙".into()
            }
        )
        .await,
        Err(SessionError::Invalid(_))
    ));

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn draft_save_updates_server_and_settings_roll_back() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::dm([AJIE], ME_ID);
    s.save_draft(&key, "写一半").await.unwrap();
    assert_eq!(s.draft(&key).unwrap().content, "写一半");
    assert!(s.draft(&key).unwrap().server_id.is_some());
    s.save_draft(&key, "写一半，又改").await.unwrap();
    s.save_draft(&key, "").await.unwrap();
    assert!(s.draft(&key).is_none());

    s.set_zulip_setting("enter_sends", json!(false))
        .await
        .unwrap();
    assert!(!s.settings().enter_sends);
    f.server.stop().await;
    assert!(
        s.set_zulip_setting("enter_sends", json!(true))
            .await
            .is_err()
    );
    assert!(!s.settings().enter_sends);

    f.hub.shutdown().await;
}

#[tokio::test]
async fn images_and_files_download_with_auth_and_dedupe() {
    let mut f = fixture().await;
    let s = &f.session;
    let url = "/user_uploads/1/ab/截图.png";
    let (a, b) = tokio::join!(s.image_file(url), s.image_file(url));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a, b);
    assert!(std::fs::metadata(&a).unwrap().len() > 0);
    assert!(!s.image_bytes(url).await.unwrap().is_empty());

    let link = UploadLink {
        path: "/user_uploads/1/cd/周报.txt".to_owned(),
        file_name: "周报.txt".to_owned(),
        is_image: false,
    };
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("周报.txt");
    s.save_as(&link, &target, 7, "t:1:x", |_, _| {})
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "本周完成了登录模块。"
    );
    assert_eq!(s.downloaded_path(&link.path).await, Some(target.clone()));
    assert_eq!(
        s.open_downloaded(&link.path).await.unwrap(),
        OpenAction::Open(target.clone())
    );
    assert_eq!(s.recent_downloads(10).await.unwrap().len(), 1);
    s.delete(DeleteTarget::DownloadRecord(link.path.clone()))
        .await
        .unwrap();
    assert!(s.downloaded_path(&link.path).await.is_none());

    let cached = s.download_to_cache(&link.path, |_, _| {}).await.unwrap();
    assert!(cached.starts_with(f._dir.path()));
    assert!(cached.exists());

    let upload = dir.path().join("上传.txt");
    std::fs::write(&upload, "内容").unwrap();
    let result = s.upload_file(&upload, |_| {}).await.unwrap();
    assert!(result.url.contains("user_uploads"));

    let max_bytes = u64::try_from(s.realm().max_file_upload_size_mib).unwrap() * 1024 * 1024;
    let oversized = dir.path().join("超大.bin");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(max_bytes + 1)
        .unwrap();
    let error = s.upload_file(&oversized, |_| {}).await.unwrap_err();
    assert!(matches!(error, SessionError::Invalid(_)), "{error}");
    assert!(error.to_string().contains("MB"), "{error}");

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn favorites_keep_local_copies_of_uploads() {
    let mut f = fixture().await;
    let s = &f.session;
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("图.png");
    std::fs::write(&image, b"png bytes").unwrap();
    let file = dir.path().join("记录.txt");
    std::fs::write(&file, "收藏的文件").unwrap();
    let image_url = s.upload_file(&image, |_| {}).await.unwrap().url;
    let file_url = s.upload_file(&file, |_| {}).await.unwrap().url;

    let key = ConversationKey::topic(DEV, "收藏副本");
    s.load_newest(&key).await.unwrap();
    let mut rx = s.subscribe();
    let id = s
        .send_message(
            &key,
            &format!("[图.png]({image_url}) [记录.txt]({file_url})"),
        )
        .await
        .unwrap();
    wait_for(
        &mut rx,
        "确认",
        |e| matches!(e, SessionEvent::MessageConfirmed { id: got, .. } if *got == id),
    )
    .await;

    async fn copy_ready(s: &zulip_session::Session, url: &str, want: bool) {
        for _ in 0..300 {
            if s.favorite_copy(url).await.is_some() == want {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("等待副本 {url}：{want}");
    }

    s.set_starred(&[id], true).await.unwrap();
    copy_ready(s, &image_url, true).await;
    copy_ready(s, &file_url, true).await;

    // 服务器上删除后，图片和文件改用副本；缩略图地址取原图的副本
    for attachment in s.attachments().await.unwrap().attachments {
        s.delete(DeleteTarget::Attachment(attachment.id))
            .await
            .unwrap();
    }
    let shown = s.image_file(&image_url).await.unwrap();
    assert_eq!(std::fs::read(&shown).unwrap(), b"png bytes");
    let thumbnail = format!(
        "{}/840x560.webp",
        image_url.replacen("/user_uploads/", "/user_uploads/thumbnail/", 1)
    );
    assert_eq!(s.image_file(&thumbnail).await.unwrap(), shown);
    let opened = s.download_to_cache(&file_url, |_, _| {}).await.unwrap();
    assert_eq!(std::fs::read_to_string(opened).unwrap(), "收藏的文件");

    s.set_starred(&[id], false).await.unwrap();
    copy_ready(s, &image_url, false).await;
    assert!(s.image_file(&image_url).await.is_err());

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn sending_clears_the_draft() {
    let mut f = fixture().await;
    let s = &f.session;
    let key = ConversationKey::topic(GAME, "新游推荐");
    assert!(s.draft(&key).is_some());
    s.send_message(&key, "推荐完了").await.unwrap();
    assert!(s.draft(&key).is_none());

    f.hub.shutdown().await;
    f.server.stop().await;
}

// 界面用 tokio::spawn 调用这些操作
#[tokio::test]
async fn operations_can_be_spawned() {
    fn is_send<T: Send>(_: T) {}
    let mut f = fixture().await;
    let s = f.session.clone();
    let key = ConversationKey::topic(GAME, "吐槽");
    is_send(s.send_message(&key, "x"));
    is_send(s.save_draft(&key, "x"));
    is_send(s.delete(DeleteTarget::Topic(key.clone())));
    is_send(s.rename(RenameTarget::Topic(key.clone()), "y"));
    is_send(s.forward_merged(&[], &[]));
    is_send(s.mark_conversation_read(&key));
    let mut pager = MessagePager::new(MessageSearch::default());
    is_send(s.next_page(&mut pager));
    is_send(s.search_message_groups("x"));
    is_send(s.locate_message(None, 1));
    is_send(s.upload_file(std::path::Path::new("x"), |_| {}));
    is_send(s.save_as(
        &UploadLink {
            path: String::new(),
            file_name: String::new(),
            is_image: false,
        },
        std::path::Path::new("x"),
        1,
        "k",
        |_, _| {},
    ));
    is_send(f.hub.login_account(1));
    is_send(f.hub.logout(1, false));
    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn remark_and_pin_share_one_entry_point() {
    use zulip_core::model::{PinItem, PinScope};

    let mut f = fixture().await;
    let s = f.session.clone();
    let user = RemarkTarget::User(AJIE);
    s.rename(RenameTarget::Remark(user.clone()), "阿杰")
        .await
        .unwrap();
    assert_eq!(s.remark(&user).as_deref(), Some("阿杰"));
    // 一对一私聊和这个人共用一条备注
    let dm = RemarkTarget::Conversation(ConversationKey::dm([AJIE], ME_ID));
    assert_eq!(s.remark(&dm).as_deref(), Some("阿杰"));
    s.rename(RenameTarget::Remark(dm), "").await.unwrap();
    assert_eq!(s.remark(&user), None);

    let item = PinItem::User(AJIE);
    assert!(!s.is_pinned(PinScope::Contacts, &item));
    s.set_pinned(PinScope::Contacts, &item, true);
    assert!(s.is_pinned(PinScope::Contacts, &item));
    assert!(!s.is_pinned(PinScope::Messages, &item));
    s.set_pinned(PinScope::Contacts, &item, false);
    assert!(!s.is_pinned(PinScope::Contacts, &item));

    f.hub.shutdown().await;
    f.server.stop().await;
}

#[tokio::test]
async fn message_references_keep_what_loaded_and_count_failures() {
    let mut f = fixture().await;
    let loaded = f.server.latest_message_id();
    let failing = loaded - 1;
    f.server.fail_message(failing);

    let result = f.session.message_references(&[loaded, failing]).await;

    assert_eq!(result.failed, 1);
    let ids: Vec<i64> = result.references.iter().map(|r| r.message_id).collect();
    assert_eq!(ids, [loaded]);

    f.hub.shutdown().await;
    f.server.stop().await;
}
