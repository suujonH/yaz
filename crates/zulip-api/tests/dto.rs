use zulip_api::{AttachmentDto, GetMessagesResult, RegisterResult, SubscriptionDto};
use zulip_core::model::Attachment;
use zulip_core::permission::GroupSettingValue;

fn register() -> RegisterResult {
    serde_json::from_str(include_str!("fixtures/register.json")).unwrap()
}

#[test]
fn register_deserializes() {
    let reg = register();

    assert_eq!(reg.queue_id, "REDACTED");
    assert!(reg.zulip_feature_level >= 421);
    assert_eq!(reg.user_id, 8);
    assert_eq!(reg.self_role(), 100);
    assert!(!reg.realm_users.is_empty());
    assert!(
        reg.realm_users
            .iter()
            .any(|u| u.user_id == 9 && !u.is_bot && u.is_active)
    );
    assert!(reg.cross_realm_bots.iter().any(|u| u.is_bot));

    let dev: Vec<&SubscriptionDto> = reg
        .subscriptions
        .iter()
        .filter(|s| s.stream_id == 4)
        .collect();
    assert_eq!(dev.len(), 1);
    assert!(!dev[0].name.is_empty());
    assert!(dev[0].color.starts_with('#'));
    assert!(dev[0].can_send_message_group.is_some());
    assert!(dev[0].can_administer_channel_group.is_some());

    assert!(reg.unread_msgs.count > 0);
    assert!(!reg.unread_msgs.streams.is_empty());
    assert!(!reg.unread_msgs.pms.is_empty());
    assert!(!reg.unread_msgs.mentions.is_empty());
    assert!(!reg.user_topics.is_empty());
    assert!(!reg.drafts.is_empty());
    assert!(!reg.starred_messages.is_empty());
    assert!(!reg.realm_user_groups.is_empty());
    assert!(
        reg.recent_private_conversations
            .iter()
            .any(|c| c.user_ids.is_empty())
    );
    assert!(reg.user_settings.enable_drafts_synchronization);
    assert!(reg.user_settings.enable_offline_push_notifications);
    assert!(!reg.realm_push_notifications_enabled);
    assert_eq!(reg.realm_new_stream_announcements_stream_id, 3);
    assert_eq!(reg.realm_signup_announcements_stream_id, -1);
    assert!(reg.reminders.is_empty());
    assert_eq!(reg.realm_message_content_edit_limit_seconds, Some(600));
    assert_eq!(reg.realm_message_edit_history_visibility_policy, "all");
    assert!(
        reg.server_emoji_data_url
            .as_deref()
            .is_some_and(|u| !u.is_empty())
    );
    assert_eq!(reg.event_queue_longpoll_timeout_seconds, 90);
}

#[test]
fn register_draft_and_topics_have_expected_shape() {
    let reg = register();
    let draft = &reg.drafts[0];
    assert_eq!(
        (draft.kind.as_str(), draft.to.as_slice()),
        ("stream", [5].as_slice())
    );
    assert!(draft.id.is_some() && draft.timestamp.is_some());
    assert!(
        reg.user_topics
            .iter()
            .any(|t| t.visibility_policy == 3 && !t.topic_name.is_empty())
    );
}

#[test]
fn register_realm_group_settings_use_server_supported_names() {
    let reg = register();
    let settings = reg.realm_group_settings();

    // Setting names not ending in _group must also be picked up
    for name in [
        "can_create_groups",
        "can_manage_all_groups",
        "direct_message_permission_group",
        "can_mention_many_users_group",
    ] {
        assert!(settings.contains_key(name), "{name}");
    }

    let allow = reg.allow_everyone_group();
    assert!(!allow[&("realm".to_owned(), "can_add_custom_emoji_group".to_owned())]);
    assert!(allow[&("stream".to_owned(), "can_send_message_group".to_owned())]);
    assert!(
        reg.permission_setting_names("stream")
            .iter()
            .any(|n| n == "can_send_message_group")
    );

    let infos = reg.permission_setting_infos();
    let info = &infos[&("realm".to_owned(), "can_add_custom_emoji_group".to_owned())];
    assert!(!info.allow_everyone);
    assert!(infos.len() >= allow.len());
}

#[test]
fn messages_topic_deserializes() {
    let result: GetMessagesResult =
        serde_json::from_str(include_str!("fixtures/messages_topic.json")).unwrap();
    assert!(!result.messages.is_empty());
    assert!(result.found_newest);
    let m = result.messages.last().unwrap();
    assert!(m.is_stream());
    assert_eq!(m.stream_id, Some(4));
    assert!(!m.subject.is_empty());
    assert!(m.recipient_user_ids().is_empty());
}

#[test]
fn messages_dm_deserializes() {
    let result: GetMessagesResult =
        serde_json::from_str(include_str!("fixtures/messages_dm.json")).unwrap();
    let m = &result.messages[0];
    assert!(!m.is_stream());
    let mut ids = m.recipient_user_ids();
    ids.sort_unstable();
    assert_eq!(ids, [8, 9]);
    assert!(m.flags.iter().any(|f| f == "read"));
}

#[test]
fn group_setting_value_reads_both_forms() {
    let number: GroupSettingValue = serde_json::from_str("12").unwrap();
    assert_eq!(number, GroupSettingValue::Group(12));
    let anonymous: GroupSettingValue =
        serde_json::from_str(r#"{"direct_members":[1,2],"direct_subgroups":[3]}"#).unwrap();
    assert_eq!(anonymous, GroupSettingValue::anonymous([2, 1], [3]));
}

#[test]
fn nulls_in_text_fields_become_defaults() {
    let sub: SubscriptionDto =
        serde_json::from_str(r#"{"stream_id":1,"name":"a","description":null,"color":null,"desktop_notifications":null}"#).unwrap();
    assert_eq!(
        (sub.stream_id, sub.description.as_str(), sub.color.as_str()),
        (1, "", "")
    );
    assert!(sub.history_public_to_subscribers);
}

#[test]
fn integer_keyed_maps_and_extra_fields() {
    let reg: RegisterResult = serde_json::from_str(
        r#"{"presences":{"9":{"active_timestamp":1792624241,"idle_timestamp":1792624000.5}},
            "user_status":{"9":{"status_text":"忙"}},"realm_unknown_setting":{"a":1}}"#,
    )
    .unwrap();
    assert_eq!(reg.presences[&9].active_timestamp, 1_792_624_241);
    assert_eq!(reg.presences[&9].idle_timestamp, 1_792_624_000);
    assert_eq!(reg.user_status[&9].status_text.as_deref(), Some("忙"));
    assert_eq!(reg.extra["realm_unknown_setting"]["a"], 1);
    assert_eq!(
        (
            reg.max_topic_length,
            reg.presence_last_update_id,
            reg.event_queue_longpoll_timeout_seconds
        ),
        (60, -1, 90)
    );
    assert!(serde_json::from_str::<RegisterResult>(r#"{"presences":{"x":{}}}"#).is_err());
}

#[test]
fn attachment_formats() {
    let new: AttachmentDto =
        serde_json::from_str(r#"{"id":1,"create_time":1700000000,"message_ids":[4,5]}"#).unwrap();
    let new = Attachment::from(new);
    assert_eq!(
        (new.created_at, new.message_ids),
        (1_700_000_000, vec![4, 5])
    );
    let old: AttachmentDto = serde_json::from_str(
        r#"{"id":1,"create_time":1700000000123,"messages":[{"id":7,"date_sent":1700000000123}]}"#,
    )
    .unwrap();
    let old = Attachment::from(old);
    assert_eq!((old.created_at, old.message_ids), (1_700_000_000, vec![7]));
}
