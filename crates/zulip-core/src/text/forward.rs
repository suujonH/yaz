//! Merged forwarding: joins several messages into a "chat record" style Markdown.

pub const HEADER: &str = "**聊天记录**\n";

const ELLIPSIS: &str = "…";

/// One message in a merged forward: sender, text of the send time (`yyyy/MM/dd HH:mm`), original Markdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardedMessage {
    pub sender_name: String,
    pub time: String,
    pub raw: String,
}

/// Each message is `**sender** yyyy/MM/dd HH:mm` followed by a quote code block, starting with [`HEADER`].
/// When over `max_chars` characters it is split between messages into several posts, each starting with the header; when a single message plus the header is still too long, the original text is truncated
/// and an ellipsis (U+2026) is appended. `max_chars` of `None` means no limit.
pub fn merge(messages: &[ForwardedMessage], max_chars: Option<usize>) -> Vec<String> {
    let header_len = HEADER.chars().count();
    let mut result = Vec::new();
    let mut current = HEADER.to_owned();
    let mut has_block = false;
    for message in messages {
        let mut block = render_block(message, None);
        if let Some(max) = max_chars {
            if header_len + block.chars().count() > max {
                block = render_block(message, Some(max.saturating_sub(header_len)));
            }
            if has_block && current.chars().count() + block.chars().count() > max {
                result.push(current.trim_end_matches('\n').to_owned());
                current = HEADER.to_owned();
            }
        }
        current.push_str(&block);
        has_block = true;
    }
    if has_block {
        result.push(current.trim_end_matches('\n').to_owned());
    }
    result
}

// When limit is not None, truncate the original text so that the whole block does not exceed that number of characters
fn render_block(message: &ForwardedMessage, limit: Option<usize>) -> String {
    let title = format!("**{}** {}\n", message.sender_name, message.time);
    let raw = message.raw.trim_end();
    // When the original text contains ```, lengthen the fence to avoid ending early
    let mut fence = "```".to_owned();
    while raw.contains(&fence) {
        fence.push('`');
    }
    let open = format!("{fence}quote\n");
    let close = format!("\n{fence}\n");
    let body = match limit {
        None => raw.to_owned(),
        Some(max) => {
            let used = [&title, &open, &close]
                .map(|s| s.chars().count())
                .iter()
                .sum::<usize>()
                + ELLIPSIS.chars().count();
            let room = max.saturating_sub(used);
            let kept: String = raw.chars().take(room).collect();
            format!("{kept}{ELLIPSIS}")
        }
    };
    format!("{title}{open}{body}{close}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(name: &str, raw: &str) -> ForwardedMessage {
        ForwardedMessage {
            sender_name: name.to_owned(),
            time: "2026/10/22 09:05".to_owned(),
            raw: raw.to_owned(),
        }
    }

    #[test]
    fn formats_each_message() {
        assert_eq!(
            merge(&[message("阿杰", "你好"), message("小雨", "在吗\n")], None),
            [
                "**聊天记录**\n**阿杰** 2026/10/22 09:05\n```quote\n你好\n```\n**小雨** 2026/10/22 09:05\n```quote\n在吗\n```"
            ]
        );
    }

    #[test]
    fn lengthens_fence_for_code() {
        assert_eq!(
            merge(&[message("阿杰", "```\ncode\n```")], None),
            ["**聊天记录**\n**阿杰** 2026/10/22 09:05\n````quote\n```\ncode\n```\n````"]
        );
    }

    #[test]
    fn splits_at_message_boundaries() {
        let raw = "a".repeat(40);
        let messages = vec![message("阿杰", &raw); 3];
        let result = merge(&messages, Some(150));
        assert_eq!(result.len(), 3);
        for part in result {
            assert!(part.starts_with(HEADER));
            assert!(part.chars().count() <= 150);
        }
    }

    #[test]
    fn truncates_single_oversized_message() {
        let result = merge(&[message("阿杰", &"a".repeat(500))], Some(120));
        assert_eq!(result.len(), 1);
        assert!(result[0].chars().count() <= 120);
        assert!(result[0].contains("a…\n```"));
    }

    #[test]
    fn empty() {
        assert!(merge(&[], Some(100)).is_empty());
    }
}
