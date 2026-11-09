//! Message content events sent from `chat.js`.

use super::actions;
use super::content::html_to_markdown;
use super::store::Chat;
use crate::services::plugins;
use dioxus::dioxus_core::current_scope_id;
use dioxus::prelude::*;
use serde::Deserialize;
use zulip_plugin_host::{Modifiers, MouseButton, mouse_gesture};

const SCRIPT: &str = include_str!("chat.js");

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum ChatEvent {
    Link {
        msg: Option<i64>,
        href: String,
    },
    Image {
        msg: i64,
        index: usize,
    },
    Mention {
        user: Option<String>,
        group: Option<String>,
        wildcard: Option<String>,
        text: String,
        x: f64,
        y: f64,
    },
    Table {
        msg: i64,
        index: usize,
        x: f64,
        y: f64,
    },
    NoticeImage {
        stream: i64,
        index: usize,
    },
    NoticeTable {
        stream: i64,
        index: usize,
        x: f64,
        y: f64,
    },
    Menu {
        msg: Option<i64>,
        sender: bool,
        x: f64,
        y: f64,
        image: Option<usize>,
        text: Option<String>,
        html: Option<String>,
    },
    Gesture {
        msg: i64,
        button: u8,
        clicks: u32,
        ctrl: bool,
        alt: bool,
        shift: bool,
        meta: bool,
    },
    Copy {
        text: String,
        html: String,
    },
    Esc,
}

fn on_gesture(mut chat: Chat, msg: i64, button: u8, clicks: u32, modifiers: Modifiers) {
    let button = match button {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        _ => return,
    };
    let Some(gesture) = mouse_gesture(button, clicks, modifiers) else {
        return;
    };
    let host = plugins();
    if !host.has_mouse_shortcut(&gesture) {
        return;
    }
    let Some(message) = chat.message(msg) else {
        return;
    };
    let session = chat.session();
    let ctx = actions::message_menu_context(&session, &chat.current_key(), &message);
    if host.invoke_mouse_shortcut(&gesture, &message, &session, &ctx)
        && button == MouseButton::Right
    {
        *chat.suppress_menu.write() = true;
    }
}

fn handle(mut chat: Chat, event: ChatEvent) {
    match event {
        ChatEvent::Link { msg, href } => actions::open_link(chat, msg.unwrap_or(0), href),
        ChatEvent::Image { msg, index } => actions::open_image(chat, msg, index),
        ChatEvent::Mention {
            user,
            group,
            wildcard,
            text,
            x,
            y,
        } => {
            let target = actions::MentionTarget::from_click(
                user.as_deref(),
                group.as_deref(),
                wildcard.as_deref(),
                &text,
            );
            if let Some(target) = target {
                actions::click_mention(chat, target, x, y);
            }
        }
        ChatEvent::Table { msg, index, x, y } => actions::table_menu(chat, msg, index, x, y),
        ChatEvent::NoticeImage { stream, index } => {
            actions::open_notice_image(chat, stream, index);
        }
        ChatEvent::NoticeTable {
            stream,
            index,
            x,
            y,
        } => actions::notice_table_menu(chat, stream, index, x, y),
        ChatEvent::Menu {
            msg,
            sender,
            x,
            y,
            image,
            text,
            html,
        } => {
            if std::mem::take(&mut *chat.suppress_menu.write()) {
                return;
            }
            let selection = text.zip(html);
            actions::show_menu(chat, msg, sender, x, y, image, selection);
        }
        ChatEvent::Gesture {
            msg,
            button,
            clicks,
            ctrl,
            alt,
            shift,
            meta,
        } => on_gesture(
            chat,
            msg,
            button,
            clicks,
            Modifiers {
                ctrl,
                alt,
                shift,
                meta,
            },
        ),
        ChatEvent::Copy { text, html } => {
            crate::platform::remember_markdown(&text, &html_to_markdown(&html));
        }
        ChatEvent::Esc => {
            if *chat.multi.peek() {
                chat.exit_multi();
            }
        }
    }
}

/// Attach the event delegation and return the id of the chat page's root element.
pub fn use_chat_events(chat: Chat) -> String {
    let id = use_hook(|| format!("chat-pane-{}", current_scope_id().0));
    let attached = id.clone();
    use_future(move || {
        let attached = attached.clone();
        async move {
            let mut eval = document::eval(&format!(
                "{SCRIPT}\nawait window.zulipChat.attach({attached:?}, (m) => dioxus.send(m));"
            ));
            while let Ok(event) = eval.recv::<ChatEvent>().await {
                handle(chat, event);
            }
        }
    });
    let detached = id.clone();
    use_drop(move || {
        document::eval(&format!("window.zulipChat.detach({detached:?})"));
    });
    id
}

/// Make mounted messages show the downloaded images; the content is not reset, so the text
/// selection is not lost.
pub fn set_image(msg: i64, url: &str, local: Option<&str>) {
    show_image(&format!("[data-msg=\"{msg}\"]"), url, local);
}

/// Same as [`set_image`], used for channel announcements (the sidebar and the more panel may show
/// the same announcement at the same time).
pub fn set_notice_image(stream: i64, url: &str, local: Option<&str>) {
    show_image(&format!("[data-notice=\"{stream}\"]"), url, local);
}

fn show_image(scope: &str, url: &str, local: Option<&str>) {
    let scope = serde_json::to_string(scope).unwrap_or_default();
    let url = serde_json::to_string(url).unwrap_or_default();
    match local.map(serde_json::to_string) {
        Some(Ok(local)) => {
            document::eval(&format!("window.zulipChat.setImage({scope},{url},{local})"));
        }
        _ => {
            document::eval(&format!("window.zulipChat.failImage({scope},{url})"));
        }
    }
}
