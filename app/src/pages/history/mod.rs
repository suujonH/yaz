//! 聊天记录窗口：一个会话的消息，按标签页（全部 / 图片与视频 / 文件 / 链接）、搜索词、成员、日期筛选，
//! 按日期分组，可定位到聊天。

use crate::services::services;
use crate::shell::{WindowShell, use_window_args};
use crate::state::{AccountScope, use_app_state, use_session_event};
use crate::ui::*;
use crate::widgets::*;
use crate::windows;
use chrono::NaiveDate;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::ConversationKey;
use zulip_core::file::FileKind;
use zulip_core::html::{UploadLink, to_plain_text};
use zulip_core::text::time::date_label;
use zulip_session::queries::DEFAULT_PAGE_SIZE;
use zulip_session::{
    ContentFilter, FileEntry, MessageItem, MessageSearch, QueryScope, Session, SessionEvent,
    group_by_date,
};

/// 服务器支持按日期定位（`anchor=date`）的最低 feature level。
const DATE_FILTER_LEVEL: i32 = 445;
/// 图片标签页每行的缩略图数。
const TILES_PER_ROW: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryArgs {
    pub key: ConversationKey,
    pub query: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    All,
    Media,
    Files,
    Links,
}

impl Tab {
    const ALL: [Self; 4] = [Self::All, Self::Media, Self::Files, Self::Links];

    const fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Media => "media",
            Self::Files => "files",
            Self::Links => "links",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Media => "图片与视频",
            Self::Files => "文件",
            Self::Links => "链接",
        }
    }

    const fn filter(self) -> ContentFilter {
        match self {
            Self::All => ContentFilter::All,
            Self::Media => ContentFilter::Media,
            Self::Files => ContentFilter::Files,
            Self::Links => ContentFilter::Links,
        }
    }

    fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|tab| tab.id() == id)
            .unwrap_or(Self::All)
    }
}

/// 图片标签页里的一格：图片，或上传的视频。
#[derive(Clone, Debug, PartialEq)]
struct Tile {
    message_id: i64,
    thumbnail: String,
    /// 图片在相册中的序号（按时间先后）；视频为 `None`。
    album_index: Option<usize>,
    video: Option<UploadLink>,
}

#[derive(Clone, Debug, PartialEq)]
enum Row {
    Header(String),
    Message(MessageItem),
    Tiles(Vec<Tile>),
    File(FileEntry),
}

impl Row {
    fn key(&self) -> u64 {
        match self {
            Self::Header(title) => header_key(title),
            Self::Message(item) => item.message.id as u64,
            Self::Tiles(tiles) => text_id(&format!(
                "tiles:{}:{}",
                tiles.first().map_or(0, |t| t.message_id),
                tiles.first().map_or("", |t| t.thumbnail.as_str())
            )),
            Self::File(entry) => text_id(&entry.upload.path),
        }
    }
}

/// 图片标签页的格子，按日期分组，每行 [`TILES_PER_ROW`] 个。同时返回相册（按时间先后的原图地址）。
fn media_rows(items: &[MessageItem]) -> (Vec<Row>, Vec<String>) {
    let groups = group_by_date(items);
    let mut album = Vec::new();
    let mut raw: Vec<(String, Vec<Tile>)> = Vec::new();
    for group in groups {
        let mut tiles = Vec::new();
        for item in group.items {
            // 一条消息里的图片按先后排列，与相册一致
            for (thumbnail, original) in message_images(item) {
                tiles.push(Tile {
                    message_id: item.message.id,
                    thumbnail,
                    album_index: Some(album.len()),
                    video: None,
                });
                album.push(original);
            }
            for upload in item
                .uploads
                .iter()
                .filter(|u| zulip_core::html::is_video_path(&u.path))
            {
                tiles.push(Tile {
                    message_id: item.message.id,
                    thumbnail: String::new(),
                    album_index: None,
                    video: Some(upload.clone()),
                });
            }
        }
        if !tiles.is_empty() {
            raw.push((group.date, tiles));
        }
    }
    // 列表从新到旧，相册从旧到新：序号翻转
    let last = album.len().saturating_sub(1);
    album.reverse();
    let mut rows = Vec::new();
    for (date, tiles) in raw {
        rows.push(Row::Header(date));
        for chunk in tiles.chunks(TILES_PER_ROW) {
            rows.push(Row::Tiles(
                chunk
                    .iter()
                    .cloned()
                    .map(|mut tile| {
                        tile.album_index = tile.album_index.map(|i| last - i);
                        tile
                    })
                    .collect(),
            ));
        }
    }
    (rows, album)
}

/// 文件标签页：上传的非图片、非视频文件，按日期分组。
fn file_rows(session: &Session, items: &[MessageItem]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut current = String::new();
    for entry in session
        .file_entries(items)
        .into_iter()
        .filter(|e| !matches!(e.kind, FileKind::Image | FileKind::Video))
    {
        let date = date_label(entry.timestamp);
        if date != current {
            rows.push(Row::Header(date.clone()));
            current = date;
        }
        rows.push(Row::File(entry));
    }
    rows
}

fn build_rows(session: &Session, tab: Tab, items: &[MessageItem]) -> (Vec<Row>, Vec<String>) {
    match tab {
        Tab::All | Tab::Links => (
            message_rows(items)
                .into_iter()
                .map(|row| match row {
                    DateRow::Header(title) => Row::Header(title),
                    DateRow::Item(item) => Row::Message(item),
                })
                .collect(),
            Vec::new(),
        ),
        Tab::Media => media_rows(items),
        Tab::Files => (file_rows(session, items), Vec::new()),
    }
}

#[component]
pub fn HistoryWindow() -> Element {
    let args = use_window_args::<HistoryArgs>();
    let scope = use_context::<AccountScope>();
    let title = services()
        .hub
        .session(scope.0)
        .map(|s| format!("{} 的聊天记录", s.title(&args.read().key)))
        .unwrap_or_default();
    rsx! {
        WindowShell { title, HistoryFrame {} }
    }
}

/// 再次打开时更新搜索词；话题改名或移动后跟着新键走。
#[component]
fn HistoryFrame() -> Element {
    let args = use_window_args::<HistoryArgs>();
    use_window_keys(|_| {});
    let mut key = use_signal(|| args.peek().key.clone());
    let mut text = use_signal(|| args.peek().query.clone().unwrap_or_default());
    use_effect(move || {
        let HistoryArgs { key: next, query } = args();
        key.set(next);
        if let Some(query) = query {
            text.set(query);
        }
    });
    use_session_event(move |event| {
        if let SessionEvent::ConversationRekeyed { old, new } = event
            && *key.peek() == old
        {
            key.set(new);
        }
    });
    rsx! {
        HistoryBody { key: "{key}", conversation: key(), text }
    }
}

/// 候选成员：话题是频道订阅者，私聊是参与者；选中后返回用户 ID。
async fn pick_member(
    overlays: Overlays,
    session: Arc<Session>,
    conversation: ConversationKey,
) -> Option<i64> {
    let ids: Vec<i64> = match &conversation {
        ConversationKey::Topic(topic) => {
            overlays
                .attempt("加载成员失败", session.load_subscribers(topic.stream_id))
                .await?
        }
        ConversationKey::Dm(dm) => {
            let mut ids = dm.other_user_ids().to_vec();
            ids.push(session.self_id());
            ids
        }
    };
    let mut items: Vec<PickerItem> = ids
        .into_iter()
        .map(|id| {
            let mut item = PickerItem::new(id as u64, session.display_name(id));
            item.avatar_url = session
                .user(id)
                .and_then(|u| u.avatar_url)
                .and_then(|url| cached_avatar(&session, &url));
            item
        })
        .collect();
    items.sort_by(|a, b| a.title.cmp(&b.title));
    let picked = overlays.pick(Picker::single("按成员筛选", items)).await?;
    picked.first().map(|id| *id as i64)
}

/// 输入日期（年-月-日）；格式不对时提示。
async fn pick_date(overlays: Overlays, initial: Option<NaiveDate>) -> Option<NaiveDate> {
    let initial = initial.map_or_else(String::new, |d| d.format("%Y-%m-%d").to_string());
    let prompt = TextPrompt::new("按日期筛选", "年-月-日，如 2026-11-05", initial);
    let text = overlays.prompt_text(prompt).await?;
    let date = parse_date(&text);
    if date.is_none() {
        overlays.toast_kind(ToastKind::Error, "日期格式不正确，例如 2026-11-05");
    }
    date
}

#[component]
fn HistoryBody(conversation: ConversationKey, text: Signal<String>) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let mut tab = use_signal(|| Tab::All);
    let applied = use_debounced(text.into(), SEARCH_DEBOUNCE);
    let sender = use_signal(|| None::<i64>);
    let until = use_signal(|| None::<NaiveDate>);
    let search_key = conversation.clone();
    let search = use_memo(move || {
        Some(MessageSearch {
            scope: QueryScope::Conversation(search_key.clone()),
            filter: tab().filter(),
            text: applied().trim().to_owned(),
            sender: sender(),
            until: until().and_then(end_of_day),
        })
    });
    let results = use_message_results(search.into(), DEFAULT_PAGE_SIZE);
    // 日期筛选：以这一天结束为锚点向前取，锚点之后的第一条不显示
    let rows = use_memo(move || {
        let Some(session) = state.session.read().clone() else {
            return Rc::new((Vec::new(), Vec::new()));
        };
        let limit = until().and_then(|date| local_timestamp(date.and_hms_opt(23, 59, 59)?));
        let items = results.items();
        Rc::new(match limit {
            Some(limit) => {
                let kept: Vec<MessageItem> = items
                    .iter()
                    .filter(|item| item.message.timestamp <= limit)
                    .cloned()
                    .collect();
                build_rows(&session, tab(), &kept)
            }
            None => build_rows(&session, tab(), &items),
        })
    });
    let keys = use_memo(move || Rc::new(rows.read().0.iter().map(Row::key).collect::<Vec<u64>>()));

    let session = state.session.read().clone();
    let can_filter_date = session
        .as_ref()
        .is_some_and(|s| s.realm().feature_level >= DATE_FILTER_LEVEL);
    let use_24h = session.as_ref().is_some_and(|s| s.use_24h());
    let query = applied().trim().to_owned();
    let sender_name = sender().and_then(|id| session.as_ref().map(|s| s.display_name(id)));
    let filter_text = [
        sender_name.map(|name| format!("成员：{name}")),
        until().map(|date| format!("{} 及之前", date.format("%Y/%m/%d"))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let status = if rows.read().0.is_empty() && !results.items().is_empty() {
        Some("没有找到相关的聊天记录".to_owned())
    } else {
        results.status("没有找到相关的聊天记录")
    };

    let locate = move |key: ConversationKey, id: i64| {
        let Some(session) = state.session.peek().clone() else {
            return;
        };
        spawn(async move {
            locate_in_chat(overlays, &session, Some(&key), id).await;
        });
    };
    let on_open = {
        let key = conversation.clone();
        Callback::new(move |item: MessageItem| locate(key.clone(), item.message.id))
    };
    let menu = {
        let key = conversation.clone();
        Callback::new(move |item: MessageItem| {
            let Some(session) = state.session.peek().clone() else {
                return Vec::new();
            };
            let id = item.message.id;
            let text = to_plain_text(&item.message.content_html);
            let (locate_key, locate_session, forward_session) =
                (key.clone(), session.clone(), session);
            vec![
                MenuEntry::item("定位到聊天", move || {
                    let (key, session) = (locate_key.clone(), locate_session.clone());
                    spawn_forever(async move {
                        locate_in_chat(overlays, &session, Some(&key), id).await;
                    });
                }),
                MenuEntry::item("复制", move || overlays.copy_text(&text)),
                MenuEntry::item("转发", move || {
                    let session = forward_session.clone();
                    spawn_forever(async move {
                        forward_messages(overlays, &session, vec![id]).await;
                    });
                }),
            ]
        })
    };
    let filter_menu = {
        let key = conversation.clone();
        move |e: MouseEvent| {
            let Some(session) = state.session.peek().clone() else {
                return;
            };
            let point = e.client_coordinates();
            let (member_session, member_key) = (session, key.clone());
            let mut items = vec![MenuEntry::item("按成员…", move || {
                let (session, key) = (member_session.clone(), member_key.clone());
                spawn_forever(async move {
                    if let Some(id) = pick_member(overlays, session, key).await {
                        set_if_alive(sender, Some(id));
                    }
                });
            })];
            if can_filter_date {
                items.push(MenuEntry::item("按日期…", move || {
                    let initial = until.peek().to_owned();
                    spawn_forever(async move {
                        if let Some(date) = pick_date(overlays, initial).await {
                            set_if_alive(until, Some(date));
                        }
                    });
                }));
            }
            if sender.peek().is_some() || until.peek().is_some() {
                items.push(MenuEntry::Separator);
                items.push(MenuEntry::item("清除筛选", move || {
                    set_if_alive(sender, None);
                    set_if_alive(until, None);
                }));
            }
            overlays.show_menu(point.x, point.y, items);
        }
    };

    let (rows_now, album) = {
        let all = rows.read();
        (Rc::new(all.0.clone()), Rc::new(all.1.clone()))
    };
    let current = tab();
    rsx! {
        div { class: "history",
            div { class: "history-bar",
                TextInput {
                    value: text(),
                    placeholder: "搜索聊天记录",
                    style_kind: InputStyle::Search,
                    icon: IconName::Search,
                    clearable: true,
                    autofocus: true,
                    oninput: move |v| {
                        let mut text = text;
                        text.set(v);
                    },
                }
                Tooltip { text: "筛选",
                    IconButton { icon: IconName::Filter, onclick: filter_menu }
                }
            }
            div { class: "history-tabs",
                Tabs {
                    items: Tab::ALL.iter().map(|t| TabItem::new(t.id(), t.label())).collect::<Vec<_>>(),
                    value: current.id(),
                    onchange: move |id: String| tab.set(Tab::from_id(&id)),
                }
            }
            if !filter_text.is_empty() {
                div { class: "history-filter text-sub",
                    span { class: "history-filter-text", "{filter_text}" }
                    Button {
                        kind: ButtonKind::Flat,
                        onclick: move |_| {
                            set_if_alive(sender, None);
                            set_if_alive(until, None);
                        },
                        "清除筛选"
                    }
                }
            }
            ResultList {
                keys: keys(),
                status,
                loading_more: results.loading_more(),
                on_more: move |()| results.load_more(),
                estimate: 72.0,
                render: move |i: usize| match &rows_now[i] {
                    Row::Header(title) => rsx! { DateHeader { title: title.clone() } },
                    Row::Message(item) => rsx! {
                        MessageResultRow {
                            item: item.clone(),
                            query: query.clone(),
                            show_links: current == Tab::Links,
                            on_open,
                            menu,
                        }
                    },
                    Row::Tiles(tiles) => rsx! {
                        div { class: "media-row",
                            for tile in tiles.iter().cloned() {
                                MediaTile {
                                    key: "{tile.message_id}-{tile.thumbnail}",
                                    conversation: conversation.clone(),
                                    tile,
                                    album: album.clone(),
                                }
                            }
                        }
                    },
                    Row::File(entry) => rsx! {
                        FileListRow { entry: entry.clone(), use_24h }
                    },
                },
            }
        }
    }
}

/// 图片格点击进查看器，视频格点击打开；右键可定位到聊天。
#[component]
fn MediaTile(conversation: ConversationKey, tile: Tile, album: Rc<Vec<String>>) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let id = tile.message_id;
    let menu_key = conversation.clone();
    let video = tile.video.clone();
    let album_index = tile.album_index;
    rsx! {
        ContextMenuArea {
            class: "media-tile",
            items: move |()| {
                let Some(session) = state.session.peek().clone() else {
                    return Vec::new();
                };
                let key = menu_key.clone();
                vec![MenuEntry::item("定位到聊天", move || {
                    let (session, key) = (session.clone(), key.clone());
                    spawn_forever(async move {
                        locate_in_chat(overlays, &session, Some(&key), id).await;
                    });
                })]
            },
            if let Some(link) = video {
                div {
                    class: "media-video",
                    onclick: {
                        let source = FileSource { link: link.clone(), message_id: id, key: conversation.clone() };
                        move |_| {
                            let Some(session) = state.session.peek().clone() else { return };
                            let source = source.clone();
                            spawn(async move { open_file(overlays, &session, &source).await });
                        }
                    },
                    Icon { name: IconName::Video, size: 28.0 }
                    span { class: "text-caption", "{link.file_name}" }
                }
            } else {
                RemoteImage {
                    url: tile.thumbnail.clone(),
                    fill: true,
                    onclick: move |_| {
                        if let (Some(session), Some(index)) = (state.session.peek().as_ref(), album_index) {
                            windows::open_image_viewer(session.account_id(), album.as_ref().clone(), index);
                        }
                    },
                }
            }
        }
    }
}
