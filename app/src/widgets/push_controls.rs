//! Enhanced-push controls for channels and topics: shared by the "Enhanced push" settings page and
//! the contacts page. All changes are submitted to Zulip immediately.

use super::SessionRef;
use crate::services::show_unavailable;
use crate::ui::*;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use serde_json::json;
use std::sync::Arc;
use zulip_core::ConversationKey;
use zulip_core::model::{Channel, UserSettings};
use zulip_core::push::{PushRules, PushTriggers, TopicVisibility};
use zulip_session::{Session, SessionEvent};

/// Mobile push is available, or, in developer mode, show features that are unavailable.
pub fn push_available(session: &Session) -> bool {
    session.realm().push_notifications_enabled || show_unavailable()
}

/// Events that affect the push controls.
pub fn push_changed(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ChannelChanged(_)
            | SessionEvent::ChannelsChanged
            | SessionEvent::UserTopicChanged { .. }
            | SessionEvent::SettingsChanged(_)
            | SessionEvent::RealmChanged
    )
}

fn email_rules(s: &UserSettings) -> PushRules {
    PushRules {
        dm_and_mention: s.enable_offline_email_notifications,
        stream: s.enable_stream_email_notifications,
        followed_topic: s.enable_followed_topic_email_notifications,
        wildcard_mentions: s.wildcard_mentions_notify,
        followed_topic_wildcard_mentions: s.enable_followed_topic_wildcard_mentions_notify,
    }
}

fn phone_rules(s: &UserSettings) -> PushRules {
    PushRules {
        dm_and_mention: s.enable_offline_push_notifications,
        stream: s.enable_stream_push_notifications,
        followed_topic: s.enable_followed_topic_push_notifications,
        wildcard_mentions: s.wildcard_mentions_notify,
        followed_topic_wildcard_mentions: s.enable_followed_topic_wildcard_mentions_notify,
    }
}

/// Effect of one notification route (mobile push or email).
#[derive(Clone, Debug, PartialEq)]
pub struct PushEffect {
    pub icon: IconName,
    /// "Mobile push: mentions, all messages".
    pub text: String,
    pub triggers: PushTriggers,
}

/// Mobile push and email that a topic (or a topic with no separate setting) will trigger, computed
/// by the server's rules.
fn effects(
    session: &Session,
    channel: &Channel,
    topic: TopicVisibility,
    phone: bool,
) -> Vec<PushEffect> {
    let settings = session.settings();
    let sub = channel.subscription.clone().unwrap_or_default();
    let line = |icon, label: &str, rules: PushRules, all: Option<bool>| {
        let triggers = rules.evaluate(sub.is_muted, all, sub.wildcard_mentions_notify, topic);
        PushEffect {
            icon,
            text: format!("{label}：{triggers}"),
            triggers,
        }
    };
    let mut lines = Vec::new();
    if phone {
        lines.push(line(
            IconName::Mobile,
            "手机推送",
            phone_rules(&settings),
            sub.push_notifications,
        ));
    }
    lines.push(line(
        IconName::Mail,
        "邮件",
        email_rules(&settings),
        sub.email_notifications,
    ));
    lines
}

/// Push state of a channel.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelPush {
    pub effects: Vec<PushEffect>,
    muted: bool,
    phone: bool,
    push_all: bool,
    email_all: bool,
    wildcard: bool,
    editable: bool,
}

pub fn channel_push(session: &Session, stream_id: i64, phone: bool) -> Option<ChannelPush> {
    let channel = session.channel(stream_id)?;
    let settings = session.settings();
    let sub = channel.subscription.clone().unwrap_or_default();
    Some(ChannelPush {
        effects: effects(session, &channel, TopicVisibility::Inherit, phone),
        muted: sub.is_muted,
        phone,
        push_all: sub
            .push_notifications
            .unwrap_or(settings.enable_stream_push_notifications),
        email_all: sub
            .email_notifications
            .unwrap_or(settings.enable_stream_email_notifications),
        wildcard: sub
            .wildcard_mentions_notify
            .unwrap_or(settings.wildcard_mentions_notify),
        editable: !channel.is_archived,
    })
}

/// Push state of a topic.
#[derive(Clone, Debug, PartialEq)]
pub struct TopicPush {
    pub effects: Vec<PushEffect>,
    policy: TopicVisibility,
    channel_muted: bool,
    editable: bool,
}

pub fn topic_push(
    session: &Session,
    stream_id: i64,
    topic: &str,
    phone: bool,
) -> Option<TopicPush> {
    let channel = session.channel(stream_id)?;
    let policy = session.topic_visibility(stream_id, topic);
    Some(TopicPush {
        effects: effects(session, &channel, policy, phone),
        policy,
        channel_muted: channel.is_muted(),
        editable: !channel.is_archived,
    })
}

/// Icon toggle at the end of a row: highlighted means on.
#[component]
fn Flag(
    icon: IconName,
    #[props(into)] tip: String,
    on: bool,
    #[props(default)] disabled: bool,
    onclick: EventHandler<()>,
) -> Element {
    rsx! {
        Tooltip { text: tip,
            IconButton {
                icon,
                size: 28.0,
                disabled,
                class: if on { "push-flag on" } else { "push-flag" },
                onclick: move |_| onclick.call(()),
            }
        }
    }
}

async fn set_subscription(
    session: Arc<Session>,
    overlays: Overlays,
    stream_id: i64,
    property: &'static str,
    on: bool,
) {
    overlays
        .attempt(
            "设置失败",
            session.set_subscription_property(stream_id, property, json!(on)),
        )
        .await;
}

/// Channel toggles: mute, mobile push, email, @all.
#[component]
pub fn ChannelPushFlags(session: SessionRef, stream_id: i64, data: ChannelPush) -> Element {
    let overlays = use_overlays();
    let editable = data.editable;
    let flag = |property: &'static str, on: bool| {
        let session = session.0.clone();
        move |()| {
            spawn_forever(set_subscription(
                session.clone(),
                overlays,
                stream_id,
                property,
                !on,
            ));
        }
    };
    rsx! {
        Flag { icon: IconName::NotificationOff, tip: "频道静音", on: data.muted, disabled: !editable, onclick: flag("is_muted", data.muted) }
        if data.phone {
            Flag { icon: IconName::Mobile, tip: "手机推送所有消息", on: data.push_all, disabled: !editable, onclick: flag("push_notifications", data.push_all) }
        }
        Flag { icon: IconName::Mail, tip: "邮件发送所有消息", on: data.email_all, disabled: !editable, onclick: flag("email_notifications", data.email_all) }
        Flag { icon: IconName::Mention, tip: "@全体成员时推送和发邮件", on: data.wildcard, disabled: !editable, onclick: flag("wildcard_mentions_notify", data.wildcard) }
    }
}

/// Topic toggles: follow channel, mute (unmute), follow; only one is highlighted at a time.
#[component]
pub fn TopicPushFlags(
    session: SessionRef,
    stream_id: i64,
    topic: String,
    data: TopicPush,
) -> Element {
    let overlays = use_overlays();
    let policy = data.policy;
    let editable = data.editable;
    let mute_set = matches!(policy, TopicVisibility::Muted | TopicVisibility::Unmuted);
    let unmute_option = policy == TopicVisibility::Unmuted
        || (policy != TopicVisibility::Muted && data.channel_muted);
    let set = |visibility: TopicVisibility| {
        let session = session.0.clone();
        let key = ConversationKey::topic(stream_id, topic.clone());
        move |()| {
            let (session, key) = (session.clone(), key.clone());
            spawn_forever(async move {
                overlays
                    .attempt("设置失败", session.set_topic_visibility(&key, visibility))
                    .await;
            });
        }
    };
    let inherit = TopicVisibility::Inherit;
    let toggle_mute = if mute_set {
        inherit
    } else if data.channel_muted {
        TopicVisibility::Unmuted
    } else {
        TopicVisibility::Muted
    };
    let toggle_follow = if policy == TopicVisibility::Followed {
        inherit
    } else {
        TopicVisibility::Followed
    };
    rsx! {
        Flag { icon: IconName::Link, tip: "跟随频道", on: policy == inherit, disabled: !editable, onclick: set(inherit) }
        Flag {
            icon: if unmute_option { IconName::Notification } else { IconName::NotificationOff },
            tip: if unmute_option { "取消静音（频道静音时照常通知）" } else { "静音" },
            on: mute_set,
            disabled: !editable,
            onclick: set(toggle_mute),
        }
        Flag { icon: IconName::Star, tip: "关注", on: policy == TopicVisibility::Followed, disabled: !editable, onclick: set(toggle_follow) }
    }
}

/// Push effect under the name: mobile and email laid out in one row. The row start is the
/// notification method; the icons after it are the situations that will send.
#[component]
pub fn PushEffectIcons(effects: Vec<PushEffect>) -> Element {
    rsx! {
        div { class: "push-effects",
            for effect in effects {
                Tooltip { key: "{effect.text}", text: effect.text.clone(),
                    div {
                        class: if effect.triggers == PushTriggers::default() { "push-effect" } else { "push-effect sends" },
                        Icon { name: effect.icon, size: 13.0, class: "lead" }
                        if effect.triggers.mention {
                            Icon { name: IconName::Mention, size: 13.0 }
                        }
                        if effect.triggers.wildcard {
                            Icon { name: IconName::Usergroup, size: 13.0 }
                        }
                        if effect.triggers.all_messages {
                            Icon { name: IconName::ChatBubble, size: 13.0 }
                        }
                    }
                }
            }
        }
    }
}
