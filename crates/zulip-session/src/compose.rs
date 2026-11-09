//! Pure logic for the composer: quotes, forwarded text, invitation email parsing.

use std::collections::HashSet;

use zulip_core::markdown::format::unused_fence;

/// Reply quote: `@_**name|id** [said](link):` (the Chinese phrase for "said", followed by a
/// fullwidth colon U+FF1A) plus a quote code block. The fence is longer than any in the original.
pub fn quote_markdown(sender_name: &str, sender_id: i64, link: &str, raw: &str) -> String {
    let fence = unused_fence(raw);
    format!("@_**{sender_name}|{sender_id}** [发言道]({link})：\n{fence}quote\n{raw}\n{fence}")
}

/// Emails in the invite box: commas, semicolons (including fullwidth ones) and whitespace are all
/// separators; duplicates are removed ignoring case, keeping the spelling of the first occurrence.
pub fn parse_emails(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split([',', '，', ';', '；', ' ', '\t', '\r', '\n'])
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.to_lowercase()))
        .map(str::to_owned)
        .collect()
}

/// Placeholder text after a message is deleted.
pub fn deleted_text(sender_name: &str) -> String {
    format!("{sender_name}的一条消息已被删除")
}

/// Preview of a merged forward.
pub fn forward_preview(count: usize) -> String {
    format!("[聊天记录] 共{count}条")
}

/// Preview of a one-by-one forward.
pub fn forward_each_preview(count: usize) -> String {
    format!("[逐条转发] 共{count}条消息")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_format() {
        assert_eq!(
            quote_markdown("张三", 5, "https://z.test/#narrow/dm/1,5-dm/near/9", "你好"),
            "@_**张三|5** [发言道](https://z.test/#narrow/dm/1,5-dm/near/9)：\n```quote\n你好\n```"
        );
    }

    #[test]
    fn quote_fence_is_longer_than_inner_fence() {
        assert_eq!(
            quote_markdown("A", 1, "l", "```py\nx\n```"),
            "@_**A|1** [发言道](l)：\n````quote\n```py\nx\n```\n````"
        );
    }

    #[test]
    fn emails_split_on_all_separators() {
        assert_eq!(
            parse_emails(" a@x.test, b@x.test；c@x.test\r\n\nd@x.test，"),
            ["a@x.test", "b@x.test", "c@x.test", "d@x.test"]
        );
    }

    #[test]
    fn emails_dedupe_ignoring_case() {
        assert_eq!(parse_emails("A@x.test a@X.test"), ["A@x.test"]);
    }

    #[test]
    fn emails_blank_input_is_empty() {
        assert!(parse_emails(" ,\n ").is_empty());
    }
}
