//! Generates and parses `#narrow/...` links of the Zulip web app.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::conversation::{ConversationKey, DmKey};
use crate::util::{eq_ignore_case, parse_digits};

const NARROW: &str = "#narrow/";

const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// A parsed internal Zulip link. `topic` is `None` when there is only a channel.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ParsedLink {
    pub stream_id: Option<i64>,
    pub topic: Option<String>,
    pub dm_user_ids: Option<Vec<i64>>,
    pub near_message_id: Option<i64>,
}

/// Same as encodeHashComponent of the Zulip web app: URL-encode, then replace % with ., and escape ( ) ..
pub fn encode_hash_component(s: &str) -> String {
    let escaped = utf8_percent_encode(s, UNRESERVED).to_string();
    let mut out = String::with_capacity(escaped.len());
    // Must be done in a single pass, otherwise a newly produced "." would be replaced again
    for c in escaped.chars() {
        match c {
            '%' => out.push('.'),
            '.' => out.push_str(".2E"),
            _ => out.push(c),
        }
    }
    out
}

/// The inverse of [`encode_hash_component`].
pub fn decode_hash_component(s: &str) -> String {
    let unescaped = s.replace('.', "%");
    match percent_decode_str(&unescaped).decode_utf8() {
        Ok(text) => text.into_owned(),
        Err(_) => s.to_owned(),
    }
}

/// Same as encode_stream_id of the Zulip web app: when `{id}-{name}` becomes noticeably longer after encoding (e.g. Chinese names), only the id is used.
/// The length is counted in UTF-16 (the web app's rule).
pub fn channel_slug(id: i64, name: &str) -> String {
    let name = if name.is_empty() { "unknown" } else { name };
    let slug = format!("{id}-{}", name.replace(' ', "-"));
    let encoded = encode_hash_component(&slug);
    let slug_len = slug.encode_utf16().count();
    if encoded.len() > slug_len + 10 || encoded.len() as f64 > slug_len as f64 * 1.5 {
        id.to_string()
    } else {
        encoded
    }
}

pub fn channel_link(realm: &str, stream_id: i64, channel_name: &str) -> String {
    format!(
        "{}{NARROW}channel/{}",
        base(realm),
        channel_slug(stream_id, channel_name)
    )
}

pub fn topic_link(realm: &str, stream_id: i64, channel_name: &str, topic: &str) -> String {
    format!(
        "{}/topic/{}",
        channel_link(realm, stream_id, channel_name),
        encode_hash_component(topic)
    )
}

/// DM link: `#narrow/dm/{ids of all participants incl. self, ascending}-dm`, and `-group` for 3 or more people.
pub fn dm_link(realm: &str, key: &DmKey, self_id: i64) -> String {
    let mut ids: Vec<i64> = key.other_user_ids().to_vec();
    ids.push(self_id);
    ids.sort_unstable();
    ids.dedup();
    let suffix = if ids.len() >= 3 { "group" } else { "dm" };
    let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
    format!("{}{NARROW}dm/{}-{suffix}", base(realm), ids.join(","))
}

/// Message link (same as by_conversation_and_time_url of the Zulip web app). Topic: topic link + `/near/{id}`;
/// DM: [`dm_link`] + `/near/{id}`.
pub fn message_link(
    realm: &str,
    key: &ConversationKey,
    self_id: i64,
    message_id: i64,
    channel_name: &str,
    topic_name: &str,
) -> String {
    let conversation = match key {
        ConversationKey::Topic(t) => topic_link(realm, t.stream_id, channel_name, topic_name),
        ConversationKey::Dm(d) => dm_link(realm, d, self_id),
    };
    format!("{conversation}/near/{message_id}")
}

/// Parses `#narrow/channel/{id}-...[/topic/{t}][/near|with/{id}]` and `#narrow/dm/{ids}-...[/near/{id}]` of this organization.
/// Links of other organizations or in other forms return `None`.
pub fn parse(url: &str, realm: &str) -> Option<ParsedLink> {
    let hash = url.find(NARROW)?;
    // Links rendered in messages are relative addresses /#narrow/...
    let prefix = url[..hash].trim_end_matches('/');
    if !prefix.is_empty() && !eq_ignore_case(prefix, base(realm).trim_end_matches('/')) {
        return None;
    }

    let parts: Vec<&str> = url[hash + NARROW.len()..].split('/').collect();
    let mut link = ParsedLink::default();
    for [operator, operand] in parts.as_chunks::<2>().0 {
        let operand = *operand;
        match *operator {
            "channel" | "stream" => {
                link.stream_id = Some(parse_digits(operand.split('-').next()?)?);
            }
            "topic" | "subject" => link.topic = Some(decode_hash_component(operand)),
            "dm" | "pm-with" => {
                let ids = operand.split('-').next()?;
                link.dm_user_ids = Some(ids.split(',').map(parse_digits).collect::<Option<_>>()?);
            }
            "near" | "with" => link.near_message_id = Some(parse_digits(operand)?),
            _ => return None,
        }
    }
    (link.stream_id.is_some() || link.dm_user_ids.is_some()).then_some(link)
}

fn base(realm: &str) -> String {
    if realm.ends_with('/') {
        realm.to_owned()
    } else {
        format!("{realm}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REALM: &str = "https://localhost.localdomain:8443/";

    #[test]
    fn hash_component_round_trip() {
        for s in ["构建失败", "a b", "v1.2.3", "f(x)", "✔ 代码评审", "100%"] {
            assert_eq!(decode_hash_component(&encode_hash_component(s)), s);
        }
    }

    #[test]
    fn encode_matches_web_app() {
        for (s, expected) in [
            ("a.b", "a.2Eb"),
            ("f(x)", "f.28x.29"),
            ("a b", "a.20b"),
            ("开", ".E5.BC.80"),
        ] {
            assert_eq!(encode_hash_component(s), expected);
        }
    }

    #[test]
    fn topic_link_and_parse() {
        let link = topic_link(REALM, 4, "开发 组", "构建 失败(1).log");
        // A Chinese name gets much longer after encoding: use only the id, same as the web version
        assert!(link.starts_with("https://localhost.localdomain:8443/#narrow/channel/4/topic/"));

        let parsed = parse(&format!("{link}/near/123"), REALM).unwrap();
        assert_eq!(parsed.stream_id, Some(4));
        assert_eq!(parsed.topic.as_deref(), Some("构建 失败(1).log"));
        assert_eq!(parsed.near_message_id, Some(123));
    }

    #[test]
    fn message_link_dm_includes_self_sorted() {
        let key = ConversationKey::dm([12, 9], 8);
        let link = message_link(REALM, &key, 8, 55, "", "");
        assert_eq!(
            link,
            "https://localhost.localdomain:8443/#narrow/dm/8,9,12-group/near/55"
        );
        assert_eq!(
            message_link(REALM, &ConversationKey::dm([9], 8), 8, 56, "", ""),
            "https://localhost.localdomain:8443/#narrow/dm/8,9-dm/near/56"
        );

        let parsed = parse(&link, REALM).unwrap();
        assert_eq!(parsed.dm_user_ids, Some(vec![8, 9, 12]));
        assert_eq!(parsed.near_message_id, Some(55));
    }

    #[test]
    fn channel_slug_matches_web_app() {
        for (name, expected) in [
            ("general", "7-general"),
            ("a b", "7-a-b"),
            ("开发组", "7"),
            ("", "7-unknown"),
        ] {
            assert_eq!(channel_slug(7, name), expected);
        }
    }

    #[test]
    fn parse_relative_forms() {
        let parsed = parse("#narrow/stream/5-games/topic/abc/with/7", REALM).unwrap();
        assert_eq!(parsed.stream_id, Some(5));
        assert_eq!(parsed.topic.as_deref(), Some("abc"));
        assert_eq!(parsed.near_message_id, Some(7));

        let parsed = parse("/#narrow/channel/4-x/topic/y", REALM).unwrap();
        assert_eq!(parsed.stream_id, Some(4));
        assert_eq!(parsed.topic.as_deref(), Some("y"));
    }

    #[test]
    fn parse_channel_only() {
        for url in [
            "/#narrow/channel/4-x",
            "https://localhost.localdomain:8443/#narrow/channel/4-x",
        ] {
            let parsed = parse(url, REALM).unwrap();
            assert_eq!(parsed.stream_id, Some(4));
            assert_eq!(parsed.topic, None);
            assert_eq!(parsed.dm_user_ids, None);
        }
    }

    #[test]
    fn parse_rejects() {
        for url in [
            "https://other.example.com/#narrow/channel/4-x/topic/y",
            "#narrow/topic/y",
            "https://example.com/page",
            "#narrow/is/starred",
        ] {
            assert_eq!(parse(url, REALM), None, "{url}");
        }
    }
}
