//! Chat composer.

mod bridge;
mod complete;
mod control;
mod core;
mod dialogs;
mod emoji;
mod language;
mod plugin_tools;
mod registry;
mod render;
mod upload;

pub use emoji::open_emoji_picker;
pub use registry::{filled_draft, is_composer_open};

use crate::state::{use_plugin_events, use_session, use_session_data, use_session_event};
use crate::ui::*;
use crate::widgets::{RemoteAvatar, RemoteImage, file_icon};
use crate::windows;
use complete::Item;
use control::{Ctl, use_ctl};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use zulip_core::ConversationKey;
use zulip_core::markdown::format::FormatKind;
use zulip_core::markdown::table::{MAX_COLUMNS, MAX_ROWS, TableAlign};
use zulip_plugin_host::PluginEvent;
use zulip_session::{ConnectionState, Session, SessionEvent};

const GRID_COLUMNS: usize = 10;
const GRID_ROWS: usize = 8;
const AVATAR_SIZE: f64 = 28.0;
const MIN_HEIGHT: f64 = 120.0;
const MAX_HEIGHT_RATIO: f64 = 0.6;

/// Height after drag resizing, shared by all conversations during this run.
static HEIGHT: AtomicU32 = AtomicU32::new(190);

/// Operations the chat page performs on the composer. `ChatPane` provides it with
/// `use_context_provider`; the composer reads it and responds.
#[derive(Clone, Copy, PartialEq)]
pub struct ComposerHandle {
    /// Text waiting to be inserted at the cursor (quotes, the plugin "fill into composer" button,
    /// etc.); set back to `None` once the composer takes it.
    pub insert: Signal<Option<String>>,
    /// When non-empty, this message is being edited: the composer shows its source and sending
    /// becomes saving.
    pub editing: Signal<Option<i64>>,
}

#[component]
pub fn Composer(conversation: ConversationKey) -> Element {
    match use_session() {
        Some(session) => rsx! {
            style { {include_str!("page.css")} }
            ComposerBody { session: crate::widgets::SessionRef(session), conversation }
        },
        None => rsx! {},
    }
}

/// Whether the current account can post in this conversation (the composer is editable).
pub fn can_post(session: &Arc<Session>, key: &ConversationKey) -> bool {
    match key {
        ConversationKey::Topic(topic) => session.channel(topic.stream_id).is_none_or(|channel| {
            session
                .permissions()
                .can_send_to_channel(&channel.permissions)
        }),
        ConversationKey::Dm(_) => true,
    }
}

#[component]
fn ComposerBody(session: crate::widgets::SessionRef, conversation: ConversationKey) -> Element {
    let overlays = use_overlays();
    let own_insert = use_signal(|| None);
    let own_editing = use_signal(|| None);
    let handle = try_consume_context::<ComposerHandle>().unwrap_or(ComposerHandle {
        insert: own_insert,
        editing: own_editing,
    });
    let ctl = use_ctl(session.0.clone(), conversation.clone(), overlays, handle);
    use_context_provider(|| ctl);

    let allowed_key = conversation.clone();
    let allowed = use_session_data(
        |e| {
            matches!(
                e,
                SessionEvent::ChannelChanged(_)
                    | SessionEvent::ChannelsChanged
                    | SessionEvent::PermissionsChanged
            )
        },
        move |s| can_post(s, &allowed_key),
    );
    use_session_event(move |event| match event {
        SessionEvent::UsersChanged | SessionEvent::ChannelsChanged | SessionEvent::EmojiChanged => {
            ctl.refresh_lookup();
        }
        SessionEvent::SettingsChanged(name) if name == "enter_sends" => ctl.send_options(),
        SessionEvent::Connection(state) => {
            let mut connected = ctl.connected;
            connected.set(state == ConnectionState::Connected);
        }
        SessionEvent::Resynced => {
            let mut connected = ctl.connected;
            connected.set(ctl.session().connection() == ConnectionState::Connected);
        }
        _ => {}
    });
    use_effect(move || ctl.on_editing(*handle.editing.read()));
    use_effect(move || {
        let text = handle.insert.read().clone();
        if let Some(text) = text {
            ctl.edit(|core| core.insert_smart(&text));
            let mut insert = handle.insert;
            insert.set(None);
        }
    });
    let account = session.0.account_id();
    use_hook({
        let conversation = conversation.clone();
        move || registry::register(account, conversation)
    });
    use_plugin_events(move |event| {
        if let PluginEvent::FillComposer {
            account_id,
            conv,
            text,
            replace,
            ..
        } = event
        {
            let session = ctl.session();
            let current = session.current_conversation();
            let key = ctl.key();
            if registry::fill_targets_composer(
                account_id,
                conv.as_ref(),
                account,
                &key,
                current.as_ref(),
            ) {
                ctl.fill(text, replace);
            }
        }
    });
    use_drop({
        let conversation = conversation.clone();
        move || {
            registry::unregister(account, conversation);
            ctl.shutdown();
        }
    });

    let permitted = allowed();
    let height = use_signal(|| f64::from(HEIGHT.load(Ordering::Relaxed)));
    let dragging = use_signal(|| false);
    rsx! {
        div { class: "composer", style: "height:{height}px",
            ResizeHandle { height }
            if permitted {
                Toolbar {}
            }
            EditBanner {}
            UploadList {}
            div {
                class: if dragging() { "composer-body dragging" } else { "composer-body" },
                ondragover: {
                    let mut dragging = dragging;
                    move |e: DragEvent| {
                        e.prevent_default();
                        if permitted && !dragging() {
                            dragging.set(true);
                        }
                    }
                },
                ondragleave: {
                    let mut dragging = dragging;
                    move |_| dragging.set(false)
                },
                ondrop: {
                    let mut dragging = dragging;
                    move |e: DragEvent| {
                        e.prevent_default();
                        dragging.set(false);
                        if permitted {
                            for file in e.files() {
                                ctl.upload_path(file.path());
                            }
                        }
                    }
                },
                ContextMenuArea {
                    class: "composer-context",
                    items: Callback::new(move |()| context_menu(ctl)),
                    div {
                        id: "{ctl.dom_id}",
                        class: "composer-editor scroll",
                        contenteditable: if permitted { "true" } else { "false" },
                        spellcheck: "false",
                        onmounted: move |_| ctl.start(),
                    }
                }
                if !permitted {
                    div { class: "composer-placeholder", "你没有在此频道发言的权限" }
                }
                TableBar {}
            }
            Footer { permitted }
            CompletionPopup {}
        }
    }
}

/// Drag the top edge to resize the height.
#[component]
fn ResizeHandle(height: Signal<f64>) -> Element {
    let mut drag = use_signal(|| None::<(f64, f64)>);
    let move_to = move |e: MouseEvent| {
        if let Some((start_y, start_height)) = drag() {
            let window = dioxus::desktop::window();
            let size = window.inner_size().to_logical::<f64>(window.scale_factor());
            let max = MIN_HEIGHT.max(size.height * MAX_HEIGHT_RATIO);
            let next = (start_height + start_y - e.client_coordinates().y).clamp(MIN_HEIGHT, max);
            height.set(next);
            HEIGHT.store(next as u32, Ordering::Relaxed);
        }
    };
    rsx! {
        div {
            class: "composer-resize",
            onmousedown: move |e| {
                e.prevent_default();
                drag.set(Some((e.client_coordinates().y, height())));
            },
        }
        if drag().is_some() {
            div {
                class: "composer-resize-mask",
                onmousemove: move_to,
                onmouseup: move |_| drag.set(None),
            }
        }
    }
}

// --------- Toolbar ---------

fn snippet_menu(ctl: Ctl) -> MenuEntry {
    let snippets = ctl.session().snippets();
    let mut items: Vec<MenuEntry> = snippets
        .iter()
        .map(|snippet| {
            let content = snippet.content.clone();
            MenuEntry::item(snippet.title.clone(), move || {
                ctl.edit(|core| core.insert_block(&content));
            })
        })
        .collect();
    if !items.is_empty() {
        items.push(MenuEntry::Separator);
    }
    items.push(MenuEntry::item("新建常用语…", move || ctl.create_snippet()).icon(IconName::Add));
    if !snippets.is_empty() {
        let delete = snippets
            .iter()
            .map(|snippet| {
                let (id, title) = (snippet.id, snippet.title.clone());
                MenuEntry::item(snippet.title.clone(), move || {
                    ctl.delete_snippet(id, title.clone());
                })
            })
            .collect();
        items.push(MenuEntry::submenu("删除常用语", delete).icon(IconName::Delete));
    }
    MenuEntry::submenu("常用语", items)
}

fn context_menu(ctl: Ctl) -> Vec<MenuEntry> {
    let selected = ctl.has_selection();
    vec![
        MenuEntry::item("剪切", move || ctl.cut())
            .icon(IconName::Cut)
            .disabled(!selected),
        MenuEntry::item("复制", move || {
            ctl.copy();
            ctl.refocus();
        })
        .icon(IconName::Copy)
        .disabled(!selected),
        MenuEntry::item("粘贴", move || ctl.paste_from_clipboard()).icon(IconName::Paste),
        MenuEntry::item("全选", move || ctl.edit(core::EditorCore::select_all)),
        MenuEntry::Separator,
        snippet_menu(ctl),
    ]
}

fn heading_menu(ctl: Ctl) -> Vec<MenuEntry> {
    let current = ctl.view.peek().heading;
    let mut items: Vec<MenuEntry> = (1..=6u8)
        .map(|level| {
            let label = ["一", "二", "三", "四", "五", "六"][usize::from(level) - 1];
            MenuEntry::item(format!("{label}级标题"), move || {
                ctl.edit(|c| c.heading(level))
            })
            .hint(format!("Ctrl+{level}"))
            .checked(current == level)
        })
        .collect();
    items.push(
        MenuEntry::item("段落", move || ctl.edit(|c| c.heading(0)))
            .hint("Ctrl+0")
            .checked(current == 0),
    );
    items
}

fn format_item(ctl: Ctl, label: &str, kind: FormatKind) -> MenuEntry {
    MenuEntry::item(label, move || ctl.edit(|c| c.format(kind)))
}

fn insert_menu(ctl: Ctl) -> Vec<MenuEntry> {
    let in_table = ctl.view.peek().table.is_some();
    let in_cell = ctl.core.peek().sel.cell.is_some();
    let topic = matches!(ctl.key(), ConversationKey::Topic(_));
    let time = MenuEntry::item("全局时间…", move || ctl.insert_time()).icon(IconName::Time);
    let mut items = Vec::new();
    if in_table {
        items.push(MenuEntry::item("上方插入段落", move || {
            ctl.edit(|c| c.paragraph_beside_table(true))
        }));
        items.push(MenuEntry::item("下方插入段落", move || {
            ctl.edit(|c| c.paragraph_beside_table(false))
        }));
        items.push(MenuEntry::Separator);
    }
    if in_cell {
        items.push(time);
    } else {
        items.extend([
            format_item(ctl, "公式块", FormatKind::MathBlock),
            format_item(ctl, "引用块", FormatKind::QuoteBlock),
            format_item(ctl, "折叠", FormatKind::Spoiler),
            time,
            format_item(ctl, "分隔线", FormatKind::Rule),
            MenuEntry::item("表格…", move || ctl.insert_table()).icon(IconName::ViewModule),
            MenuEntry::item("任务列表", move || {
                ctl.edit(|c| {
                    let edit = core::todo_edit(c.text());
                    c.apply(&edit);
                });
            }),
        ]);
        if topic {
            items.push(MenuEntry::item("投票…", move || ctl.create_poll()).icon(IconName::Poll));
        }
    }
    items.push(MenuEntry::Separator);
    items.push(format_item(ctl, "删除线", FormatKind::Strikethrough).icon(IconName::Strikethrough));
    items.push(format_item(ctl, "行内公式", FormatKind::Math).icon(IconName::Formula));
    items
}

#[component]
fn FormatButton(icon: IconName, tip: &'static str, kind: FormatKind) -> Element {
    let ctl = use_context::<Ctl>();
    let active = ctl.view.read().active.contains(&kind);
    rsx! {
        Tooltip { text: tip,
            IconButton {
                icon,
                class: if active { "active" } else { "" },
                onclick: move |_| ctl.edit(|c| c.format(kind)),
            }
        }
    }
}

#[component]
fn MenuButton(icon: IconName, tip: &'static str, items: Callback<(), Vec<MenuEntry>>) -> Element {
    let ctl = use_context::<Ctl>();
    rsx! {
        Tooltip { text: tip,
            IconButton {
                icon,
                onclick: move |e: MouseEvent| {
                    let at = e.client_coordinates();
                    ctl.overlays.show_menu(at.x, at.y, items.call(()));
                },
            }
        }
    }
}

#[component]
fn Toolbar() -> Element {
    let ctl = use_context::<Ctl>();
    let rich = !(ctl.source)();
    let in_code = ctl.view.read().in_code;
    let in_cell = ctl.view.read().table.is_some();
    rsx! {
        div {
            class: "composer-toolbar",
            onmousedown: |e| e.prevent_default(),
            div { class: "composer-tools",
                Tooltip { text: "表情",
                    IconButton {
                        icon: IconName::Smile,
                        onclick: move |e: MouseEvent| {
                            let at = e.client_coordinates();
                            emoji::open_emoji_picker(
                                ctl.overlays,
                                Rect::point(at.x, at.y),
                                ctl.session(),
                                move |name: String| {
                                    ctl.edit(|c| c.insert_smart(&format!(":{name}:")));
                                },
                            );
                        },
                    }
                }
                Tooltip { text: "发送文件",
                    IconButton { icon: IconName::Folder, onclick: move |_| ctl.pick_and_upload(false) }
                }
                Tooltip { text: "发送图片",
                    IconButton { icon: IconName::Image, onclick: move |_| ctl.pick_and_upload(true) }
                }
                plugin_tools::PluginTools {}
            }
            div { class: "composer-tools",
                // What cannot be used at the cursor is hidden: in code nothing (the whole group
                // collapses like in source mode), in a table cell no block formats (the insert
                // menu filters its own items)
                div { class: if rich && !in_code { "composer-format" } else { "composer-format collapsed" },
                    FormatButton { icon: IconName::Bold, tip: "加粗 (Ctrl+B)", kind: FormatKind::Bold }
                    FormatButton { icon: IconName::Italic, tip: "斜体 (Ctrl+I)", kind: FormatKind::Italic }
                    FormatButton { icon: IconName::Code, tip: "代码 (Ctrl+Shift+C)", kind: FormatKind::Code }
                    FormatButton { icon: IconName::Link, tip: "链接 (Ctrl+Shift+L)", kind: FormatKind::Link }
                    if !in_cell {
                        FormatButton { icon: IconName::Quote, tip: "引用", kind: FormatKind::Quote }
                        FormatButton { icon: IconName::ListNumbered, tip: "有序列表", kind: FormatKind::NumberedList }
                        FormatButton { icon: IconName::ListBulleted, tip: "无序列表", kind: FormatKind::BulletedList }
                        MenuButton {
                            icon: IconName::Format,
                            tip: "段落",
                            items: Callback::new(move |()| heading_menu(ctl)),
                        }
                    }
                    MenuButton {
                        icon: IconName::Add,
                        tip: "插入",
                        items: Callback::new(move |()| insert_menu(ctl)),
                    }
                }
                Tooltip { text: if rich { "显示源码" } else { "显示效果" },
                    IconButton {
                        icon: IconName::SourceCode,
                        class: if rich { "" } else { "active" },
                        onclick: move |_| ctl.set_source(rich),
                    }
                }
                Tooltip { text: "聊天记录",
                    IconButton {
                        icon: IconName::History,
                        onclick: move |_| windows::open_history(ctl.account(), ctl.key(), None),
                    }
                }
            }
        }
    }
}

#[component]
fn EditBanner() -> Element {
    let ctl = use_context::<Ctl>();
    let edit = ctl.edit.read().clone();
    let Some(edit) = edit else {
        return rsx! {};
    };
    let id = edit.id;
    rsx! {
        div { class: "composer-editbar",
            span {
                class: "composer-editbar-text",
                onclick: move |_| windows::open_chat(ctl.account(), ctl.key(), Some(id)),
                "编辑消息：{edit.preview}"
            }
            IconButton {
                icon: IconName::Close,
                size: 22.0,
                icon_size: 12.0,
                onclick: move |_| {
                    let mut editing = ctl.handle.editing;
                    editing.set(None);
                },
            }
        }
    }
}

#[component]
fn UploadList() -> Element {
    let ctl = use_context::<Ctl>();
    let uploads = ctl.uploads.read().clone();
    if uploads.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "composer-files",
            for row in uploads {
                FileCard {
                    key: "{row.id}",
                    name: row.name.clone(),
                    icon: file_icon(&row.name),
                    progress: row.progress,
                    menu: {
                        let id = row.id;
                        Callback::new(move |()| {
                            vec![MenuEntry::item("取消上传", move || ctl.cancel_upload(id))]
                        })
                    },
                }
            }
        }
    }
}

// --------- Table toolbar ---------

#[component]
fn TableBar() -> Element {
    let ctl = use_context::<Ctl>();
    let rect = *ctl.table_rect.read();
    let view = ctl.view.read().clone();
    let Some(rect) = rect else {
        return rsx! {};
    };
    if view.table.is_none() {
        return rsx! {};
    }
    let align_button = |icon: IconName, tip: &'static str, align: TableAlign| {
        rsx! {
            Tooltip { text: tip,
                IconButton {
                    icon,
                    size: 26.0,
                    class: if view.align == align { "active" } else { "" },
                    onclick: move |_| ctl.edit(|c| c.table_op(|t, caret| t.set_align(caret, align))),
                }
            }
        }
    };
    rsx! {
        div {
            class: "composer-table-bar",
            style: "top:{rect.top + 2.0}px;left:{rect.left}px;width:{rect.width}px",
            onmousedown: |e| e.prevent_default(),
            div {
                Tooltip { text: "调整表格大小",
                    IconButton {
                        icon: IconName::ViewModule,
                        size: 26.0,
                        onclick: move |e: MouseEvent| {
                            let at = e.client_coordinates();
                            open_size_grid(ctl, Rect::point(at.x, at.y));
                        },
                    }
                }
                {align_button(IconName::AlignLeft, "左对齐", TableAlign::Left)}
                {align_button(IconName::AlignCenter, "居中", TableAlign::Center)}
                {align_button(IconName::AlignRight, "右对齐", TableAlign::Right)}
            }
            // "More actions" and "Delete table" are fixed at the far right of the composer and do
            // not move with table width or horizontal scrolling
            div {
                Tooltip { text: "更多操作",
                    IconButton {
                        icon: IconName::MoreVertical,
                        size: 26.0,
                        onclick: move |e: MouseEvent| {
                            let at = e.client_coordinates();
                            ctl.overlays.show_menu(at.x, at.y, table_menu(ctl));
                        },
                    }
                }
                Tooltip { text: "删除表格",
                    IconButton {
                        icon: IconName::Delete,
                        size: 26.0,
                        onclick: move |_| ctl.edit(|c| c.table_op(|t, _| Some(t.delete_table()))),
                    }
                }
            }
        }
    }
}

fn table_menu(ctl: Ctl) -> Vec<MenuEntry> {
    let op =
        |label: &str, run: fn(&mut core::EditorCore)| MenuEntry::item(label, move || ctl.edit(run));
    vec![
        op("上方插入行", |c| {
            c.table_op(|t, at| t.insert_row(at, false))
        }),
        op("下方插入行", |c| {
            c.table_op(|t, at| t.insert_row(at, true))
        }),
        op("左侧插入列", |c| {
            c.table_op(|t, at| t.insert_column(at, false))
        }),
        op("右侧插入列", |c| {
            c.table_op(|t, at| t.insert_column(at, true))
        }),
        MenuEntry::Separator,
        op("上移该行", |c| c.table_op(|t, at| t.move_row(at, -1))),
        op("下移该行", |c| c.table_op(|t, at| t.move_row(at, 1))),
        op("左移该列", |c| {
            c.table_op(|t, at| t.move_column(at, -1))
        }),
        op("右移该列", |c| c.table_op(|t, at| t.move_column(at, 1))),
        MenuEntry::Separator,
        op("删除行", |c| c.table_op(|t, at| Some(t.delete_row(at)))).danger(),
        op("删除列", |c| c.table_op(|t, at| t.delete_column(at))).danger(),
        MenuEntry::Separator,
        MenuEntry::item("复制表格", move || {
            ctl.copy_table();
            ctl.refocus();
        }),
        op("格式化表格源码", |c| {
            c.table_op(|t, at| Some(t.format(at)))
        }),
        op("删除表格", |c| {
            c.table_op(|t, _| Some(t.delete_table()))
        })
        .danger(),
    ]
}

fn open_size_grid(ctl: Ctl, anchor: Rect) {
    ctl.overlays
        .open_popover(anchor, PopoverOptions::default(), move |handle| {
            rsx! { SizeGrid { handle, ctl } }
        });
}

#[component]
fn SizeGrid(handle: PopoverHandle, ctl: Ctl) -> Element {
    let (current_columns, current_rows) = ctl.view.peek().table.unwrap_or((2, 1));
    let mut hover = use_signal(|| (current_columns, current_rows));
    let (columns, rows) = hover();
    let shown_columns = GRID_COLUMNS.max(current_columns).min(MAX_COLUMNS);
    let shown_rows = GRID_ROWS.max(current_rows).min(MAX_ROWS);
    rsx! {
        div {
            class: "composer-grid",
            style: "grid-template-columns:repeat({shown_columns},13px)",
            for r in 1..=shown_rows {
                for c in 1..=shown_columns {
                    div {
                        key: "{r}-{c}",
                        class: match (c <= columns && r <= rows, r == 1) {
                            (true, true) => "composer-grid-cell on head",
                            (true, false) => "composer-grid-cell on",
                            _ => "composer-grid-cell",
                        },
                        onmouseenter: move |_| hover.set((c.max(2), r)),
                        onclick: move |_| {
                            handle.close();
                            ctl.edit(|core| core.table_op(|t, at| Some(t.resize(at, c.max(2), r))));
                        },
                    }
                }
            }
        }
        div { class: "composer-grid-size", "{columns} 列 × {rows} 行" }
    }
}

// --------- Bottom ---------

fn send_menu(ctl: Ctl) -> Vec<MenuEntry> {
    let enter_sends = ctl.session().settings().enter_sends;
    let editing = ctl.edit.peek().is_some();
    let can_send = ctl.can_send() && !editing;
    vec![
        MenuEntry::item("按Enter键发送消息", move || ctl.set_enter_sends(true))
            .checked(enter_sends),
        MenuEntry::item("按Ctrl+Enter键发送消息", move || {
            ctl.set_enter_sends(false)
        })
        .checked(!enter_sends),
        MenuEntry::Separator,
        MenuEntry::item("定时发送…", move || ctl.send_later())
            .icon(IconName::Time)
            .disabled(!can_send),
        MenuEntry::item("发送并提醒…", move || ctl.send_with_reminder()).disabled(!can_send),
    ]
}

#[component]
fn Footer(permitted: bool) -> Element {
    let ctl = use_context::<Ctl>();
    let view = ctl.view.read().clone();
    let uploading = !ctl.uploads.read().is_empty();
    let sending = *ctl.sending.read();
    let editing = ctl.edit.read().is_some();
    let connected = *ctl.connected.read();
    let enabled = permitted
        && connected
        && !sending
        && !uploading
        && (editing || (!view.blank && !view.quote_only));
    rsx! {
        div {
            class: "composer-foot",
            onmousedown: |e| e.prevent_default(),
            Button {
                kind: ButtonKind::Primary,
                class: "composer-send",
                disabled: !enabled,
                onclick: move |_| ctl.send(),
                if editing { "保存" } else { "发送" }
            }
            span { class: "composer-send-split" }
            // The send-mode setting does not need posting permission and is always available; menu
            // items that cannot be used disable themselves
            Tooltip { text: "发送设置",
                IconButton {
                    icon: IconName::ChevronDown,
                    class: "composer-send-more",
                    size: 32.0,
                    icon_size: 14.0,
                    onclick: move |e: MouseEvent| {
                        let at = e.client_coordinates();
                        ctl.overlays.show_menu(at.x, at.y, send_menu(ctl));
                    },
                }
            }
        }
    }
}

// --------- Completion popup ---------

#[component]
fn CompletionPopup() -> Element {
    let ctl = use_context::<Ctl>();
    let popup = ctl.popup.read().clone();
    let Some(popup) = popup else {
        return rsx! {};
    };
    rsx! {
        div {
            class: "overlay-card scroll composer-popup",
            style: "left:min({popup.x}px, calc(100vw - 310px));bottom:calc(100vh - {popup.y}px + 4px)",
            onmousedown: |e| e.prevent_default(),
            for (index, item) in popup.items.iter().cloned().enumerate() {
                ListRow {
                    key: "{item.insert}",
                    density: RowDensity::Compact,
                    leading: leading(&item),
                    title: item.title.clone(),
                    subtitle: item.subtitle.clone(),
                    selected: index == popup.selected,
                    onclick: move |_| ctl.accept_popup(Some(index)),
                }
            }
        }
    }
}

fn leading(item: &Item) -> Option<Element> {
    if let Some(text) = &item.avatar_text {
        return Some(rsx! {
            RemoteAvatar { text: text.clone(), url: item.avatar_url.clone(), size: AVATAR_SIZE }
        });
    }
    if let Some(glyph) = &item.glyph {
        return Some(rsx! { span { class: "composer-popup-glyph", "{glyph}" } });
    }
    item.avatar_url.as_ref().map(|url| {
        rsx! {
            span { class: "composer-popup-glyph",
                RemoteImage { url: url.clone(), size: 22.0 }
            }
        }
    })
}
