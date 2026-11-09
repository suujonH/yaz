//! 草稿 Markdown 的单行预览与显示文字。

use super::{
    FenceKind, Line, LineKind, MarkdownModel, MarkdownResolver, PrefixKind, Span, SpanKind, parse,
};
use crate::html;
use crate::text::time;
use crate::util::static_regex;

static_regex!(WHITESPACE = r"\s+");

/// 草稿的单行预览，与 [`html::to_preview`] 对发送后消息的规则相同：去掉引用；代码块、公式、剧透、
/// 图片、视频、文件替换为 [代码]、[公式]、[折叠]、[图片]、[视频]、[文件]名称；提及、频道、时间为显示文字；
/// 列表项前加 “•” 或序号；去掉格式符号；连续空白合并为一个空格。
/// `emoji_text` 返回 Unicode 表情的文字，返回 `None` 时显示 :名称:。
pub fn to_preview(
    text: &str,
    resolver: &dyn MarkdownResolver,
    emoji_text: &dyn Fn(&str) -> Option<String>,
) -> String {
    let model = parse(text, resolver);
    let mut out = String::new();
    // 外层的围栏；剧透围栏中的内容整个显示为 [折叠]
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
            // 连续的缩进代码是一个代码块
            LineKind::IndentedCode if previous != Some(LineKind::IndentedCode) => {
                out.push_str(" [代码] ")
            }
            LineKind::Paragraph | LineKind::Heading | LineKind::ListItem | LineKind::TableRow => {
                out.push(' ');
                // 列表项前加圆点 “•” 或序号 “1.”（与输入框中的显示相同）
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

/// 输入框中显示的文字（复制到其它程序用）：保留换行；去掉格式符号；引用每行前加 “&gt; ”，列表圆点为 “• ”；
/// 代码块、公式块保留原文；提及、频道、时间、表情与输入框中的显示相同；链接为“文字 (地址)”，图片为 [图片]。
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
            // 引用、代码、公式围栏的符号行不显示；剧透显示标题
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
                // 列表标记前的缩进（嵌套列表）保留
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
    // [from, to) 的显示文字。加粗等只去掉符号，内容中可能还有其它元素
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
                // 没有符号的元素（网址）：内容就是它自己
                self.append_plain(out, s.start, s.end);
            } else {
                self.append(out, s.content_start, s.content_end);
            }
            pos = s.end;
        }
        self.append_plain(out, pos, to);
    }

    // 表格单元格之间的 | 换成空格
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

    // 元素的替换文字；返回 None 表示只去掉符号、显示内容。预览与消息预览一样，替换文字两侧加空格
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
            // 外链图片 ![…](…)
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
            // [](地址) 显示地址
            SpanKind::Link if label.is_empty() || label == data => data.to_owned(),
            SpanKind::Link if !preview => format!("{label} ({data})"),
            _ => return None,
        };
        // [](地址) 的地址不加空格
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
