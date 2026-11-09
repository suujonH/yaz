//! Chat records window: the conversations whose messages match the search on the left, the matching
//! messages of the selected one on the right. A message can be located in the main window, or
//! viewed in context inside the window (loading older and newer messages while scrolling).

use crate::shell::{WindowShell, use_window_args};
use crate::state::use_app_state;
use crate::ui::*;
use crate::widgets::*;
use crate::windows;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::ConversationKey;
use zulip_core::html::to_plain_text;
use zulip_session::queries::DEFAULT_PAGE_SIZE;
use zulip_session::{
    ContextDirection, MessageGroup, MessageItem, MessageSearch, QueryScope, Session,
};

/// Messages fetched on each side of the message when viewing context, and per page after that.
const CONTEXT_PAGE: u32 = 30;

#[derive(Clone, Debug, PartialEq)]
pub struct RecordsArgs {
    pub query: String,
    /// The conversation selected at first; `None`: the first one found.
    pub key: Option<ConversationKey>,
}

#[component]
pub fn MessageRecordsWindow() -> Element {
    rsx! {
        WindowShell { title: "聊天记录", RecordsFrame {} }
    }
}

/// Locate the message (it may have been moved) and show it in the main window.
async fn locate_in_main(overlays: Overlays, session: Arc<Session>, key: ConversationKey, id: i64) {
    if let Some(located) = overlays
        .attempt("定位失败", session.locate_message(Some(&key), id))
        .await
    {
        windows::show_chat_in_main(session.account_id(), located.key, Some(located.message_id));
    }
}

/// Context menu of a message. `context`: "view context" is offered (not inside the context view).
fn message_menu(
    overlays: Overlays,
    session: Arc<Session>,
    item: &MessageItem,
    context: Option<Signal<Option<i64>>>,
) -> Vec<MenuEntry> {
    let id = item.message.id;
    let key = item.message.key.clone();
    let text = to_plain_text(&item.message.content_html);
    let locate_session = session.clone();
    let mut items = vec![MenuEntry::item("定位到聊天", move || {
        spawn_forever(locate_in_main(
            overlays,
            locate_session.clone(),
            key.clone(),
            id,
        ));
    })];
    if let Some(context) = context {
        items.push(MenuEntry::item("查看上下文", move || {
            let mut context = context;
            context.set(Some(id));
        }));
    }
    items.push(MenuEntry::item("复制", move || overlays.copy_text(&text)));
    items.push(MenuEntry::item("转发", move || {
        let session = session.clone();
        spawn_forever(async move {
            forward_messages(overlays, &session, vec![id]).await;
        });
    }));
    items
}

/// Reopening the window with a new search replaces the text and the selection.
#[component]
fn RecordsFrame() -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let args = use_window_args::<RecordsArgs>();
    use_window_keys(|_| {});
    let mut text = use_signal(|| args.peek().query.clone());
    let mut selected = use_signal(|| args.peek().key.clone());
    use_effect(move || {
        let RecordsArgs { query, key } = args();
        text.set(query);
        selected.set(key);
    });
    let applied = use_debounced(text.into(), SEARCH_DEBOUNCE);
    let mut groups = use_signal(Vec::<MessageGroup>::new);
    let mut searching = use_signal(|| false);
    use_resource(move || async move {
        let query = applied().trim().to_owned();
        let Some(session) = state.session.peek().clone() else {
            return;
        };
        if query.is_empty() {
            groups.set(Vec::new());
            return;
        }
        searching.set(true);
        let found = overlays
            .attempt("搜索失败", session.search_message_groups(&query))
            .await;
        searching.set(false);
        if let Some(list) = found {
            // Keep the selection when it is still found, otherwise select the first
            let keep = selected
                .peek()
                .as_ref()
                .is_some_and(|key| list.iter().any(|g| &g.key == key));
            if !keep {
                selected.set(list.first().map(|g| g.key.clone()));
            }
            groups.set(list);
        }
    });

    let query = applied().trim().to_owned();
    let list = groups();
    let current = selected();
    let count_text = current
        .as_ref()
        .and_then(|key| list.iter().find(|g| &g.key == key))
        .map(|g| g.count_text.clone());
    let status = if searching() && list.is_empty() {
        Some("搜索中…")
    } else if list.is_empty() && !query.is_empty() {
        Some("没有找到相关的聊天记录")
    } else {
        None
    };
    rsx! {
        div { class: "records",
            div { class: "records-bar",
                TextInput {
                    value: text(),
                    placeholder: "搜索聊天记录",
                    style_kind: InputStyle::Search,
                    icon: IconName::Search,
                    clearable: true,
                    autofocus: true,
                    oninput: move |v| text.set(v),
                }
            }
            div { class: "records-main",
                div { class: "records-groups scroll",
                    for group in list.iter().cloned() {
                        ListRow {
                            key: "{group.key}",
                            density: RowDensity::Normal,
                            leading: rsx! { Avatar { text: group.title.clone(), size: 36.0 } },
                            title: group.title.clone(),
                            subtitle: group.count_text.clone(),
                            selected: current.as_ref() == Some(&group.key),
                            onclick: {
                                let key = group.key.clone();
                                move |_| selected.set(Some(key.clone()))
                            },
                        }
                    }
                    if let Some(text) = status {
                        div { class: "records-status text-sub", "{text}" }
                    }
                }
                div { class: "records-detail",
                    if let (Some(key), Some(count_text)) = (current, count_text) {
                        RecordsDetail {
                            key: "{key}",
                            conversation: key.clone(),
                            query: query.clone(),
                            count_text,
                        }
                    }
                }
            }
        }
    }
}

/// The selected conversation: its matching messages, or one message in context.
#[component]
fn RecordsDetail(conversation: ConversationKey, query: String, count_text: String) -> Element {
    let state = use_app_state();
    let context = use_signal(|| None::<i64>);
    let mut context_open = context;
    if let Some(anchor) = context() {
        return rsx! {
            ContextView {
                key: "{anchor}",
                conversation,
                anchor,
                on_back: move |()| context_open.set(None),
            }
        };
    }
    let enter = {
        let key = conversation.clone();
        move |_| {
            if let Some(account) = *state.account.peek() {
                windows::show_chat_in_main(account, key.clone(), None);
            }
        }
    };
    rsx! {
        div { class: "records-head",
            span { class: "records-count text-sub", "找到{count_text}" }
            Button { kind: ButtonKind::Flat, onclick: enter,
                "进入聊天"
                Icon { name: IconName::ChevronRight, size: 12.0 }
            }
        }
        RecordsResults { conversation, query, context }
    }
}

#[component]
fn RecordsResults(
    conversation: ConversationKey,
    query: String,
    context: Signal<Option<i64>>,
) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let search = use_memo(use_reactive!(|conversation, query| {
        Some(MessageSearch {
            scope: QueryScope::Conversation(conversation),
            text: query,
            ..MessageSearch::default()
        })
    }));
    let results = use_message_results(search.into(), DEFAULT_PAGE_SIZE);
    let rows = use_memo(move || Rc::new(message_rows(&results.items())));
    let keys = use_memo(move || {
        Rc::new(
            rows.read()
                .iter()
                .map(|row| match row {
                    DateRow::Header(title) => header_key(title),
                    DateRow::Item(item) => item.message.id as u64,
                })
                .collect::<Vec<u64>>(),
        )
    });
    let locate = Callback::new(move |item: MessageItem| {
        if let Some(session) = state.session.peek().clone() {
            spawn(locate_in_main(
                overlays,
                session,
                item.message.key.clone(),
                item.message.id,
            ));
        }
    });
    let menu = Callback::new(move |item: MessageItem| {
        state
            .session
            .peek()
            .clone()
            .map_or_else(Vec::new, |session| {
                message_menu(overlays, session, &item, Some(context))
            })
    });
    let rows_now = rows();
    rsx! {
        ResultList {
            keys: keys(),
            status: results.status("没有找到相关的聊天记录"),
            loading_more: results.loading_more(),
            on_more: move |()| results.load_more(),
            estimate: 72.0,
            render: move |i: usize| match &rows_now[i] {
                DateRow::Header(title) => rsx! { DateHeader { title: title.clone() } },
                DateRow::Item(item) => rsx! {
                    MessageResultRow {
                        item: item.clone(),
                        query: query.clone(),
                        on_open: locate,
                        menu,
                        jump: locate,
                    }
                },
            },
        }
    }
}

/// The messages around `anchor`, oldest first; older ones load at the top and newer ones at the
/// bottom while scrolling.
#[component]
fn ContextView(conversation: ConversationKey, anchor: i64, on_back: EventHandler<()>) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let list = use_virtual_list(VirtualListConfig {
        estimate: 72.0,
        ..VirtualListConfig::default()
    });
    let mut items = use_signal(|| Rc::new(Vec::<MessageItem>::new()));
    let mut found_oldest = use_signal(|| false);
    let mut found_newest = use_signal(|| false);
    let mut loading = use_signal(|| true);
    let mut failed = use_signal(|| false);
    // One page loading in each direction at a time
    let busy_older = use_signal(|| false);
    let busy_newer = use_signal(|| false);

    {
        let conversation = conversation.clone();
        let list = list.clone();
        use_hook(move || {
            spawn(async move {
                let Some(session) = state.session.peek().clone() else {
                    return;
                };
                let page = overlays
                    .attempt(
                        "加载失败",
                        session.message_context(
                            &conversation,
                            anchor,
                            ContextDirection::Around,
                            CONTEXT_PAGE,
                        ),
                    )
                    .await;
                loading.set(false);
                let Some(page) = page else {
                    failed.set(true);
                    return;
                };
                found_oldest.set(page.found_oldest);
                found_newest.set(page.found_newest);
                items.set(Rc::new(page.items));
                list.scroll_to_key(anchor as u64, ScrollAlign::Center);
            });
        });
    }

    let load = Callback::new(move |direction: ContextDirection| {
        let (mut busy, done) = match direction {
            ContextDirection::Older => (busy_older, found_oldest),
            _ => (busy_newer, found_newest),
        };
        if *busy.peek() || *done.peek() || *loading.peek() {
            return;
        }
        let edge = {
            let current = items.peek();
            match direction {
                ContextDirection::Older => current.first().map(|i| i.message.id),
                _ => current.last().map(|i| i.message.id),
            }
        };
        let (Some(edge), Some(session)) = (edge, state.session.peek().clone()) else {
            return;
        };
        let key = conversation.clone();
        busy.set(true);
        spawn(async move {
            let page = overlays
                .attempt(
                    "加载失败",
                    session.message_context(&key, edge, direction, CONTEXT_PAGE),
                )
                .await;
            busy.set(false);
            let Some(page) = page else { return };
            let mut next: Vec<MessageItem> = items.peek().as_ref().clone();
            match direction {
                ContextDirection::Older => {
                    found_oldest.set(page.found_oldest);
                    let mut older = page.items;
                    older.extend(next);
                    next = older;
                }
                _ => {
                    found_newest.set(page.found_newest);
                    next.extend(page.items);
                }
            }
            items.set(Rc::new(next));
        });
    });

    let all = items();
    let keys = Rc::new(
        all.iter()
            .map(|i| i.message.id as u64)
            .collect::<Vec<u64>>(),
    );
    let locate = Callback::new(move |item: MessageItem| {
        if let Some(session) = state.session.peek().clone() {
            spawn(locate_in_main(
                overlays,
                session,
                item.message.key.clone(),
                item.message.id,
            ));
        }
    });
    let menu = Callback::new(move |item: MessageItem| {
        state
            .session
            .peek()
            .clone()
            .map_or_else(Vec::new, |session| {
                message_menu(overlays, session, &item, None)
            })
    });
    let status = if loading() {
        Some("加载中…")
    } else if failed() {
        Some("加载失败")
    } else if all.is_empty() {
        Some("消息已删除或无法查看")
    } else {
        None
    };
    rsx! {
        div { class: "records-head",
            Button { kind: ButtonKind::Flat, onclick: move |_| on_back.call(()),
                Icon { name: IconName::ChevronLeft, size: 12.0 }
                "返回"
            }
        }
        div { class: "result-list",
            VirtualList {
                handle: list,
                keys,
                render: move |i: usize| rsx! {
                    MessageResultRow {
                        item: all[i].clone(),
                        query: "",
                        highlighted: all[i].message.id == anchor,
                        on_open: locate,
                        menu,
                        jump: locate,
                    }
                },
                on_reach_top: move |()| load.call(ContextDirection::Older),
                on_reach_bottom: move |()| load.call(ContextDirection::Newer),
            }
            if let Some(text) = status {
                div { class: "result-status text-sub", "{text}" }
            }
            if busy_older() || busy_newer() {
                div { class: "result-more text-sub", "加载中…" }
            }
        }
    }
}
