//! Recent searches: what was opened from search results (contacts, channels, topics, files), newest
//! first, kept per account.

use serde::{Deserialize, Serialize};
use zulip_core::ConversationKey;
use zulip_core::html::UploadLink;
use zulip_store::AccountSettings;

use crate::queries::{ChannelHit, ContactHit};
use crate::session::Session;

/// How many recent searches are kept.
pub const RECENT_SEARCH_LIMIT: usize = 20;

/// One recent search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecentSearch {
    Contact(i64),
    Channel(i64),
    Topic(ConversationKey),
    File {
        link: UploadLink,
        message_id: i64,
        key: ConversationKey,
        /// Second line of the row, as shown when it was opened ("Sent to ..." and so on).
        source: String,
    },
}

/// A recent search ready to show: contacts and channels as search hits (current names), files as
/// saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecentHit {
    Contact(ContactHit),
    Channel(ChannelHit),
    File {
        link: UploadLink,
        message_id: i64,
        key: ConversationKey,
        source: String,
    },
}

/// A conversation key as saved: topic names keep their case (a parsed key string is lowercase).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum SavedKey {
    Topic { stream_id: i64, topic: String },
    Dm { users: Vec<i64> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Saved {
    Contact {
        user_id: i64,
    },
    Channel {
        stream_id: i64,
    },
    Topic {
        stream_id: i64,
        topic: String,
    },
    File {
        path: String,
        name: String,
        is_image: bool,
        message_id: i64,
        key: SavedKey,
        #[serde(default)]
        source: String,
    },
}

fn save_key(key: &ConversationKey) -> SavedKey {
    match key {
        ConversationKey::Topic(t) => SavedKey::Topic {
            stream_id: t.stream_id,
            topic: t.topic.clone(),
        },
        ConversationKey::Dm(d) => SavedKey::Dm {
            users: d.other_user_ids().to_vec(),
        },
    }
}

fn load_key(key: SavedKey, self_id: i64) -> ConversationKey {
    match key {
        SavedKey::Topic { stream_id, topic } => ConversationKey::topic(stream_id, topic),
        SavedKey::Dm { users } => ConversationKey::dm(users, self_id),
    }
}

/// `None` for a topic item holding a DM key (never recorded).
fn save(item: &RecentSearch) -> Option<Saved> {
    Some(match item {
        RecentSearch::Contact(user_id) => Saved::Contact { user_id: *user_id },
        RecentSearch::Channel(stream_id) => Saved::Channel {
            stream_id: *stream_id,
        },
        RecentSearch::Topic(key) => match save_key(key) {
            SavedKey::Topic { stream_id, topic } => Saved::Topic { stream_id, topic },
            SavedKey::Dm { .. } => return None,
        },
        RecentSearch::File {
            link,
            message_id,
            key,
            source,
        } => Saved::File {
            path: link.path.clone(),
            name: link.file_name.clone(),
            is_image: link.is_image,
            message_id: *message_id,
            key: save_key(key),
            source: source.clone(),
        },
    })
}

fn load(saved: Saved, self_id: i64) -> RecentSearch {
    match saved {
        Saved::Contact { user_id } => RecentSearch::Contact(user_id),
        Saved::Channel { stream_id } => RecentSearch::Channel(stream_id),
        Saved::Topic { stream_id, topic } => {
            RecentSearch::Topic(ConversationKey::topic(stream_id, topic))
        }
        Saved::File {
            path,
            name,
            is_image,
            message_id,
            key,
            source,
        } => RecentSearch::File {
            link: UploadLink {
                path,
                file_name: name,
                is_image,
            },
            message_id,
            key: load_key(key, self_id),
            source,
        },
    }
}

/// `item` first, without an older copy of it, at most [`RECENT_SEARCH_LIMIT`]. Files are the same
/// when their upload path is.
fn push_front(list: &mut Vec<RecentSearch>, item: RecentSearch) {
    let same = |other: &RecentSearch| match (other, &item) {
        (RecentSearch::File { link: a, .. }, RecentSearch::File { link: b, .. }) => {
            a.path == b.path
        }
        (a, b) => a == b,
    };
    list.retain(|other| !same(other));
    list.insert(0, item);
    list.truncate(RECENT_SEARCH_LIMIT);
}

impl Session {
    fn recent_search_settings(&self) -> AccountSettings {
        AccountSettings::new(&self.db, self.account_id)
    }

    /// Recent searches, newest first. Unreadable saved data counts as none.
    pub fn recent_searches(&self) -> Vec<RecentSearch> {
        let self_id = self.self_id();
        self.recent_search_settings()
            .recent_search()
            .and_then(|json| serde_json::from_str::<Vec<Saved>>(&json).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|saved| load(saved, self_id))
            .collect()
    }

    /// Recent searches ready to show, newest first. Contacts no longer active and channels no longer
    /// known are left out.
    pub fn recent_search_hits(&self) -> Vec<RecentHit> {
        self.recent_searches()
            .into_iter()
            .filter_map(|item| self.recent_hit(item))
            .collect()
    }

    fn recent_hit(&self, item: RecentSearch) -> Option<RecentHit> {
        Some(match item {
            RecentSearch::Contact(user_id) => {
                let user = self.user(user_id).filter(|u| u.is_active)?;
                RecentHit::Contact(ContactHit {
                    user_id,
                    display_name: self.display_name(user_id),
                    avatar_url: user.avatar_url,
                })
            }
            RecentSearch::Channel(stream_id) => {
                self.channel(stream_id)?;
                let subtitle = match self.topics(stream_id) {
                    Some(topics) => format!("{}个话题", topics.len()),
                    None => "频道".to_owned(),
                };
                RecentHit::Channel(ChannelHit::Channel {
                    stream_id,
                    title: format!("#{}", self.channel_display_name(stream_id)),
                    subtitle,
                })
            }
            RecentSearch::Topic(key) => {
                let ConversationKey::Topic(topic) = &key else {
                    return None;
                };
                self.channel(topic.stream_id)?;
                let title = topic.topic.clone();
                let subtitle = format!("频道：#{}", self.channel_display_name(topic.stream_id));
                RecentHit::Channel(ChannelHit::Topic {
                    key,
                    title,
                    subtitle,
                    max_id: 0,
                })
            }
            RecentSearch::File {
                link,
                message_id,
                key,
                source,
            } => RecentHit::File {
                link,
                message_id,
                key,
                source,
            },
        })
    }

    /// Record an item opened from search results (moves it to the front when already there).
    pub fn add_recent_search(&self, item: RecentSearch) {
        let mut list = self.recent_searches();
        push_front(&mut list, item);
        self.write_recent_searches(&list);
    }

    pub fn clear_recent_searches(&self) {
        self.write_recent_searches(&[]);
    }

    fn write_recent_searches(&self, list: &[RecentSearch]) {
        let saved: Vec<Saved> = list.iter().filter_map(save).collect();
        if let Ok(json) = serde_json::to_string(&saved) {
            self.recent_search_settings().set_recent_search(&json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates() {
        let mut list = Vec::new();
        push_front(&mut list, RecentSearch::Contact(1));
        push_front(&mut list, RecentSearch::Channel(2));
        push_front(&mut list, RecentSearch::Contact(1));
        assert_eq!(
            list,
            vec![RecentSearch::Contact(1), RecentSearch::Channel(2)]
        );
        for id in 0..30 {
            push_front(&mut list, RecentSearch::Channel(100 + id));
        }
        assert_eq!(list.len(), RECENT_SEARCH_LIMIT);
    }

    #[test]
    fn topic_keeps_its_case() {
        let item = RecentSearch::Topic(ConversationKey::topic(5, "Hello World"));
        let json = serde_json::to_string(&vec![save(&item).unwrap()]).unwrap();
        let back: Vec<Saved> = serde_json::from_str(&json).unwrap();
        match load(back[0].clone(), 9) {
            RecentSearch::Topic(ConversationKey::Topic(t)) => assert_eq!(t.topic, "Hello World"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
