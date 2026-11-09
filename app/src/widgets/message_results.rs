//! 消息查询结果的共用部分：收藏、聊天记录、文件都是“条件 → 分页加载 → 按日期分组的列表”。
//! 这里有分页加载的 hook、带状态提示的虚拟列表、日期标题和消息行。

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

/// 输入框的值停止变化这么久之后才作为查询条件。
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Ready,
    Failed,
}

/// 分页的消息查询结果。条件变化、账号切换、事件落后时从头加载。
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

/// `search` 为 `None` 时不查询（还没有选择来源）。
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
    /// 已加载的消息，新的在前。
    pub fn items(self) -> Rc<Vec<MessageItem>> {
        self.items.read().clone()
    }

    pub fn phase(self) -> Phase {
        (self.phase)()
    }

    pub fn loading_more(self) -> bool {
        (self.loading_more)()
    }

    /// 列表中间的提示文字；有内容时为 `None`。`empty` 是没有结果时的文字。
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

    /// 接近列表底部时加载更早的消息；失败时下次滚动再试。
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

    /// 从结果中去掉消息（取消收藏之后）。
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

/// 输入框的值停止变化一段时间后才生效；第一个值立即生效。
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

/// 列表中的一行：日期标题或内容。
#[derive(Clone, Debug, PartialEq)]
pub enum DateRow<T> {
    Header(String),
    Item(T),
}

/// 每组一个标题，后面是这组的内容。
pub fn with_headers<T>(groups: impl IntoIterator<Item = (String, Vec<T>)>) -> Vec<DateRow<T>> {
    let mut rows = Vec::new();
    for (title, items) in groups {
        rows.push(DateRow::Header(title));
        rows.extend(items.into_iter().map(DateRow::Item));
    }
    rows
}

/// 消息按日期（新的在前）分组。
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

/// 虚拟列表的键：标题按文字，内容按 `item_key`。
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

/// 带状态提示的虚拟列表：中间的提示文字（加载中、没有结果）、底部的“加载中…”、接近底部时加载更多。
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

/// 消息的文字内容；只有附件的消息没有文字。
pub fn message_text(item: &MessageItem) -> String {
    if is_attachment_only(&item.message.content_html) {
        String::new()
    } else {
        to_plain_text(&item.message.content_html)
    }
}

/// 消息里的图片：显示地址和原图地址。没有内联预览的上传图片用上传路径。
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

/// 消息里的外部链接（不含上传的文件和消息内的锚点）。
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

/// 点击缩略图：在图片查看器里打开这条消息的图片。
fn open_images(session: &Session, originals: Vec<String>, index: usize) {
    windows::open_image_viewer(session.account_id(), originals, index);
}

/// 查询结果里的一条消息：发送者头像、名字、时间，以及文字、图片、文件。
/// 多选时点击勾选，双击定位（`on_open`）。
#[component]
pub fn MessageResultRow(
    item: MessageItem,
    #[props(into)] query: String,
    /// 标题里同时显示所在会话（跨会话的结果）。
    #[props(default)]
    show_conversation: bool,
    /// 列出消息里的链接（链接标签页）。
    #[props(default)]
    show_links: bool,
    selection: Option<Selection>,
    on_open: EventHandler<MessageItem>,
    menu: Option<Callback<MessageItem, Vec<MenuEntry>>>,
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
