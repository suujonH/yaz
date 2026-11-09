//! 把消息中选中的文字转为 Markdown（复制后粘贴到输入框，发送的格式与消息相同）。

/// 段落中一段文字的来源（由消息 HTML 得到）。`[start, end)` 为段落文字中的区间。
/// `markdown` 不为 `None` 的是整体元素（提及、表情、时间、图片、公式），选中其中任何部分都复制整个 Markdown；
/// 其余为普通文字，按格式加上符号。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct InlinePiece {
    pub start: usize,
    pub end: usize,
    pub markdown: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub href: Option<String>,
}

impl InlinePiece {
    fn same_style(&self, other: &Self) -> bool {
        self.bold == other.bold
            && self.italic == other.italic
            && self.strike == other.strike
            && self.code == other.code
            && self.href == other.href
    }
}

/// 段落文字 `text` 中 `[start, end)` 的 Markdown。不属于任何片段的文字（换行等）原样保留；
/// 格式相同的相邻文字合并后只加一次符号；符号不包住两端的空白（否则服务器不认为是格式）。
pub fn from_pieces(text: &str, pieces: &[InlinePiece], start: usize, end: usize) -> String {
    let start = text.floor_char_boundary(start);
    let end = text.floor_char_boundary(end).max(start);
    let mut out = String::new();
    let mut group = String::new();
    let mut group_style: Option<&InlinePiece> = None;
    let mut pos = start;

    fn flush(out: &mut String, group: &mut String, style: &mut Option<&InlinePiece>) {
        if let Some(s) = style.take()
            && !group.is_empty()
        {
            out.push_str(&wrap(group, s));
        }
        group.clear();
    }

    let mut selected: Vec<&InlinePiece> = pieces
        .iter()
        .filter(|p| p.end > start && p.start < end)
        .collect();
    selected.sort_by_key(|p| p.start);
    for p in selected {
        if p.start > pos {
            flush(&mut out, &mut group, &mut group_style);
            out.push_str(&text[pos..p.start]);
        }
        if let Some(markdown) = &p.markdown {
            flush(&mut out, &mut group, &mut group_style);
            out.push_str(markdown);
        } else {
            if group_style.is_some_and(|s| !s.same_style(p)) {
                flush(&mut out, &mut group, &mut group_style);
            }
            group_style.get_or_insert(p);
            group.push_str(&text[p.start.max(start)..p.end.min(end)]);
        }
        pos = pos.max(p.end.min(end));
    }
    flush(&mut out, &mut group, &mut group_style);
    if pos < end {
        out.push_str(&text[pos..end]);
    }
    out
}

/// 代码块中选中的文字：多行或整个代码块为围栏代码块，单行的一部分为行内代码。
pub fn from_code(code: &str, language: &str, start: usize, end: usize) -> String {
    let start = code.floor_char_boundary(start);
    let end = code.floor_char_boundary(end).max(start);
    let selected = &code[start..end];
    if !selected.contains('\n') && (start > 0 || end < code.len()) {
        return code_span(selected);
    }
    let fence = "`".repeat(3.max(longest_tick_run(selected) + 1));
    format!("{fence}{language}\n{selected}\n{fence}")
}

fn wrap(s: &str, style: &InlinePiece) -> String {
    // 行内代码中的空白有意义，整个包住
    let core = if style.code { s } else { s.trim() };
    if core.trim().is_empty() {
        return s.to_owned();
    }
    let lead_len = s.find(core).unwrap_or(0);
    let (lead, trail) = (&s[..lead_len], &s[lead_len + core.len()..]);
    let mut core = core.to_owned();
    if style.code {
        core = code_span(&core);
    }
    if style.strike {
        core = format!("~~{core}~~");
    }
    if style.italic {
        core = format!("*{core}*");
    }
    if style.bold {
        core = format!("**{core}**");
    }
    if let Some(href) = &style.href
        && &core != href
    {
        core = format!("[{core}]({href})");
    }
    format!("{lead}{core}{trail}")
}

// 内容中有反引号时用更长的反引号包围；内容以反引号开头或结尾时加空格
fn code_span(code: &str) -> String {
    let ticks = "`".repeat(longest_tick_run(code) + 1);
    let pad = if code.starts_with('`') || code.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{code}{pad}{ticks}")
}

fn longest_tick_run(s: &str) -> usize {
    s.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(start: usize, end: usize) -> InlinePiece {
        InlinePiece {
            start,
            end,
            ..Default::default()
        }
    }

    fn whole(start: usize, end: usize, markdown: &str) -> InlinePiece {
        InlinePiece {
            markdown: Some(markdown.to_owned()),
            ..plain(start, end)
        }
    }

    #[test]
    fn selection() {
        // 段落 “普通粗体@甲 链接￼”：每段的字节区间由累加得到
        let parts = ["普通", "粗体", "@甲", " ", "链接", "￼"];
        let mut at = vec![0];
        for part in parts {
            at.push(at[at.len() - 1] + part.len());
        }
        let text = parts.concat();
        let pieces = [
            plain(at[0], at[1]),
            InlinePiece {
                bold: true,
                ..plain(at[1], at[2])
            },
            whole(at[2], at[3], "@**甲|1**"),
            plain(at[3], at[4]),
            InlinePiece {
                href: Some("https://a.com".into()),
                ..plain(at[4], at[5])
            },
            whole(at[5], at[6], ":smile:"),
        ];
        for (start, end, expected) in [
            (
                0,
                at[6],
                "普通**粗体**@**甲|1** [链接](https://a.com):smile:",
            ),
            (3, at[1] + 3, "通**粗**"),
            (at[2] + 1, at[3], "@**甲|1**"),
            (at[4] + 3, at[5], "[接](https://a.com)"),
            (0, 0, ""),
        ] {
            assert_eq!(from_pieces(&text, &pieces, start, end), expected);
        }
    }

    #[test]
    fn merges_same_style_and_keeps_spaces_outside() {
        let bold = |start, end| InlinePiece {
            bold: true,
            ..plain(start, end)
        };
        assert_eq!(
            from_pieces("a b c", &[bold(0, 2), bold(2, 4), plain(4, 5)], 0, 5),
            "**a b** c"
        );
    }

    #[test]
    fn nested_styles_and_code() {
        let pieces = [
            InlinePiece {
                bold: true,
                italic: true,
                ..plain(0, 2)
            },
            InlinePiece {
                code: true,
                ..plain(2, 4)
            },
        ];
        assert_eq!(from_pieces("xy`z", &pieces, 0, 4), "***xy***`` `z ``");
    }

    #[test]
    fn text_outside_pieces_is_raw() {
        let strike = |start, end| InlinePiece {
            strike: true,
            ..plain(start, end)
        };
        assert_eq!(
            from_pieces("a\nb", &[strike(0, 1), strike(2, 3)], 0, 3),
            "~~a~~\n~~b~~"
        );
    }

    #[test]
    fn link_with_same_text_is_bare_url() {
        let text = "https://a.com";
        let piece = InlinePiece {
            href: Some(text.into()),
            ..plain(0, text.len())
        };
        assert_eq!(from_pieces(text, &[piece], 0, text.len()), text);
    }

    #[test]
    fn code_selection() {
        for (start, end, expected) in [
            (0, 10, "```py\nprint(1)\nx\n```"),
            (0, 5, "`print`"),
            (6, 10, "```py\n1)\nx\n```"),
        ] {
            assert_eq!(from_code("print(1)\nx", "py", start, end), expected);
        }
    }
}
