//! Text handling of @ completion and mentions in the composer.

use crate::util::{char_before, contains_ignore_case, starts_with_ignore_case, static_regex};

/// Character used to cover mentions: neither `@` nor whitespace, so the @ in a mention does not trigger completion. Takes one byte, offsets unchanged.
pub const MASK_CHAR: char = '\u{1}';

static_regex!(STREAM_WILDCARD = r"@\*\*(?:all|everyone|channel|stream)\*\* ?");
static_regex!(TOPIC_WILDCARD = r"@\*\*topic\*\* ?");

/// A mention in the composer: `[start, start + len)` is `markdown`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionSpan {
    pub start: usize,
    pub len: usize,
    pub markdown: String,
}

/// Whether a mention is being typed before the cursor: search backwards for `@` that is at the start or after whitespace, with no whitespace in between.
/// Returns the position of `@` and the text between it and the cursor (may be empty). Already completed mentions (`@**`, `@*`, `@_`) do not trigger.
pub fn try_get_query(text: &str, caret: usize) -> Option<(usize, &str)> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let (at, _) = text[..caret]
        .char_indices()
        .rev()
        .take_while(|(_, c)| !c.is_whitespace())
        .find(|(_, c)| *c == '@')?;
    if char_before(text, at).is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let query = &text[at + 1..caret];
    (!query.starts_with(['*', '_'])).then_some((at, query))
}

/// Match degree (case-insensitive): 0 when any field starts with the keyword or the keyword is empty, 1 when it contains the keyword, `None` when nothing matches.
pub fn match_rank(query: &str, fields: &[Option<&str>]) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let fields = || fields.iter().flatten().filter(|f| !f.is_empty());
    if fields().any(|f| starts_with_ignore_case(f, query)) {
        Some(0)
    } else if fields().any(|f| contains_ignore_case(f, query)) {
        Some(1)
    } else {
        None
    }
}

/// Replaces the text of mentions with [`MASK_CHAR`], used to decide completion. Ranges beyond the text are ignored.
pub fn mask(text: &str, spans: &[MentionSpan]) -> String {
    let mut result = text.to_owned();
    for span in spans {
        let end = span.start + span.len;
        if end <= result.len()
            && result.is_char_boundary(span.start)
            && result.is_char_boundary(end)
        {
            result.replace_range(span.start..end, &MASK_CHAR.to_string().repeat(span.len));
        }
    }
    result
}

/// Display text of a mention: `name` is the name in `@**...**` (or user group `@*...*`). `name|id`
/// takes the name; all, everyone, channel, stream give "everyone" (Chinese label) and topic gives
/// "topic participants" (Chinese label); silent mentions have no @.
pub fn display(name: &str, silent: bool, is_group: bool) -> String {
    let at = if silent { "" } else { "@" };
    if is_group {
        return format!("{at}{name}");
    }
    let plain = match name.rfind('|') {
        Some(bar) if bar > 0 => &name[..bar],
        _ => match name {
            "all" | "everyone" | "channel" | "stream" => "全体成员",
            "topic" => "话题参与者",
            other => other,
        },
    };
    format!("{at}{plain}")
}

/// Removes the @everyone in the Markdown (@topic participants when `topic` is true), together with one following space.
/// Silent mentions (`@_`) do not notify and are kept.
pub fn remove_wildcards(markdown: &str, topic: bool) -> String {
    let pattern = if topic {
        &TOPIC_WILDCARD
    } else {
        &STREAM_WILDCARD
    };
    let mut out = String::with_capacity(markdown.len());
    let mut pos = 0;
    for m in pattern.find_iter(markdown) {
        let allowed = char_before(markdown, m.start())
            .is_none_or(|c| c.is_whitespace() || "'\"({[/<".contains(c));
        if allowed {
            out.push_str(&markdown[pos..m.start()]);
            pos = m.end();
        }
    }
    out.push_str(&markdown[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_found() {
        for (text, start, query) in [("@", 0, ""), ("你好 @阿", 7, "阿"), ("a\n@jie", 2, "jie")]
        {
            assert_eq!(
                try_get_query(text, text.len()),
                Some((start, query)),
                "{text}"
            );
        }
    }

    #[test]
    fn query_not_found() {
        for text in ["mail@example", "@阿 杰", "@**阿杰|9** ", "@*组*", "abc", ""] {
            assert_eq!(try_get_query(text, text.len()), None, "{text}");
        }
    }

    #[test]
    fn query_ignores_masked_mention() {
        let text = "@**阿杰|9**";
        let span = MentionSpan {
            start: 0,
            len: text.len(),
            markdown: text.to_owned(),
        };
        assert_eq!(try_get_query(&mask(text, &[span]), text.len()), None);
    }

    #[test]
    fn mask_ignores_spans_out_of_range() {
        let span = MentionSpan {
            start: 3,
            len: 2,
            markdown: "@**甲|1**".to_owned(),
        };
        assert_eq!(mask("a", &[span]), "a");
    }

    #[test]
    fn rank() {
        for (query, expected) in [
            ("", Some(0)),
            ("阿", Some(0)),
            ("杰", Some(1)),
            ("U01", Some(0)),
            ("zz", None),
        ] {
            assert_eq!(
                match_rank(query, &[Some("阿杰"), None, Some("u01@example.com")]),
                expected,
                "{query}"
            );
        }
    }

    #[test]
    fn display_text() {
        for (name, silent, group, expected) in [
            ("阿杰|9", false, false, "@阿杰"),
            ("阿杰|9", true, false, "阿杰"),
            ("all", false, false, "@全体成员"),
            ("everyone", false, false, "@全体成员"),
            ("topic", false, false, "@话题参与者"),
            ("无编号", false, false, "@无编号"),
            ("前端组", false, true, "@前端组"),
        ] {
            assert_eq!(display(name, silent, group), expected);
        }
    }

    #[test]
    fn wildcards_removed() {
        for (markdown, topic, expected) in [
            ("@**all** 开会", false, "开会"),
            ("a @**everyone** b @**channel**", false, "a b "),
            ("@**topic** @**all** x", false, "@**topic** x"),
            ("@**topic** @**all** x", true, "@**all** x"),
            ("@_**all** x", false, "@_**all** x"),
            ("@**all|5** x", false, "@**all|5** x"),
            ("mail@**all**", false, "mail@**all**"),
        ] {
            assert_eq!(remove_wildcards(markdown, topic), expected, "{markdown}");
        }
    }
}
