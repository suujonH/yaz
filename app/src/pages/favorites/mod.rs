//! 收藏窗口：已加星标的消息，按分类和关键词筛选，按日期分组，可定位、转发、取消收藏，支持多选。

use crate::shell::{WindowShell, use_window_args};
use crate::state::{use_app_state, use_session_event};
use crate::ui::*;
use crate::widgets::*;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use zulip_core::html::to_plain_text;
use zulip_session::queries::DEFAULT_PAGE_SIZE;
use zulip_session::{ContentFilter, MessageItem, MessageSearch, QueryScope, SessionEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    All,
    Chat,
    Media,
    Files,
    Links,
}

impl Category {
    const ALL: [Self; 5] = [Self::All, Self::Chat, Self::Media, Self::Files, Self::Links];

    const fn filter(self) -> ContentFilter {
        match self {
            Self::All => ContentFilter::All,
            Self::Chat => ContentFilter::PlainChat,
            Self::Media => ContentFilter::Media,
            Self::Files => ContentFilter::Files,
            Self::Links => ContentFilter::Links,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::All => "全部收藏",
            Self::Chat => "聊天记录",
            Self::Media => "图片与视频",
            Self::Files => "文件",
            Self::Links => "链接",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            _ => self.title(),
        }
    }

    const fn icon(self) -> IconName {
        match self {
            Self::All => IconName::App,
            Self::Chat => IconName::Time,
            Self::Media => IconName::Image,
            Self::Files => IconName::Folder,
            Self::Links => IconName::Link,
        }
    }
}

#[component]
pub fn FavoritesWindow() -> Element {
    rsx! {
        WindowShell { title: "收藏", FavoritesFrame {} }
    }
}

/// 窗口参数是账号 ID，窗口内可以切换。
#[component]
fn FavoritesFrame() -> Element {
    let account = use_window_args::<i64>();
    let epoch = use_window_account(account);
    use_window_keys(|_| {});
    rsx! {
        FavoritesBody { key: "{account}-{epoch}", account }
    }
}

#[component]
fn FavoritesBody(account: Signal<i64>) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let mut category = use_signal(|| Category::All);
    let mut query = use_signal(String::new);
    let applied = use_debounced(query.into(), SEARCH_DEBOUNCE);
    let search = use_memo(move || {
        Some(MessageSearch {
            scope: QueryScope::Starred,
            filter: category().filter(),
            text: applied().trim().to_owned(),
            ..MessageSearch::default()
        })
    });
    let results = use_message_results(search.into(), DEFAULT_PAGE_SIZE);
    let selection = use_selection();
    use_session_event(move |event| match event {
        SessionEvent::StarredChanged { id, starred: false } => {
            results.remove(&[id]);
            selection.retain(|selected| selected != id);
        }
        SessionEvent::StarredChanged { starred: true, .. } if applied.peek().trim().is_empty() => {
            results.reload();
        }
        _ => {}
    });
    let rows = use_memo(move || std::rc::Rc::new(message_rows(&results.items())));
    let keys = use_memo(move || row_keys(&rows.read(), |item| item.message.id as u64));

    let menu = Callback::new(move |item: MessageItem| {
        let Some(session) = state.session.peek().clone() else {
            return Vec::new();
        };
        let id = item.message.id;
        let (key, unstar_session, forward_session) =
            (item.message.key.clone(), session.clone(), session.clone());
        let text = to_plain_text(&item.message.content_html);
        vec![
            MenuEntry::item("取消收藏", move || {
                let session = unstar_session.clone();
                spawn_forever(async move {
                    unstar(overlays, &session, &[id]).await;
                });
            }),
            MenuEntry::item("复制", move || overlays.copy_text(&text)),
            MenuEntry::item("转发", move || {
                let session = forward_session.clone();
                spawn_forever(async move {
                    forward_messages(overlays, &session, vec![id]).await;
                });
            }),
            MenuEntry::item("定位到聊天", {
                let session = session.clone();
                move || {
                    let (session, key) = (session.clone(), key.clone());
                    spawn_forever(async move {
                        locate_in_chat(overlays, &session, Some(&key), id).await;
                    });
                }
            }),
            MenuEntry::Separator,
            MenuEntry::item("多选", move || {
                selection.set_active(true);
                selection.toggle(id);
            }),
        ]
    });
    let on_open = move |item: MessageItem| {
        let Some(session) = state.session.peek().clone() else {
            return;
        };
        spawn(async move {
            locate_in_chat(overlays, &session, Some(&item.message.key), item.message.id).await;
        });
    };

    let current = category();
    let text = applied().trim().to_owned();
    let empty = if text.is_empty() {
        "暂无收藏"
    } else {
        "未找到相关收藏"
    };
    let rows_now = rows();
    let multi = selection.is_active();
    rsx! {
        SplitView {
            nav: rsx! {
                AccountSwitcher { value: account(), onchange: move |id| account.set(id) }
                TextInput {
                    value: query(),
                    placeholder: "搜索",
                    style_kind: InputStyle::Search,
                    icon: IconName::Search,
                    clearable: true,
                    oninput: move |v| query.set(v),
                }
                for item in Category::ALL {
                    NavRow {
                        key: "{item.label()}",
                        icon: item.icon(),
                        label: item.label(),
                        selected: item == current,
                        onclick: move |_| category.set(item),
                    }
                }
            },
            PaneHeader { title: current.title(),
                Button {
                    kind: ButtonKind::Flat,
                    onclick: move |_| selection.set_active(!multi),
                    if multi { "完成" } else { "多选" }
                }
            }
            ResultList {
                keys: keys(),
                status: results.status(empty),
                loading_more: results.loading_more(),
                on_more: move |()| results.load_more(),
                render: move |i: usize| match &rows_now[i] {
                    DateRow::Header(title) => rsx! { DateHeader { title: title.clone() } },
                    DateRow::Item(item) => rsx! {
                        MessageResultRow {
                            item: item.clone(),
                            query: text.clone(),
                            show_conversation: true,
                            selection,
                            on_open,
                            menu,
                        }
                    },
                },
            }
            SelectionBar {
                count: selection.count(),
                on_close: move |()| selection.clear(),
                Button {
                    onclick: move |_| {
                        let ids = selection.ids();
                        selection.clear();
                        let Some(session) = state.session.peek().clone() else { return };
                        spawn(async move {
                            unstar(overlays, &session, &ids).await;
                        });
                    },
                    "取消收藏"
                }
                Button {
                    onclick: move |_| {
                        let ids = selection.ids();
                        let Some(session) = state.session.peek().clone() else { return };
                        spawn(async move {
                            forward_messages(overlays, &session, ids).await;
                        });
                    },
                    "转发"
                }
            }
        }
    }
}
