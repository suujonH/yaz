use std::collections::HashMap;

use serde_json::Value;
use zulip_api::{RegisterResult, SubscriptionDto, TopicDto};
use zulip_core::model::{Channel, TopicInfo};
use zulip_core::push::TopicVisibility;

use crate::convert::{channel_from_stream, channel_from_subscription};

/// Channels, topic lists (loaded on demand), subscribers (loaded on demand) and topic follow state.
#[derive(Default)]
pub struct Channels {
    by_id: HashMap<i64, Channel>,
    topics: HashMap<i64, Vec<TopicInfo>>,
    subscribers: HashMap<i64, Vec<i64>>,
    user_topics: HashMap<(i64, String), TopicVisibility>,
}

impl Channels {
    pub fn load(reg: &RegisterResult) -> Self {
        let mut by_id = HashMap::new();
        for dto in reg.streams.iter().chain(&reg.never_subscribed) {
            by_id.insert(dto.stream_id, channel_from_stream(dto));
        }
        for dto in &reg.unsubscribed {
            by_id.insert(dto.stream_id, channel_from_subscription(dto, false));
        }
        for dto in &reg.subscriptions {
            by_id.insert(dto.stream_id, channel_from_subscription(dto, true));
        }
        let user_topics = reg
            .user_topics
            .iter()
            .filter_map(|t| {
                Some((
                    (t.stream_id, t.topic_name.to_lowercase()),
                    TopicVisibility::from_i32(t.visibility_policy)?,
                ))
            })
            .collect();
        Self {
            by_id,
            user_topics,
            ..Self::default()
        }
    }

    pub fn get(&self, id: i64) -> Option<&Channel> {
        self.by_id.get(&id)
    }

    pub fn all(&self) -> impl Iterator<Item = &Channel> {
        self.by_id.values()
    }

    pub fn subscribed(&self) -> Vec<Channel> {
        let mut list: Vec<Channel> = self
            .by_id
            .values()
            .filter(|c| c.is_subscribed())
            .cloned()
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name).then(a.stream_id.cmp(&b.stream_id)));
        list
    }

    /// Loaded topics (`max_id` descending); `None` if never loaded.
    pub fn topics(&self, stream_id: i64) -> Option<&[TopicInfo]> {
        self.topics.get(&stream_id).map(Vec::as_slice)
    }

    pub fn set_topics(&mut self, stream_id: i64, topics: &[TopicDto]) {
        let mut list: Vec<TopicInfo> = topics
            .iter()
            .map(|t| TopicInfo {
                name: t.name.clone(),
                max_id: t.max_id,
            })
            .collect();
        list.sort_by_key(|t| std::cmp::Reverse(t.max_id));
        self.topics.insert(stream_id, list);
    }

    pub fn invalidate_topics(&mut self, stream_id: i64) -> bool {
        self.topics.remove(&stream_id).is_some()
    }

    /// A new message updates the loaded topic list; returns whether the list changed.
    pub fn on_new_message(&mut self, stream_id: i64, topic: &str, id: i64) -> bool {
        let Some(list) = self.topics.get_mut(&stream_id) else {
            return false;
        };
        match list
            .iter_mut()
            .find(|t| t.name.to_lowercase() == topic.to_lowercase())
        {
            Some(existing) => existing.max_id = existing.max_id.max(id),
            None => list.push(TopicInfo {
                name: topic.to_owned(),
                max_id: id,
            }),
        }
        list.sort_by_key(|t| std::cmp::Reverse(t.max_id));
        true
    }

    pub fn subscribers(&self, stream_id: i64) -> Option<&[i64]> {
        self.subscribers.get(&stream_id).map(Vec::as_slice)
    }

    pub fn set_subscribers(&mut self, stream_id: i64, ids: Vec<i64>) {
        self.subscribers.insert(stream_id, ids);
    }

    pub fn visibility(&self, stream_id: i64, topic: &str) -> TopicVisibility {
        self.user_topics
            .get(&(stream_id, topic.to_lowercase()))
            .copied()
            .unwrap_or(TopicVisibility::Inherit)
    }

    /// `user_topic` event; returns `(channel, topic)`.
    pub fn apply_user_topic(&mut self, event: &Value) -> Option<(i64, String)> {
        let stream_id = event.get("stream_id")?.as_i64()?;
        let topic = event.get("topic_name")?.as_str()?.to_owned();
        let policy = event.get("visibility_policy")?.as_i64()? as i32;
        let key = (stream_id, topic.to_lowercase());
        match TopicVisibility::from_i32(policy) {
            Some(TopicVisibility::Inherit) | None => self.user_topics.remove(&key),
            Some(visibility) => self.user_topics.insert(key, visibility),
        };
        Some((stream_id, topic))
    }

    /// `subscription` event; returns the channels that changed.
    pub fn apply_subscription_event(&mut self, event: &Value) -> Vec<i64> {
        let list = || {
            event
                .get("subscriptions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        match event.get("op").and_then(Value::as_str).unwrap_or_default() {
            "add" => list()
                .into_iter()
                .filter_map(|s| serde_json::from_value::<SubscriptionDto>(s).ok())
                .map(|dto| {
                    self.topics.remove(&dto.stream_id);
                    self.by_id
                        .insert(dto.stream_id, channel_from_subscription(&dto, true));
                    dto.stream_id
                })
                .collect(),
            "remove" => list()
                .iter()
                .filter_map(|s| s.get("stream_id")?.as_i64())
                .inspect(|id| {
                    if let Some(channel) = self.by_id.get_mut(id) {
                        channel.subscription = None;
                    }
                })
                .collect(),
            "update" => {
                let id = event.get("stream_id").and_then(Value::as_i64);
                let property = event.get("property").and_then(Value::as_str);
                let value = event.get("value");
                if let (Some(id), Some(property), Some(value)) = (id, property, value)
                    && let Some(channel) = self.by_id.get_mut(&id)
                {
                    channel.apply_subscription_property(property, value);
                    return vec![id];
                }
                Vec::new()
            }
            op @ ("peer_add" | "peer_remove") => self.apply_peers(event, op == "peer_add"),
            _ => Vec::new(),
        }
    }

    fn apply_peers(&mut self, event: &Value, add: bool) -> Vec<i64> {
        let ids = |name: &str| -> Vec<i64> {
            event
                .get(name)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default()
        };
        let users = ids("user_ids");
        let streams = ids("stream_ids");
        for stream in &streams {
            if let Some(list) = self.subscribers.get_mut(stream) {
                list.retain(|u| !users.contains(u));
                if add {
                    list.extend(&users);
                }
            }
            if let Some(channel) = self.by_id.get_mut(stream) {
                let delta = users.len() as i32;
                channel.subscriber_count =
                    (channel.subscriber_count + if add { delta } else { -delta }).max(0);
            }
        }
        streams
    }

    /// `stream` event; returns the channels that changed.
    pub fn apply_stream_event(&mut self, event: &Value) -> Vec<i64> {
        let streams = || {
            event
                .get("streams")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        match event.get("op").and_then(Value::as_str).unwrap_or_default() {
            "create" => streams()
                .into_iter()
                .filter_map(|s| serde_json::from_value(s).ok())
                .map(|dto: zulip_api::StreamDto| {
                    self.by_id
                        .entry(dto.stream_id)
                        .or_insert_with(|| channel_from_stream(&dto));
                    dto.stream_id
                })
                .collect(),
            "delete" => {
                let mut ids: Vec<i64> = event
                    .get("stream_ids")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default();
                ids.extend(
                    streams()
                        .iter()
                        .filter_map(|s| s.get("stream_id")?.as_i64()),
                );
                for id in &ids {
                    self.by_id.remove(id);
                    self.topics.remove(id);
                    self.subscribers.remove(id);
                }
                ids
            }
            "update" => {
                let Some(id) = event.get("stream_id").and_then(Value::as_i64) else {
                    return Vec::new();
                };
                if let Some(channel) = self.by_id.get_mut(&id) {
                    channel.apply_stream_update(event);
                }
                vec![id]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channels() -> Channels {
        let reg: RegisterResult = serde_json::from_value(json!({
            "streams": [{"stream_id": 1, "name": "公开"}, {"stream_id": 2, "name": "乙"}],
            "subscriptions": [{"stream_id": 2, "name": "乙", "color": "#fff"}, {"stream_id": 3, "name": "甲"}],
            "unsubscribed": [{"stream_id": 4, "name": "退订"}],
            "user_topics": [{"stream_id": 2, "topic_name": "Muted", "visibility_policy": 1}],
        }))
        .unwrap();
        Channels::load(&reg)
    }

    #[test]
    fn load_keeps_subscription_data_and_sorts() {
        let c = channels();
        assert!(!c.get(1).unwrap().is_subscribed());
        assert_eq!(c.get(2).unwrap().color, "#fff");
        assert!(!c.get(4).unwrap().is_subscribed());
        let names: Vec<_> = c.subscribed().iter().map(|c| c.stream_id).collect();
        assert_eq!(names.len(), 2);
        assert_eq!(c.visibility(2, "muted"), TopicVisibility::Muted);
    }

    #[test]
    fn topics_update_on_new_message_only_when_loaded() {
        let mut c = channels();
        assert!(!c.on_new_message(2, "x", 10));
        c.set_topics(
            2,
            &[
                TopicDto {
                    name: "a".into(),
                    max_id: 1,
                },
                TopicDto {
                    name: "B".into(),
                    max_id: 5,
                },
            ],
        );
        assert_eq!(c.topics(2).unwrap()[0].name, "B");
        assert!(c.on_new_message(2, "A", 7));
        assert_eq!(c.topics(2).unwrap()[0].name, "a");
        assert!(c.on_new_message(2, "新", 9));
        assert_eq!(c.topics(2).unwrap()[0].name, "新");
    }

    #[test]
    fn subscription_events() {
        let mut c = channels();
        c.set_subscribers(2, vec![1]);
        assert_eq!(
            c.apply_subscription_event(
                &json!({"op": "peer_add", "stream_ids": [2], "user_ids": [5]})
            ),
            [2]
        );
        assert_eq!(c.subscribers(2).unwrap(), [1, 5]);
        assert_eq!(c.get(2).unwrap().subscriber_count, 1);
        c.apply_subscription_event(&json!({"op": "remove", "subscriptions": [{"stream_id": 2}]}));
        assert!(!c.get(2).unwrap().is_subscribed());
        c.apply_subscription_event(
            &json!({"op": "update", "stream_id": 3, "property": "is_muted", "value": true}),
        );
        assert!(c.get(3).unwrap().is_muted());
        c.apply_stream_event(&json!({"op": "delete", "streams": [{"stream_id": 3}]}));
        assert!(c.get(3).is_none());
    }
}
