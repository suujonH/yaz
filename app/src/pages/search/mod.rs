//! Global search: the results panel below the search box (recent searches while the box is
//! empty), the "More" window and the chat records window. Opening a contact, channel, topic or file
//! from the results (panel or "More" window) is recorded as a recent search; chat records are not.

mod records;

pub use records::{MessageRecordsWindow, RecordsArgs};

use crate::pages::main_window::use_main_ui;
use crate::shell::{WindowShell, use_window_args};
use crate::state::{AppCommand, send_command, use_app_state};
use crate::ui::*;
use crate::widgets::{FileSource, RemoteAvatar, file_icon, open_file};
use crate::windows;
use dioxus::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use zulip_core::{ConversationKey, contains_ignore_case};
use zulip_session::{
    ChannelHit, ContactHit, ContentFilter, FileEntry, MessageGroup, MessagePager, MessageSearch,
    QueryScope, RecentHit, RecentSearch, Session,
};

const SECTION_LIMIT: usize = 3;
const MESSAGE_DEBOUNCE: Duration = Duration::from_millis(400);
/// Messages with attachments fetched per file search; their files are then matched by name.
const FILE_SEARCH_MESSAGES: u32 = 100;

/// Which section the "More" window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchSection {
    Contacts,
    Channels,
    Messages,
    Files,
}

impl SearchSection {
    const fn title(self) -> &'static str {
        match self {
            Self::Contacts => "联系人",
            Self::Channels => "频道与话题",
            Self::Messages => "聊天记录",
            Self::Files => "文件",
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

/// Load the topics of subscribed channels, so that searching can find topics.
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

/// Message search: debounced, old results are kept until new results arrive.
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

/// File search: messages with attachments matching the text, keeping the files whose name contains
/// it. Debounced like message search; failures only go to the log (message search already reports
/// them).
fn use_file_search(query: ReadSignal<String>) -> Signal<Vec<FileEntry>> {
    let state = use_app_state();
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
        let search = MessageSearch {
            scope: QueryScope::Everywhere,
            filter: ContentFilter::Attachments,
            text: text.clone(),
            ..MessageSearch::default()
        };
        let mut pager = MessagePager::with_page_size(search, FILE_SEARCH_MESSAGES);
        match session.next_page(&mut pager).await {
            Ok(items) => {
                let mut entries = session.file_entries(&items);
                entries.retain(|e| contains_ignore_case(&e.upload.file_name, &text));
                results.set(entries);
            }
            Err(error) => log::warn!("搜索文件失败：{error}"),
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
        ChannelHit::Topic { key, .. } => windows::show_chat_in_main(account, key.clone(), None),
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
                    session.add_recent_search(RecentSearch::Contact(user_id));
                    windows::show_chat_in_main(
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
                if let Some(session) = state.session.peek().as_ref() {
                    session.add_recent_search(match &hit {
                        ChannelHit::Channel { stream_id, .. } => RecentSearch::Channel(*stream_id),
                        ChannelHit::Topic { key, .. } => RecentSearch::Topic(key.clone()),
                    });
                    open_channel_hit(session.account_id(), &hit);
                }
                on_open.call(());
            },
        }
    }
}

fn file_source(entry: &FileEntry) -> FileSource {
    FileSource {
        link: entry.upload.clone(),
        message_id: entry.message_id,
        key: entry.key.clone(),
    }
}

/// One file: click opens it (downloading first when needed). `subtitle` says where it was sent.
#[component]
fn FileHitRow(
    source: FileSource,
    subtitle: String,
    query: String,
    on_open: EventHandler<()>,
) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let title = source.link.file_name.clone();
    let icon = file_icon(&source.link.path);
    rsx! {
        ListRow {
            density: RowDensity::Compact,
            leading: rsx! {
                Icon { name: icon, size: 32.0, class: "search-file-icon" }
            },
            title,
            query,
            subtitle: subtitle.clone(),
            onclick: move |_| {
                if let Some(session) = state.session.peek().clone() {
                    session.add_recent_search(RecentSearch::File {
                        link: source.link.clone(),
                        message_id: source.message_id,
                        key: source.key.clone(),
                        source: subtitle.clone(),
                    });
                    let source = source.clone();
                    spawn(async move { open_file(overlays, &session, &source).await });
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
                    windows::open_message_records(
                        account,
                        RecordsArgs {
                            query: search.clone(),
                            key: Some(key.clone()),
                        },
                    );
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
                        let Some(account) = *state.account.peek() else { return };
                        if section == SearchSection::Messages {
                            windows::open_message_records(account, RecordsArgs { query: query.clone(), key: None });
                        } else {
                            windows::open_search_more(account, SearchMoreArgs { section, query: query.clone() });
                        }
                    },
                    "更多"
                    Icon { name: IconName::ChevronRight, size: 12.0 }
                }
            }
        }
    }
}

/// Recent searches, shown while the search box is empty; nothing when there are none.
#[component]
fn RecentSearches(on_open: EventHandler<()>) -> Element {
    let state = use_app_state();
    // Bumped by "clear" so the list is read again
    let mut cleared = use_signal(|| 0u32);
    let _ = cleared();
    let session = state.session.read().clone();
    let hits = session
        .as_ref()
        .map_or_else(Vec::new, |s| s.recent_search_hits());
    if hits.is_empty() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "search-panel scroll",
            onmousedown: move |e| e.stop_propagation(),
            div { class: "search-head",
                span { class: "text-sub", "最近搜索" }
                IconButton {
                    icon: IconName::Delete,
                    class: "search-recent-clear",
                    size: 24.0,
                    icon_size: 16.0,
                    onclick: move |_| {
                        if let Some(session) = state.session.peek().as_ref() {
                            session.clear_recent_searches();
                        }
                        cleared += 1;
                    },
                }
            }
            for (i, hit) in hits.into_iter().enumerate() {
                match hit {
                    RecentHit::Contact(hit) => rsx! {
                        ContactHitRow { key: "{i}", hit, query: String::new(), on_open }
                    },
                    RecentHit::Channel(hit) => rsx! {
                        ChannelHitRow { key: "{i}", hit, query: String::new(), on_open }
                    },
                    RecentHit::File { link, message_id, key, source } => rsx! {
                        FileHitRow {
                            key: "{i}",
                            source: FileSource { link, message_id, key },
                            subtitle: source,
                            query: String::new(),
                            on_open,
                        }
                    },
                }
            }
        }
    }
}

/// Search results in a card floating over the middle column (recent searches while the box is
/// empty). Esc and clicking outside are handled by the main window.
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
    let files = use_file_search(search.into());
    let query = search.read().trim().to_owned();
    let session = state.session.read().clone();
    let contacts = session
        .as_ref()
        .map_or_else(Vec::new, |s| s.search_contacts(&query));
    let channels = session
        .as_ref()
        .map_or_else(Vec::new, |s| s.search_channels(&query));
    let groups = messages.read().clone();
    let file_hits = files.read().clone();
    let empty =
        contacts.is_empty() && channels.is_empty() && groups.is_empty() && file_hits.is_empty();
    let close = move |()| {
        ui.search.set(String::new());
        ui.search_open.set(false);
    };
    if query.is_empty() {
        return rsx! {
            RecentSearches { on_open: close }
        };
    }
    rsx! {
        div {
            class: "search-panel scroll",
            // Pressing inside the results does not hide them (see the main page)
            onmousedown: move |e| e.stop_propagation(),
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
            if !file_hits.is_empty() {
                SectionHead { section: SearchSection::Files, more: file_hits.len() > SECTION_LIMIT, query: query.clone() }
                for entry in file_hits.into_iter().take(SECTION_LIMIT) {
                    FileHitRow {
                        key: "{entry.upload.path}",
                        source: file_source(&entry),
                        subtitle: entry.source.clone(),
                        query: query.clone(),
                        on_open: close,
                    }
                }
            }
        }
    }
}

/// The "More" window: all results of this section.
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
    let files = use_file_search(text.into());
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
                SearchSection::Files => rsx! {
                    for entry in files() {
                        FileHitRow {
                            key: "{entry.upload.path}",
                            source: file_source(&entry),
                            subtitle: entry.source.clone(),
                            query: query.clone(),
                            on_open: close,
                        }
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
