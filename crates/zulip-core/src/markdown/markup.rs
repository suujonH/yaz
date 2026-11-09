//! 输入框中列表、引用的行首符号的续写与删除。规则参照 CodeMirror 6（`@codemirror/lang-markdown` 的
//! `insertNewlineContinueMarkup` 与 `deleteMarkupBackward`）：
//! - 换行：列表项、引用中换行时，新行补上同样的行首符号；有序列表的序号加一，后面连续的序号随之顺延；
//!   任务列表补未完成的 `[ ] `。光标在行首符号中间时只换行。
//! - 只有行首符号的空列表项、空引用行上换行：去掉最里层的符号（结束这一层列表 / 引用），不新起一行。
//! - 退格：光标紧跟在行首符号后面时，删掉最里层的一个符号（列表符号连同它前面的缩进）。
//!
//! 空列表项（`- `）、空引用行（`> `）在 [`super::parse`] 中不算列表 / 引用，所以这里按行的文字自己识别符号。

use super::format::MarkdownEdit;
use super::{LineKind, MarkdownModel};
use crate::util::{char_before, static_regex};

static_regex!(
    MARKUP = r"^(?<quotes>(?: {0,3}> ?)*)(?:(?<indent> *)(?<marker>[*+\-]|(?<number>\d{1,9})\.)(?<space> +)(?<task>\[[ xX]\] +)?)?"
);

/// 一行开头的符号。偏移相对行首。
#[derive(Debug, PartialEq, Eq)]
struct Markup {
    /// 每一层引用的 `>`（连同前后的空格）。
    quotes: Vec<(usize, usize)>,
    list: Option<ListMarkup>,
    /// 全部符号之后（内容开始处）。
    end: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct ListMarkup {
    /// 缩进开始处（最后一层引用之后）。
    start: usize,
    indent: usize,
    /// 圆点符号，或有序列表的序号与它在行中的位置。
    marker: Marker,
    space: String,
    task: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Marker {
    Bullet(char),
    Number { value: u64, at: (usize, usize) },
}

fn markup(line: &str) -> Option<Markup> {
    let m = MARKUP.captures(line)?;
    let end = m.get(0).map_or(0, |g| g.end());
    if end == 0 {
        return None;
    }
    let mut quotes = Vec::new();
    let mut pos = 0;
    let quote_text = m.name("quotes").map_or("", |g| g.as_str());
    while pos < quote_text.len() {
        let rest = &quote_text[pos..];
        let len = rest.find('>').map_or(rest.len(), |i| {
            i + 1 + usize::from(rest.as_bytes().get(i + 1) == Some(&b' '))
        });
        quotes.push((pos, pos + len));
        pos += len;
    }
    let list = m.name("marker").map(|marker| {
        let indent = m.name("indent").map_or(0, |g| g.as_str().len());
        ListMarkup {
            start: quote_text.len(),
            indent,
            marker: match m.name("number") {
                Some(n) => Marker::Number {
                    value: n.as_str().parse().unwrap_or(1),
                    at: (n.start(), n.end()),
                },
                None => Marker::Bullet(marker.as_str().chars().next().unwrap_or('-')),
            },
            space: m.name("space").map_or(" ", |g| g.as_str()).to_owned(),
            task: m.name("task").is_some(),
        }
    });
    Some(Markup { quotes, list, end })
}

/// 代码、公式、表格、分隔线、标题行中不续写也不删除符号。
fn is_literal(kind: LineKind) -> bool {
    matches!(
        kind,
        LineKind::FenceOpen
            | LineKind::FenceClose
            | LineKind::FenceBody
            | LineKind::IndentedCode
            | LineKind::TableRow
            | LineKind::TableSeparator
            | LineKind::Rule
            | LineKind::Heading
    )
}

/// 光标所在行的（起点、文字、符号）。
fn line_markup<'a>(
    text: &'a str,
    model: &MarkdownModel,
    at: usize,
) -> Option<(usize, &'a str, Markup)> {
    let line = &model.lines[model.line_index_at(at)?];
    if is_literal(line.kind) {
        return None;
    }
    let raw = &text[line.start..line.end];
    Some((line.start, raw, markup(raw)?))
}

/// 最里层符号的起点（相对行首）：列表符号连同缩进，否则最后一层引用。
fn innermost_start(markup: &Markup) -> usize {
    match &markup.list {
        Some(list) => list.start,
        None => markup.quotes.last().map_or(0, |q| q.0),
    }
}

/// 换行。选中的文字被换行代替；没有要续写的符号时只插入换行。
pub fn newline(text: &str, model: &MarkdownModel, start: usize, end: usize) -> MarkdownEdit {
    let plain = MarkdownEdit::new(start, end - start, "\n".to_owned(), start + 1, start + 1);
    let Some((line_start, raw, markup)) = line_markup(text, model, start) else {
        return plain;
    };
    let rel = start - line_start;
    if rel < markup.end {
        return plain;
    }
    let line_end = line_start + raw.len();
    // 空的列表项 / 引用行：去掉最里层的符号
    if start == end && raw[markup.end..].trim().is_empty() {
        let from = line_start + innermost_start(&markup);
        return MarkdownEdit::new(from, line_end - from, String::new(), from, from);
    }
    let mut prefix = String::new();
    for &(s, e) in &markup.quotes {
        prefix.push_str(&raw[s..e]);
    }
    let mut next_number = None;
    if let Some(list) = &markup.list {
        prefix.push_str(&" ".repeat(list.indent));
        match list.marker {
            Marker::Bullet(c) => prefix.push(c),
            Marker::Number { value, .. } => {
                prefix.push_str(&format!("{}.", value + 1));
                next_number = Some(value + 1);
            }
        }
        prefix.push_str(&list.space);
        if list.task {
            prefix.push_str("[ ] ");
        }
    }
    let inserted = format!("\n{prefix}");
    let caret = start + inserted.len();
    let Some(number) = next_number else {
        return MarkdownEdit::new(start, end - start, inserted, caret, caret);
    };
    // 后面同一列表中连续的序号顺延
    let renumbered = renumber_after(text, model, start, end, &markup, number);
    match renumbered {
        Some((until, tail)) => MarkdownEdit::new(
            start,
            until - start,
            format!("{inserted}{tail}"),
            caret,
            caret,
        ),
        None => MarkdownEdit::new(start, end - start, inserted, caret, caret),
    }
}

/// 新插入的序号为 `number` 时，后面同一列表（同样的引用层数和缩进）里从 `number` 起连续的序号各加一。
/// 返回（改到的位置，`end` 到这里的新文字）；没有要改的序号时为 `None`。
fn renumber_after(
    text: &str,
    model: &MarkdownModel,
    start: usize,
    end: usize,
    current: &Markup,
    number: u64,
) -> Option<(usize, String)> {
    let list = current.list.as_ref()?;
    let first = model.line_index_at(end.max(start))? + 1;
    let mut expected = number;
    let mut changes = Vec::new();
    for line in &model.lines[first..] {
        if line.kind == LineKind::Blank || is_literal(line.kind) {
            break;
        }
        let raw = &text[line.start..line.end];
        let m = markup(raw).unwrap_or(Markup {
            quotes: Vec::new(),
            list: None,
            end: 0,
        });
        if m.quotes.len() != current.quotes.len() {
            break;
        }
        match &m.list {
            Some(l) if l.indent == list.indent => match l.marker {
                Marker::Number { value, at } if value == expected => {
                    changes.push((line.start + at.0, line.start + at.1, expected + 1));
                    expected += 1;
                }
                _ => break,
            },
            // 更深一层的列表项、项目里缩进的续行
            Some(l) if l.indent > list.indent => {}
            None if raw[m.end..].starts_with(' ') => {}
            _ => break,
        }
    }
    let until = changes.last()?.1;
    let mut tail = String::new();
    let mut pos = end;
    for (s, e, value) in changes {
        tail.push_str(&text[pos..s]);
        tail.push_str(&value.to_string());
        pos = e;
    }
    Some((until, tail))
}

/// 退格：光标紧跟在行首符号后面时删掉最里层的一个符号；否则为 `None`（按普通退格处理）。
pub fn delete_markup_backward(
    text: &str,
    model: &MarkdownModel,
    caret: usize,
) -> Option<MarkdownEdit> {
    let (line_start, _, markup) = line_markup(text, model, caret)?;
    if caret - line_start != markup.end {
        return None;
    }
    let from = line_start + innermost_start(&markup);
    Some(MarkdownEdit::new(
        from,
        caret - from,
        String::new(),
        from,
        from,
    ))
}

/// 普通退格：删掉光标前的一个字符。
pub fn delete_char_backward(text: &str, caret: usize) -> Option<MarkdownEdit> {
    let c = char_before(text, caret)?;
    let from = caret - c.len_utf8();
    Some(MarkdownEdit::new(
        from,
        c.len_utf8(),
        String::new(),
        from,
        from,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{NoResolver, parse};

    /// `|` 是光标。返回换行后的文字（`|` 标出新光标）。
    fn enter(doc: &str) -> String {
        let at = doc.find('|').unwrap();
        let text = doc.replace('|', "");
        let model = parse(&text, &NoResolver);
        let edit = newline(&text, &model, at, at);
        let mut out = edit.apply(&text);
        out.insert(edit.selection_start, '|');
        out
    }

    fn backspace(doc: &str) -> Option<String> {
        let at = doc.find('|').unwrap();
        let text = doc.replace('|', "");
        let model = parse(&text, &NoResolver);
        let edit = delete_markup_backward(&text, &model, at)?;
        let mut out = edit.apply(&text);
        out.insert(edit.selection_start, '|');
        Some(out)
    }

    #[test]
    fn enter_continues_lists_and_quotes() {
        assert_eq!(enter("- a|"), "- a\n- |");
        assert_eq!(enter("* a|"), "* a\n* |");
        assert_eq!(enter("1. 111|"), "1. 111\n2. |");
        assert_eq!(enter("9.  a|"), "9.  a\n10.  |");
        assert_eq!(enter("  - a|"), "  - a\n  - |");
        assert_eq!(enter("> a|"), "> a\n> |");
        assert_eq!(enter("> - a|"), "> - a\n> - |");
        assert_eq!(enter("- [x] done|"), "- [x] done\n- [ ] |");
        // 光标在内容中间：后半段带到新的一项
        assert_eq!(enter("- ab|cd"), "- ab\n- |cd");
    }

    #[test]
    fn enter_outside_markup_is_plain() {
        assert_eq!(enter("abc|"), "abc\n|");
        assert_eq!(enter("## 标题|"), "## 标题\n|");
        assert_eq!(enter("```\n- a|\n```"), "```\n- a\n|\n```");
        // 光标在符号中间
        assert_eq!(enter("-| a"), "-\n| a");
    }

    #[test]
    fn enter_on_empty_item_removes_innermost_markup() {
        assert_eq!(enter("- a\n- |"), "- a\n|");
        assert_eq!(enter("1. a\n2. |"), "1. a\n|");
        assert_eq!(enter("> - a\n> - |"), "> - a\n> |");
        assert_eq!(enter("> a\n> |"), "> a\n|");
        assert_eq!(enter("  - |"), "|");
    }

    #[test]
    fn enter_renumbers_following_items() {
        assert_eq!(enter("1. a|\n2. b\n3. c"), "1. a\n2. |\n3. b\n4. c");
        // 嵌套的项目不影响；序号不连续时停下
        assert_eq!(
            enter("1. a|\n   - x\n2. b\n5. c"),
            "1. a\n2. |\n   - x\n3. b\n5. c"
        );
        // 空行之后是另一个列表
        assert_eq!(enter("1. a|\n\n2. b"), "1. a\n2. |\n\n2. b");
    }

    #[test]
    fn backspace_removes_innermost_markup() {
        assert_eq!(backspace("- |").as_deref(), Some("|"));
        assert_eq!(backspace("2. |").as_deref(), Some("|"));
        assert_eq!(backspace("1. a\n2. |").as_deref(), Some("1. a\n|"));
        assert_eq!(backspace("> - a|").as_deref(), None);
        assert_eq!(backspace("> - |a").as_deref(), Some("> |a"));
        assert_eq!(backspace("> |a").as_deref(), Some("|a"));
        assert_eq!(backspace("  - |a").as_deref(), Some("|a"));
        assert_eq!(backspace("- [ ] |a").as_deref(), Some("|a"));
        assert_eq!(backspace("a|").as_deref(), None);
        assert_eq!(backspace("-| a").as_deref(), None);
    }

    #[test]
    fn plain_backspace_respects_characters() {
        let edit = delete_char_backward("中文", 6).unwrap();
        assert_eq!(edit.apply("中文"), "中");
        assert!(delete_char_backward("", 0).is_none());
    }
}
