//! 会话免打扰级别，与 Zulip 服务端规则下频道消息的手机推送 / 邮件触发条件
//! （规则同 zerver/lib/notification_data.py 的 user_allows_notifications_in_StreamTopic）。

use std::fmt;

/// 会话的四级免打扰，数值保存在 SQLite 中，不能修改。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum DndLevel {
    /// 允许消息提醒。
    #[default]
    Notify = 0,
    /// 接收消息但不提醒。
    Silent = 1,
    /// 收进群助手且不提醒。
    Assistant = 2,
    /// 屏蔽群消息：不显示、不提醒、不计入未读。
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

/// Zulip 话题的 visibility_policy。
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

/// 某一路外部通知（手机推送或邮件）会因哪些频道消息触发。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PushTriggers {
    /// @我（包括 @我所在的用户组）。
    pub mention: bool,
    /// @全体成员（频道或话题通配符）。
    pub wildcard: bool,
    /// 所有消息。
    pub all_messages: bool,
}

/// “@我、@全体、全部消息”；没有任何触发时为“无”。
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

/// 计算一路通知所需的全局设置（推送和邮件各一份）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PushRules {
    /// enable_offline_push_notifications / enable_offline_email_notifications。
    pub dm_and_mention: bool,
    /// enable_stream_push_notifications / enable_stream_email_notifications。
    pub stream: bool,
    /// enable_followed_topic_push_notifications / enable_followed_topic_email_notifications。
    pub followed_topic: bool,
    /// wildcard_mentions_notify（推送和邮件共用）。
    pub wildcard_mentions: bool,
    /// enable_followed_topic_wildcard_mentions_notify（共用）。
    pub followed_topic_wildcard_mentions: bool,
}

impl PushRules {
    /// `channel_all` 为频道的 push_notifications / email_notifications，
    /// `channel_wildcard` 为频道的 wildcard_mentions_notify；`None` 表示跟随全局。
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
            // @全体 与 @我 一样受“私信和 @我”开关限制
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

// 话题静音时不通知；频道静音时只有取消静音的话题照常（关注的话题也不例外），
// 但频道上单独设置的 @全体 可以越过频道静音；其余情况频道设置优先，未设置时用全局设置
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
            // 频道上单独设置的 @全体 可以越过频道静音，但不能越过话题静音
            (true, None, Some(true), V::Inherit, MW),
            (true, None, Some(true), V::Muted, M),
            (true, None, None, V::Unmuted, MW),
            // 频道静音时“全部消息”来自关注的话题，而不是频道设置
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
