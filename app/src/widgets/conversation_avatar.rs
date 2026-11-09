//! 会话头像，以及按会话键查找消息页的行（头像、标题、预览、未读）。

use super::{RemoteAvatar, local_image};
use crate::services::services;
use crate::ui::Avatar;
use dioxus::prelude::*;
use std::collections::HashMap;
use zulip_core::model::ConversationNotice;
use zulip_core::{ConversationFolder, ConversationKey};
use zulip_session::{ConversationAvatar, ConversationRow, Session};

/// 会话的头像：私聊是对方头像，话题和群聊是文字色块。
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

/// 指定账号的头像。窗口没有绑定账号时用（通知弹窗、托盘列表）。
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

/// 消息页的全部会话行，包括会话夹里的。
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

/// 会话行里的红色提示，如“[有人@我]”。
pub const fn notice_label(notice: ConversationNotice) -> Option<&'static str> {
    match notice {
        ConversationNotice::None => None,
        ConversationNotice::AtAll => Some("[@全体成员]"),
        ConversationNotice::AtMe => Some("[有人@我]"),
        ConversationNotice::NewFile => Some("[有新文件]"),
    }
}
