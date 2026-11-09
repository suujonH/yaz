//! Whether to notify, and what the notification contains. Popups and sounds are handled by the app.

use zulip_core::html;
use zulip_core::model::ChatMessage;
use zulip_core::notice::SystemNoticeKind;
use zulip_core::push::DndLevel;
use zulip_core::{ConversationFolder, ConversationKey, ListEntry};

use crate::event::{NotificationAvatar, NotificationRequest};
use crate::state::State;

pub const GROUP_AVATAR_TEXT: &str = "群";
pub const GROUP_AVATAR_COLOR: &str = "#0052D9";

/// Decision result: `(show popup, play sound)`.
type Decision = (bool, bool);

fn decide_mention(state: &State, m: &ChatMessage) -> Option<Decision> {
    let z = &state.settings;
    (m.flags.mentioned
        || m.flags.has_alert_word
        || (m.flags.wildcard_mentioned && state.account.wildcard_mentions_notify()))
    .then_some((z.enable_desktop_notifications, z.enable_sounds))
}

// For DMs the master switch decides; topic mentions and channel settings (the global setting when
// unset) are independent triggers, and any one of them is enough to notify
fn decide(state: &State, m: &ChatMessage) -> Decision {
    let z = &state.settings;
    let ConversationKey::Topic(t) = &m.key else {
        return (z.enable_desktop_notifications, z.enable_sounds);
    };
    let channel = state.channels.get(t.stream_id);
    let mut popup = channel
        .and_then(|c| c.subscription.as_ref()?.desktop_notifications)
        .unwrap_or(z.enable_stream_desktop_notifications);
    let mut sound = channel
        .and_then(|c| c.subscription.as_ref()?.audible_notifications)
        .unwrap_or(z.enable_stream_audible_notifications);
    if let Some((mention_popup, mention_sound)) = decide_mention(state, m) {
        popup |= mention_popup;
        sound |= mention_sound;
    }
    (popup, sound)
}

/// Whether a new message notifies. No notification for system notices, own messages, read messages,
/// messages intercepted by a plugin, blocked senders, blocked conversations, or when being viewed
/// in the foreground; do-not-disturb conversations only notify for @ mentions and alert words, and
/// share the Topic Assistant notification slot.
pub fn on_new_message(state: &State, m: &ChatMessage) -> Option<NotificationRequest> {
    if m.notice != SystemNoticeKind::None
        || m.sender_id == state.users.self_id
        || m.is_read()
        || state.messages.is_hidden(m)
        || state.users.is_muted(m.sender_id)
        || state.view.is_actively_viewing(&m.key)
    {
        return None;
    }
    let level = state.conversations.effective_level(&m.key);
    if level == DndLevel::Blocked {
        return None;
    }
    let muted = level != DndLevel::Notify;
    let (popup, sound) = if muted {
        decide_mention(state, m)?
    } else {
        decide(state, m)
    };
    let silenced = state.local.notification_muted();
    let slot = if muted {
        ListEntry::Folder(ConversationFolder::Helper)
    } else {
        ListEntry::Conversation(m.key.clone())
    };
    Some(NotificationRequest {
        key: m.key.clone(),
        slot,
        message_id: Some(m.id).filter(|id| *id > 0),
        title: state.title(&m.key),
        body: body_of(state, m),
        avatar: avatar_of(state, m),
        show_popup: popup && !silenced,
        play_sound: sound && !silenced && state.settings.notification_sound != "none",
        is_reminder: false,
    })
}

fn body_of(state: &State, m: &ChatMessage) -> String {
    if matches!(m.key, ConversationKey::Dm(_))
        && !state.settings.pm_content_in_desktop_notifications
    {
        return "你收到了一条新消息".to_owned();
    }
    let preview = html::to_preview(&m.content_html);
    if matches!(&m.key, ConversationKey::Dm(d) if !d.is_group_dm()) {
        preview
    } else {
        format!("{}：{preview}", state.display_name(m.sender_id))
    }
}

fn avatar_of(state: &State, m: &ChatMessage) -> NotificationAvatar {
    match &m.key {
        ConversationKey::Topic(t) => NotificationAvatar::Text {
            text: state.title(&m.key),
            color: state
                .channels
                .get(t.stream_id)
                .map(|c| c.color.clone())
                .unwrap_or_default(),
        },
        ConversationKey::Dm(d) if d.is_group_dm() => NotificationAvatar::Text {
            text: GROUP_AVATAR_TEXT.to_owned(),
            color: GROUP_AVATAR_COLOR.to_owned(),
        },
        _ => match state
            .users
            .get(m.sender_id)
            .and_then(|u| u.avatar_url.clone())
            .or_else(|| m.avatar_url.clone())
        {
            Some(url) => NotificationAvatar::Image(url),
            None => NotificationAvatar::Text {
                text: state.title(&m.key),
                color: String::new(),
            },
        },
    }
}

/// A message reminder is due: not affected by the conversation's do-not-disturb; uses the DM
/// notification settings.
pub fn on_reminder(state: &State, m: &ChatMessage) -> NotificationRequest {
    let z = &state.settings;
    let silenced = state.local.notification_muted();
    NotificationRequest {
        key: m.key.clone(),
        slot: ListEntry::Conversation(m.key.clone()),
        message_id: None,
        title: "消息提醒".to_owned(),
        body: "你设置的消息提醒到时间了".to_owned(),
        avatar: NotificationAvatar::Text {
            text: "提醒".to_owned(),
            color: String::new(),
        },
        show_popup: z.enable_desktop_notifications && !silenced,
        play_sound: z.enable_sounds && !silenced && z.notification_sound != "none",
        is_reminder: true,
    }
}
