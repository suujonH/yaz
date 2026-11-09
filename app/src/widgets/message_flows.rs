//! Shared message operation flows: locate in chat, forward, unfavorite.

use super::conversation_avatar::conversation_rows;
use super::{cached_avatar, key_id};
use crate::ui::*;
use crate::windows;
use std::future::Future;
use std::sync::Arc;
use zulip_core::ConversationKey;
use zulip_session::{ConversationAvatar, Session, SessionError};

const MAX_FORWARD_TARGETS: usize = 9;

/// Locate a message in the chat: favorites, records, files, and reminders all go through here. When
/// `key` is unknown the conversation layer looks it up.
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

/// Unfavorite.
pub async fn unstar(overlays: Overlays, session: &Arc<Session>, ids: &[i64]) -> bool {
    overlays
        .attempt("取消收藏失败", session.set_starred(ids, false))
        .await
        .is_some()
}

/// Whether messages can be sent to this conversation: a subscribed, non-archived channel with
/// posting permission, or a DM with an active user.
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

/// Conversations that can be forwarded to: those on the message page (including conversation
/// folders), plus friends not chatted with yet.
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

/// Forward: pick the target conversation, then send the content with `send`. On success shows the
/// "forwarded" notice and returns true; returns false on cancel or failure.
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

/// Forward a piece of text (links to files and images).
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

/// Forward the original message text. Return value is the same as [`forward`].
pub async fn forward_messages(overlays: Overlays, session: &Arc<Session>, ids: Vec<i64>) -> bool {
    let sender = session.clone();
    forward(overlays, session, move |keys| async move {
        sender.forward_each(&ids, &keys).await
    })
    .await
}

/// Open a link: message links of this organization are located inside the client; http(s) and mail
/// links go to the system.
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
