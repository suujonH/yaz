//! Pure logic for topic names and topic operations.

use zulip_core::model::{Channel, RealmInfo};
use zulip_core::permission::PermissionEvaluator;

pub const RESOLVED_PREFIX: &str = "✔ ";
pub const RESOLVED_LABEL: &str = "(已解决)";

pub fn is_resolved_name(name: &str) -> bool {
    name.starts_with(RESOLVED_PREFIX)
}

/// Strips the leading checkmark and space; there may be several.
pub fn strip_resolved(name: &str) -> &str {
    let mut rest = name;
    while let Some(stripped) = rest.strip_prefix(RESOLVED_PREFIX) {
        rest = stripped;
    }
    rest
}

pub fn toggle_resolved_name(name: &str) -> String {
    if is_resolved_name(name) {
        strip_resolved(name).to_owned()
    } else {
        format!("{RESOLVED_PREFIX}{name}")
    }
}

/// Channels a topic can be moved to: other channels that are subscribed, not archived, where the
/// user may post and create topics, and that allow naming topics.
pub fn move_targets(
    channels: &[Channel],
    from_stream: i64,
    permissions: &PermissionEvaluator,
    realm: &RealmInfo,
) -> Vec<Channel> {
    channels
        .iter()
        .filter(|c| {
            c.stream_id != from_stream
                && c.is_subscribed()
                && !c.is_archived
                && permissions.can_send_to_channel(&c.permissions)
                && permissions.can_create_topic(&c.permissions)
                && realm.allows_named_topics(c)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zulip_core::permission::PermissionInputs;

    #[test]
    fn resolved_names() {
        assert!(is_resolved_name("✔ 完成"));
        assert!(!is_resolved_name("完成 ✔ "));
        assert_eq!(strip_resolved("✔ ✔ 完成"), "完成");
        assert_eq!(strip_resolved("完成"), "完成");
        assert_eq!(toggle_resolved_name("完成"), "✔ 完成");
        assert_eq!(toggle_resolved_name("✔ 完成"), "完成");
    }

    #[test]
    fn move_targets_filter() {
        let evaluator = PermissionEvaluator::new(PermissionInputs {
            self_id: 1,
            role: 100,
            groups: HashMap::new(),
            realm_group_settings: HashMap::new(),
            allow_everyone_group: HashMap::new(),
            allow_message_editing: true,
            edit_limit_seconds: None,
            delete_limit_seconds: None,
        });
        let realm = RealmInfo::default();
        let channel = |id: i64, subscribed: bool, archived: bool| Channel {
            stream_id: id,
            subscription: subscribed.then(zulip_core::model::Subscription::default),
            is_archived: archived,
            topics_policy: "inherit".into(),
            ..Channel::default()
        };
        let list = [
            channel(1, true, false),
            channel(2, true, false),
            channel(3, false, false),
            channel(4, true, true),
        ];
        let ids: Vec<i64> = move_targets(&list, 1, &evaluator, &realm)
            .iter()
            .map(|c| c.stream_id)
            .collect();
        assert_eq!(ids, [2]);
    }
}
