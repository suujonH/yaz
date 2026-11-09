//! Forwarding: merged into a chat history, or forwarded one by one.

use std::sync::Arc;

use zulip_core::ConversationKey;
use zulip_core::text::forward;
use zulip_core::text::time::datetime_label;

use crate::error::Result;
use crate::hooks::OutgoingKind;
use crate::session::Session;

impl Session {
    /// Merged forward to multiple conversations: several messages are combined into a "chat
    /// history", split into several messages when too long.
    pub async fn forward_merged(
        self: &Arc<Self>,
        ids: &[i64],
        targets: &[ConversationKey],
    ) -> Result<()> {
        let mut items = Vec::new();
        let mut max_chars = None;
        for id in ids {
            let raw = self.raw_content(*id).await?;
            let state = self.state.read();
            let max = state.realm.max_message_length;
            max_chars = (max > 0).then_some(max as usize);
            let Some(message) = state.messages.get(*id) else {
                continue;
            };
            items.push(forward::ForwardedMessage {
                sender_name: state.display_name(message.sender_id),
                time: datetime_label(message.timestamp),
                raw,
            });
        }
        for text in forward::merge(&items, max_chars) {
            for target in targets {
                self.send_as(target, &text, OutgoingKind::Forward).await?;
            }
        }
        Ok(())
    }

    /// One-by-one forward to multiple conversations.
    pub async fn forward_each(
        self: &Arc<Self>,
        ids: &[i64],
        targets: &[ConversationKey],
    ) -> Result<()> {
        for id in ids {
            let raw = self.raw_content(*id).await?;
            for target in targets {
                self.send_as(target, &raw, OutgoingKind::Forward).await?;
            }
        }
        Ok(())
    }
}
