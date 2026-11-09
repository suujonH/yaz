//! 正文中的引用消息（由“引用”插入）：一行“@_**发送者|id** [发言道](消息链接)：”加 quote 围栏。

use super::{FenceKind, LineKind, MarkdownModel, NoResolver, parse};
use crate::util::{is_blank, parse_digits, static_regex};

// 简体中文译文“发言道”与英文原文“said”都认；冒号可以是全角或半角
static_regex!(
    HEADER = r"^@_\*\*(?<name>[^*|\n]+?)(?:\|(?<id>\d+))?\*\* \[(?:发言道|said)\]\((?<link>[^)\s]+)\)[：:][ \t]*$"
);
// 消息链接中的消息 id：…/near/603 或 …/with/603
static_regex!(NEAR = r"/(?:near|with)/(\d+)");

/// 第 `header_line` 行是引用行，下一行起是 quote 围栏，到第 `close_line` 行结束。
/// `[start, end)` 为整段（不含最后的换行），`[body_start, body_end)` 为被引用的原文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteBlock {
    pub header_line: usize,
    pub close_line: usize,
    pub start: usize,
    pub end: usize,
    pub body_start: usize,
    pub body_end: usize,
    pub sender_name: String,
    pub sender_id: Option<i64>,
    pub link: String,
    pub message_id: Option<i64>,
}

/// 找出 `text` 中所有完整的引用消息（`model` 为它的解析结果）。
/// 只认最外层的：引用行后面紧跟 quote 围栏，并且围栏已经结束；还没有写完的不算。
pub fn find(model: &MarkdownModel, text: &str) -> Vec<QuoteBlock> {
    let lines = &model.lines;
    let mut result = Vec::new();
    let mut i = 0;
    while i + 1 < lines.len() {
        let header = &lines[i];
        let open = &lines[i + 1];
        let captures = (header.kind == LineKind::Paragraph
            && header.quote_depth == 0
            && open.kind == LineKind::FenceOpen
            && open.fence == Some(FenceKind::Quote)
            && open.quote_depth == 1)
            .then(|| HEADER.captures(&text[header.start..header.end]))
            .flatten();
        let close = captures.as_ref().and_then(|_| closing_line(model, i + 2));
        let (Some(m), Some(close)) = (captures, close) else {
            i += 1;
            continue;
        };

        let (body_start, body_end) = if close > i + 2 {
            (lines[i + 2].start, lines[close - 1].end)
        } else {
            (lines[close].start, lines[close].start)
        };
        let link = &m["link"];
        result.push(QuoteBlock {
            header_line: i,
            close_line: close,
            start: header.start,
            end: lines[close].end,
            body_start,
            body_end,
            sender_name: m["name"].to_owned(),
            sender_id: m.name("id").and_then(|id| parse_digits(id.as_str())),
            link: link.to_owned(),
            message_id: NEAR.captures(link).and_then(|near| parse_digits(&near[1])),
        });
        i = close + 1;
    }
    result
}

// 与围栏开始配对的结束行：中间可以有嵌套的围栏
fn closing_line(model: &MarkdownModel, from: usize) -> Option<usize> {
    let mut depth = 1;
    for (k, line) in model.lines.iter().enumerate().skip(from) {
        match line.kind {
            LineKind::FenceOpen => depth += 1,
            LineKind::FenceClose => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// 除了引用消息只有空白：只引用、没有写自己的内容（不能发送）。
pub fn has_only_quotes(text: &str) -> bool {
    let quotes = find(&parse(text, &NoResolver), text);
    if quotes.is_empty() {
        return false;
    }
    let mut rest = 0;
    for quote in &quotes {
        if !is_blank(&text[rest..quote.start]) {
            return false;
        }
        rest = quote.end;
    }
    is_blank(&text[rest..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER_LINE: &str =
        "@_**群主大大|8** [发言道](https://example.com/#narrow/dm/8,9-dm/near/603)：";

    fn find_in(text: &str) -> Vec<QuoteBlock> {
        find(&parse(text, &NoResolver), text)
    }

    #[test]
    fn quote_inserted_by_composer() {
        let text = format!("前面\n{HEADER_LINE}\n```quote\n第一行\n第二行\n```\n后面");
        let blocks = find_in(&text);
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(block.sender_name, "群主大大");
        assert_eq!(block.sender_id, Some(8));
        assert_eq!(block.message_id, Some(603));
        assert_eq!(text.find("@_"), Some(block.start));
        assert_eq!(text.find("\n后面"), Some(block.end));
        assert_eq!(&text[block.body_start..block.body_end], "第一行\n第二行");
        assert_eq!((block.header_line, block.close_line), (1, 5));
    }

    #[test]
    fn nested_fence_and_several_quotes() {
        let nested = format!("{HEADER_LINE}\n````quote\n```quote\n里面\n```\n外面\n````");
        let second = HEADER_LINE.replace("603", "604");
        let text = format!("{nested}\n{second}\n```quote\n\n```");
        let blocks = find_in(&text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].end, nested.len());
        assert_eq!(
            &text[blocks[0].body_start..blocks[0].body_end],
            "```quote\n里面\n```\n外面"
        );
        assert_eq!(blocks[1].message_id, Some(604));
        assert_eq!(blocks[1].end, text.len());
    }

    #[test]
    fn ignores_incomplete_or_ordinary_quotes() {
        // 围栏还没有结束
        assert!(find_in(&format!("{HEADER_LINE}\n```quote\n内容")).is_empty());
        // 没有引用行的引用块
        assert!(find_in("```quote\n内容\n```").is_empty());
        // 引用行后面不是引用围栏
        assert!(find_in(&format!("{HEADER_LINE}\n```\n代码\n```")).is_empty());
        // 英文原文与半角冒号也认，没有 id 时 sender_id 为 None
        let english = find_in(
            "@_**Bob** [said](https://example.com/#narrow/channel/1-a/topic/b/with/77):\n```quote\nhi\n```",
        );
        assert_eq!(english.len(), 1);
        assert_eq!(english[0].sender_id, None);
        assert_eq!(english[0].message_id, Some(77));
    }

    #[test]
    fn only_quotes() {
        let quote = format!("{HEADER_LINE}\n```quote\n内容\n```");
        assert!(has_only_quotes(&format!("{quote}\n")));
        assert!(has_only_quotes(&format!("\n{quote}\n\n{quote}\n")));
        assert!(!has_only_quotes(&format!("{quote}\n回复")));
        assert!(!has_only_quotes(&format!("回复\n{quote}")));
        assert!(!has_only_quotes("普通消息"));
        assert!(!has_only_quotes(""));
    }
}
