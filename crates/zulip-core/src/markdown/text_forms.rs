//! Single-line preview and display text of draft Markdown.

use super::{
    FenceKind, Line, LineKind, MarkdownModel, MarkdownResolver, PrefixKind, Span, SpanKind, parse,
};
use crate::html;
use crate::text::time;
use crate::util::static_regex;

static_regex!(WHITESPACE = r"\s+");

/// Single-line preview of a draft, with the same rules as [`html::to_preview`] for sent messages: quotes removed; code blocks, formulas, spoilers,
/// images, videos and files replaced by the [code], [formula], [collapsed], [image], [video], [file]name labels (Chinese UI text); mentions, channels and times shown as their display text;
/// list items prefixed with the bullet (U+2022) or number; format symbols removed; runs of whitespace merged into one space.
/// `emoji_text` returns the text of a Unicode emoji; when it returns `None`, :name: is shown.
pub fn to_preview(
    text: &str,
    resolver: &dyn MarkdownResolver,
    emoji_text: &dyn Fn(&str) -> Option<String>,
) -> String {
    let model = parse(text, resolver);
    let mut out = String::new();
    // Outer fence; the content of a spoiler fence is shown entirely as [collapsed]
    let mut fences = Vec::new();
    let mut prev_kind = None;
    for line in &model.lines {
        let in_spoiler = fences.contains(&FenceKind::Spoiler);
        if line.kind == LineKind::FenceOpen {
            fences.push(line.fence.unwrap_or(FenceKind::Code));
        } else if line.kind == LineKind::FenceClose {
            fences.pop();
        }
        let previous = prev_kind.replace(line.kind);
        if in_spoiler || line.quote_depth > 0 {
            continue;
        }
        match line.kind {
            LineKind::FenceOpen => out.push_str(match line.fence {
                Some(FenceKind::Math) => " [公式] ",
                Some(FenceKind::Spoiler) => " [折叠] ",
                _ => " [代码] ",
            }),
            // Consecutive indented code is one code block
            LineKind::IndentedCode if previous != Some(LineKind::IndentedCode) => {
                out.push_str(" [代码] ")
            }
            LineKind::Paragraph | LineKind::Heading | LineKind::ListItem | LineKind::TableRow => {
                out.push(' ');
                // List items get the bullet (U+2022) or number "1." in front (same as the display in the composer)
                if line.kind == LineKind::ListItem
                    && let Some(marker) = line.prefixes.last()
                {
                    out.push_str(if marker.kind == PrefixKind::Bullet {
                        "•"
                    } else {
                        text[marker.start..marker.end].trim()
                    });
                    out.push(' ');
                }
                append_line(&mut out, text, &model, line, true, emoji_text);
                out.push(' ');
            }
            _ => {}
        }
    }
    WHITESPACE.replace_all(&out, " ").trim().to_owned()
}

/// Text shown in the composer (for copying into other programs): newlines kept; format symbols removed; each quote line prefixed with "&gt; ", and list bullets shown as the bullet (U+2022) plus a space;
/// code blocks and math blocks keep their original text; mentions, channels, times and emoji are shown as in the composer; a link becomes "text (address)", an image becomes [image].
pub fn to_plain_text(
    text: &str,
    resolver: &dyn MarkdownResolver,
    emoji_text: &dyn Fn(&str) -> Option<String>,
) -> String {
    let model = parse(text, resolver);
    let mut lines = Vec::new();
    for line in &model.lines {
        let mut out = "> ".repeat(line.quote_depth);
        match line.kind {
            // The symbol lines of quote, code and math fences are not shown; a spoiler shows its title
            LineKind::FenceOpen
                if line.fence == Some(FenceKind::Spoiler)
                    && line.fence_info.as_deref().is_some_and(|i| !i.is_empty()) =>
            {
                out.push_str(line.fence_info.as_deref().unwrap_or(""));
            }
            LineKind::FenceOpen
            | LineKind::FenceClose
            | LineKind::Rule
            | LineKind::TableSeparator => continue,
            LineKind::FenceBody => out.push_str(&text[line.content_start..line.end]),
            LineKind::IndentedCode => {
                let code = &text[line.content_start..line.end];
                out.push_str(
                    code.strip_prefix('\t')
                        .unwrap_or_else(|| &code[4.min(code.len())..]),
                );
            }
            LineKind::Blank => out.clear(),
            LineKind::ListItem => {
                // The indentation before the list marker (nested lists) is kept
                if let Some(marker) = line.prefixes.last() {
                    let indent_start = match line.prefixes.len() {
                        n if n > 1 => line.prefixes[n - 2].end,
                        _ => line.start,
                    };
                    out.push_str(&text[indent_start..marker.start]);
                    out.push_str(if marker.kind == PrefixKind::Bullet {
                        "• "
                    } else {
                        &text[marker.start..marker.end]
                    });
                }
                append_line(&mut out, text, &model, line, false, emoji_text);
            }
            _ => append_line(&mut out, text, &model, line, false, emoji_text),
        }
        lines.push(out);
    }
    lines.join("\n")
}

struct InlineWriter<'a> {
    text: &'a str,
    spans: Vec<&'a Span>,
    table_row: bool,
    preview: bool,
    emoji_text: &'a dyn Fn(&str) -> Option<String>,
}

fn append_line(
    out: &mut String,
    text: &str,
    model: &MarkdownModel,
    line: &Line,
    preview: bool,
    emoji_text: &dyn Fn(&str) -> Option<String>,
) {
    let writer = InlineWriter {
        text,
        spans: model
            .spans
            .iter()
            .filter(|s| s.start >= line.content_start && s.end <= line.end)
            .collect(),
        table_row: preview && line.kind == LineKind::TableRow,
        preview,
        emoji_text,
    };
    writer.append(out, line.content_start, line.end);
}

impl InlineWriter<'_> {
    // Display text of [from, to). Bold etc. only drop the symbols, and the content may contain other elements
    fn append(&self, out: &mut String, from: usize, to: usize) {
        let mut pos = from;
        for s in &self.spans {
            if s.start < pos || s.end > to {
                continue;
            }
            self.append_plain(out, pos, s.start);
            if let Some(replacement) = self.span_text(s) {
                out.push_str(&replacement);
            } else if s.content_start == s.start && s.content_end == s.end {
                // Elements without symbols (URLs): the content is the element itself
                self.append_plain(out, s.start, s.end);
            } else {
                self.append(out, s.content_start, s.content_end);
            }
            pos = s.end;
        }
        self.append_plain(out, pos, to);
    }

    // The | between table cells is replaced by a space
    fn append_plain(&self, out: &mut String, from: usize, to: usize) {
        if to <= from {
            return;
        }
        let plain = &self.text[from..to];
        if self.table_row {
            out.push_str(&plain.replace('|', " "));
        } else {
            out.push_str(plain);
        }
    }

    // Replacement text of an element; returning None means only the symbols are dropped and the content shown. As in message previews, replacement text is padded with spaces on both sides
    fn span_text(&self, s: &Span) -> Option<String> {
        let text = self.text;
        let preview = self.preview;
        let label = &text[s.content_start..s.content_end];
        let data = s.data.as_deref().unwrap_or("");
        let upload = html::normalize_upload_path(data);
        let is_link = s.kind == SpanKind::Link;
        let result = match s.kind {
            SpanKind::Math => {
                if preview {
                    "[公式]".to_owned()
                } else {
                    text[s.start..s.end].to_owned()
                }
            }
            SpanKind::UploadImage => "[图片]".to_owned(),
            SpanKind::UploadPlaceholder => String::new(),
            SpanKind::UserMention | SpanKind::GroupMention | SpanKind::ChannelLink => {
                data.to_owned()
            }
            SpanKind::Time => time::parse_datetime(data)
                .map_or_else(|| data.to_owned(), |t| time::local_label(&t)),
            SpanKind::Emoji => (self.emoji_text)(data).unwrap_or_else(|| format!(":{data}:")),
            // External image ![...](...)
            SpanKind::Link if text.as_bytes()[s.start] == b'!' => "[图片]".to_owned(),
            SpanKind::Link | SpanKind::UploadFile if upload.is_some() => {
                if !preview {
                    label.to_owned()
                } else if upload.as_deref().is_some_and(html::is_video_path) {
                    "[视频]".to_owned()
                } else {
                    format!("[文件]{}", label.trim())
                }
            }
            // [](address) shows the address
            SpanKind::Link if label.is_empty() || label == data => data.to_owned(),
            SpanKind::Link if !preview => format!("{label} ({data})"),
            _ => return None,
        };
        // The address of [](address) gets no extra spaces
        if !preview || (is_link && !result.starts_with('[')) {
            return Some(result);
        }
        Some(format!(" {result} "))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::FakeResolver;
    use super::*;

    fn emoji(name: &str) -> Option<String> {
        (name == "smile").then(|| "😄".to_owned())
    }

    #[test]
    fn preview_matches_message_preview() {
        for (text, expected) in [
            ("你好\n\n世界", "你好 世界"),
            ("**粗** *斜* ~~删~~ `代码`", "粗 斜 删 代码"),
            ("*a **b** c*", "a b c"),
            ("hi @**甲|1** and @_**甲|1**", "hi @甲 and 甲"),
            ("@*开发* 看看", "@开发 看看"),
            ("@**乙|2**", "@乙|2"),
            ("#**综合>午饭** 去", "#综合 > 午饭 去"),
            ("笑:smile:", "笑 😄"),
            (
                "[文档](https://a.com) [](https://b.com)",
                "文档 https://b.com",
            ),
            ("看 ![](https://a.com/x.png)", "看 [图片]"),
            ("[a.png](/user_uploads/1/ab/a.png)", "[图片]"),
            ("[a.mp4](/user_uploads/1/ab/a.mp4)", "[视频]"),
            ("[报告.pdf](/user_uploads/1/ab/r.pdf)", "[文件]报告.pdf"),
            ("[Uploading a.png…]() 好", "好"),
            ("$$x^2$$ 是", "[公式] 是"),
            ("- 一\n- 二\n1. 三", "• 一 • 二 1. 三"),
            ("- @**甲|1** 好", "• @甲 好"),
            ("## 标题\n正文", "标题 正文"),
            ("> 引用\n回复", "回复"),
            ("```quote\n引用\n```\n回复", "回复"),
            ("前\n```py\nprint(1)\n```\n后", "前 [代码] 后"),
            ("```math\nx\n```", "[公式]"),
            ("```spoiler 标题\n```py\nx\n```\n内容\n```\n后", "[折叠] 后"),
            ("前\n\n    a\n    b\n\n后", "前 [代码] 后"),
            ("a | b\n--- | ---\n1 | 2", "a b 1 2"),
            ("---\n文字", "文字"),
        ] {
            assert_eq!(to_preview(text, &FakeResolver, &emoji), expected, "{text}");
        }
    }

    #[test]
    fn plain_text_is_displayed_text() {
        for (text, expected) in [
            ("**粗** *斜*\n\n第二段", "粗 斜\n\n第二段"),
            ("hi @**甲|1** #**综合** :smile:", "hi @甲 #综合 😄"),
            (
                "[文档](https://a.com) [](https://b.com) https://c.com",
                "文档 (https://a.com) https://b.com https://c.com",
            ),
            (
                "[报告.pdf](/user_uploads/1/ab/r.pdf) ![](https://a.com/x.png)",
                "报告.pdf [图片]",
            ),
            ("- 一\n  - 二\n1. 三", "• 一\n  • 二\n1. 三"),
            ("## 标题", "标题"),
            ("> 引用 **粗**\n回复", "> 引用 粗\n回复"),
            ("```quote\n引用\n```", "> 引用"),
            ("```py\nx = **1**\n```", "x = **1**"),
            ("```spoiler 标题\n内容\n```", "标题\n内容"),
            ("    code", "code"),
            ("$$x^2$$", "$$x^2$$"),
        ] {
            assert_eq!(
                to_plain_text(text, &FakeResolver, &emoji),
                expected,
                "{text}"
            );
        }
    }
}
