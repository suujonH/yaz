//! Expandable blocks for channels and topics: shared by enhanced push and topic message settings.
//! Topics are loaded the first time they are expanded.

use super::Store;
use crate::services::show_unavailable;
use crate::ui::*;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;

/// The feature is available, or in developer mode unavailable features are shown.
pub fn shows(available: bool) -> bool {
    available || show_unavailable()
}

/// One channel: the title row (click to expand) and its topics.
#[component]
pub fn ChannelBlock(
    stream_id: i64,
    /// Title row's right-side content.
    trailing: Option<Element>,
    /// Content of one topic (name).
    topic: Callback<String, Element>,
) -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let session = store.session();
    let overlays = use_overlays();
    let mut open = use_signal(|| false);
    let Some(channel) = session.channel(stream_id) else {
        return rsx! {};
    };
    let topics = session.topics(stream_id);
    let toggle = {
        let session = session.clone();
        move |_| {
            let opening = !open();
            open.set(opening);
            if opening && session.topics(stream_id).is_none() {
                let session = session.clone();
                spawn_forever(async move {
                    overlays
                        .attempt("加载话题失败", session.load_topics(stream_id))
                        .await;
                });
            }
        }
    };
    let name = session.channel_display_name(stream_id);
    let fill = (!channel.color.is_empty()).then(|| channel.color.clone());
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            leading: rsx! {
                Icon { name: IconName::ChevronRight, size: 14.0, class: if open() { "settings-chevron open" } else { "settings-chevron" } }
                Avatar { text: name.clone(), size: 24.0, fill, owner: "{stream_id}" }
            },
            title: name,
            trailing,
            onclick: toggle,
        }
        ExpandPanel { open: open(),
            div { class: "settings-topics",
                match topics {
                    None => rsx! { div { class: "text-sub settings-topics-note", "正在加载…" } },
                    Some(topics) if topics.is_empty() => rsx! { div { class: "text-sub settings-topics-note", "暂无话题" } },
                    Some(topics) => rsx! {
                        for t in topics {
                            div { key: "{t.name}", {topic.call(t.name.clone())} }
                        }
                    },
                }
            }
        }
    }
}
