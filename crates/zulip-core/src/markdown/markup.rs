//! Continuation and deletion of line-start symbols of lists and quotes in the composer. The rules follow CodeMirror 6 (`@codemirror/lang-markdown`
//! `insertNewlineContinueMarkup` and `deleteMarkupBackward`):
//! - Newline: on a newline in a list item or quote, the new line gets the same line-start symbols; the number of an ordered list is incremented, and the consecutive numbers after it follow;
//!   a task list gets an unchecked `[ ] `. With the cursor in the middle of the line-start symbols, only a newline is inserted.
//! - Newline on an empty list item or empty quote line (only line-start symbols): remove the innermost symbol (ending that list / quote level) without starting a new line.
//! - Backspace: with the cursor right after the line-start symbols, delete the innermost symbol (a list symbol together with the indentation before it).
//!
//! Empty list items (`- `) and empty quote lines (`> `) do not count as lists / quotes in [`super::parse`], so the symbols are recognized here from the line text itself.

use super::format::MarkdownEdit;
use super::{LineKind, MarkdownModel};
use crate::util::{char_before, static_regex};

static_regex!(
    MARKUP = r"^(?<quotes>(?: {0,3}> ?)*)(?:(?<indent> *)(?<marker>[*+\-]|(?<number>\d{1,9})\.)(?<space> +)(?<task>\[[ xX]\] +)?)?"
);

/// Symbols at the start of a line. Offsets are relative to the line start.
#[derive(Debug, PartialEq, Eq)]
struct Markup {
    /// The `>` of each quote level (together with the spaces around it).
    quotes: Vec<(usize, usize)>,
    list: Option<ListMarkup>,
    /// After all symbols (where the content starts).
    end: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct ListMarkup {
    /// Where the indentation starts (after the last quote level).
    start: usize,
    indent: usize,
    /// A bullet symbol, or the number of an ordered list and its position in the line.
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

/// Symbols are neither continued nor deleted in code, formula, table, divider and heading lines.
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

/// (Start, text, symbols) of the line containing the cursor.
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

/// Start of the innermost symbol (relative to the line start): a list symbol together with its indentation, otherwise the last quote level.
fn innermost_start(markup: &Markup) -> usize {
    match &markup.list {
        Some(list) => list.start,
        None => markup.quotes.last().map_or(0, |q| q.0),
    }
}

/// Newline. The selected text is replaced by the newline; when there are no symbols to continue, only a newline is inserted.
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
    // Empty list item / quote line: remove the innermost symbol
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
    // Consecutive numbers after it in the same list follow
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

/// When the newly inserted number is `number`, each consecutive number from `number` on in the same list below (same quote depth and indentation) is incremented by one.
/// Returns (the position changed up to, the new text from `end` to there); `None` when there are no numbers to change.
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
            // Deeper list items, and indented continuation lines inside an item
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

/// Backspace: with the cursor right after the line-start symbols delete the innermost symbol; otherwise `None` (handled as a normal backspace).
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

/// Normal backspace: delete the character before the cursor.
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

    /// `|` is the cursor. Returns the text after the newline (`|` marks the new cursor).
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
        // Cursor in the middle of the content: the second half is carried to the new item
        assert_eq!(enter("- ab|cd"), "- ab\n- |cd");
    }

    #[test]
    fn enter_outside_markup_is_plain() {
        assert_eq!(enter("abc|"), "abc\n|");
        assert_eq!(enter("## 标题|"), "## 标题\n|");
        assert_eq!(enter("```\n- a|\n```"), "```\n- a\n|\n```");
        // Cursor in the middle of the symbols
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
        // Nested items do not matter; stop when the numbers are not consecutive
        assert_eq!(
            enter("1. a|\n   - x\n2. b\n5. c"),
            "1. a\n2. |\n   - x\n3. b\n5. c"
        );
        // Another list after a blank line
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
