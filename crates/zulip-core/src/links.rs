//! 生成和解析 Zulip 网页端的 `#narrow/…` 链接。

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::conversation::{ConversationKey, DmKey};
use crate::util::{eq_ignore_case, parse_digits};

const NARROW: &str = "#narrow/";

const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// 解析出的 Zulip 内部链接。只有频道时 `topic` 为 `None`。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ParsedLink {
    pub stream_id: Option<i64>,
    pub topic: Option<String>,
    pub dm_user_ids: Option<Vec<i64>>,
    pub near_message_id: Option<i64>,
}

/// 与 Zulip 网页端 encodeHashComponent 一致：URL 编码后把 % 换成 .，并转义 ( ) .。
pub fn encode_hash_component(s: &str) -> String {
    let escaped = utf8_percent_encode(s, UNRESERVED).to_string();
    let mut out = String::with_capacity(escaped.len());
    // 必须一次扫描完成，否则新生成的 “.” 会被再次替换
    for c in escaped.chars() {
        match c {
            '%' => out.push('.'),
            '.' => out.push_str(".2E"),
            _ => out.push(c),
        }
    }
    out
}

/// [`encode_hash_component`] 的反向操作。
pub fn decode_hash_component(s: &str) -> String {
    let unescaped = s.replace('.', "%");
    match percent_decode_str(&unescaped).decode_utf8() {
        Ok(text) => text.into_owned(),
        Err(_) => s.to_owned(),
    }
}

/// 与 Zulip 网页端 encode_stream_id 相同：`{id}-{名称}` 编码后明显变长（例如中文名称）时只用 id。
/// 长度按 UTF-16 计算（网页端的规则）。
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

/// 私聊链接：`#narrow/dm/{全部参与者 id，含自己，升序}-dm`，3 人及以上为 `-group`。
pub fn dm_link(realm: &str, key: &DmKey, self_id: i64) -> String {
    let mut ids: Vec<i64> = key.other_user_ids().to_vec();
    ids.push(self_id);
    ids.sort_unstable();
    ids.dedup();
    let suffix = if ids.len() >= 3 { "group" } else { "dm" };
    let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
    format!("{}{NARROW}dm/{}-{suffix}", base(realm), ids.join(","))
}

/// 消息链接（与 Zulip 网页端 by_conversation_and_time_url 相同）。话题：话题链接 + `/near/{id}`；
/// 私聊：[`dm_link`] + `/near/{id}`。
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

/// 解析本组织的 `#narrow/channel/{id}-…[/topic/{t}][/near|with/{id}]` 与 `#narrow/dm/{ids}-…[/near/{id}]`。
/// 其它组织的链接或其它形式返回 `None`。
pub fn parse(url: &str, realm: &str) -> Option<ParsedLink> {
    let hash = url.find(NARROW)?;
    // 消息中渲染的链接是相对地址 /#narrow/…
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
        // 中文名称编码后变长太多：与网页版相同只用 id
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
