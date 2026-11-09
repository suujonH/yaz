//! 全局搜索：搜索框下方的结果面板和“更多”窗口。

use crate::pages::main_window::use_main_ui;
use crate::shell::{WindowShell, use_window_args};
use crate::state::{AppCommand, send_command, use_app_state};
use crate::ui::*;
use crate::widgets::RemoteAvatar;
use crate::windows;
use dioxus::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use zulip_core::ConversationKey;
use zulip_session::{ChannelHit, ContactHit, MessageGroup, Session};

const SECTION_LIMIT: usize = 3;
const MESSAGE_DEBOUNCE: Duration = Duration::from_millis(400);

/// “更多”窗口显示哪一节。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchSection {
    Contacts,
    Channels,
    Messages,
}

impl SearchSection {
    const fn title(self) -> &'static str {
        match self {
            Self::Contacts => "联系人",
            Self::Channels => "频道与话题",
            Self::Messages => "聊天记录",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchMoreArgs {
    pub section: SearchSection,
    pub query: String,
}

impl SearchMoreArgs {
    pub fn window_title(&self) -> String {
        format!("{}：{}", self.section.title(), self.query)
    }
}

/// 加载订阅频道的话题，搜索才能搜到话题。
fn load_all_topics(session: &Arc<Session>) {
    for channel in session.subscribed_channels() {
        if session.topics(channel.stream_id).is_none() {
            let session = session.clone();
            spawn(async move {
                if let Err(error) = session.load_topics(channel.stream_id).await {
                    log::warn!("加载话题失败：{error}");
                }
            });
        }
    }
}

/// 消息搜索：防抖，新结果到达前保留旧结果。
fn use_message_search(query: ReadSignal<String>) -> Signal<Vec<MessageGroup>> {
    let state = use_app_state();
    let overlays = use_overlays();
    let mut results = use_signal(Vec::new);
    use_resource(move || async move {
        let text = query().trim().to_owned();
        tokio::time::sleep(MESSAGE_DEBOUNCE).await;
        let Some(session) = state.session.peek().clone() else {
            return;
        };
        if text.is_empty() {
            results.set(Vec::new());
            return;
        }
        if let Some(groups) = overlays
            .attempt("搜索失败", session.search_message_groups(&text))
            .await
        {
            results.set(groups);
        }
    });
    results
}

fn open_channel_hit(account: i64, hit: &ChannelHit) {
    match hit {
        ChannelHit::Channel { stream_id, .. } => {
            send_command(AppCommand::ShowChannel {
                account,
                stream_id: *stream_id,
            });
            send_command(AppCommand::ShowMain);
        }
        ChannelHit::Topic { key, .. } => windows::open_chat(account, key.clone(), None),
    }
}

#[component]
fn ContactHitRow(hit: ContactHit, query: String, on_open: EventHandler<()>) -> Element {
    let state = use_app_state();
    let user_id = hit.user_id;
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            leading: rsx! {
                RemoteAvatar { text: hit.display_name.clone(), url: hit.avatar_url.clone(), size: 32.0 }
            },
            title: hit.display_name.clone(),
            query,
            onclick: move |_| {
                if let Some(session) = state.session.peek().as_ref() {
                    windows::open_chat(
                        session.account_id(),
                        ConversationKey::dm([user_id], session.self_id()),
                        None,
                    );
                }
                on_open.call(());
            },
        }
    }
}

#[component]
fn ChannelHitRow(hit: ChannelHit, query: String, on_open: EventHandler<()>) -> Element {
    let state = use_app_state();
    let (title, subtitle) = match &hit {
        ChannelHit::Channel {
            title, subtitle, ..
        }
        | ChannelHit::Topic {
            title, subtitle, ..
        } => (title.clone(), subtitle.clone()),
    };
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            leading: rsx! { Avatar { text: title.clone(), size: 32.0 } },
            title,
            query,
            subtitle,
            onclick: move |_| {
                if let Some(account) = *state.account.peek() {
                    open_channel_hit(account, &hit);
                }
                on_open.call(());
            },
        }
    }
}

#[component]
fn MessageHitRow(group: MessageGroup, query: String, on_open: EventHandler<()>) -> Element {
    let state = use_app_state();
    let key = group.key.clone();
    let search = query.clone();
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            leading: rsx! { Avatar { text: group.title.clone(), size: 32.0 } },
            title: group.title.clone(),
            subtitle: group.count_text.clone(),
            onclick: move |_| {
                if let Some(account) = *state.account.peek() {
                    windows::open_history(account, key.clone(), Some(search.clone()));
                }
                on_open.call(());
            },
        }
    }
}

#[component]
fn SectionHead(section: SearchSection, more: bool, query: String) -> Element {
    let state = use_app_state();
    rsx! {
        div { class: "search-head",
            span { class: "text-sub", "{section.title()}" }
            if more {
                Button {
                    kind: ButtonKind::Flat,
                    onclick: move |_| {
                        if let Some(account) = *state.account.peek() {
                            windows::open_search_more(account, SearchMoreArgs { section, query: query.clone() });
                        }
                    },
                    "更多"
                }
            }
        }
    }
}

/// 中间栏的搜索结果，盖在当前页面上。Esc 和点击外部由主窗口处理。
#[component]
pub fn SearchPanel() -> Element {
    let state = use_app_state();
    let mut ui = use_main_ui();
    let search = ui.search;
    use_hook(|| {
        if let Some(session) = state.session.peek().as_ref() {
            load_all_topics(session);
        }
    });
    let messages = use_message_search(search.into());
    let query = search.read().trim().to_owned();
    let session = state.session.read().clone();
    let contacts = session
        .as_ref()
        .map_or_else(Vec::new, |s| s.search_contacts(&query));
    let channels = session
        .as_ref()
        .map_or_else(Vec::new, |s| s.search_channels(&query));
    let groups = messages.read().clone();
    let empty = contacts.is_empty() && channels.is_empty() && groups.is_empty();
    let close = move |()| ui.search.set(String::new());
    rsx! {
        div { class: "search-panel scroll",
            if empty {
                div { class: "search-empty text-sub", "未找到相关结果" }
            }
            if !contacts.is_empty() {
                SectionHead { section: SearchSection::Contacts, more: contacts.len() > SECTION_LIMIT, query: query.clone() }
                for hit in contacts.into_iter().take(SECTION_LIMIT) {
                    ContactHitRow { key: "{hit.user_id}", hit, query: query.clone(), on_open: close }
                }
            }
            if !channels.is_empty() {
                SectionHead { section: SearchSection::Channels, more: channels.len() > SECTION_LIMIT, query: query.clone() }
                for (i, hit) in channels.into_iter().take(SECTION_LIMIT).enumerate() {
                    ChannelHitRow { key: "{i}", hit, query: query.clone(), on_open: close }
                }
            }
            if !groups.is_empty() {
                SectionHead { section: SearchSection::Messages, more: groups.len() > SECTION_LIMIT, query: query.clone() }
                for group in groups.into_iter().take(SECTION_LIMIT) {
                    MessageHitRow { key: "{group.key}", group, query: query.clone(), on_open: close }
                }
            }
        }
    }
}

/// “更多”窗口：这一节的全部结果。
#[component]
pub fn SearchMoreWindow() -> Element {
    let args = use_window_args::<SearchMoreArgs>();
    let SearchMoreArgs { section, query } = args();
    let title = args.read().window_title();
    rsx! {
        WindowShell { title,
            SearchMoreBody { key: "{query}", section, query: query.clone() }
        }
    }
}

#[component]
fn SearchMoreBody(section: SearchSection, query: String) -> Element {
    let state = use_app_state();
    let mut text = use_signal(|| query.clone());
    use_effect(use_reactive!(|query| text.set(query)));
    let messages = use_message_search(text.into());
    let session = state.session.read().clone();
    let close = move |()| {};
    rsx! {
        div { class: "search-more scroll",
            match section {
                SearchSection::Contacts => rsx! {
                    for hit in session.as_ref().map_or_else(Vec::new, |s| s.search_contacts(&query)) {
                        ContactHitRow { key: "{hit.user_id}", hit, query: query.clone(), on_open: close }
                    }
                },
                SearchSection::Channels => rsx! {
                    for (i, hit) in session.as_ref().map_or_else(Vec::new, |s| s.search_channels(&query)).into_iter().enumerate() {
                        ChannelHitRow { key: "{i}", hit, query: query.clone(), on_open: close }
                    }
                },
                SearchSection::Messages => rsx! {
                    for group in messages() {
                        MessageHitRow { key: "{group.key}", group, query: query.clone(), on_open: close }
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn more_window_title_has_section_and_query() {
        let args = SearchMoreArgs {
            section: SearchSection::Channels,
            query: "abc".to_owned(),
        };
        assert_eq!(args.window_title(), "频道与话题：abc");
    }
}
