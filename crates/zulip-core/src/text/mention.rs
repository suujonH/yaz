//! 输入框中 @ 补全与提及的文字处理。

use crate::util::{char_before, contains_ignore_case, starts_with_ignore_case, static_regex};

/// 遮盖提及用的字符：不是 `@` 也不是空白，提及中的 @ 不会触发补全。占一个字节，偏移不变。
pub const MASK_CHAR: char = '\u{1}';

static_regex!(STREAM_WILDCARD = r"@\*\*(?:all|everyone|channel|stream)\*\* ?");
static_regex!(TOPIC_WILDCARD = r"@\*\*topic\*\* ?");

/// 输入框中的一个提及：`[start, start + len)` 为 `markdown`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionSpan {
    pub start: usize,
    pub len: usize,
    pub markdown: String,
}

/// 光标前是否正在输入提及：向前找到 `@`，它位于开头或空白之后，且中间没有空白。
/// 返回 `@` 的位置和它到光标之间的文字（可以为空）。已经补全的提及（`@**`、`@*`、`@_`）不触发。
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

/// 匹配程度（忽略大小写）：任一字段以关键字开头或关键字为空时为 0，包含关键字为 1，都不匹配为 `None`。
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

/// 提及的文字换成 [`MASK_CHAR`]，用于判断补全。超出文字范围的区间忽略。
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

/// 提及的显示文字：`name` 为 `@**…**`（或用户组 `@*…*`）中的名字。
/// `名字|id` 取名字，all、everyone、channel、stream 为“全体成员”，topic 为“话题参与者”；静默提及没有 @。
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

/// 去掉 Markdown 中的 @全体成员（`topic` 为 true 时为 @话题参与者），连同后面的一个空格。
/// 静默提及（`@_`）不通知，保留。
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
