//! 输入框的格式指令：
//! 加粗、斜体等成对符号可以加上或取消（没有选中文字、光标在该格式中时取消光标所在的格式）；
//! 引用、列表、标题按行加上、去掉或互换；代码块、引用块、剧透、公式块用围栏包住选中的文字；分隔线插入模板；
//! 表格见 [`insert_table`]。

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

/// 一次修改：把 `[start, start + len)` 换成 `replacement`，之后选中 `[selection_start, selection_end)`（修改后文档的绝对位置）。
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

    /// 应用到 `text`，返回修改后的文字。
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

/// `model` 是文字的解析结果；给出时，没有选中文字且光标在该格式中会取消光标所在的格式。
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

    // 选中链接中的一部分（如插入后选中的 url，或已输入的地址）：取消该链接，而不是再包一层
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

    // 没有选中文字：作用于光标所在的单词（与双击选中的范围相同，连续的中文算一个词），光标留在所在文字处；
    // 链接选中占位的 url。光标在提及等整体元素中时不扩展（否则会拆坏它）
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

    // 成对符号与链接不包住两端的空白
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

/// 把选中范围涉及的行设为 `level` 级标题（0 为普通段落）；所有行已是该级标题时改回段落。
/// 引用的 `>` 保留在标题前。
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

/// 光标处是否已有该格式（格式按钮显示为按下）。
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

/// 光标所在行的标题级别；不是标题为 0。
pub fn heading_level_at(model: &MarkdownModel, caret: usize) -> u8 {
    line_at(model, caret)
        .filter(|l| l.kind == LineKind::Heading)
        .map_or(0, |l| l.heading_level)
}

/// 插入全局时间 `<time:ISO> `（前面不是空白时先加空格），替换选中部分。
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

/// 放入输入框的多行内容（粘贴、常用语、插件填入、编辑消息）：结尾不是换行时补一个换行，光标落在新的一行，
/// 不会接在代码块、引用等的最后一行后面。单行内容原样返回。
pub fn with_trailing_line(content: &str) -> String {
    if content.contains('\n') && !content.ends_with('\n') {
        format!("{content}\n")
    } else {
        content.to_owned()
    }
}

/// 与 Zulip 网页端 smart_insert_block 相同：把 `block` 作为独立的段落放到 `[start, end)`，
/// 不在开头时前面补足 `padding_newlines` 个换行，后面也补足（已有的换行计算在内）。返回要插入的文字。
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

/// 与 Zulip 网页端 fenced_code.get_unused_fence 相同：比 `content` 中行首（最多缩进 3 个空格）最长的 ``` 多一个 `，
/// 至少 3 个。
pub fn unused_fence(content: &str) -> String {
    let longest = FENCE_LENGTH
        .captures_iter(content)
        .map(|m| m[1].len() + 1)
        .max();
    "`".repeat(longest.unwrap_or(3).max(3))
}

/// 插入空表格：前后与其它文字之间空一行，光标在第一个单元格。
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

// ───────────── 光标所在的格式 ─────────────

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

// 光标在其中（不含两端）的该格式元素，有嵌套时取最内层
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

// 去掉元素的符号，光标留在所在文字处。粗斜体（***）只去掉加粗或斜体的那部分；
// 链接变为“文字 地址”（与选中整个链接时相同：地址是占位的 url 或与文字相同时只留文字，文字为空时只留地址）
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
        // 光标在文字中时位置不变；在地址中时落在留下的地址的同一位置，地址被去掉时落在文字末尾
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

// ───────────── 成对符号与链接 ─────────────

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

// 加粗（2 个 *）与斜体（1 个 *）：按两侧连续的 * 个数判断是否已有该格式（*** 同时是加粗和斜体）
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

// 去掉选中文字外侧的符号
fn unwrap_outside(text: &str, s: usize, e: usize, marker: usize) -> MarkdownEdit {
    MarkdownEdit::new(
        s - marker,
        e - s + 2 * marker,
        text[s..e].to_owned(),
        s - marker,
        e - marker,
    )
}

// 去掉选中文字内首尾的符号
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

// 选中整个链接时去掉链接（地址不是占位的 url 时保留在文字后）；否则包成 [文字](url) 并选中 url
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

// ───────────── 按行 ─────────────

/// 作用于 `[s, e)` 涉及的整行，`transform` 返回每行的新内容（行数不变）。
/// 有选中文字时选中修改后的整段；没有时光标随所在行行首的增减移动（落在被删掉的符号中时移到行首）。
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

// 多行时跳过空行；只有一行（光标所在行）时空行也处理
fn skip_blank(source: &[&str], index: usize) -> bool {
    source.len() > 1 && is_blank(source[index])
}

// 所有行都已是目标列表时去掉标记，否则换成目标列表（编号从 1 递增）
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

// 所有行都是引用时去掉一层 >，否则每行加上 "> "
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

// ───────────── 块 ─────────────

// 代码块、引用块、剧透、公式块：围栏单独成行，选中文字中有 ``` 时围栏加长
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

// 分隔线：前面必须是空行（否则 --- 会把上一行变成二级标题），光标放到下一行
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

// 在 s 处开始新的块需要补的换行：s 前一行有文字时补到空一行
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

    // 选中范围用 « » 标出（没有 » 时 « 为光标）
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

    // 带解析结果：光标在格式中时取消该格式
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

    // 与网页版 smart_insert_block 相同：前后补足两个换行，开头不补
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
