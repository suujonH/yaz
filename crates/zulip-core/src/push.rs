//! Do-not-disturb levels of a conversation, matching the mobile push / email trigger conditions for channel messages under Zulip server rules
//! (same rules as user_allows_notifications_in_StreamTopic in zerver/lib/notification_data.py).

use std::fmt;

/// Four do-not-disturb levels of a conversation; the numeric values are stored in SQLite and must not be changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum DndLevel {
    /// Allow message notifications.
    #[default]
    Notify = 0,
    /// Receive messages without notifying.
    Silent = 1,
    /// Moved into the topic assistant without notifying.
    Assistant = 2,
    /// Block group messages: not shown, no notifications, not counted as unread.
    Blocked = 3,
}

impl DndLevel {
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Notify),
            1 => Some(Self::Silent),
            2 => Some(Self::Assistant),
            3 => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// The visibility_policy of a Zulip topic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum TopicVisibility {
    #[default]
    Inherit = 0,
    Muted = 1,
    Unmuted = 2,
    Followed = 3,
}

impl TopicVisibility {
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Inherit),
            1 => Some(Self::Muted),
            2 => Some(Self::Unmuted),
            3 => Some(Self::Followed),
            _ => None,
        }
    }
}

/// Which channel messages trigger one external notification path (mobile push or email).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PushTriggers {
    /// @me (including @ of user groups I belong to).
    pub mention: bool,
    /// @everyone (channel or topic wildcard).
    pub wildcard: bool,
    /// All messages.
    pub all_messages: bool,
}

/// "@me, @everyone, all messages" (Chinese labels); "none" (Chinese label) when nothing triggers.
impl fmt::Display for PushTriggers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<&str> = [
            (self.mention, "@我"),
            (self.wildcard, "@全体"),
            (self.all_messages, "全部消息"),
        ]
        .into_iter()
        .filter_map(|(on, text)| on.then_some(text))
        .collect();
        f.write_str(&if parts.is_empty() {
            "无".to_owned()
        } else {
            parts.join("、")
        })
    }
}

/// Global settings needed to compute one notification path (one copy for push and one for email).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PushRules {
    /// enable_offline_push_notifications / enable_offline_email_notifications.
    pub dm_and_mention: bool,
    /// enable_stream_push_notifications / enable_stream_email_notifications.
    pub stream: bool,
    /// enable_followed_topic_push_notifications / enable_followed_topic_email_notifications.
    pub followed_topic: bool,
    /// wildcard_mentions_notify (shared by push and email).
    pub wildcard_mentions: bool,
    /// enable_followed_topic_wildcard_mentions_notify (shared).
    pub followed_topic_wildcard_mentions: bool,
}

impl PushRules {
    /// `channel_all` is the channel's push_notifications / email_notifications, `channel_wildcard`
    /// is the channel's wildcard_mentions_notify; `None` means follow the global setting.
    pub fn evaluate(
        &self,
        channel_muted: bool,
        channel_all: Option<bool>,
        channel_wildcard: Option<bool>,
        topic: TopicVisibility,
    ) -> PushTriggers {
        let followed = topic == TopicVisibility::Followed;
        PushTriggers {
            mention: self.dm_and_mention,
            // @everyone, like @me, is limited by the "DMs and @me" switch
            wildcard: self.dm_and_mention
                && ((followed && self.followed_topic_wildcard_mentions)
                    || allows(
                        channel_muted,
                        topic,
                        channel_wildcard,
                        self.wildcard_mentions,
                        true,
                    )),
            all_messages: (followed && self.followed_topic)
                || allows(channel_muted, topic, channel_all, self.stream, false),
        }
    }
}

// No notification when the topic is muted; when the channel is muted only unmuted topics work as usual (followed topics are no exception),
// but an @everyone setting made on the channel itself can override the channel mute; otherwise the channel setting takes priority, and the global setting is used when unset
fn allows(
    channel_muted: bool,
    topic: TopicVisibility,
    channel_setting: Option<bool>,
    global_setting: bool,
    overrides_mute: bool,
) -> bool {
    if topic == TopicVisibility::Muted {
        return false;
    }
    if channel_muted && topic != TopicVisibility::Unmuted {
        return overrides_mute && channel_setting == Some(true);
    }
    channel_setting.unwrap_or(global_setting)
}

#[cfg(test)]
mod tests {
    use super::TopicVisibility as V;
    use super::*;

    const DEFAULTS: PushRules = PushRules {
        dm_and_mention: true,
        stream: false,
        followed_topic: true,
        wildcard_mentions: true,
        followed_topic_wildcard_mentions: true,
    };

    const fn t(mention: bool, wildcard: bool, all_messages: bool) -> PushTriggers {
        PushTriggers {
            mention,
            wildcard,
            all_messages,
        }
    }

    const M: PushTriggers = t(true, false, false);
    const MW: PushTriggers = t(true, true, false);
    const MWA: PushTriggers = t(true, true, true);

    #[test]
    fn evaluate() {
        let cases = [
            (false, None, None, V::Inherit, MW),
            (false, None, None, V::Muted, M),
            (false, None, None, V::Followed, MWA),
            (true, None, None, V::Inherit, M),
            // An @everyone setting made on the channel itself can override the channel mute, but not the topic mute
            (true, None, Some(true), V::Inherit, MW),
            (true, None, Some(true), V::Muted, M),
            (true, None, None, V::Unmuted, MW),
            // When the channel is muted, "all messages" comes from followed topics rather than the channel setting
            (true, Some(true), None, V::Followed, MWA),
            (true, Some(true), None, V::Inherit, M),
            (false, Some(true), None, V::Inherit, MWA),
            (false, None, Some(false), V::Inherit, M),
        ];
        for (muted, all, wildcard, policy, expected) in cases {
            assert_eq!(DEFAULTS.evaluate(muted, all, wildcard, policy), expected);
        }
    }

    #[test]
    fn channel_setting_overrides_global() {
        let rules = PushRules {
            stream: true,
            ..DEFAULTS
        };
        assert_eq!(rules.evaluate(false, None, None, V::Inherit), MWA);
        assert_eq!(rules.evaluate(false, Some(false), None, V::Inherit), MW);
    }

    #[test]
    fn wildcard_requires_dm_and_mention() {
        let rules = PushRules {
            dm_and_mention: false,
            ..DEFAULTS
        };
        assert_eq!(
            rules.evaluate(false, None, Some(true), V::Inherit),
            PushTriggers::default()
        );
        assert_eq!(
            rules.evaluate(false, None, None, V::Followed),
            t(false, false, true)
        );
    }

    #[test]
    fn describe() {
        assert_eq!(PushTriggers::default().to_string(), "无");
        assert_eq!(t(true, false, true).to_string(), "@我、全部消息");
        assert_eq!(MWA.to_string(), "@我、@全体、全部消息");
    }

    #[test]
    fn stored_values() {
        assert_eq!(DndLevel::Blocked as i32, 3);
        assert_eq!(DndLevel::from_i32(2), Some(DndLevel::Assistant));
        assert_eq!(DndLevel::from_i32(4), None);
        assert_eq!(TopicVisibility::from_i32(3), Some(V::Followed));
    }
}
