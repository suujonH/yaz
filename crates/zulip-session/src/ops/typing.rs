//! "Typing" indicator: shows the other side's typing state and sends your own.

use std::sync::Arc;
use std::time::Duration;

use zulip_core::ConversationKey;

use crate::session::Session;

impl Session {
    pub fn typing_text(&self, key: &ConversationKey) -> Option<String> {
        let state = self.state.read();
        let names: Vec<String> = state
            .typing
            .users(key)
            .into_iter()
            .map(|id| state.display_name(id))
            .collect();
        crate::state::misc::typing_text(key, &names)
    }

    /// Called when the composer content changes: sends "typing" at the frequency the server
    /// requires, and automatically sends stop after typing stops.
    pub fn on_compose_input(self: &Arc<Self>, key: &ConversationKey, has_text: bool) {
        if !has_text {
            self.stop_typing();
            return;
        }
        let (enabled, wait, stopped) = {
            let state = self.state.read();
            let enabled = match key {
                ConversationKey::Topic(_) => state.settings.send_stream_typing_notifications,
                ConversationKey::Dm(d) => {
                    !d.is_self_dm() && state.settings.send_private_typing_notifications
                }
            };
            (
                enabled,
                Duration::from_millis(state.realm.typing_started_wait_ms.max(0) as u64),
                Duration::from_millis(state.realm.typing_stopped_wait_ms.max(1000) as u64),
            )
        };
        if !enabled {
            return;
        }
        let (send_start, stop_previous, generation) = {
            let mut sender = self.typing_sender.lock();
            sender.generation += 1;
            let now = std::time::Instant::now();
            let (send_start, stop_previous) = match &sender.current {
                Some((current, at)) if current == key => (now.duration_since(*at) >= wait, None),
                Some((current, _)) => (true, Some(current.clone())),
                None => (true, None),
            };
            if send_start {
                sender.current = Some((key.clone(), now));
            }
            (send_start, stop_previous, sender.generation)
        };
        if let Some(previous) = stop_previous {
            self.spawn_typing("stop", previous);
        }
        if send_start {
            self.spawn_typing("start", key.clone());
        }
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(stopped).await;
            if this.typing_sender.lock().generation == generation {
                this.stop_typing();
            }
        });
    }

    /// Called after sending or clearing the composer.
    pub fn stop_typing(self: &Arc<Self>) {
        let current = {
            let mut sender = self.typing_sender.lock();
            sender.generation += 1;
            sender.current.take()
        };
        if let Some((key, _)) = current {
            self.spawn_typing("stop", key);
        }
    }

    fn spawn_typing(self: &Arc<Self>, op: &'static str, key: ConversationKey) {
        let this = self.clone();
        tokio::spawn(async move {
            let (stream, topic, users) = {
                let state = this.state.read();
                match &key {
                    ConversationKey::Topic(t) => {
                        (Some(t.stream_id), Some(state.topic_name(&key)), Vec::new())
                    }
                    ConversationKey::Dm(d) => (None, None, d.other_user_ids().to_vec()),
                }
            };
            if let Err(error) = this
                .client
                .send_typing(op, stream, topic.as_deref(), &users)
                .await
            {
                log::info!("发送输入状态失败：{error}");
            }
        });
    }
}
