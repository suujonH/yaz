//! 已读状态：标记已读、标记未读。

use std::sync::Arc;

use zulip_api::{FlagOp, FlagsNarrowUpdate, MessageFlag, NarrowTerm};
use zulip_core::ConversationKey;

use crate::error::Result;
use crate::event::SessionEvent;
use crate::session::Session;

pub(super) const FLAG_BATCH: usize = 1000;
const NARROW_FLAG_BATCH: u32 = 5000;

impl Session {
    /// 标记已读。先改本地，再分批发送。
    pub async fn mark_read(self: &Arc<Self>, ids: &[i64]) -> Result<()> {
        let ids: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
        if ids.is_empty() {
            return Ok(());
        }
        let keys = self.state.write().unread.remove_ids(&ids);
        if !keys.is_empty() {
            self.emit(SessionEvent::UnreadChanged(keys));
        }
        for batch in ids.chunks(FLAG_BATCH) {
            self.client
                .update_flags(batch, FlagOp::Add, MessageFlag::Read)
                .await?;
        }
        Ok(())
    }

    /// 整个会话标记已读。不依赖本地缓存的未读。
    pub async fn mark_conversation_read(self: &Arc<Self>, key: &ConversationKey) -> Result<()> {
        let keys = {
            let mut state = self.state.write();
            let ids = state.unread.ids(key);
            state.unread.remove_ids(&ids)
        };
        if !keys.is_empty() {
            self.emit(SessionEvent::UnreadChanged(keys));
        }
        let mut narrow = self.narrow_of(key);
        narrow.push(NarrowTerm::is("unread"));
        let mut anchor = "oldest".to_owned();
        let mut include_anchor = false;
        loop {
            let result = self
                .client
                .update_flags_for_narrow(&FlagsNarrowUpdate {
                    narrow: narrow.clone(),
                    anchor: anchor.clone(),
                    include_anchor,
                    num_before: 0,
                    num_after: NARROW_FLAG_BATCH,
                    op: FlagOp::Add,
                    flag: MessageFlag::Read,
                })
                .await?;
            if result.found_newest {
                return Ok(());
            }
            let Some(last) = result.last_processed_id else {
                return Ok(());
            };
            anchor = last.to_string();
            include_anchor = false;
        }
    }

    /// 从这条消息开始的全部消息标记为未读。
    pub async fn mark_unread_from(&self, key: &ConversationKey, message_id: i64) -> Result<()> {
        let narrow = self.narrow_of(key);
        let mut anchor = message_id.to_string();
        let mut include_anchor = true;
        loop {
            let result = self
                .client
                .update_flags_for_narrow(&FlagsNarrowUpdate {
                    narrow: narrow.clone(),
                    anchor: anchor.clone(),
                    include_anchor,
                    num_before: 0,
                    num_after: NARROW_FLAG_BATCH,
                    op: FlagOp::Remove,
                    flag: MessageFlag::Read,
                })
                .await?;
            if result.found_newest {
                return Ok(());
            }
            let Some(last) = result.last_processed_id else {
                return Ok(());
            };
            anchor = last.to_string();
            include_anchor = false;
        }
    }
}
