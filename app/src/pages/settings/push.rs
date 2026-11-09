//! Enhanced push: mobile push, email, @all, automatic topic following, and per-channel and
//! per-topic push settings. All changes are submitted to Zulip immediately.

use super::Store;
use super::channels::{ChannelBlock, shows};
use super::field::*;
use crate::ui::*;
use crate::widgets::{
    ChannelPushFlags, PushEffect, SessionRef, TopicPushFlags, channel_push, mock_notice, topic_push,
};
use dioxus::prelude::*;
use zulip_core::ConversationKey;

const BATCHING: [(i32, &str); 8] = [
    (120, "2 分钟"),
    (300, "5 分钟"),
    (600, "10 分钟"),
    (1800, "30 分钟"),
    (3600, "1 小时"),
    (21600, "6 小时"),
    (86400, "1 天"),
    (604800, "1 周"),
];
const REALM_NAME: [(i32, &str); 3] = [(1, "自动"), (2, "总是"), (3, "从不")];
const AUTO_POLICY: [(i32, &str); 4] = [
    (1, "我参与的话题"),
    (2, "我发过消息的话题"),
    (3, "我发起的话题"),
    (4, "从不"),
];

fn options(items: &[(i32, &str)]) -> Vec<(i32, String)> {
    items.iter().map(|(v, t)| (*v, (*t).to_owned())).collect()
}

/// Email batching wait time: when the current value is not among the fixed options (set on the
/// web), "Custom" is appended at the end.
fn batching_options(current: i32) -> Vec<(i32, String)> {
    let mut list = options(&BATCHING);
    if !BATCHING.iter().any(|(v, _)| *v == current) {
        list.push((current, format!("自定义（{current} 秒）")));
    }
    list
}

#[component]
fn Effects(lines: Vec<PushEffect>) -> Element {
    rsx! {
        div { class: "settings-effects",
            for line in lines {
                Tooltip { key: "{line.text}", text: line.text.clone(),
                    div { class: "settings-effect text-sub",
                        Icon { name: line.icon, size: 12.0 }
                        span { "{line.text}" }
                    }
                }
            }
        }
    }
}

/// One row of the mobile / email columns.
#[component]
fn MatrixRow(
    #[props(into)] label: String,
    phone: Option<Bind<bool>>,
    email: Bind<bool>,
    /// Only one switch, deciding both mobile push and email.
    #[props(default)]
    single: bool,
) -> Element {
    rsx! {
        SettingRow { label, description: email.hint.clone(),
            div { class: "settings-cells",
                if let Some(phone) = phone {
                    div { class: "settings-cell",
                        Toggle { checked: phone.value, onchange: move |v| phone.set(v) }
                    }
                }
                div { class: if single { "settings-cell wide" } else { "settings-cell" },
                    Toggle { checked: email.value, onchange: move |v| email.set(v) }
                }
            }
        }
    }
}

/// Mobile push settings: when the server has push disabled (shown in developer mode) it is only
/// simulated.
fn phone_bind(bind: Bind<bool>, available: bool, overlays: Overlays) -> Bind<bool> {
    if available {
        bind
    } else {
        Bind::new(bind.value, move |_| mock_notice(overlays))
    }
}

#[component]
fn ChannelFlags(stream_id: i64, push_available: bool) -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let session = store.session();
    let Some(data) = channel_push(&session, stream_id, push_available) else {
        return rsx! {};
    };
    rsx! {
        Effects { lines: data.effects.clone() }
        ChannelPushFlags { session: SessionRef(session), stream_id, data }
    }
}

#[component]
fn TopicFlags(stream_id: i64, topic: String, push_available: bool) -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let session = store.session();
    let Some(data) = topic_push(&session, stream_id, &topic, push_available) else {
        return rsx! {};
    };
    rsx! {
        Effects { lines: data.effects.clone() }
        TopicPushFlags { session: SessionRef(session), stream_id, topic, data }
    }
}

#[component]
pub fn EnhancedPush() -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let overlays = use_overlays();
    let session = store.session();
    let push_enabled = session.realm().push_notifications_enabled;
    let push_available = shows(push_enabled);
    let phone = |name: &'static [&'static str]| -> Option<Bind<bool>> {
        push_available.then(|| phone_bind(field(Source::Zulip(name)), push_enabled, overlays))
    };
    let batching = field::<i32>(Source::Zulip(&[
        "email_notifications_batching_period_seconds",
    ]));
    let batching_list = batching_options(batching.value);
    let mut channels = session.subscribed_channels();
    channels.sort_by_key(|c| c.name.to_lowercase());
    let (archived, active): (Vec<_>, Vec<_>) = channels.into_iter().partition(|c| c.is_archived);
    let block = |stream_id: i64| {
        let session = session.clone();
        rsx! {
            ChannelBlock {
                key: "{stream_id}",
                stream_id,
                trailing: rsx! { ChannelFlags { stream_id, push_available } },
                topic: move |topic: String| {
                    let title = session.topic_title_with_original(&ConversationKey::topic(stream_id, topic.clone()));
                    rsx! {
                        SettingRow { label: title,
                            TopicFlags { stream_id, topic: topic.clone(), push_available }
                        }
                    }
                },
            }
        }
    };
    rsx! {
        SettingsPage { title: "强化推送",
            Group { title: if push_available { "推送和邮件的内容" } else { "邮件的内容" },
                if push_available {
                    SettingRow { label: "",
                        div { class: "settings-cells text-sub",
                            div { class: "settings-cell", "手机" }
                            div { class: "settings-cell", "邮件" }
                        }
                    }
                }
                MatrixRow {
                    label: "私信和 @我",
                    phone: phone(&["enable_offline_push_notifications"]),
                    email: field(Source::Zulip(&["enable_offline_email_notifications"])),
                }
                MatrixRow {
                    label: "关注的话题中的所有消息",
                    phone: phone(&["enable_followed_topic_push_notifications"]),
                    email: field(Source::Zulip(&["enable_followed_topic_email_notifications"])),
                }
                MatrixRow {
                    label: "频道中的所有消息",
                    phone: phone(&["enable_stream_push_notifications"]),
                    email: field(Source::Zulip(&["enable_stream_email_notifications"])),
                }
                MatrixRow {
                    label: "@全体成员",
                    email: field(Source::Zulip(&["wildcard_mentions_notify"])),
                    single: push_available,
                }
                MatrixRow {
                    label: "关注的话题中 @全体成员",
                    email: field(Source::Zulip(&["enable_followed_topic_wildcard_mentions_notify"])),
                    single: push_available,
                }
            }
            if push_available {
                Group { title: "推送选项",
                    ToggleRow {
                        label: "在线时也推送",
                        bind: phone_bind(field(Source::Zulip(&["enable_online_push_notifications"])), push_enabled, overlays),
                    }
                }
            }
            Group { title: "邮件选项",
                ChoiceRow { label: "邮件合并等待时间", bind: batching, options: batching_list }
                ToggleRow { label: "邮件中包含消息内容", bind: field(Source::Zulip(&["message_content_in_email_notifications"])) }
                ChoiceRow {
                    label: "邮件标题中包含组织名称",
                    bind: field(Source::Zulip(&["realm_name_in_email_notifications_policy"])),
                    options: options(&REALM_NAME),
                }
                ToggleRow { label: "长时间未登录时发送摘要邮件", bind: field(Source::Zulip(&["enable_digest_emails"])) }
            }
            Group { title: "话题自动关注",
                ChoiceRow {
                    label: "自动关注话题",
                    bind: field(Source::Zulip(&["automatically_follow_topics_policy"])),
                    options: options(&AUTO_POLICY),
                }
                ToggleRow { label: "被 @ 时自动关注话题", bind: field(Source::Zulip(&["automatically_follow_topics_where_mentioned"])) }
                ChoiceRow {
                    label: "在静音频道中自动取消话题静音",
                    bind: field(Source::Zulip(&["automatically_unmute_topics_in_muted_streams_policy"])),
                    options: options(&AUTO_POLICY),
                }
            }
            div { class: "setting-section", "频道与话题" }
            SettingCard {
                for channel in active {
                    {block(channel.stream_id)}
                }
                if !archived.is_empty() {
                    Expander { title: format!("已归档的频道（{}）", archived.len()),
                        for channel in archived {
                            {block(channel.stream_id)}
                        }
                    }
                }
            }
        }
    }
}
