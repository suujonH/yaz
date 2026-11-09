//! Shared part of message query results: favorites, chat history, and files are all "condition ->
//! paged load -> list grouped by date". This holds the paged-loading hook, the virtual list with
//! status hints, date headers, and message rows.

use super::files_flow::FileItemCard;
use super::message_flows::open_link;
use super::panes::Selection;
use super::remote_image::RemoteImage;
use super::{RemoteAvatar, text_id};
use crate::state::{AppState, use_app_state, use_session_event};
use crate::ui::*;
use crate::windows;
use dioxus::prelude::*;
use std::rc::Rc;
use std::time::Duration;
use zulip_core::html::{is_attachment_only, parse_fragment, to_plain_text};
use zulip_core::text::time::{clock_time, from_unix};
use zulip_session::{
    MessageItem, MessagePager, MessageSearch, Session, SessionEvent, group_by_date,
};

/// Only after the composer value stops changing for this long is it used as the query condition.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Ready,
    Failed,
}

/// Paged message query results. Loads from the start when the condition changes, the account
/// switches, or events fall behind.
#[derive(Clone, Copy)]
pub struct MessageResults {
    items: Signal<Rc<Vec<MessageItem>>>,
    phase: Signal<Phase>,
    loading_more: Signal<bool>,
    pager: CopyValue<Option<MessagePager>>,
    generation: CopyValue<u64>,
    reload: Signal<u32>,
    state: AppState,
    overlays: Overlays,
}

/// No query when `search` is `None` (no source chosen yet).
pub fn use_message_results(
    search: ReadSignal<Option<MessageSearch>>,
    page_size: u32,
) -> MessageResults {
    let state = use_app_state();
    let overlays = use_overlays();
    let mut items = use_signal(|| Rc::new(Vec::<MessageItem>::new()));
    let mut phase = use_signal(|| Phase::Loading);
    let mut loading_more = use_signal(|| false);
    let pager = use_hook(|| CopyValue::new(None::<MessagePager>));
    let generation = use_hook(|| CopyValue::new(0u64));
    let mut reload = use_signal(|| 0u32);
    use_session_event(move |event| {
        if event == SessionEvent::Resynced {
            reload += 1;
        }
    });
    use_resource(move || async move {
        let query = search();
        let _ = reload();
        let session = state.session.read().clone();
        let (mut generation, mut pager) = (generation, pager);
        *generation.write() += 1;
        *pager.write() = None;
        loading_more.set(false);
        items.set(Rc::default());
        let (Some(session), Some(query)) = (session, query) else {
            phase.set(Phase::Ready);
            return;
        };
        phase.set(Phase::Loading);
        let mut paging = MessagePager::with_page_size(query, page_size);
        match session.next_page(&mut paging).await {
            Ok(page) => {
                items.set(Rc::new(page));
                if !paging.is_done() {
                    *pager.write() = Some(paging);
                }
                phase.set(Phase::Ready);
            }
            Err(error) => {
                phase.set(Phase::Failed);
                overlays.report_error("加载失败", error);
            }
        }
    });
    MessageResults {
        items,
        phase,
        loading_more,
        pager,
        generation,
        reload,
        state,
        overlays,
    }
}

impl MessageResults {
    /// Loaded messages, newest first.
    pub fn items(self) -> Rc<Vec<MessageItem>> {
        self.items.read().clone()
    }

    pub fn phase(self) -> Phase {
        (self.phase)()
    }

    pub fn loading_more(self) -> bool {
        (self.loading_more)()
    }

    /// Hint text in the middle of the list; `None` when there is content. `empty` is the text for
    /// no results.
    pub fn status(self, empty: &str) -> Option<String> {
        if !self.items.read().is_empty() {
            return None;
        }
        Some(
            match self.phase() {
                Phase::Loading => "加载中…",
                Phase::Failed => "加载失败",
                Phase::Ready => empty,
            }
            .to_owned(),
        )
    }

    pub fn reload(mut self) {
        self.reload += 1;
    }

    /// Load earlier messages when near the bottom of the list; on failure retry on the next scroll.
    pub fn load_more(mut self) {
        if *self.loading_more.peek() || *self.phase.peek() != Phase::Ready {
            return;
        }
        let Some(mut paging) = std::mem::take(&mut *self.pager.write()) else {
            return;
        };
        let Some(session) = self.state.session.peek().clone() else {
            return;
        };
        let generation = *self.generation.peek();
        self.loading_more.set(true);
        spawn(async move {
            let result = session.next_page(&mut paging).await;
            if *self.generation.peek() != generation {
                return;
            }
            self.loading_more.set(false);
            match result {
                Ok(page) => {
                    let mut all = self.items.peek().as_ref().clone();
                    all.extend(page);
                    self.items.set(Rc::new(all));
                }
                Err(error) => self.overlays.report_error("加载失败", error),
            }
            if !paging.is_done() {
                *self.pager.write() = Some(paging);
            }
        });
    }

    /// Remove a message from the results (after unfavoriting).
    pub fn remove(mut self, ids: &[i64]) {
        let kept: Vec<MessageItem> = self
            .items
            .peek()
            .iter()
            .filter(|item| !ids.contains(&item.message.id))
            .cloned()
            .collect();
        self.items.set(Rc::new(kept));
    }
}

/// The composer value takes effect only after it stops changing for a while; the first value takes
/// effect immediately.
pub fn use_debounced(value: ReadSignal<String>, delay: Duration) -> ReadSignal<String> {
    let mut applied = use_signal(|| value.peek().clone());
    use_resource(move || async move {
        let next = value();
        if next == *applied.peek() {
            return;
        }
        tokio::time::sleep(delay).await;
        applied.set(next);
    });
    applied.into()
}

/// One row of the list: a date header or content.
#[derive(Clone, Debug, PartialEq)]
pub enum DateRow<T> {
    Header(String),
    Item(T),
}

/// One header per group, followed by the group's content.
pub fn with_headers<T>(groups: impl IntoIterator<Item = (String, Vec<T>)>) -> Vec<DateRow<T>> {
    let mut rows = Vec::new();
    for (title, items) in groups {
        rows.push(DateRow::Header(title));
        rows.extend(items.into_iter().map(DateRow::Item));
    }
    rows
}

/// Group messages by date (newest first).
pub fn message_rows(items: &[MessageItem]) -> Vec<DateRow<MessageItem>> {
    with_headers(
        group_by_date(items)
            .into_iter()
            .map(|group| (group.date, group.items.to_vec())),
    )
}

pub fn header_key(title: &str) -> u64 {
    text_id(&format!("header:{title}"))
}

/// Key of the virtual list: headers by text, content by `item_key`.
pub fn row_keys<T>(rows: &[DateRow<T>], item_key: impl Fn(&T) -> u64) -> Rc<Vec<u64>> {
    Rc::new(
        rows.iter()
            .map(|row| match row {
                DateRow::Header(title) => header_key(title),
                DateRow::Item(item) => item_key(item),
            })
            .collect(),
    )
}

/// Virtual list with status hints: hint text in the middle (loading, no results), a "loading..." at
/// the bottom, and load more when near the bottom.
#[component]
pub fn ResultList(
    keys: Rc<Vec<u64>>,
    render: Callback<usize, Element>,
    #[props(into)] status: Option<String>,
    #[props(default)] loading_more: bool,
    on_more: EventHandler<()>,
    #[props(default = 64.0)] estimate: f64,
) -> Element {
    let list = use_virtual_list(VirtualListConfig {
        estimate,
        ..VirtualListConfig::default()
    });
    rsx! {
        div { class: "result-list",
            VirtualList {
                handle: list,
                keys,
                render,
                on_reach_bottom: move |()| on_more.call(()),
            }
            if let Some(text) = status {
                div { class: "result-status text-sub", "{text}" }
            }
            if loading_more {
                div { class: "result-more text-sub", "加载中…" }
            }
        }
    }
}

#[component]
pub fn DateHeader(#[props(into)] title: String) -> Element {
    rsx! {
        div { class: "result-date text-sub", "{title}" }
    }
}

/// Text content of a message; a message with only attachments has no text.
pub fn message_text(item: &MessageItem) -> String {
    if is_attachment_only(&item.message.content_html) {
        String::new()
    } else {
        to_plain_text(&item.message.content_html)
    }
}

/// Images in a message: display URL and original URL. Uploaded images without an inline preview use
/// the upload path.
pub fn message_images(item: &MessageItem) -> Vec<(String, String)> {
    if item.images.is_empty() {
        item.uploads
            .iter()
            .filter(|upload| upload.is_image)
            .map(|upload| (upload.path.clone(), upload.path.clone()))
            .collect()
    } else {
        item.images
            .iter()
            .map(|image| (image.src.clone(), image.original()))
            .collect()
    }
}

/// External links in a message (excluding uploaded files and in-message anchors).
pub fn message_links(item: &MessageItem) -> Vec<String> {
    let doc = parse_fragment(&item.message.content_html);
    let mut links: Vec<String> = Vec::new();
    for node in doc.root_element().descendants() {
        let Some(element) = node.value().as_element() else {
            continue;
        };
        let Some(href) = element.attr("href").filter(|_| element.name() == "a") else {
            continue;
        };
        if !href.starts_with('#')
            && !href.starts_with("/user_uploads/")
            && !links.iter().any(|l| l == href)
        {
            links.push(href.to_owned());
        }
    }
    links
}

/// Click a thumbnail: open this message's images in the image viewer.
fn open_images(session: &Session, originals: Vec<String>, index: usize) {
    windows::open_image_viewer(session.account_id(), originals, index);
}

/// A message in the query results: sender avatar, name, time, plus text, images, and files. With
/// multi-select, click toggles the check; double-click locates (`on_open`).
#[component]
pub fn MessageResultRow(
    item: MessageItem,
    #[props(into)] query: String,
    /// Also show the conversation it belongs to in the title (for cross-conversation results).
    #[props(default)]
    show_conversation: bool,
    /// List the links in the message (links tab).
    #[props(default)]
    show_links: bool,
    selection: Option<Selection>,
    on_open: EventHandler<MessageItem>,
    menu: Option<Callback<MessageItem, Vec<MenuEntry>>>,
    /// A button on the right, shown while the row is hovered, that jumps to the message.
    jump: Option<EventHandler<MessageItem>>,
    /// Marked as the message being looked at (viewing context).
    #[props(default)]
    highlighted: bool,
) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let Some(session) = state.session.read().clone() else {
        return rsx! {};
    };
    let message = item.message.clone();
    let user = session.user(message.sender_id);
    let name = match &user {
        Some(_) => session.display_name(message.sender_id),
        None => message.sender_name.clone(),
    };
    let avatar = user
        .and_then(|u| u.avatar_url)
        .or_else(|| message.avatar_url.clone());
    let title = if show_conversation {
        format!("{name} · {}", session.display_title(&message.key))
    } else {
        name.clone()
    };
    let time = from_unix(message.timestamp).map(|t| clock_time(t, session.use_24h(), false));
    let text = message_text(&item);
    let images = message_images(&item);
    let originals: Vec<String> = images
        .iter()
        .map(|(_, original)| original.clone())
        .collect();
    let files: Vec<_> = item
        .uploads
        .iter()
        .filter(|upload| !upload.is_image)
        .cloned()
        .collect();
    let links = if show_links {
        message_links(&item)
    } else {
        Vec::new()
    };
    let id = message.id;
    let open_item = item.clone();
    let menu_item = item.clone();
    let jump_item = item.clone();
    rsx! {
        ListRow {
            density: RowDensity::Normal,
            leading: rsx! {
                RemoteAvatar { text: name, url: avatar, size: 36.0 }
            },
            title,
            meta: time,
            subtitle_prefix: rsx! {
                div { class: "result-body",
                    if !text.is_empty() {
                        div { class: "result-text", HighlightText { text, query: query.clone() } }
                    }
                    if !images.is_empty() {
                        div { class: "result-thumbs",
                            for (index, (src, _)) in images.into_iter().enumerate() {
                                RemoteImage {
                                    key: "{index}",
                                    url: src,
                                    onclick: {
                                        let session = session.clone();
                                        let originals = originals.clone();
                                        move |e: MouseEvent| {
                                            e.stop_propagation();
                                            open_images(&session, originals.clone(), index);
                                        }
                                    },
                                }
                            }
                        }
                    }
                    for url in links {
                        div {
                            key: "{url}",
                            class: "result-link",
                            onclick: {
                                let (session, url) = (session.clone(), url.clone());
                                move |e: MouseEvent| {
                                    e.stop_propagation();
                                    let (session, url) = (session.clone(), url.clone());
                                    spawn(async move { open_link(overlays, &session, &url).await });
                                }
                            },
                            "{url}"
                        }
                    }
                    for upload in files {
                        FileItemCard {
                            key: "{upload.path}",
                            link: upload,
                            message_id: id,
                            conversation: message.key.clone(),
                        }
                    }
                }
            },
            trailing: rsx! {
                if let Some(jump) = jump {
                    Tooltip { text: "定位到聊天",
                        IconButton {
                            icon: IconName::ChatBubble,
                            class: "result-jump",
                            onclick: move |e: MouseEvent| {
                                e.stop_propagation();
                                jump.call(jump_item.clone());
                            },
                        }
                    }
                }
            },
            selected: highlighted,
            checked: selection.and_then(|s| s.checked(id)),
            onclick: move |_| {
                if let Some(selection) = selection
                    && selection.is_active()
                {
                    selection.toggle(id);
                }
            },
            ondoubleclick: move |_| on_open.call(open_item.clone()),
            menu: menu.map(|menu| Callback::new(move |()| menu.call(menu_item.clone()))),
        }
    }
}
