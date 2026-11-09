//! 接口数据结构 → 领域模型。

use std::collections::HashMap;

use zulip_api::{MessageDto, RegisterResult, StreamDto, SubscriptionDto, UserDto};
use zulip_core::ConversationKey;
use zulip_core::model::{Channel, ChatMessage, MessageFlags, RealmInfo, Subscription, ZulipUser};
use zulip_core::permission::{
    ChannelGroupSettings, PermissionInputs, PermissionSettingInfo, SettingScope, UserGroup,
};

pub fn key_of_message(dto: &MessageDto, self_id: i64) -> ConversationKey {
    if dto.is_stream() {
        ConversationKey::topic(dto.stream_id.unwrap_or_default(), dto.subject.clone())
    } else {
        ConversationKey::dm(dto.recipient_user_ids(), self_id)
    }
}

pub fn message_from_dto(dto: &MessageDto, self_id: i64) -> ChatMessage {
    let mut message = ChatMessage::new(
        dto.id,
        key_of_message(dto, self_id),
        dto.sender_id,
        dto.timestamp,
    );
    message.sender_name.clone_from(&dto.sender_full_name);
    message.sender_email.clone_from(&dto.sender_email);
    message.avatar_url.clone_from(&dto.avatar_url);
    message.content_html.clone_from(&dto.content);
    message.flags = MessageFlags::from_names(&dto.flags);
    message.reactions.clone_from(&dto.reactions);
    message.last_edit_timestamp = dto.last_edit_timestamp;
    message.is_me_message = dto.is_me_message;
    if dto.is_stream() {
        message.topic.clone_from(&dto.subject);
        message.stream_id = dto.stream_id;
    }
    message.submessages.clone_from(&dto.submessages);
    message.submessages.sort_by_key(|s| s.id);
    message
}

pub fn user_from_dto(dto: &UserDto) -> ZulipUser {
    ZulipUser {
        id: dto.user_id,
        email: dto.email.clone(),
        delivery_email: dto.delivery_email.clone(),
        full_name: dto.full_name.clone(),
        avatar_url: dto.avatar_url.clone(),
        is_bot: dto.is_bot,
        bot_type: dto.bot_type,
        bot_owner_id: dto.bot_owner_id,
        role: dto.role,
        is_active: dto.is_active,
        timezone: dto.timezone.clone(),
        profile_data: dto.profile_data.clone().unwrap_or_default(),
    }
}

pub fn channel_from_stream(dto: &StreamDto) -> Channel {
    Channel {
        stream_id: dto.stream_id,
        name: dto.name.clone(),
        description: dto.description.clone(),
        rendered_description: dto.rendered_description.clone(),
        invite_only: dto.invite_only,
        is_web_public: dto.is_web_public,
        is_archived: dto.is_archived,
        history_public_to_subscribers: dto.history_public_to_subscribers,
        subscriber_count: dto.subscriber_count,
        creator_id: dto.creator_id,
        date_created: dto.date_created,
        folder_id: dto.folder_id,
        topics_policy: dto.topics_policy.to_lowercase(),
        permissions: ChannelGroupSettings {
            can_administer_channel_group: dto.can_administer_channel_group.clone(),
            can_add_subscribers_group: dto.can_add_subscribers_group.clone(),
            can_remove_subscribers_group: dto.can_remove_subscribers_group.clone(),
            can_send_message_group: dto.can_send_message_group.clone(),
            can_move_messages_out_of_channel_group: dto
                .can_move_messages_out_of_channel_group
                .clone(),
            can_move_messages_within_channel_group: dto
                .can_move_messages_within_channel_group
                .clone(),
            can_resolve_topics_group: dto.can_resolve_topics_group.clone(),
            can_create_topic_group: dto.can_create_topic_group.clone(),
            can_delete_any_message_group: dto.can_delete_any_message_group.clone(),
            can_delete_own_message_group: dto.can_delete_own_message_group.clone(),
            can_subscribe_group: dto.can_subscribe_group.clone(),
        },
        ..Channel::default()
    }
}

/// `subscribed` 为 false：已退订的频道，只保留颜色。
pub fn channel_from_subscription(dto: &SubscriptionDto, subscribed: bool) -> Channel {
    Channel {
        color: dto.color.clone(),
        subscription: subscribed.then_some(Subscription {
            is_muted: dto.is_muted,
            desktop_notifications: dto.desktop_notifications,
            audible_notifications: dto.audible_notifications,
            push_notifications: dto.push_notifications,
            email_notifications: dto.email_notifications,
            wildcard_mentions_notify: dto.wildcard_mentions_notify,
        }),
        ..channel_from_stream(&dto.stream)
    }
}

fn announcement_channel(id: i64) -> Option<i64> {
    (id > 0).then_some(id)
}

pub fn realm_from_register(reg: &RegisterResult, url: &str) -> RealmInfo {
    RealmInfo {
        url: url.trim_end_matches('/').to_owned(),
        feature_level: reg.zulip_feature_level,
        zulip_version: reg.zulip_version.clone(),
        name: reg.realm_name.clone(),
        icon_url: reg.realm_icon_url.clone(),
        max_message_length: reg.max_message_length,
        max_topic_length: reg.max_topic_length,
        max_file_upload_size_mib: reg.max_file_upload_size_mib,
        upload_quota_mib: reg.realm_upload_quota_mib,
        empty_topic_display_name: reg.realm_empty_topic_display_name.clone(),
        server_emoji_data_url: reg.server_emoji_data_url.clone(),
        allow_message_editing: reg.realm_allow_message_editing,
        push_notifications_enabled: reg.realm_push_notifications_enabled,
        new_channels_announcements_channel_id: announcement_channel(
            reg.realm_new_stream_announcements_stream_id,
        ),
        signup_announcements_channel_id: announcement_channel(
            reg.realm_signup_announcements_stream_id,
        ),
        zulip_update_announcements_channel_id: announcement_channel(
            reg.realm_zulip_update_announcements_stream_id,
        ),
        moderation_request_channel_id: announcement_channel(
            reg.realm_moderation_request_channel_id,
        ),
        edit_limit_seconds: reg.realm_message_content_edit_limit_seconds.map(i64::from),
        delete_limit_seconds: reg
            .realm_message_content_delete_limit_seconds
            .map(i64::from),
        enable_read_receipts: reg.realm_enable_read_receipts,
        edit_history_visibility_policy: reg.realm_message_edit_history_visibility_policy.clone(),
        topics_policy: reg.realm_topics_policy.to_lowercase(),
        profile_fields: reg.custom_profile_fields.clone(),
        longpoll_timeout_seconds: reg.event_queue_longpoll_timeout_seconds as i32,
        typing_started_wait_ms: reg.server_typing_started_wait_period_milliseconds as i32,
        typing_stopped_wait_ms: reg.server_typing_stopped_wait_period_milliseconds as i32,
        typing_started_expiry_ms: reg.server_typing_started_expiry_period_milliseconds as i32,
        presence_ping_interval_seconds: reg.server_presence_ping_interval_seconds as i32,
        presence_offline_threshold_seconds: reg.server_presence_offline_threshold_seconds as i32,
    }
}

/// 频道权限设置（`server_supported_permission_settings.stream`）可以设置的组的限制。
pub fn channel_permission_infos(reg: &RegisterResult) -> HashMap<String, PermissionSettingInfo> {
    reg.permission_setting_infos()
        .into_iter()
        .filter(|((scope, _), _)| scope == "stream")
        .map(|((_, name), info)| {
            let info = PermissionSettingInfo {
                allow_nobody: info.allow_nobody,
                allow_everyone: info.allow_everyone,
                allowed_system_groups: info.allowed_system_groups,
            };
            (name, info)
        })
        .collect()
}

pub fn permission_inputs(
    reg: &RegisterResult,
    groups: HashMap<i64, UserGroup>,
) -> PermissionInputs {
    let allow_everyone_group = reg
        .allow_everyone_group()
        .into_iter()
        .filter_map(|((scope, name), allowed)| {
            let scope = match scope.as_str() {
                "realm" => SettingScope::Realm,
                "stream" => SettingScope::Stream,
                "group" => SettingScope::Group,
                _ => return None,
            };
            Some(((scope, name), allowed))
        })
        .collect();
    PermissionInputs {
        self_id: reg.user_id,
        role: reg.self_role(),
        groups,
        realm_group_settings: reg.realm_group_settings(),
        allow_everyone_group,
        allow_message_editing: reg.realm_allow_message_editing,
        edit_limit_seconds: reg.realm_message_content_edit_limit_seconds.map(i64::from),
        delete_limit_seconds: reg
            .realm_message_content_delete_limit_seconds
            .map(i64::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dto(value: serde_json::Value) -> MessageDto {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn stream_message_key_and_fields() {
        let m = dto(json!({
            "id": 5, "sender_id": 2, "sender_full_name": "阿杰", "type": "stream",
            "stream_id": 3, "subject": "Topic", "content": "<p>x</p>", "timestamp": 9,
            "flags": ["read"], "reactions": [{"emoji_name": "a", "emoji_code": "b", "reaction_type": "c", "user_id": 1}],
            "edit_history": [{"timestamp": 8, "prev_rendered_content": "<p>2</p>"}, {"timestamp": 7, "prev_rendered_content": "<p>1</p>"}, {"timestamp": 6}],
        }));
        let m = message_from_dto(&m, 1);
        assert_eq!(m.key, ConversationKey::topic(3, "topic"));
        assert_eq!(m.topic, "Topic");
        assert!(m.is_read());
        assert_eq!(m.reactions[0].user_id, 1);
    }

    #[test]
    fn private_message_keys_exclude_self() {
        let dm =
            dto(json!({"id": 1, "type": "private", "display_recipient": [{"id": 1}, {"id": 2}]}));
        assert_eq!(key_of_message(&dm, 1), ConversationKey::dm([2], 1));
        let own = dto(json!({"id": 1, "type": "private", "display_recipient": [{"id": 1}]}));
        assert_eq!(key_of_message(&own, 1), ConversationKey::dm([1], 1));
    }
}
