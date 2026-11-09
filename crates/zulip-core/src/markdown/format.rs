//! Format commands of the composer: paired symbols such as bold and italic can be added or removed
//! (with no selection and the cursor inside that format, the format at the cursor is removed);
//! quote, list and heading are added, removed or switched per line; code block, quote block,
//! spoiler and math block wrap the selected text in fences; the divider inserts a template; for
//! tables see [`insert_table`].

use chrono::{DateTime, FixedOffset};

use super::table::empty_table;
use super::{Line, LineKind, MarkdownModel, PrefixKind, Span, SpanKind};
use crate::util::{char_after, char_before, clamp_offset, is_blank, static_regex};

const LINK_PLACEHOLDER: &str = "url";
const SPOILER_HEADER: &str = "Header";

static_regex!(FENCE_LENGTH = r"(?m)^ {0,3}(`{3,})");
static_regex!(LINK = r"^\[(?<text>.*)\]\((?<url>.*)\)$");
static_regex!(BULLET = r"^(?<indent>[ \t]*)[-*+] ");
static_regex!(NUMBERED = r"^(?<indent>[ \t]*)\d+\. ");
static_regex!(QUOTE_LINE = r"^ {0,3}> ?");
static_regex!(QUOTE_PREFIXES = r"^(?: {0,3}> ?)*");
static_regex!(HEADING_PREFIX = r"^(?<hashes>#{1,6})(?:[ \t]+|$)");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormatKind {
    Bold,
    Italic,
    Strikethrough,
    Link,
    Code,
    Math,
    Quote,
    BulletedList,
    NumberedList,
    CodeBlock,
    MathBlock,
    QuoteBlock,
    Spoiler,
    Rule,
}

/// One edit: replaces `[start, start + len)` with `replacement`, then selects `[selection_start, selection_end)` (absolute positions in the edited document).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownEdit {
    pub start: usize,
    pub len: usize,
    pub replacement: String,
    pub selection_start: usize,
    pub selection_end: usize,
}

impl MarkdownEdit {
    pub fn new(
        start: usize,
        len: usize,
        replacement: String,
        selection_start: usize,
        selection_end: usize,
    ) -> Self {
        Self {
            start,
            len,
            replacement,
            selection_start,
            selection_end,
        }
    }

    /// Applies to `text` and returns the edited text.
    pub fn apply(&self, text: &str) -> String {
        format!(
            "{}{}{}",
            &text[..self.start],
            self.replacement,
            &text[self.start + self.len..]
        )
    }
}

fn selection(text: &str, selection_start: usize, selection_end: usize) -> (usize, usize) {
    let s = clamp_offset(text, selection_start.min(selection_end));
    let e = clamp_offset(text, selection_start.max(selection_end));
    (s, e)
}

fn trim_end_offset(text: &str, s: usize, mut e: usize) -> usize {
    while e > s {
        match char_before(text, e) {
            Some(c) if c.is_whitespace() => e -= c.len_utf8(),
            _ => break,
        }
    }
    e
}

fn trim_start_offset(text: &str, mut s: usize, e: usize) -> usize {
    while s < e {
        match char_after(text, s) {
            Some(c) if c.is_whitespace() => s += c.len_utf8(),
            _ => break,
        }
    }
    s
}

/// `model` is the parse result of the text; when given, with no selection and the cursor inside that format, the format at the cursor is removed.
pub fn apply(
    text: &str,
    selection_start: usize,
    selection_end: usize,
    kind: FormatKind,
    model: Option<&MarkdownModel>,
) -> MarkdownEdit {
    let (s, e) = selection(text, selection_start, selection_end);

    match kind {
        FormatKind::BulletedList | FormatKind::NumberedList => {
            let numbered = kind == FormatKind::NumberedList;
            return lines(text, s, trim_end_offset(text, s, e), |l| list(l, numbered));
        }
        FormatKind::Quote => return lines(text, s, trim_end_offset(text, s, e), quote),
        FormatKind::CodeBlock
        | FormatKind::MathBlock
        | FormatKind::QuoteBlock
        | FormatKind::Spoiler => {
            return block(text, s, e, kind);
        }
        FormatKind::Rule => return rule(text, s, e),
        _ => {}
    }

    if s == e
        && let Some(span) = model.and_then(|m| active_span(m, s, kind))
    {
        return remove(text, span, kind, s);
    }

    // Part of a link is selected (such as the url selected after insertion, or an address already typed): remove that link instead of wrapping another layer
    if kind == FormatKind::Link
        && s < e
        && let Some(link) = model.and_then(|m| {
            m.spans
                .iter()
                .filter(|sp| {
                    sp.kind == SpanKind::Link
                        && sp.start <= s
                        && e <= sp.end
                        && (sp.start, sp.end) != (s, e)
                })
                .min_by_key(|sp| sp.end - sp.start)
        })
    {
        return remove(text, link, kind, e);
    }

    // No selection: acts on the word at the cursor (same range as a double-click selection; consecutive Chinese characters count as one word), and the cursor stays in the text;
    // for a link the placeholder url is selected. Does not expand when the cursor is in a whole element such as a mention (otherwise it would be broken)
    if s == e {
        let (ws, we) = word_at(text, s);
        let in_atomic = model.is_some_and(|m| {
            m.spans
                .iter()
                .any(|sp| sp.kind.is_atomic() && sp.start < s && s < sp.end)
        });
        if we > ws && !in_atomic {
            let edit = apply(text, ws, we, kind, model);
            if kind == FormatKind::Link {
                return edit;
            }
            let caret = edit.selection_start + (s - ws);
            return MarkdownEdit {
                selection_start: caret,
                selection_end: caret,
                ..edit
            };
        }
    }

    // Paired symbols and links do not wrap the whitespace at both ends
    let te = trim_end_offset(text, s, e);
    let ts = trim_start_offset(text, s, te);
    let multiline = text[ts..te].contains('\n');

    match kind {
        FormatKind::Code if multiline => block(text, s, e, FormatKind::CodeBlock),
        FormatKind::Math if multiline => block(text, s, e, FormatKind::MathBlock),
        FormatKind::Bold => stars(text, ts, te, 2),
        FormatKind::Italic => stars(text, ts, te, 1),
        FormatKind::Strikethrough => pair(text, ts, te, b'~', 2),
        FormatKind::Code => pair(text, ts, te, b'`', 1),
        FormatKind::Math => pair(text, ts, te, b'$', 2),
        FormatKind::Link => link(text, ts, te),
        _ => unreachable!("块和按行的格式已在前面返回"),
    }
}

/// Sets the lines touched by the selection to a heading of level `level` (0 is a normal paragraph); when all lines are already that heading level, changes them back to paragraphs.
/// The `>` of a quote stays before the heading.
pub fn set_heading(
    text: &str,
    selection_start: usize,
    selection_end: usize,
    level: u8,
) -> MarkdownEdit {
    let (s, e) = selection(text, selection_start, selection_end);
    let level = level.min(6);
    lines(text, s, trim_end_offset(text, s, e), |source| {
        let skip = |i: usize| skip_blank(source, i);
        let all_at_level = source
            .iter()
            .enumerate()
            .all(|(i, l)| skip(i) || heading_level(l) == level);
        let target = if all_at_level { 0 } else { level };
        source
            .iter()
            .enumerate()
            .map(|(i, line)| {
                if skip(i) {
                    return (*line).to_owned();
                }
                let quote = quote_prefix_len(line);
                let rest = HEADING_PREFIX.replace(&line[quote..], "");
                let hashes = if target > 0 {
                    format!("{} ", "#".repeat(usize::from(target)))
                } else {
                    String::new()
                };
                format!("{}{hashes}{rest}", &line[..quote])
            })
            .collect()
    })
}

/// Whether the cursor is already in that format (the format button is shown as pressed).
pub fn is_active(model: &MarkdownModel, caret: usize, kind: FormatKind) -> bool {
    if !span_kinds(kind).is_empty() {
        return active_span(model, caret, kind).is_some();
    }
    let Some(line) = line_at(model, caret) else {
        return false;
    };
    let has_prefix = |prefix: PrefixKind| line.prefixes.iter().any(|p| p.kind == prefix);
    match kind {
        FormatKind::Quote => has_prefix(PrefixKind::Quote),
        FormatKind::BulletedList => {
            line.kind == LineKind::ListItem && has_prefix(PrefixKind::Bullet)
        }
        FormatKind::NumberedList => {
            line.kind == LineKind::ListItem && has_prefix(PrefixKind::Ordered)
        }
        _ => false,
    }
}

/// Heading level of the line at the cursor; 0 if it is not a heading.
pub fn heading_level_at(model: &MarkdownModel, caret: usize) -> u8 {
    line_at(model, caret)
        .filter(|l| l.kind == LineKind::Heading)
        .map_or(0, |l| l.heading_level)
}

/// Inserts a global time `<time:ISO> ` (adds a space first if the previous character is not whitespace), replacing the selection.
pub fn insert_time(
    text: &str,
    selection_start: usize,
    selection_end: usize,
    time: DateTime<FixedOffset>,
) -> MarkdownEdit {
    let (s, e) = selection(text, selection_start, selection_end);
    let before = if char_before(text, s).is_some_and(|c| !c.is_whitespace()) {
        " "
    } else {
        ""
    };
    let insert = format!("{before}<time:{}> ", time.format("%Y-%m-%dT%H:%M:%S%:z"));
    let caret = s + insert.len();
    MarkdownEdit::new(s, e - s, insert, caret, caret)
}

/// Multi-line content put into the composer (paste, canned replies, plugin fill, message edit): if it does not end with a newline, one is added and the cursor lands on the new line,
/// so it is not appended after the last line of a code block, quote, etc. Single-line content is returned unchanged.
pub fn with_trailing_line(content: &str) -> String {
    if content.contains('\n') && !content.ends_with('\n') {
        format!("{content}\n")
    } else {
        content.to_owned()
    }
}

/// Same as smart_insert_block of the Zulip web app: puts `block` as a separate paragraph at `[start, end)`,
/// adds `padding_newlines` newlines before it when not at the start, and also after it (existing newlines are counted). Returns the text to insert.
pub fn pad_block(
    text: &str,
    start: usize,
    end: usize,
    block: &str,
    padding_newlines: usize,
) -> String {
    let bytes = text.as_bytes();
    let before = if start > 0 {
        let existing = bytes[..start]
            .iter()
            .rev()
            .take(padding_newlines)
            .take_while(|b| **b == b'\n')
            .count();
        padding_newlines - existing
    } else {
        0
    };
    let after = bytes[end..]
        .iter()
        .take(padding_newlines)
        .take_while(|b| **b == b'\n')
        .count();
    format!(
        "{}{block}{}",
        "\n".repeat(before),
        "\n".repeat(padding_newlines - after)
    )
}

/// Same as fenced_code.get_unused_fence of the Zulip web app: one more ` than the longest ``` at the start of a line in `content` (indented by at most 3 spaces),
/// at least 3.
pub fn unused_fence(content: &str) -> String {
    let longest = FENCE_LENGTH
        .captures_iter(content)
        .map(|m| m[1].len() + 1)
        .max();
    "`".repeat(longest.unwrap_or(3).max(3))
}

/// Inserts an empty table: separated from other text before and after by a blank line, with the cursor in the first cell.
pub fn insert_table(
    text: &str,
    selection_start: usize,
    selection_end: usize,
    columns: usize,
    rows: usize,
) -> MarkdownEdit {
    let (s, e) = selection(text, selection_start, selection_end);
    let before = blank_line_before(text, s);
    let bytes = text.as_bytes();
    let after = if e >= text.len() {
        ""
    } else if bytes[e] != b'\n' {
        "\n\n"
    } else if bytes.get(e + 1).is_some_and(|b| *b != b'\n') {
        "\n"
    } else {
        ""
    };
    let replacement = format!("{before}{}{after}", empty_table(columns, rows));
    let caret = s + before.len() + 2;
    MarkdownEdit::new(s, e - s, replacement, caret, caret)
}

// ------------- format at the cursor -------------

fn span_kinds(kind: FormatKind) -> &'static [SpanKind] {
    match kind {
        FormatKind::Bold => &[SpanKind::Strong, SpanKind::StrongEm],
        FormatKind::Italic => &[SpanKind::Em, SpanKind::StrongEm],
        FormatKind::Strikethrough => &[SpanKind::Del],
        FormatKind::Code => &[SpanKind::Code],
        FormatKind::Math => &[SpanKind::Math],
        FormatKind::Link => &[SpanKind::Link],
        _ => &[],
    }
}

// The format element containing the cursor (excluding both ends); the innermost one when nested
fn active_span(model: &MarkdownModel, caret: usize, kind: FormatKind) -> Option<&Span> {
    let kinds = span_kinds(kind);
    model
        .spans
        .iter()
        .filter(|s| kinds.contains(&s.kind) && s.start < caret && caret < s.end)
        .min_by_key(|s| s.end - s.start)
}

fn line_at(model: &MarkdownModel, caret: usize) -> Option<&Line> {
    model.clamped_line_index(caret).map(|i| &model.lines[i])
}

fn word_at(text: &str, caret: usize) -> (usize, usize) {
    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    let start = text[..caret]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(caret, |(i, _)| i);
    let end = text[caret..]
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(text.len(), |(i, _)| caret + i);
    (start, end)
}

// Removes the element's symbols, leaving the cursor in the text. For bold-italic (***) only the bold or italic part is removed;
// a link becomes "text address" (same as when the whole link is selected: if the address is the placeholder url or equals the text only the text remains; if the text is empty only the address remains)
fn remove(text: &str, span: &Span, kind: FormatKind, caret: usize) -> MarkdownEdit {
    let len = span.end - span.start;
    if kind == FormatKind::Link {
        let label = &text[span.content_start..span.content_end];
        let url = span.data.as_deref().unwrap_or("");
        let keep_url =
            !label.is_empty() && !url.is_empty() && url != LINK_PLACEHOLDER && url != label;
        let plain = if label.is_empty() {
            url.to_owned()
        } else if keep_url {
            format!("{label} {url}")
        } else {
            label.to_owned()
        };
        // With the cursor in the text the position is unchanged; in the address it lands at the same position in the remaining address, and at the end of the text when the address is removed
        let url_start = text[span.content_end..]
            .find(url)
            .map(|i| span.content_end + i);
        let offset = match url_start {
            Some(url_start) if caret > span.content_end => {
                if keep_url {
                    (label.len() + 1 + caret).saturating_sub(url_start)
                } else if label.is_empty() {
                    caret.saturating_sub(url_start)
                } else {
                    label.len()
                }
            }
            _ => caret.saturating_sub(span.content_start),
        };
        let at = span.start + clamp_offset(&plain, offset);
        return MarkdownEdit::new(span.start, len, plain, at, at);
    }

    let (open, close) = match (kind, span.kind) {
        (FormatKind::Bold, SpanKind::StrongEm) => (2, 2),
        (FormatKind::Italic, SpanKind::StrongEm) => (1, 1),
        _ => (span.content_start - span.start, span.end - span.content_end),
    };
    let inner = &text[span.start + open..span.end - close];
    let at = span.start + clamp_offset(inner, caret.saturating_sub(span.start + open));
    MarkdownEdit::new(span.start, len, inner.to_owned(), at, at)
}

// ------------- paired symbols and links -------------

fn run_before(text: &str, index: usize, c: u8) -> usize {
    text.as_bytes()[..index]
        .iter()
        .rev()
        .take_while(|b| **b == c)
        .count()
}

fn run_after(text: &str, index: usize, c: u8) -> usize {
    text.as_bytes()[index..]
        .iter()
        .take_while(|b| **b == c)
        .count()
}

// Bold (2 *) and italic (1 *): decide whether the format exists from the number of consecutive * on both sides (*** is both bold and italic)
fn stars(text: &str, s: usize, e: usize, marker: usize) -> MarkdownEdit {
    let has = |count: usize| {
        if marker == 1 {
            matches!(count, 1 | 3)
        } else {
            matches!(count, 2 | 3)
        }
    };

    let before = run_before(text, s, b'*');
    let after = run_after(text, e, b'*');
    if before == after && has(before) {
        return unwrap_outside(text, s, e, marker);
    }

    let selected = &text[s..e];
    let lead = run_after(selected, 0, b'*');
    let trail = run_before(selected, selected.len(), b'*');
    if lead == trail && selected.len() > 2 * lead && has(lead) {
        return strip(text, s, e, marker);
    }

    wrap(text, s, e, &"*".repeat(marker))
}

fn pair(text: &str, s: usize, e: usize, c: u8, marker: usize) -> MarkdownEdit {
    if run_before(text, s, c) == marker && run_after(text, e, c) == marker {
        return unwrap_outside(text, s, e, marker);
    }
    let selected = &text[s..e];
    if selected.len() > 2 * marker
        && run_after(selected, 0, c) == marker
        && run_before(selected, selected.len(), c) == marker
    {
        return strip(text, s, e, marker);
    }
    wrap(text, s, e, &char::from(c).to_string().repeat(marker))
}

// Remove the symbols outside the selected text
fn unwrap_outside(text: &str, s: usize, e: usize, marker: usize) -> MarkdownEdit {
    MarkdownEdit::new(
        s - marker,
        e - s + 2 * marker,
        text[s..e].to_owned(),
        s - marker,
        e - marker,
    )
}

// Remove the symbols at the start and end inside the selected text
fn strip(text: &str, s: usize, e: usize, marker: usize) -> MarkdownEdit {
    let inner = &text[s + marker..e - marker];
    MarkdownEdit::new(s, e - s, inner.to_owned(), s, s + inner.len())
}

fn wrap(text: &str, s: usize, e: usize, symbol: &str) -> MarkdownEdit {
    MarkdownEdit::new(
        s,
        e - s,
        format!("{symbol}{}{symbol}", &text[s..e]),
        s + symbol.len(),
        e + symbol.len(),
    )
}

// When the whole link is selected, remove the link (keep the address after the text if it is not the placeholder url); otherwise wrap as [text](url) and select the url
fn link(text: &str, s: usize, e: usize) -> MarkdownEdit {
    let selected = &text[s..e];
    if let Some(m) = LINK.captures(selected) {
        let plain = if &m["url"] == LINK_PLACEHOLDER {
            m["text"].to_owned()
        } else {
            format!("{} {}", &m["text"], &m["url"])
        };
        let end = s + plain.len();
        return MarkdownEdit::new(s, e - s, plain, s, end);
    }
    let url_start = s + selected.len() + 3;
    MarkdownEdit::new(
        s,
        e - s,
        format!("[{selected}]({LINK_PLACEHOLDER})"),
        url_start,
        url_start + LINK_PLACEHOLDER.len(),
    )
}

// ------------- per line -------------

/// Acts on the whole lines touched by `[s, e)`; `transform` returns the new content of each line (the number of lines stays the same).
/// With a selection, the whole edited range is selected; without one, the cursor moves with the added or removed line-start characters (moves to the line start if it lands in a removed symbol).
fn lines(
    text: &str,
    s: usize,
    e: usize,
    transform: impl FnOnce(&[&str]) -> Vec<String>,
) -> MarkdownEdit {
    let line_start = text[..s].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[e..].find('\n').map_or(text.len(), |i| e + i);
    let source: Vec<&str> = text[line_start..line_end].split('\n').collect();
    let result = transform(&source);
    let replacement = result.join("\n");
    let len = line_end - line_start;
    if s != e {
        let end = line_start + replacement.len();
        return MarkdownEdit::new(line_start, len, replacement, line_start, end);
    }
    let first = &result[0];
    let column = (s - line_start + first.len()).saturating_sub(source[0].len());
    let caret = line_start + clamp_offset(first, column);
    MarkdownEdit::new(line_start, len, replacement, caret, caret)
}

// With multiple lines skip empty lines; with only one line (the cursor line) empty lines are processed too
fn skip_blank(source: &[&str], index: usize) -> bool {
    source.len() > 1 && is_blank(source[index])
}

// When all lines are already the target list, remove the marker; otherwise switch to the target list (numbering increases from 1)
fn list(source: &[&str], numbered: bool) -> Vec<String> {
    let (target, other) = if numbered {
        (&*NUMBERED, &*BULLET)
    } else {
        (&*BULLET, &*NUMBERED)
    };
    let remove = source
        .iter()
        .enumerate()
        .all(|(i, l)| skip_blank(source, i) || target.is_match(l));

    let mut number = 1;
    source
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if skip_blank(source, i) {
                return (*line).to_owned();
            }
            let captures = target.captures(line).or_else(|| other.captures(line));
            let (indent, body) = match &captures {
                Some(m) => (
                    m.name("indent").map_or("", |g| g.as_str()),
                    &line[m.get(0).map_or(0, |g| g.end())..],
                ),
                None => {
                    let body = line.trim_start_matches([' ', '\t']);
                    (&line[..line.len() - body.len()], body)
                }
            };
            let marker = if remove {
                String::new()
            } else if numbered {
                number += 1;
                format!("{}. ", number - 1)
            } else {
                "- ".to_owned()
            };
            format!("{indent}{marker}{body}")
        })
        .collect()
}

// When all lines are quotes, remove one level of >; otherwise add "> " to each line
fn quote(source: &[&str]) -> Vec<String> {
    let remove = source
        .iter()
        .enumerate()
        .all(|(i, l)| skip_blank(source, i) || QUOTE_LINE.is_match(l));
    source
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if skip_blank(source, i) {
                (*line).to_owned()
            } else if remove {
                QUOTE_LINE.replace(line, "").into_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect()
}

fn quote_prefix_len(line: &str) -> usize {
    QUOTE_PREFIXES.find(line).map_or(0, |m| m.end())
}

fn heading_level(line: &str) -> u8 {
    HEADING_PREFIX
        .captures(&line[quote_prefix_len(line)..])
        .map_or(0, |m| m["hashes"].len() as u8)
}

// ------------- blocks -------------

// Code block, quote block, spoiler, math block: fences on their own lines; the fence is lengthened if the selected text contains ```
fn block(text: &str, s: usize, e: usize, kind: FormatKind) -> MarkdownEdit {
    let bytes = text.as_bytes();
    let (mut s, mut e) = (s, e);
    while s < e && bytes[s] == b'\n' {
        s += 1;
    }
    while e > s && bytes[e - 1] == b'\n' {
        e -= 1;
    }
    let selected = &text[s..e];
    let mut fence = "```".to_owned();
    while selected.contains(&fence) {
        fence.push('`');
    }
    let open = match kind {
        FormatKind::QuoteBlock => format!("{fence}quote"),
        FormatKind::Spoiler => format!("{fence}spoiler {SPOILER_HEADER}"),
        FormatKind::MathBlock => format!("{fence}math"),
        _ => fence.clone(),
    };
    let before = if s > 0 && bytes[s - 1] != b'\n' {
        "\n"
    } else {
        ""
    };
    let after = if e < text.len() && bytes[e] != b'\n' {
        "\n"
    } else {
        ""
    };
    let replacement = format!("{before}{open}\n{selected}\n{fence}{after}");

    let open_start = s + before.len();
    let content_start = open_start + open.len() + 1;
    let (sel_start, sel_end) = match kind {
        FormatKind::CodeBlock => (open_start + fence.len(), open_start + fence.len()),
        FormatKind::Spoiler => (
            open_start + fence.len() + "spoiler ".len(),
            open_start + open.len(),
        ),
        _ => (content_start, content_start + selected.len()),
    };
    MarkdownEdit::new(s, e - s, replacement, sel_start, sel_end)
}

// Divider: must be preceded by a blank line (otherwise --- would turn the previous line into a level-2 heading), and the cursor goes to the next line
fn rule(text: &str, s: usize, e: usize) -> MarkdownEdit {
    let line_break_after = text.as_bytes().get(e) == Some(&b'\n');
    let replacement = format!(
        "{}---{}",
        blank_line_before(text, s),
        if line_break_after { "" } else { "\n" }
    );
    let caret = s + replacement.len() + usize::from(line_break_after);
    MarkdownEdit::new(s, e - s, replacement, caret, caret)
}

// Newlines needed to start a new block at s: if the line before s has text, pad up to one blank line
fn blank_line_before(text: &str, s: usize) -> &'static str {
    let bytes = text.as_bytes();
    if s == 0 {
        ""
    } else if bytes[s - 1] != b'\n' {
        "\n\n"
    } else if s >= 2 && bytes[s - 2] != b'\n' {
        "\n"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::super::{NoResolver, parse};
    use super::FormatKind as K;
    use super::*;

    // The selection range is marked with the guillemets U+00AB and U+00BB (if there is no U+00BB, U+00AB marks the cursor)
    fn unmark(marked: &str) -> (String, usize, usize) {
        let s = marked.find('«').unwrap();
        let without = marked.replacen('«', "", 1);
        match without.find('»') {
            Some(e) => (without.replacen('»', "", 1), s, e),
            None => (without, s, s),
        }
    }

    fn mark(text: &str, edit: &MarkdownEdit) -> String {
        let mut result = edit.apply(text);
        if edit.selection_start == edit.selection_end {
            result.insert(edit.selection_start, '«');
        } else {
            result.insert(edit.selection_end, '»');
            result.insert(edit.selection_start, '«');
        }
        result
    }

    fn run(marked: &str, kind: FormatKind) -> String {
        let (text, s, e) = unmark(marked);
        mark(&text, &apply(&text, s, e, kind, None))
    }

    // With a parse result: when the cursor is in a format, that format is removed
    fn run_at(marked: &str, kind: FormatKind) -> String {
        let (text, s, e) = unmark(marked);
        let model = parse(&text, &NoResolver);
        mark(&text, &apply(&text, s, e, kind, Some(&model)))
    }

    fn check(cases: &[(&str, FormatKind, &str)]) {
        for (marked, kind, expected) in cases {
            assert_eq!(&run(marked, *kind), expected, "{marked} {kind:?}");
        }
    }

    #[test]
    fn wrap_pairs() {
        check(&[
            ("a «b» c", K::Bold, "a **«b»** c"),
            ("a «b» c", K::Italic, "a *«b»* c"),
            ("a «b» c", K::Strikethrough, "a ~~«b»~~ c"),
            ("a «b» c", K::Code, "a `«b»` c"),
            ("a «b» c", K::Math, "a $$«b»$$ c"),
            ("a« b »c", K::Bold, "a **«b»** c"),
            ("a «", K::Bold, "a **«**"),
            ("a «", K::Italic, "a *«*"),
            ("a «", K::Code, "a `«`"),
            ("a «", K::Math, "a $$«$$"),
        ]);
    }

    #[test]
    fn unwrap_pairs() {
        check(&[
            ("a **«b»** c", K::Bold, "a «b» c"),
            ("a *«b»* c", K::Italic, "a «b» c"),
            ("a ~~«b»~~ c", K::Strikethrough, "a «b» c"),
            ("a `«b»` c", K::Code, "a «b» c"),
            ("a $$«b»$$ c", K::Math, "a «b» c"),
            ("a «**b**» c", K::Bold, "a «b» c"),
            ("a «*b*» c", K::Italic, "a «b» c"),
            ("a «~~b~~» c", K::Strikethrough, "a «b» c"),
            ("a «`b`» c", K::Code, "a «b» c"),
        ]);
    }

    #[test]
    fn star_counting() {
        check(&[
            ("**«x»**", K::Italic, "***«x»***"),
            ("*«x»*", K::Bold, "***«x»***"),
            ("***«x»***", K::Italic, "**«x»**"),
            ("***«x»***", K::Bold, "*«x»*"),
            ("«**x**»", K::Italic, "*«**x**»*"),
        ]);
    }

    #[test]
    fn link_format() {
        for (marked, expected) in [
            ("看«文字»吧", "看[文字](«url»)吧"),
            ("«", "[](«url»)"),
            ("«[文字](url)»", "«文字»"),
            ("«[文字](https://a.com)»", "«文字 https://a.com»"),
        ] {
            assert_eq!(run(marked, K::Link), expected, "{marked}");
        }
    }

    #[test]
    fn list_format() {
        check(&[
            ("«a\nb»", K::BulletedList, "«- a\n- b»"),
            ("«a\n\nb»", K::NumberedList, "«1. a\n\n2. b»"),
            ("«- a\n- b»", K::BulletedList, "«a\nb»"),
            ("«1. a\n2. b»", K::NumberedList, "«a\nb»"),
            ("«- a\n- b»", K::NumberedList, "«1. a\n2. b»"),
            ("x\n  a«b\nc\n»", K::BulletedList, "x\n«  - ab\n- c»\n"),
        ]);
    }

    #[test]
    fn block_format() {
        check(&[
            ("x«a\nb»y", K::Code, "x\n```«\na\nb\n```\ny"),
            ("«", K::CodeBlock, "```«\n\n```"),
            ("x «a» y", K::QuoteBlock, "x \n```quote\n«a»\n```\n y"),
            ("«a»", K::Spoiler, "```spoiler «Header»\na\n```"),
            ("«a\nb»", K::Math, "```math\n«a\nb»\n```"),
            ("«a\n```\nb»", K::Code, "````«\na\n```\nb\n````"),
            ("«", K::MathBlock, "```math\n«\n```"),
        ]);
    }

    #[test]
    fn remove_at_caret() {
        for (marked, kind, expected) in [
            ("a **b«c** d", K::Bold, "a b«c d"),
            ("a *b«c* d", K::Italic, "a b«c d"),
            ("***b«c***", K::Italic, "**b«c**"),
            ("***b«c***", K::Bold, "*b«c*"),
            ("a ~~b«c~~", K::Strikethrough, "a b«c"),
            ("`co«de`", K::Code, "co«de"),
            (
                "x [文«字](https://a.com) y",
                K::Link,
                "x 文«字 https://a.com y",
            ),
            (
                "x [文字](https://a«.com) y",
                K::Link,
                "x 文字 https://a«.com y",
            ),
            ("x [文«字](url) y", K::Link, "x 文«字 y"),
            ("x [](https://a«.com) y", K::Link, "x https://a«.com y"),
            ("x [文字](u«rl) y", K::Link, "x 文字« y"),
            ("x [文字](«url») y", K::Link, "x 文字« y"),
            (
                "x [文字](«https://a.com») y",
                K::Link,
                "x 文字 https://a.com« y",
            ),
            (
                "x [«文»字](https://a.com) y",
                K::Link,
                "x 文«字 https://a.com y",
            ),
            ("a **b** «c", K::Bold, "a **b** **«c**"),
            ("hel«lo world", K::Bold, "**hel«lo** world"),
            ("hel«lo world", K::Italic, "*hel«lo* world"),
            ("你好«世界，再见", K::Bold, "**你好«世界**，再见"),
            ("hel«lo", K::Link, "[hello](«url»)"),
            ("hel«lo", K::Code, "`hel«lo`"),
            ("a «, b", K::Bold, "a **«**, b"),
            ("a «**b**", K::Bold, "a **«****b**"),
        ] {
            assert_eq!(run_at(marked, kind), expected, "{marked} {kind:?}");
        }
    }

    #[test]
    fn lines_at_caret() {
        check(&[
            ("a«b", K::Quote, "> a«b"),
            ("> a«b", K::Quote, "a«b"),
            (">«ab", K::Quote, "«ab"),
            ("«a\n\nb»", K::Quote, "«> a\n\n> b»"),
            ("«> a\n> b»", K::Quote, "«a\nb»"),
            ("x\na«b", K::BulletedList, "x\n- a«b"),
            ("- a«b", K::BulletedList, "a«b"),
            ("1. a«b", K::BulletedList, "- a«b"),
            ("«", K::NumberedList, "1. «"),
        ]);
    }

    #[test]
    fn heading() {
        for (marked, level, expected) in [
            ("ab«c", 2, "## ab«c"),
            ("## ab«c", 2, "ab«c"),
            ("# ab«c", 3, "### ab«c"),
            ("## a«b", 0, "a«b"),
            ("> a«b", 1, "> # a«b"),
            ("«a\n\n## b»", 1, "«# a\n\n# b»"),
            ("«", 1, "# «"),
        ] {
            let (text, s, e) = unmark(marked);
            assert_eq!(
                mark(&text, &set_heading(&text, s, e, level)),
                expected,
                "{marked}"
            );
        }
    }

    #[test]
    fn insert_rule() {
        check(&[
            ("abc«", K::Rule, "abc\n\n---\n«"),
            ("«", K::Rule, "---\n«"),
            ("a\n«\nb", K::Rule, "a\n\n---\n«b"),
        ]);
    }

    #[test]
    fn table_insertion() {
        for (text, caret, expected) in [
            ("abc", 3, "abc\n\n| « |  |\n| --- | --- |\n|  |  |"),
            ("x", 0, "| « |  |\n| --- | --- |\n|  |  |\n\nx"),
        ] {
            assert_eq!(
                mark(text, &insert_table(text, caret, caret, 2, 2)),
                expected
            );
        }
    }

    #[test]
    fn active_formats() {
        for (marked, kind, expected) in [
            ("a **b«c**", K::Bold, true),
            ("a **bc**«", K::Bold, false),
            ("***b«***", K::Italic, true),
            ("[a«](u)", K::Link, true),
            ("> a«", K::Quote, true),
            ("- a«", K::BulletedList, true),
            ("- a«", K::NumberedList, false),
            ("2. a«", K::NumberedList, true),
        ] {
            let (text, s, _) = unmark(marked);
            assert_eq!(
                is_active(&parse(&text, &NoResolver), s, kind),
                expected,
                "{marked}"
            );
        }
    }

    #[test]
    fn heading_level_at_caret() {
        for (marked, expected) in [("### a«", 3), ("a\n## b«", 2), ("a«", 0)] {
            let (text, s, _) = unmark(marked);
            assert_eq!(heading_level_at(&parse(&text, &NoResolver), s), expected);
        }
    }

    #[test]
    fn time_insertion() {
        let time = FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 24, 15, 0, 0)
            .unwrap();
        for (marked, expected) in [
            ("在«", "在 <time:2026-10-24T15:00:00+08:00> «"),
            ("在 «", "在 <time:2026-10-24T15:00:00+08:00> «"),
            ("«旧»", "<time:2026-10-24T15:00:00+08:00> «"),
        ] {
            let (text, s, e) = unmark(marked);
            assert_eq!(mark(&text, &insert_time(&text, s, e, time)), expected);
        }
    }

    #[test]
    fn trailing_line() {
        for (content, expected) in [
            ("你好", "你好"),
            ("```quote\n引用\n```", "```quote\n引用\n```\n"),
            ("第一行\n第二行\n", "第一行\n第二行\n"),
            ("", ""),
        ] {
            assert_eq!(with_trailing_line(content), expected);
        }
    }

    // Same as smart_insert_block in the web version: pad two newlines before and after, none at the start
    #[test]
    fn padded_block() {
        for (marked, expected) in [
            ("«", "B\n\n"),
            ("前«", "\n\nB\n\n"),
            ("前\n«", "\nB\n\n"),
            ("前\n\n«\n后", "B\n"),
            ("«\n\n后", "B"),
        ] {
            let (text, s, e) = unmark(marked);
            assert_eq!(pad_block(&text, s, e, "B", 2), expected, "{marked}");
        }
    }

    #[test]
    fn unused_fences() {
        for (content, expected) in [
            ("你好", "```"),
            ("a ``` b", "```"),
            ("```\nx\n```", "````"),
            ("   `````py\nx", "``````"),
            ("    ```\nx", "```"),
        ] {
            assert_eq!(unused_fence(content), expected, "{content}");
        }
    }
}
