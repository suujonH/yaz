//! Message actions: context menu, links, images, files, copy.

use super::content::{html_to_markdown, table_markdown};
use super::dialogs;
use super::message::ReactionGroup;
use super::store::Chat;
use crate::pages::composer::open_emoji_picker;
use crate::platform::{self, ClipboardImage};
use crate::services::plugins;
use crate::state::{AppCommand, send_command};
use crate::ui::{Icon, IconName, MenuEntry, Overlays, Rect};
use crate::widgets::{
    self, FileSource, RemoteImage, ask_future_time, menu_conv, open_file, open_topic_card,
    open_user_card, plugin_context_menus, plugin_menu_entries,
};
use crate::windows;
use dioxus::prelude::*;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use zulip_core::ConversationKey;
use zulip_core::emoji::{self, EmojiEntry};
use zulip_core::html::{
    extract_inline_images, extract_uploads, normalize_upload_path, to_plain_text, upload_link_of,
};
use zulip_core::links;
use zulip_core::model::{ChatMessage, MessageState};
use zulip_core::text::time;
use zulip_plugin_host::{ContextMenuTarget, MenuContext};
use zulip_session::{DeleteTarget, Session};

const QUICK_REACTIONS: usize = 7;
const CARD_ANCHOR: f64 = 4.0;
const HOUR: i64 = 3600;
const STAR_FLY_MS: u64 = 500;

fn anchor_at(x: f64, y: f64) -> Rect {
    Rect {
        x,
        y,
        w: CARD_ANCHOR,
        h: CARD_ANCHOR,
    }
}

pub fn open_title_card(chat: Chat, e: &MouseEvent) {
    let at = e.client_coordinates();
    let anchor = anchor_at(at.x, at.y);
    match chat.current_key() {
        key @ ConversationKey::Topic(_) => open_topic_card(chat.overlays, anchor, key),
        ConversationKey::Dm(d) => {
            let self_id = chat.session().self_id();
            let others = d.other_user_ids();
            if let [user] = others {
                open_user_card(chat.overlays, anchor, *user);
            } else if d.is_self_dm() {
                open_user_card(chat.overlays, anchor, self_id);
            }
        }
    }
}

fn open_external(chat: Chat, url: &str) {
    chat.overlays.open_file(Path::new(url));
}

pub fn open_in_browser(chat: Chat) {
    open_external(chat, &super::web_link(chat));
}

/// A link address in a message may be a relative path.
pub fn absolute_url(realm: &str, href: &str) -> String {
    if href.contains("://") || href.starts_with("mailto:") {
        href.to_owned()
    } else {
        format!(
            "{}/{}",
            realm.trim_end_matches('/'),
            href.trim_start_matches('/')
        )
    }
}

/// Unified link handling: internal links locate a message or open a conversation, uploaded files go
/// through the file flow, everything else uses the system browser.
pub fn open_link(chat: Chat, message_id: i64, href: String) {
    let session = chat.session();
    if let Some(path) = normalize_upload_path(&href) {
        let source = FileSource {
            link: upload_link_of(&path, None),
            message_id,
            key: chat.current_key(),
        };
        chat.spawn(async move { open_file(chat.overlays, &session, &source).await });
        return;
    }
    let url = absolute_url(session.realm_url(), &href);
    let Some(parsed) = links::parse(&url, session.realm_url()) else {
        chat.spawn(async move { widgets::open_link(chat.overlays, &session, &url).await });
        return;
    };
    let self_id = session.self_id();
    chat.spawn(async move {
        if parsed.near_message_id.is_some() {
            match session.locate_link(&url).await {
                Ok(Some(found)) => {
                    if found.key == chat.current_key() {
                        chat.jump_to(found.message_id, true, crate::ui::ScrollAlign::Center)
                            .await;
                    } else {
                        windows::open_chat(chat.account, found.key, Some(found.message_id));
                    }
                }
                Ok(None) => {}
                Err(error) => chat.overlays.report_error("定位消息失败", error),
            }
            return;
        }
        match (parsed.stream_id, parsed.topic, parsed.dm_user_ids) {
            (Some(stream), Some(topic), _) => {
                windows::open_chat(chat.account, ConversationKey::topic(stream, topic), None)
            }
            (Some(stream_id), None, _) => send_command(AppCommand::ShowChannel {
                account: chat.account,
                stream_id,
            }),
            (_, _, Some(ids)) => {
                windows::open_chat(chat.account, ConversationKey::dm(ids, self_id), None);
            }
            _ => {}
        }
    });
}

/// The images in the whole bubble form an album.
pub fn open_image(chat: Chat, msg: i64, index: usize) {
    if let Some(message) = chat.message(msg) {
        open_album(chat, &message.content_html, index);
    }
}

fn notice_html(chat: Chat, stream: i64) -> Option<String> {
    chat.session()
        .channel(stream)
        .map(|channel| channel.rendered_description)
}

pub fn open_notice_image(chat: Chat, stream: i64, index: usize) {
    if let Some(html) = notice_html(chat, stream) {
        open_album(chat, &html, index);
    }
}

fn open_album(chat: Chat, html: &str, index: usize) {
    let album: Vec<String> = extract_inline_images(html)
        .iter()
        .map(|image| image.original())
        .collect();
    if !album.is_empty() {
        let last = album.len() - 1;
        windows::open_image_viewer(chat.account, album, index.min(last));
    }
}

/// A mention that was clicked in a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MentionTarget {
    /// User id and the displayed name.
    User(i64, String),
    /// User group name.
    Group(String),
    /// Wildcard name: `all` or `topic`.
    Wildcard(String),
}

impl MentionTarget {
    /// Derived from the attributes of the mention element; `None` when it cannot be recognized.
    pub fn from_click(
        user: Option<&str>,
        group: Option<&str>,
        wildcard: Option<&str>,
        text: &str,
    ) -> Option<Self> {
        let name = text.trim_start_matches('@').to_owned();
        if let Some(wildcard) = wildcard {
            return Some(Self::Wildcard(wildcard.to_owned()));
        }
        if group.is_some() {
            return (!name.is_empty()).then_some(Self::Group(name));
        }
        let id = user?.parse().ok()?;
        Some(Self::User(id, name))
    }

    /// Markdown to insert into the composer. A user mention uses the current full name (the
    /// displayed name if not found).
    fn markup(&self, full_name: Option<String>) -> String {
        match self {
            Self::User(id, name) => format!("@**{}|{id}** ", full_name.as_deref().unwrap_or(name)),
            Self::Group(name) => format!("@*{name}* "),
            Self::Wildcard(name) => format!("@**{name}** "),
        }
    }
}

/// Clicking a mention: a user mention shows the profile card; a group / everyone mention is
/// inserted at the cursor in the composer (nothing when the composer cannot post; without
/// permission to @-mention everyone, only a hint is shown).
pub fn click_mention(chat: Chat, target: MentionTarget, x: f64, y: f64) {
    // A user (in messages and in notices like "X moved this topic"): their profile card
    if let MentionTarget::User(id, _) = target {
        open_user_card(chat.overlays, anchor_at(x, y), id);
        return;
    }
    let session = chat.session();
    let key = chat.current_key();
    if !crate::pages::composer::can_post(&session, &key) {
        return;
    }
    if let MentionTarget::Wildcard(name) = &target
        && !session.is_wildcard_allowed(&key)
    {
        chat.overlays.toast(if name == "topic" {
            "没有权限使用 @话题参与者"
        } else {
            "没有权限使用 @全体成员"
        });
        return;
    }
    let full_name = match &target {
        MentionTarget::User(id, _) => session.user(*id).map(|u| u.full_name),
        _ => None,
    };
    let mut insert = chat.composer.insert;
    insert.set(Some(target.markup(full_name)));
}

pub fn table_menu(chat: Chat, msg: i64, index: usize, x: f64, y: f64) {
    if let Some(message) = chat.message(msg) {
        show_table_menu(chat, &message.content_html, index, x, y);
    }
}

pub fn notice_table_menu(chat: Chat, stream: i64, index: usize, x: f64, y: f64) {
    if let Some(html) = notice_html(chat, stream) {
        show_table_menu(chat, &html, index, x, y);
    }
}

fn show_table_menu(chat: Chat, html: &str, index: usize, x: f64, y: f64) {
    chat.overlays
        .show_menu(x, y, table_menu_entries(chat.overlays, html, index));
}

/// Menu for the `index`-th table in a message: copy the source, copy the formatted source. Quotes
/// expanded in the composer use it too.
pub fn table_menu_entries(overlays: Overlays, html: &str, index: usize) -> Vec<MenuEntry> {
    let entry = |label: &str, padded: bool| {
        let html = html.to_owned();
        MenuEntry::item(label, move || {
            if let Some(source) = table_markdown(&html, index, padded) {
                crate::platform::remember_markdown(&source, &source);
                overlays.copy_text(&source);
            }
        })
    };
    vec![
        entry("复制表格", false).icon(IconName::Copy),
        entry("格式化表格源码", true).icon(IconName::SourceCode),
    ]
}

fn copy_with_markdown(chat: Chat, text: &str, markdown: &str) {
    crate::platform::remember_markdown(text, markdown);
    chat.overlays.copy_text(text);
}

/// The text and Markdown source of a message.
async fn copy_message(chat: Chat, message: Arc<ChatMessage>) {
    let text = to_plain_text(&message.content_html);
    let markdown = match &message.state {
        MessageState::Sending(echo) | MessageState::Failed(echo) => echo.raw_content.clone(),
        _ => chat
            .session()
            .raw_content(message.id)
            .await
            .unwrap_or_else(|_| text.clone()),
    };
    copy_with_markdown(chat, &text, &markdown);
}

async fn image_file(chat: Chat, msg: i64, index: usize) -> Option<std::path::PathBuf> {
    let message = chat.message(msg)?;
    let image = extract_inline_images(&message.content_html)
        .into_iter()
        .nth(index)?;
    chat.overlays
        .attempt("下载图片失败", chat.session().image_file(&image.original()))
        .await
}

fn copy_image(chat: Chat, msg: i64, index: usize) {
    chat.spawn(async move {
        let Some(path) = image_file(chat, msg, index).await else {
            return;
        };
        let decoded = image::open(&path).map(|img| {
            let rgba = img.to_rgba8();
            ClipboardImage {
                width: rgba.width() as usize,
                height: rgba.height() as usize,
                rgba: rgba.into_raw(),
            }
        });
        match decoded {
            Ok(image) => match platform::copy_image(&image) {
                Ok(()) => chat.overlays.toast("已复制"),
                Err(error) => chat.overlays.report_error("复制失败", error),
            },
            Err(error) => chat.overlays.report_error("复制失败", error),
        }
    });
}

fn save_image(chat: Chat, msg: i64, index: usize) {
    chat.spawn(async move {
        let Some(path) = image_file(chat, msg, index).await else {
            return;
        };
        let name = path.file_name().map_or_else(
            || "图片.png".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        chat.overlays
            .save_as(&name, |target| async move {
                tokio::fs::copy(&path, target).await.map(|_| ())
            })
            .await;
    });
}

fn image_folder(chat: Chat, msg: i64, index: usize) {
    chat.spawn(async move {
        if let Some(path) = image_file(chat, msg, index).await
            && let Some(parent) = path.parent()
        {
            chat.overlays.open_file(parent);
        }
    });
}

pub fn show_menu(
    chat: Chat,
    msg: Option<i64>,
    sender: bool,
    x: f64,
    y: f64,
    image: Option<usize>,
    selection: Option<(String, String)>,
) {
    if *chat.multi.peek() {
        return;
    }
    let items = match msg.and_then(|id| chat.message(id)) {
        Some(message) if sender => sender_menu(chat, &message, x, y),
        Some(message) if message.id > 0 => {
            let bar = reaction_bar(chat, message.id);
            let items = message_menu(chat, message, image, selection);
            chat.overlays.show_menu_with_bar(x, y, bar, items);
            return;
        }
        Some(message) => message_menu(chat, message, image, selection),
        None => blank_menu(chat, y),
    };
    chat.overlays.show_menu(x, y, items);
}

/// `y`: where it was right-clicked; switching the source mode keeps the message there in place.
fn blank_menu(chat: Chat, y: f64) -> Vec<MenuEntry> {
    let source = *chat.source_mode.peek();
    vec![
        MenuEntry::item("源代码模式", move || {
            if let Some(list) = chat.list.peek().as_ref() {
                list.hold_item_at(y);
            }
            let mut mode = chat.source_mode;
            mode.set(!source);
        })
        .checked(source),
    ]
}

fn sender_menu(chat: Chat, message: &ChatMessage, x: f64, y: f64) -> Vec<MenuEntry> {
    let session = chat.session();
    let user = message.sender_id;
    let name = session.display_name(user);
    let muted = session.is_user_muted(user);
    let mut items = vec![
        MenuEntry::item("发送消息", {
            let self_id = session.self_id();
            move || windows::open_chat(chat.account, ConversationKey::dm([user], self_id), None)
        }),
        MenuEntry::item("@TA", move || {
            let mut insert = chat.composer.insert;
            insert.set(Some(format!("@**{name}|{user}** ")));
        }),
        MenuEntry::item("查看资料", move || {
            open_user_card(chat.overlays, anchor_at(x, y), user);
        }),
    ];
    if user != session.self_id() {
        items.push(MenuEntry::item(
            if muted {
                "取消屏蔽"
            } else {
                "屏蔽此人"
            },
            move || toggle_muted(chat, user, !muted),
        ));
    }
    let plugin_items = plugin_context_menus(&session, &ContextMenuTarget::User(user));
    if !plugin_items.is_empty() {
        items.push(MenuEntry::Separator);
        items.extend(plugin_items);
    }
    items
}

/// Confirm before muting; unmuting runs directly.
pub fn toggle_muted(chat: Chat, user: i64, mute: bool) {
    let session = chat.session();
    chat.spawn(async move {
        if mute {
            chat.overlays
                .confirm_then(
                    crate::ui::Confirm::new("屏蔽后将不再显示对方的消息").danger("屏蔽"),
                    "屏蔽失败",
                    || session.set_user_muted(user, true),
                )
                .await;
        } else {
            chat.overlays
                .attempt("取消屏蔽失败", session.set_user_muted(user, false))
                .await;
        }
    });
}

fn reaction_bar(chat: Chat, id: i64) -> impl Fn(Callback<()>) -> Element + 'static {
    let session = chat.session();
    let emojis: Vec<EmojiEntry> = session
        .quick_reactions()
        .into_iter()
        .take(QUICK_REACTIONS)
        .collect();
    move |close: Callback<()>| {
        let buttons = emojis.iter().cloned().map(|entry| {
            let text = emoji_text(&entry);
            let image = entry.image_url.clone();
            let name = entry.name.clone();
            rsx! {
                button {
                    key: "{name}",
                    class: "reaction-pick",
                    r#type: "button",
                    title: ":{name}:",
                    onclick: move |_| {
                        close.call(());
                        toggle_reaction(chat, id, entry.clone());
                    },
                    if let Some(url) = image {
                        RemoteImage { url, size: 22.0 }
                    } else {
                        "{text}"
                    }
                }
            }
        });
        rsx! {
            div { class: "reaction-row",
                {buttons}
                button {
                    class: "reaction-pick",
                    r#type: "button",
                    title: "更多表情",
                    onclick: move |e: MouseEvent| {
                        close.call(());
                        let at = e.client_coordinates();
                        pick_reaction(chat, id, at.x, at.y);
                    },
                    Icon { name: IconName::Add, size: 16.0 }
                }
            }
        }
    }
}

fn emoji_text(entry: &EmojiEntry) -> String {
    entry
        .text
        .clone()
        .or_else(|| emoji::unicode_from_code(&entry.code))
        .unwrap_or_else(|| format!(":{}:", entry.name))
}

fn toggle_reaction(chat: Chat, id: i64, entry: EmojiEntry) {
    chat.spawn(async move {
        let session = chat.session();
        session.add_recent_emoji(&entry.name);
        chat.overlays
            .attempt("操作失败", session.toggle_reaction(id, &entry))
            .await;
    });
}

/// Clicking an existing reaction on a message: all emoji info is in the reaction, no need to look
/// up the catalog (it may be missing there, e.g. a deactivated custom emoji).
pub fn toggle_group_reaction(chat: Chat, id: i64, group: ReactionGroup) {
    chat.spawn(async move {
        let session = chat.session();
        chat.overlays
            .attempt(
                "操作失败",
                session.toggle_reaction_of(id, &group.name, &group.code, &group.kind),
            )
            .await;
    });
}

fn toggle_reaction_by_name(chat: Chat, id: i64, name: &str) {
    let entry = chat.session().emoji_catalog().get(name).cloned();
    match entry {
        Some(entry) => toggle_reaction(chat, id, entry),
        None => chat.overlays.report_error("操作失败", "找不到这个表情"),
    }
}

fn pick_reaction(chat: Chat, id: i64, x: f64, y: f64) {
    open_emoji_picker(
        chat.overlays,
        anchor_at(x, y),
        chat.session(),
        move |name: String| toggle_reaction_by_name(chat, id, &name),
    );
}

/// Display condition for message plugin menus: shared by the context menu and mouse gestures.
pub fn message_menu_context(
    session: &Session,
    key: &ConversationKey,
    message: &ChatMessage,
) -> MenuContext {
    MenuContext {
        conv: menu_conv(key),
        own: message.sender_id == session.self_id(),
        has_uploads: !extract_uploads(&message.content_html).is_empty(),
        can_edit: session.message_actions(message.id).can_edit,
    }
}

fn reminder_at(chat: Chat, id: i64, deliver_at: i64) {
    chat.spawn(async move {
        let result = chat.session().create_reminder(id, deliver_at).await;
        match result {
            Ok(_) => {
                let text = time::datetime_label(deliver_at);
                chat.overlays.toast(format!("将在 {text} 提醒你"));
            }
            Err(error) => chat.overlays.report_error("设置提醒失败", error),
        }
    });
}

/// 9:00 tomorrow in local time (Unix seconds).
fn tomorrow_nine() -> Option<i64> {
    let tomorrow = chrono::Local::now().date_naive().succ_opt()?;
    let at = tomorrow.and_hms_opt(9, 0, 0)?;
    at.and_local_timezone(chrono::Local)
        .earliest()
        .map(|t| t.timestamp())
}

fn reminder_menu(chat: Chat, id: i64) -> MenuEntry {
    MenuEntry::submenu(
        "提醒我",
        vec![
            MenuEntry::item("1 小时后", move || {
                reminder_at(chat, id, chrono::Utc::now().timestamp() + HOUR);
            }),
            MenuEntry::item("明天 9:00", move || {
                if let Some(at) = tomorrow_nine() {
                    reminder_at(chat, id, at);
                }
            }),
            MenuEntry::item("自定义…", move || {
                chat.spawn(async move {
                    if let Some(at) = ask_future_time(chat.overlays, "提醒我", None).await {
                        reminder_at(chat, id, at);
                    }
                });
            }),
        ],
    )
    .icon(IconName::Time)
}

fn message_menu(
    chat: Chat,
    message: Arc<ChatMessage>,
    image: Option<usize>,
    selection: Option<(String, String)>,
) -> Vec<MenuEntry> {
    let session = chat.session();
    let id = message.id;
    let mut items = Vec::new();
    if id <= 0 {
        items.push(copy_entry(chat, message, image, selection));
        return items;
    }
    let actions = session.message_actions(id);
    items.push(copy_entry(chat, message.clone(), image, selection));
    if let Some(index) = image {
        items.push(
            MenuEntry::item("另存为", move || save_image(chat, id, index)).icon(IconName::Download),
        );
        items.push(
            MenuEntry::item("打开文件夹", move || image_folder(chat, id, index))
                .icon(IconName::Folder),
        );
    }
    let starred = message.flags.starred;
    items.push(
        MenuEntry::item(if starred { "取消收藏" } else { "收藏" }, move || {
            if starred {
                set_starred(chat, vec![id], false);
            } else {
                star_with_animation(chat, id);
            }
        })
        .icon(if starred {
            IconName::StarFilled
        } else {
            IconName::Star
        }),
    );
    items.push(
        MenuEntry::item("引用", move || {
            chat.spawn(async move {
                if let Some(markdown) = chat
                    .overlays
                    .attempt("引用失败", chat.session().quote_message(id))
                    .await
                {
                    let mut insert = chat.composer.insert;
                    insert.set(Some(markdown));
                }
            });
        })
        .icon(IconName::Quote),
    );
    items.push(
        MenuEntry::item("转发", move || {
            super::dialogs::forward(chat, vec![id], false)
        })
        .icon(IconName::Share),
    );
    items.push(MenuEntry::item("多选", move || chat.enter_multi(id)).icon(IconName::CheckCircle));
    if actions.can_edit {
        items.push(
            MenuEntry::item("编辑", move || {
                let mut editing = chat.composer.editing;
                editing.set(Some(id));
            })
            .icon(IconName::Edit),
        );
    }
    items.push(MenuEntry::Separator);

    let mut more = Vec::new();
    let ctx = message_menu_context(&session, &chat.current_key(), &message);
    for plugin in plugins().plugins() {
        let invoke: Rc<dyn Fn(i32)> = {
            let (plugin, message, session) = (plugin.clone(), message.clone(), session.clone());
            Rc::new(move |id| plugins().invoke_message_menu(&plugin, id, &message, &session))
        };
        more.extend(plugin_menu_entries(
            plugin.active_message_menus(),
            &ctx,
            &invoke,
        ));
    }
    if !more.is_empty() {
        more.push(MenuEntry::Separator);
    }
    {
        let link_session = session.clone();
        more.push(MenuEntry::item("复制链接", move || {
            let link = link_session.message_link(&chat.current_key(), id);
            chat.overlays.copy_text(&link);
        }));
    }
    if actions.can_show_edit_history {
        more.push(MenuEntry::item("编辑历史", move || {
            dialogs::show_history(chat, id);
        }));
    }
    let key = chat.current_key();
    let shows_receipts = match &key {
        ConversationKey::Topic(_) => true,
        ConversationKey::Dm(d) => d.is_group_dm(),
    };
    if actions.can_show_read_receipts && shows_receipts {
        more.push(MenuEntry::item("已读成员", move || {
            dialogs::show_receipts(chat, id);
        }));
    }
    more.push(MenuEntry::item("从这里标为未读", move || {
        chat.spawn(async move {
            let mut suspended = chat.auto_read_suspended;
            *suspended.write() = true;
            let key = chat.current_key();
            chat.overlays
                .attempt("操作失败", chat.session().mark_unread_from(&key, id))
                .await;
        });
    }));
    more.push(reminder_menu(chat, id));
    items.push(MenuEntry::submenu("更多", more).icon(IconName::More));
    if actions.can_delete {
        items.push(
            MenuEntry::item("删除", move || {
                chat.spawn(async move {
                    let session = chat.session();
                    chat.overlays
                        .delete("这条消息", None, || {
                            session.delete(DeleteTarget::Message(id))
                        })
                        .await;
                });
            })
            .icon(IconName::Delete)
            .danger(),
        );
    }
    items
}

fn copy_entry(
    chat: Chat,
    message: Arc<ChatMessage>,
    image: Option<usize>,
    selection: Option<(String, String)>,
) -> MenuEntry {
    MenuEntry::item("复制", move || {
        if let Some(index) = image {
            copy_image(chat, message.id, index);
        } else if let Some((text, html)) = &selection {
            copy_with_markdown(chat, text, &html_to_markdown(html));
        } else {
            chat.spawn(copy_message(chat, message.clone()));
        }
    })
    .icon(IconName::Copy)
}

/// Favorite a message: the bubble flies to the main menu button, and a hint shows once the
/// animation ends and the favorite succeeds.
fn star_with_animation(chat: Chat, id: i64) {
    document::eval(&format!("window.zulipChat.flyToMenu({id}, {STAR_FLY_MS})"));
    chat.spawn(async move {
        let flight = tokio::time::sleep(Duration::from_millis(STAR_FLY_MS));
        let done = chat
            .overlays
            .attempt("操作失败", chat.session().set_starred(&[id], true))
            .await;
        flight.await;
        if done.is_some() {
            chat.overlays.toast("收藏成功");
        }
    });
}

fn set_starred(chat: Chat, ids: Vec<i64>, starred: bool) {
    chat.spawn(async move {
        let done = chat
            .overlays
            .attempt("操作失败", chat.session().set_starred(&ids, starred))
            .await;
        if done.is_some() {
            chat.overlays.toast(if starred {
                "收藏成功"
            } else {
                "已取消收藏"
            });
        }
    });
}

pub fn failed_menu(chat: Chat, local_id: String, x: f64, y: f64) {
    let retry = local_id.clone();
    chat.overlays.show_menu(
        x,
        y,
        vec![
            MenuEntry::item("重新发送", move || {
                let retry = retry.clone();
                chat.spawn(async move {
                    chat.overlays
                        .attempt("发送失败", chat.session().retry_send(&retry))
                        .await;
                });
            })
            .icon(IconName::Refresh),
            MenuEntry::item("放弃发送", move || {
                chat.session().discard_failed(&local_id)
            })
            .icon(IconName::Delete)
            .danger(),
        ],
    );
}

pub fn selected_ids(chat: Chat) -> Vec<i64> {
    chat.selected.peek().iter().copied().collect()
}

pub fn star_selected(chat: Chat) {
    set_starred(chat, selected_ids(chat), true);
    chat.exit_multi();
}

/// Multi-select copy: each message is a "name: time" line followed by its content, with a blank
/// line between messages.
pub fn copy_selected(chat: Chat) {
    let session = chat.session();
    let use_24h = session.use_24h();
    let now = chrono::Local::now().naive_local();
    let text = selected_ids(chat)
        .iter()
        .filter_map(|id| chat.message(*id))
        .map(|m| {
            let when = time::from_unix(m.timestamp)
                .map_or_else(String::new, |t| time::precise_time(t, now, use_24h));
            format!(
                "{}: {when}\n{}",
                session.display_name(m.sender_id),
                to_plain_text(&m.content_html)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    chat.overlays.copy_text(&text);
    chat.exit_multi();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_links_join_realm() {
        assert_eq!(
            absolute_url("https://z.test/", "/user_uploads/a"),
            "https://z.test/user_uploads/a"
        );
        assert_eq!(absolute_url("https://z.test", "http://x/y"), "http://x/y");
        assert_eq!(absolute_url("https://z.test", "mailto:a@b"), "mailto:a@b");
    }

    #[test]
    fn mention_markup_by_kind() {
        let target = |user, group, wildcard, text| {
            MentionTarget::from_click(user, group, wildcard, text).map(|t| t.markup(None))
        };
        assert_eq!(
            target(Some("9"), None, None, "@张三").unwrap(),
            "@**张三|9** "
        );
        assert_eq!(
            MentionTarget::User(9, "旧名".into()).markup(Some("张三".into())),
            "@**张三|9** "
        );
        // Shown as the "all members" label, insert the wildcard name
        assert_eq!(
            target(Some("*"), None, Some("all"), "@全体成员").unwrap(),
            "@**all** "
        );
        assert_eq!(
            target(None, None, Some("topic"), "@话题参与者").unwrap(),
            "@**topic** "
        );
        assert_eq!(
            target(None, Some("3"), None, "@后端组").unwrap(),
            "@*后端组* "
        );
        assert_eq!(target(None, None, None, "@谁"), None);
        assert_eq!(target(Some("*"), None, None, "@全体成员"), None);
    }
}
