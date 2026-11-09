//! 转发：合并成聊天记录，或逐条转发。

use std::sync::Arc;

use zulip_core::ConversationKey;
use zulip_core::text::forward;
use zulip_core::text::time::datetime_label;

use crate::error::Result;
use crate::hooks::OutgoingKind;
use crate::session::Session;

impl Session {
    /// 合并转发到多个会话：多条消息拼成“聊天记录”，超长时拆成多条。
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

    /// 逐条转发到多个会话。
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
