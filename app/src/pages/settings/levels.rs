//! Topic message settings: lists the four mute levels of topics per channel, plus group direct
//! messages.

use super::Store;
use super::channels::ChannelBlock;
use super::field::*;
use crate::ui::*;
use dioxus::prelude::*;
use zulip_core::ConversationKey;
use zulip_core::push::DndLevel;

fn levels() -> Vec<(DndLevel, String)> {
    [
        (DndLevel::Notify, "允许消息提醒"),
        (DndLevel::Silent, "接收消息但不提醒"),
        (DndLevel::Assistant, "收进话题助手且不提醒"),
        (DndLevel::Blocked, "屏蔽消息"),
    ]
    .into_iter()
    .map(|(level, text)| (level, text.to_owned()))
    .collect()
}

/// Mute level of one conversation (topic or group direct message); takes effect immediately once
/// chosen.
#[component]
fn LevelRow(key_of: ConversationKey) -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let session = store.session();
    let title = match &key_of {
        ConversationKey::Topic(_) => session.topic_title_with_original(&key_of),
        ConversationKey::Dm(_) => session.title(&key_of),
    };
    let level = session.conversation_level(&key_of);
    let bind = Bind::new(level, {
        let key = key_of.clone();
        move |level| session.set_conversation_level(&key, level)
    });
    rsx! {
        ChoiceRow { label: title, bind, options: levels() }
    }
}

#[component]
pub fn GroupMessages() -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let session = store.session();
    let mut channels = session.subscribed_channels();
    channels.sort_by_key(|c| c.name.to_lowercase());
    let (archived, active): (Vec<_>, Vec<_>) = channels.into_iter().partition(|c| c.is_archived);
    let group_dms: Vec<ConversationKey> = session
        .message_rows()
        .iter()
        .filter_map(|row| row.key().cloned())
        .filter(|key| matches!(key, ConversationKey::Dm(dm) if dm.is_group_dm()))
        .collect();
    let block = |stream_id: i64| {
        rsx! {
            SettingCard {
                key: "{stream_id}",
                ChannelBlock {
                    stream_id,
                    topic: move |name: String| rsx! { LevelRow { key_of: ConversationKey::topic(stream_id, name) } },
                }
            }
        }
    };
    rsx! {
        SettingsPage { title: "话题消息设置",
            div { class: "setting-section", "频道" }
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
            if !group_dms.is_empty() {
                Group { title: "多人私聊",
                    for key in group_dms {
                        LevelRow { key: "{key}", key_of: key.clone() }
                    }
                }
            }
        }
    }
}
