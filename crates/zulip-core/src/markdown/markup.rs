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

static_regex!(QUOTES = r"^(?: {0,3}> ?)*");
static_regex!(ITEM = r"^(?<indent> *)(?<marker>[*+\-]|(?<number>\d{1,9})\.)(?<space> +)");
static_regex!(TASK = r"^\[[ xX]\] +");

/// Symbols at the start of a line. Offsets are relative to the line start.
#[derive(Debug, PartialEq, Eq)]
struct Markup {
    /// The `>` of each quote level (together with the spaces around it).
    quotes: Vec<(usize, usize)>,
    list: Option<ListMarkup>,
    /// Indentation alone (no list symbol): the spaces of a continuation line inside a list item.
    lead: usize,
    /// After all symbols (where the content starts).
    end: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct ListMarkup {
    /// Where the indentation starts (after the last quote level, or after the outer list symbols
    /// on the same line).
    start: usize,
    /// Column of the symbol after the quote symbols (outer list symbols on the same line count
    /// as indentation).
    indent: usize,
    /// Content columns (after the quote symbols) of the outer list symbols on the same line:
    /// `- 1. a` is a numbered list inside a bullet item, the bullet item's content at column 2.
    outer: Vec<usize>,
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
    let quote_text = QUOTES.find(line).map_or("", |g| g.as_str());
    let mut quotes = Vec::new();
    let mut pos = 0;
    while pos < quote_text.len() {
        let rest = &quote_text[pos..];
        let len = rest.find('>').map_or(rest.len(), |i| {
            i + 1 + usize::from(rest.as_bytes().get(i + 1) == Some(&b' '))
        });
        quotes.push((pos, pos + len));
        pos += len;
    }
    // List symbols; several in a row are nested lists, the innermost is the item continued and
    // deleted
    let base = quote_text.len();
    let mut end = base;
    let mut list: Option<ListMarkup> = None;
    while let Some(m) = ITEM.captures(&line[end..]) {
        let mut outer = Vec::new();
        if let Some(above) = list.take() {
            outer = above.outer;
            outer.push(end - base);
        }
        let marker = &m["marker"];
        list = Some(ListMarkup {
            start: end,
            indent: end - base + m["indent"].len(),
            outer,
            marker: match m.name("number") {
                Some(n) => Marker::Number {
                    value: n.as_str().parse().unwrap_or(1),
                    at: (end + n.start(), end + n.end()),
                },
                None => Marker::Bullet(marker.chars().next().unwrap_or('-')),
            },
            space: m["space"].to_owned(),
            task: false,
        });
        end += m.get(0).map_or(0, |g| g.end());
    }
    if let Some(list) = &mut list
        && let Some(task) = TASK.find(&line[end..])
    {
        list.task = true;
        end += task.end();
    }
    let lead = if list.is_none() {
        line[end..].len() - line[end..].trim_start_matches(' ').len()
    } else {
        0
    };
    end += lead;
    if end == 0 {
        return None;
    }
    Some(Markup {
        quotes,
        list,
        lead,
        end,
    })
}

/// Indentation stops above line `index` (relative to the quote symbols, `depth` levels of them):
/// the content columns of the list items of the list it continues. Blank and indented lines are
/// passed; a line at the start column without a list symbol, other quote depth or a literal line
/// (code, table) ends the list.
fn indent_stops(text: &str, model: &MarkdownModel, index: usize, depth: usize) -> Vec<usize> {
    let mut stops = Vec::new();
    for line in model.lines[..index].iter().rev() {
        if line.kind == LineKind::Blank {
            continue;
        }
        if is_literal(line.kind) {
            break;
        }
        let Some(m) = markup(&text[line.start..line.end]) else {
            break;
        };
        if m.quotes.len() != depth {
            break;
        }
        let base = m.quotes.last().map_or(0, |q| q.1);
        match &m.list {
            Some(list) => {
                stops.extend(&list.outer);
                stops.push(m.end - base);
            }
            None if m.lead > 0 => {}
            None => break,
        }
    }
    stops
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
    // Empty list item / quote line: remove the innermost symbol (indentation alone is kept)
    let indent_only = markup.list.is_none() && markup.lead > 0;
    if start == end && raw[markup.end..].trim().is_empty() && !indent_only {
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
    } else {
        // A continuation line keeps its indentation
        prefix.push_str(&" ".repeat(markup.lead));
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
            lead: 0,
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
            None if m.lead > 0 => {}
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
    // Indentation alone: back one indentation stop (the content column of a list item above), or
    // to the line start
    if markup.list.is_none() && markup.lead > 0 {
        let index = model.line_index_at(caret)?;
        let base = markup.end - markup.lead;
        let stop = indent_stops(text, model, index, markup.quotes.len())
            .into_iter()
            .filter(|s| *s < markup.lead)
            .max()
            .unwrap_or(0);
        let from = line_start + base + stop;
        return Some(MarkdownEdit::new(
            from,
            caret - from,
            String::new(),
            from,
            from,
        ));
    }
    let from = line_start + innermost_start(&markup);
    // A list item right after another one keeps its indentation: the symbol becomes spaces, so the
    // line continues the item above (backspace again removes the indentation). A symbol after
    // outer ones on the same line (`- 1. |a`) is just deleted, the outer item stays.
    let index = model.line_index_at(caret)?;
    let after_item = markup.list.as_ref().is_some_and(|l| l.outer.is_empty())
        && index > 0
        && line_markup(text, model, model.lines[index - 1].start)
            .is_some_and(|(_, _, above)| above.list.is_some());
    let replacement = if after_item {
        " ".repeat(text[from..caret].chars().count())
    } else {
        String::new()
    };
    let to = from + replacement.len();
    Some(MarkdownEdit::new(from, caret - from, replacement, to, to))
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
    fn nested_symbols_on_one_line() {
        // Enter continues the innermost list, under the outer item's content
        assert_eq!(enter("- 1. a|"), "- 1. a\n  2. |");
        assert_eq!(enter("- 1. a|\n  2. b"), "- 1. a\n  2. |\n  3. b");
        // Empty innermost item: back to the outer item
        assert_eq!(enter("- 1. |"), "- |");
        // Backspace deletes the innermost symbol only
        assert_eq!(backspace("- a\n- 1. |b").as_deref(), Some("- a\n- |b"));
        // Indentation steps include the outer item's content column
        assert_eq!(
            backspace("- 1. a\n     |b").as_deref(),
            Some("- 1. a\n  |b")
        );
    }

    #[test]
    fn indentation_steps_back_and_continues() {
        // Backspace: one indentation stop back, then the line start
        assert_eq!(backspace("1. a\n   |b").as_deref(), Some("1. a\n|b"));
        assert_eq!(
            backspace("1. a\n   - b\n     |c").as_deref(),
            Some("1. a\n   - b\n   |c")
        );
        // Enter keeps the indentation, also on an empty indented line
        assert_eq!(enter("1. a\n   b|"), "1. a\n   b\n   |");
        assert_eq!(enter("1. a\n   |"), "1. a\n   \n   |");
    }

    #[test]
    fn backspace_removes_innermost_markup() {
        assert_eq!(backspace("- |").as_deref(), Some("|"));
        assert_eq!(backspace("2. |").as_deref(), Some("|"));
        assert_eq!(backspace("1. a\n2. |").as_deref(), Some("1. a\n   |"));
        assert_eq!(backspace("- a\n- |b").as_deref(), Some("- a\n  |b"));
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
