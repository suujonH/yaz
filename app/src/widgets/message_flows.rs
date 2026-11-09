//! 消息相关的共用操作流程：定位到聊天、转发、取消收藏。

use super::conversation_avatar::conversation_rows;
use super::{cached_avatar, key_id};
use crate::ui::*;
use crate::windows;
use std::future::Future;
use std::sync::Arc;
use zulip_core::ConversationKey;
use zulip_session::{ConversationAvatar, Session, SessionError};

const MAX_FORWARD_TARGETS: usize = 9;

/// 在聊天中定位到消息：收藏、记录、文件、提醒都走这里。`key` 未知时由会话层查询。
pub async fn locate_in_chat(
    overlays: Overlays,
    session: &Arc<Session>,
    key: Option<&ConversationKey>,
    message_id: i64,
) {
    if let Some(located) = overlays
        .attempt("定位失败", session.locate_message(key, message_id))
        .await
    {
        windows::open_chat(session.account_id(), located.key, Some(located.message_id));
    }
}

/// 取消收藏。
pub async fn unstar(overlays: Overlays, session: &Arc<Session>, ids: &[i64]) -> bool {
    overlays
        .attempt("取消收藏失败", session.set_starred(ids, false))
        .await
        .is_some()
}

/// 能不能向这个会话发消息：已订阅、未归档且有发言权限的频道，或活跃用户的私聊。
pub fn can_send_to(session: &Session, key: &ConversationKey) -> bool {
    match key {
        ConversationKey::Topic(topic) => session.channel(topic.stream_id).is_some_and(|c| {
            c.is_subscribed()
                && !c.is_archived
                && session.permissions().can_send_to_channel(&c.permissions)
        }),
        ConversationKey::Dm(dm) => dm
            .other_user_ids()
            .iter()
            .all(|id| session.user(*id).is_some_and(|u| u.is_active)),
    }
}

/// 可以转发到的会话：消息页里的（含会话夹），再加上还没有聊过的好友。
fn forward_targets(session: &Arc<Session>) -> Vec<(ConversationKey, PickerItem)> {
    let rows = conversation_rows(session);
    let mut ordered: Vec<_> = session
        .message_rows()
        .into_iter()
        .filter_map(|row| row.key().cloned())
        .collect();
    ordered.extend(
        rows.keys()
            .filter(|key| !ordered.contains(key))
            .cloned()
            .collect::<Vec<_>>(),
    );
    let self_id = session.self_id();
    let friends = session
        .friend_sections()
        .into_iter()
        .flat_map(|section| section.items)
        .map(|friend| ConversationKey::dm([friend.user_id], self_id));
    for key in friends {
        if !ordered.contains(&key) {
            ordered.push(key);
        }
    }
    ordered
        .into_iter()
        .filter(|key| can_send_to(session, key))
        .map(|key| {
            let mut item = PickerItem::new(key_id(&key), session.display_title(&key));
            if let ConversationKey::Topic(topic) = &key {
                item = item.subtitle(format!(
                    "#{}",
                    session.channel_display_name(topic.stream_id)
                ));
            }
            match rows.get(&key).map(|row| &row.avatar) {
                Some(ConversationAvatar::Image(Some(url))) => {
                    item.avatar_url = cached_avatar(session, url);
                }
                Some(ConversationAvatar::Text { color, .. }) if !color.is_empty() => {
                    item.avatar_fill = Some(color.clone());
                }
                _ => {}
            }
            (key, item)
        })
        .collect()
}

/// 转发：选择目标会话，再用 `send` 发送内容。成功后提示“已转发”并返回 true；取消或失败时返回 false。
pub async fn forward<F: Future<Output = Result<(), SessionError>>>(
    overlays: Overlays,
    session: &Arc<Session>,
    send: impl FnOnce(Vec<ConversationKey>) -> F,
) -> bool {
    let targets = forward_targets(session);
    let items = targets.iter().map(|(_, item)| item.clone()).collect();
    let picker = Picker::multi("转发", items, Vec::new())
        .max_selected(MAX_FORWARD_TARGETS)
        .confirm_text("发送");
    let Some(picked) = overlays.pick(picker).await else {
        return false;
    };
    let keys: Vec<ConversationKey> = targets
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| picked.contains(&key_id(key)))
        .collect();
    if keys.is_empty() || overlays.attempt("转发失败", send(keys)).await.is_none() {
        return false;
    }
    overlays.toast("已转发");
    true
}

/// 转发一段文字（文件和图片的链接）。
pub async fn forward_text(overlays: Overlays, session: &Arc<Session>, text: String) {
    let sender = session.clone();
    forward(overlays, session, move |keys| async move {
        for key in &keys {
            sender.send_message(key, &text).await?;
        }
        Ok(())
    })
    .await;
}

/// 转发消息原文。返回值同 [`forward`]。
pub async fn forward_messages(overlays: Overlays, session: &Arc<Session>, ids: Vec<i64>) -> bool {
    let sender = session.clone();
    forward(overlays, session, move |keys| async move {
        sender.forward_each(&ids, &keys).await
    })
    .await
}

/// 打开链接：本组织的消息链接在客户端中定位，http(s) 和邮件链接交给系统。
pub async fn open_link(overlays: Overlays, session: &Arc<Session>, url: &str) {
    match session.locate_link(url).await {
        Ok(Some(located)) => {
            windows::open_chat(session.account_id(), located.key, Some(located.message_id))
        }
        Ok(None) => {
            let lower = url.to_ascii_lowercase();
            if ["http://", "https://", "mailto:"]
                .iter()
                .any(|scheme| lower.starts_with(scheme))
            {
                overlays.open_file(std::path::Path::new(url));
            } else {
                overlays.toast("不支持打开这个链接");
            }
        }
        Err(error) => overlays.report_error("打开链接失败", error),
    }
}
