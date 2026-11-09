//! Conversation keys: topics and DMs (including group DMs and DMs with yourself). Besides
//! conversations, message-list entries also include conversation folders.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use crate::util::parse_digits;

/// Topic. Topic names are case-insensitive (same as Zulip).
#[derive(Clone, Debug)]
pub struct TopicKey {
    pub stream_id: i64,
    pub topic: String,
}

impl TopicKey {
    pub fn new(stream_id: i64, topic: impl Into<String>) -> Self {
        Self {
            stream_id,
            topic: topic.into(),
        }
    }

    fn folded(&self) -> impl Iterator<Item = char> + '_ {
        self.topic.chars().flat_map(char::to_lowercase)
    }
}

impl PartialEq for TopicKey {
    fn eq(&self, other: &Self) -> bool {
        self.stream_id == other.stream_id && self.folded().eq(other.folded())
    }
}

impl Eq for TopicKey {}

impl Hash for TopicKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.stream_id.hash(state);
        self.folded().for_each(|c| c.hash(state));
    }
}

/// The other participants of a DM: sorted, deduplicated, excluding self; empty means a DM with
/// yourself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct DmKey {
    other_user_ids: Vec<i64>,
}

impl DmKey {
    pub fn new(user_ids: impl IntoIterator<Item = i64>, self_id: i64) -> Self {
        let mut ids: Vec<i64> = user_ids.into_iter().filter(|&id| id != self_id).collect();
        ids.sort_unstable();
        ids.dedup();
        Self {
            other_user_ids: ids,
        }
    }

    pub fn other_user_ids(&self) -> &[i64] {
        &self.other_user_ids
    }

    pub fn is_self_dm(&self) -> bool {
        self.other_user_ids.is_empty()
    }

    pub fn is_group_dm(&self) -> bool {
        self.other_user_ids.len() >= 2
    }
}

/// Identifier of a conversation. [`Display`](fmt::Display) / [`FromStr`] give the form stored in
/// SQLite: topic `t:{channel id}:{lowercase topic}`, DM `d:{id1,id2}` (with yourself it is `d:`). A
/// parsed topic name is lowercase and may only be used for comparison, not for display or requests.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConversationKey {
    Topic(TopicKey),
    Dm(DmKey),
}

impl ConversationKey {
    pub fn topic(stream_id: i64, topic: impl Into<String>) -> Self {
        Self::Topic(TopicKey::new(stream_id, topic))
    }

    pub fn dm(user_ids: impl IntoIterator<Item = i64>, self_id: i64) -> Self {
        Self::Dm(DmKey::new(user_ids, self_id))
    }
}

impl fmt::Display for ConversationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Topic(t) => write!(f, "t:{}:{}", t.stream_id, t.topic.to_lowercase()),
            Self::Dm(d) => {
                let ids: Vec<String> = d.other_user_ids.iter().map(i64::to_string).collect();
                write!(f, "d:{}", ids.join(","))
            }
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("无效的会话键：{0}")]
pub struct ParseKeyError(pub String);

impl FromStr for ConversationKey {
    type Err = ParseKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseKeyError(s.to_owned());
        if let Some(rest) = s.strip_prefix("t:") {
            let (id, topic) = rest.split_once(':').ok_or_else(invalid)?;
            return Ok(Self::topic(parse_digits(id).ok_or_else(invalid)?, topic));
        }
        let body = s.strip_prefix("d:").ok_or_else(invalid)?;
        if body.is_empty() {
            return Ok(Self::Dm(DmKey::default()));
        }
        let mut ids = body
            .split(',')
            .map(parse_digits)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(invalid)?;
        ids.sort_unstable();
        ids.dedup();
        Ok(Self::Dm(DmKey {
            other_user_ids: ids,
        }))
    }
}

/// Entry in the message list that represents a group of conversations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConversationFolder {
    /// Topic assistant: channel topics whose message setting is "move into the topic assistant".
    Helper,
    /// Resolved topics (when "collapse resolved topics" is on).
    Resolved,
}

impl fmt::Display for ConversationFolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Helper => "helper",
            Self::Resolved => "resolved",
        })
    }
}

/// A row of the message list: a conversation or a conversation folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ListEntry {
    Conversation(ConversationKey),
    Folder(ConversationFolder),
}

impl ListEntry {
    pub fn conversation(&self) -> Option<&ConversationKey> {
        match self {
            Self::Conversation(key) => Some(key),
            Self::Folder(_) => None,
        }
    }
}

impl fmt::Display for ListEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversation(key) => key.fmt(f),
            Self::Folder(folder) => folder.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::hash::DefaultHasher;

    use super::*;

    fn hash_of(key: &ConversationKey) -> u64 {
        let mut h = DefaultHasher::new();
        key.hash(&mut h);
        h.finish()
    }

    fn parse(s: &str) -> ConversationKey {
        s.parse().unwrap()
    }

    #[test]
    fn topic_round_trip() {
        for (id, topic) in [
            (4, "构建失败"),
            (5, "a:b:c"),
            (6, "MixedCase Topic"),
            (7, ""),
        ] {
            let key = ConversationKey::topic(id, topic);
            let parsed = parse(&key.to_string());
            assert_eq!(key, parsed);
            assert_eq!(hash_of(&key), hash_of(&parsed));
        }
    }

    #[test]
    fn topic_is_case_insensitive() {
        let a = ConversationKey::topic(1, "Hello");
        let b = ConversationKey::topic(1, "hELLO");
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
        assert_ne!(a, ConversationKey::topic(2, "Hello"));
        assert_eq!(a.to_string(), "t:1:hello");
    }

    #[test]
    fn topic_with_colons_keeps_remainder() {
        let ConversationKey::Topic(t) = parse("t:12:x:y:z") else {
            panic!("应为话题");
        };
        assert_eq!(t.stream_id, 12);
        assert_eq!(t.topic, "x:y:z");
    }

    #[test]
    fn dm_round_trip() {
        let cases: [(&[i64], &str); 3] = [(&[], "d:"), (&[9], "d:9"), (&[12, 9, 10], "d:9,10,12")];
        for (ids, expected) in cases {
            let key = ConversationKey::dm(ids.iter().copied(), 8);
            assert_eq!(key.to_string(), expected);
            assert_eq!(key, parse(expected));
        }
    }

    #[test]
    fn dm_removes_self_dedupes_and_sorts() {
        let key = DmKey::new([10, 8, 9, 10], 8);
        assert_eq!(key.other_user_ids(), [9, 10]);
        assert!(key.is_group_dm());
        assert!(!key.is_self_dm());
        assert!(DmKey::new([8], 8).is_self_dm());
    }

    #[test]
    fn dm_keys_work_in_maps() {
        let map = HashMap::from([(ConversationKey::dm([9, 10], 8), 1)]);
        assert!(map.contains_key(&ConversationKey::dm([10, 9], 8)));
        assert!(map.contains_key(&parse("d:9,10")));
        assert!(!map.contains_key(&ConversationKey::dm([9], 8)));
    }

    #[test]
    fn list_entry_text() {
        assert_eq!(
            ListEntry::Folder(ConversationFolder::Helper).to_string(),
            "helper"
        );
        let topic = ConversationKey::topic(1, "A");
        let entry = ListEntry::Conversation(topic.clone());
        assert_eq!(entry.to_string(), "t:1:a");
        assert_eq!(entry.conversation(), Some(&topic));
    }

    #[test]
    fn invalid_keys_are_errors() {
        for s in [
            "x:1",
            "t:abc:topic",
            "t:1",
            "d:1,a",
            "",
            "helper",
            "resolved",
        ] {
            assert!(s.parse::<ConversationKey>().is_err(), "{s}");
        }
    }

    #[test]
    fn different_kinds_are_not_equal() {
        let topic = ConversationKey::topic(1, "");
        let dm = ConversationKey::dm([], 8);
        assert_ne!(topic, dm);
    }
}
