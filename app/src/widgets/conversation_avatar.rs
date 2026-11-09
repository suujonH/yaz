//! Conversation avatar, and looking up message-page rows (avatar, title, preview, unread) by
//! conversation key.

use super::{RemoteAvatar, local_image};
use crate::services::services;
use crate::ui::Avatar;
use dioxus::prelude::*;
use std::collections::HashMap;
use zulip_core::model::ConversationNotice;
use zulip_core::{ConversationFolder, ConversationKey};
use zulip_session::{ConversationAvatar, ConversationRow, Session};

/// Conversation avatar: for a DM it is the other party's avatar; for topics and group chats it is a
/// colored text block.
#[component]
pub fn ConversationAvatarView(
    avatar: ConversationAvatar,
    #[props(into)] title: String,
    #[props(default = 40.0)] size: f64,
) -> Element {
    match avatar {
        ConversationAvatar::Image(url) => rsx! {
            RemoteAvatar { text: title, url, size }
        },
        ConversationAvatar::Text { text, color } => rsx! {
            Avatar { text, fill: (!color.is_empty()).then_some(color), size }
        },
    }
}

/// Avatar of a specific account. Used when the window is not bound to an account (notification
/// popup, tray list).
#[component]
pub fn AccountAvatar(
    account: i64,
    #[props(into)] text: String,
    #[props(into)] url: Option<String>,
    #[props(into)] fill: Option<String>,
    #[props(default = 40.0)] size: f64,
) -> Element {
    let image = use_resource(use_reactive!(|account, url| async move {
        local_image(services().hub.session(account)?, url?).await
    }));
    let src = image.read().clone().flatten();
    rsx! {
        Avatar { text, src, fill, size }
    }
}

/// All conversation rows of the message page, including those inside conversation folders.
pub fn conversation_rows(session: &Session) -> HashMap<ConversationKey, ConversationRow> {
    let folders = [ConversationFolder::Helper, ConversationFolder::Resolved]
        .into_iter()
        .flat_map(|folder| session.folder_rows(folder));
    session
        .message_rows()
        .into_iter()
        .chain(folders)
        .filter_map(|row| Some((row.key()?.clone(), row)))
        .collect()
}

/// Red hint in a conversation row, such as the "someone mentioned me" tag.
pub const fn notice_label(notice: ConversationNotice) -> Option<&'static str> {
    match notice {
        ConversationNotice::None => None,
        ConversationNotice::AtAll => Some("[@全体成员]"),
        ConversationNotice::AtMe => Some("[有人@我]"),
        ConversationNotice::NewFile => Some("[有新文件]"),
    }
}
