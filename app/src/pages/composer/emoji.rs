//! Emoji picker.

use crate::ui::*;
use crate::widgets::RemoteImage;
use dioxus::prelude::*;
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::emoji::EmojiEntry;
use zulip_session::Session;

const PER_ROW: usize = 9;
const CELL_SIZE: f64 = 36.0;

/// Open the emoji picker above `anchor`; after a pick, call `on_pick` (the emoji name).
pub fn open_emoji_picker(
    overlays: Overlays,
    anchor: Rect,
    session: Arc<Session>,
    on_pick: impl Fn(String) + 'static,
) {
    // The callback belongs to the picker itself: the entry that opened it (e.g. a context menu) may
    // already be closed
    let on_pick = Rc::new(on_pick);
    overlays.open_popover(
        anchor,
        PopoverOptions {
            placement: Placement::Top,
            ..PopoverOptions::default()
        },
        move |handle| {
            rsx! {
                EmojiPicker {
                    session: crate::widgets::SessionRef(session.clone()),
                    handle,
                    on_pick: {
                        let on_pick = on_pick.clone();
                        move |name: String| on_pick(name)
                    },
                }
            }
        },
    );
}

#[derive(PartialEq)]
enum Row {
    Title(&'static str),
    Cells(Vec<EmojiEntry>),
}

fn rows(session: &Session, query: &str) -> Vec<Row> {
    let catalog = session.emoji_catalog();
    let mut rows = Vec::new();
    let chunks = |rows: &mut Vec<Row>, list: Vec<EmojiEntry>| {
        rows.extend(list.chunks(PER_ROW).map(|chunk| Row::Cells(chunk.to_vec())));
    };
    if query.is_empty() {
        let recent = session.recent_emoji();
        if !recent.is_empty() {
            rows.push(Row::Title("最近使用"));
            chunks(&mut rows, recent);
        }
        rows.push(Row::Title("全部表情"));
        chunks(&mut rows, catalog.all().to_vec());
    } else {
        chunks(
            &mut rows,
            catalog.search(query).into_iter().cloned().collect(),
        );
    }
    rows
}

#[component]
fn EmojiPicker(
    session: crate::widgets::SessionRef,
    handle: PopoverHandle,
    on_pick: Callback<String>,
) -> Element {
    let mut query = use_signal(String::new);
    let list = use_virtual_list(VirtualListConfig {
        estimate: CELL_SIZE,
        overscan: 200.0,
        stick_to_bottom: false,
        start_at_bottom: false,
    });
    let manager = session.0.clone();
    let rows = use_memo(move || Rc::new(rows(&manager, query.read().trim())));
    let current = rows.read().clone();
    let keys: Rc<Vec<u64>> = Rc::new((0..current.len() as u64).collect());
    let recorder = session.0.clone();
    rsx! {
        div { class: "composer-emoji",
            TextInput {
                value: query(),
                placeholder: "搜索表情",
                icon: IconName::Search,
                autofocus: true,
                clearable: true,
                oninput: move |v| query.set(v),
            }
            div { class: "composer-emoji-list",
                VirtualList {
                    handle: list,
                    keys,
                    render: Callback::new(move |index: usize| {
                        let current = rows.read().clone();
                        match current.get(index) {
                            Some(Row::Title(title)) => rsx! { div { class: "composer-emoji-title", "{title}" } },
                            Some(Row::Cells(cells)) => {
                                let recorder = recorder.clone();
                                rsx! {
                                    div { class: "composer-emoji-row",
                                        for entry in cells.clone() {
                                            EmojiCell {
                                                key: "{entry.name}",
                                                entry,
                                                onpick: {
                                                    let recorder = recorder.clone();
                                                    move |name: String| {
                                                        recorder.add_recent_emoji(&name);
                                                        on_pick.call(name);
                                                        handle.close();
                                                    }
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                            None => rsx! {},
                        }
                    }),
                }
            }
        }
    }
}

#[component]
fn EmojiCell(entry: EmojiEntry, onpick: EventHandler<String>) -> Element {
    let name = entry.name.clone();
    rsx! {
        button {
            class: "composer-emoji-cell",
            r#type: "button",
            title: ":{entry.name}:",
            onclick: move |_| onpick.call(name.clone()),
            match (&entry.text, &entry.image_url) {
                (_, Some(url)) => rsx! { RemoteImage { url: url.clone(), size: 24.0 } },
                (Some(text), None) => rsx! { "{text}" },
                _ => rsx! {},
            }
        }
    }
}
