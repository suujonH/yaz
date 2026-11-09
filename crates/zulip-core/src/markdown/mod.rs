//! 按 Zulip 服务器的 Markdown 规则（Python-Markdown 加 zerver/lib/markdown 的自定义规则）标出块与行内元素的区间，
//! 供输入框显示格式。不生成 HTML。与服务器一致的要点：没有反斜杠转义、下划线不是强调、加粗等不能跨行、
//! 找不到的用户 / 频道 / 表情按普通文字。

pub mod code_language;
pub mod format;
pub mod inline;
pub mod markup;
pub mod quote;
pub mod table;
pub mod table_wrap;

mod text_forms;

pub use text_forms::{to_plain_text, to_preview};

use crate::html;
use crate::text::{mention, time};
use crate::util::{
    char_after, char_before, is_blank, parse_digits, static_fancy_regex, static_regex,
};

/// 已被高优先级元素占用的字符（相当于服务器的占位符）：不是空白也不是 Markdown 符号。占一个字节，偏移不变。
const MASKED: &str = "\u{2}";

const WILDCARDS: [&str; 5] = ["all", "everyone", "channel", "stream", "topic"];

/// 指向 `url` 的链接 `[文件名](地址)`。服务器没有反斜杠转义，文件名中的方括号去掉，否则不配对时链接不成立。
pub fn link_markdown(name: &str, url: &str) -> String {
    let clean: String = name.chars().filter(|c| !matches!(c, '[' | ']')).collect();
    format!("[{clean}]({url})")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineKind {
    Paragraph,
    Blank,
    Heading,
    ListItem,
    Rule,
    FenceOpen,
    FenceClose,
    FenceBody,
    IndentedCode,
    TableRow,
    TableSeparator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FenceKind {
    Code,
    Quote,
    Spoiler,
    Math,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrefixKind {
    Quote,
    Heading,
    Bullet,
    Ordered,
}

/// 行首要隐藏 / 替换的前缀区间（引用的每个 "> "、标题的 "## "、列表的 "- " / "1. "）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefix {
    pub kind: PrefixKind,
    pub start: usize,
    pub end: usize,
}

/// 文档中的一行。偏移都是相对整个文档的绝对位置；`end` 不含换行符。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub start: usize,
    pub end: usize,
    pub kind: LineKind,
    /// 按位置排列。
    pub prefixes: Vec<Prefix>,
    /// 行内容（去掉前缀后）的起点，行内解析从这里开始。
    pub content_start: usize,
    /// 标题级别 1～6，其它行为 0。
    pub heading_level: u8,
    /// "> " 层数加上外层 ` ```quote ` 的层数。
    pub quote_depth: usize,
    /// 所在（或本行打开 / 关闭的）围栏种类；不在围栏中为 `None`。
    pub fence: Option<FenceKind>,
    /// FenceOpen 行：代码语言或剧透标题。
    pub fence_info: Option<String>,
}

impl Line {
    fn new(start: usize, end: usize, kind: LineKind) -> Self {
        Self {
            start,
            end,
            kind,
            prefixes: Vec::new(),
            content_start: start,
            heading_level: 0,
            quote_depth: 0,
            fence: None,
            fence_info: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpanKind {
    Code,
    StrongEm,
    Strong,
    Em,
    Del,
    Math,
    Link,
    Url,
    UserMention,
    GroupMention,
    ChannelLink,
    Time,
    Emoji,
    UploadImage,
    UploadFile,
    UploadPlaceholder,
}

impl SpanKind {
    /// 在输入框中作为一个整体显示的元素（光标进入时也不展开符号）。
    pub fn is_atomic(self) -> bool {
        matches!(
            self,
            Self::UserMention
                | Self::GroupMention
                | Self::ChannelLink
                | Self::Time
                | Self::Emoji
                | Self::UploadImage
                | Self::UploadFile
                | Self::UploadPlaceholder
        )
    }
}

/// 行内元素：`[start, end)` 为整个元素，`[content_start, content_end)` 为显示的内容；两侧为符号。
/// `data`：Link / Url / UploadImage / UploadFile 为地址；提及 / 频道为显示文字；Emoji 为名称；Time 为原始时间文字；
/// UploadPlaceholder 为文件名；Code / Math 等为 `None`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub kind: SpanKind,
    pub start: usize,
    pub end: usize,
    pub content_start: usize,
    pub content_end: usize,
    pub data: Option<String>,
}

/// 解析结果。`spans` 按 `start` 升序。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MarkdownModel {
    pub lines: Vec<Line>,
    pub spans: Vec<Span>,
}

impl MarkdownModel {
    /// `offset` 所在的行号（0 起）；没有行时为 `None`。
    pub fn line_index_at(&self, offset: usize) -> Option<usize> {
        self.lines
            .partition_point(|l| l.start <= offset)
            .checked_sub(1)
    }

    fn clamped_line_index(&self, offset: usize) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }
        Some(self.line_index_at(offset).unwrap_or(0))
    }
}

/// 判断提及、频道、表情是否存在（服务器找不到时按普通文字处理）。
pub trait MarkdownResolver {
    fn has_user(&self, name: &str, id: Option<i64>) -> bool;
    fn has_group(&self, name: &str) -> bool;
    fn has_channel(&self, name: &str) -> bool;
    fn has_emoji(&self, name: &str) -> bool;
}

/// 什么都不存在：只需要行的划分和不依赖数据的格式时使用。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoResolver;

impl MarkdownResolver for NoResolver {
    fn has_user(&self, _: &str, _: Option<i64>) -> bool {
        false
    }
    fn has_group(&self, _: &str) -> bool {
        false
    }
    fn has_channel(&self, _: &str) -> bool {
        false
    }
    fn has_emoji(&self, _: &str) -> bool {
        false
    }
}

pub fn parse(text: &str, resolver: &dyn MarkdownResolver) -> MarkdownModel {
    let lines = parse_lines(text);
    let mut spans = Vec::new();
    for line in &lines {
        match line.kind {
            LineKind::Paragraph | LineKind::Heading | LineKind::ListItem => {
                parse_inline(text, line.content_start, line.end, resolver, &mut spans);
            }
            LineKind::TableRow => {
                let mut cell_start = line.content_start;
                for pipe in table_pipes(text, line.content_start, line.end) {
                    parse_inline(text, cell_start, pipe, resolver, &mut spans);
                    cell_start = pipe + 1;
                }
                parse_inline(text, cell_start, line.end, resolver, &mut spans);
            }
            _ => {}
        }
    }
    spans.sort_by_key(|s| s.start);
    MarkdownModel { lines, spans }
}

/// 提及（用户与用户组）的区间，供 [`mention::mask`] 遮盖。
pub fn mention_spans(model: &MarkdownModel, text: &str) -> Vec<mention::MentionSpan> {
    model
        .spans
        .iter()
        .filter(|s| {
            matches!(s.kind, SpanKind::UserMention | SpanKind::GroupMention) && s.end <= text.len()
        })
        .map(|s| mention::MentionSpan {
            start: s.start,
            len: s.end - s.start,
            markdown: text[s.start..s.end].to_owned(),
        })
        .collect()
}

// ───────────── 块 ─────────────

fn parse_lines(text: &str) -> Vec<Line> {
    let mut raw = Vec::new();
    let mut pos = 0;
    for (nl, _) in text.match_indices('\n') {
        raw.push((pos, nl));
        pos = nl + 1;
    }
    raw.push((pos, text.len()));

    let mut result = Vec::with_capacity(raw.len());
    let mut fences: Vec<(String, FenceKind)> = Vec::new();
    let mut state = BlockState::default();

    for (i, &(start, end)) in raw.iter().enumerate() {
        let line = &text[start..end];
        let quotes = fences.iter().filter(|f| f.1 == FenceKind::Quote).count();
        let top = fences.last().map(|f| (f.0.as_str(), f.1));
        let closes = top.is_some_and(|(fence, _)| line.trim_end() == fence);
        let in_literal =
            top.is_some_and(|(_, kind)| matches!(kind, FenceKind::Code | FenceKind::Math));

        let md = if closes || in_literal {
            let fence = top.map(|(_, kind)| kind);
            let kind = if closes {
                LineKind::FenceClose
            } else {
                LineKind::FenceBody
            };
            if closes {
                fences.pop();
            }
            state.in_table = false;
            Line {
                fence,
                quote_depth: quotes,
                ..Line::new(start, end, kind)
            }
        } else if let Some(m) = FENCE.captures(line) {
            let lang = m.name("lang").map_or("", |g| g.as_str());
            let header = m.name("header").map_or("", |g| g.as_str().trim());
            let (kind, info) = match lang.to_lowercase().as_str() {
                "quote" | "quoted" => (FenceKind::Quote, None),
                "spoiler" => (FenceKind::Spoiler, Some(header.to_owned())),
                "math" => (FenceKind::Math, None),
                _ => (FenceKind::Code, Some(lang.to_owned())),
            };
            fences.push((m["fence"].to_owned(), kind));
            state.in_table = false;
            Line {
                fence: Some(kind),
                fence_info: info,
                quote_depth: quotes + usize::from(kind == FenceKind::Quote),
                ..Line::new(start, end, LineKind::FenceOpen)
            }
        } else {
            let mut md = classify_line(text, &raw, i, &mut state);
            md.quote_depth += quotes;
            md.fence = fences.last().map(|f| f.1);
            md
        };

        state.prev_kind = Some(md.kind);
        if md.kind != LineKind::Blank {
            state.last_non_blank = Some(md.kind);
        }
        result.push(md);
    }
    result
}

#[derive(Default)]
struct BlockState {
    prev_kind: Option<LineKind>,
    last_non_blank: Option<LineKind>,
    table_separator_at: Option<usize>,
    in_table: bool,
}

/// 一个引用前缀 ` {0,3}> ?` 的长度。
fn quote_prefix_len(s: &str) -> Option<usize> {
    let spaces = s.bytes().take_while(|b| *b == b' ').count();
    if spaces > 3 || s.as_bytes().get(spaces) != Some(&b'>') {
        return None;
    }
    Some(spaces + 1 + usize::from(s.as_bytes().get(spaces + 1) == Some(&b' ')))
}

// 去掉所有 > 和空格后为空的行不是引用
fn quote_prefix_lengths(line: &str) -> Vec<usize> {
    let mut lengths = Vec::new();
    if !line.chars().any(|c| c != '>' && c != ' ') {
        return lengths;
    }
    let mut rest = 0;
    while let Some(len) = quote_prefix_len(&line[rest..]) {
        lengths.push(len);
        rest += len;
    }
    lengths
}

// 不在代码 / 公式围栏中的一行（可能在引用 / 剧透围栏中）
fn classify_line(text: &str, raw: &[(usize, usize)], index: usize, state: &mut BlockState) -> Line {
    let (start, end) = raw[index];
    let line = &text[start..end];
    if is_blank(line) {
        state.in_table = false;
        return Line::new(start, end, LineKind::Blank);
    }

    let mut prefixes = Vec::new();
    let mut rest = 0;
    for len in quote_prefix_lengths(line) {
        prefixes.push(Prefix {
            kind: PrefixKind::Quote,
            start: start + rest,
            end: start + rest + len,
        });
        rest += len;
    }
    let depth = prefixes.len();
    let body = &line[rest..];
    let make = |kind: LineKind, prefixes: Vec<Prefix>| Line {
        content_start: prefixes.last().map_or(start, |p| p.end),
        prefixes,
        quote_depth: depth,
        ..Line::new(start, end, kind)
    };

    if state.table_separator_at == Some(index) {
        state.table_separator_at = None;
        return make(LineKind::TableSeparator, prefixes);
    }
    if state.in_table {
        if body.contains('|') {
            return make(LineKind::TableRow, prefixes);
        }
        state.in_table = false;
    }

    if matches!(
        state.prev_kind,
        None | Some(LineKind::Blank | LineKind::IndentedCode)
    ) && (body.starts_with("    ") || body.starts_with('\t'))
        && (state.prev_kind == Some(LineKind::IndentedCode)
            || state.last_non_blank != Some(LineKind::ListItem))
    {
        return make(LineKind::IndentedCode, prefixes);
    }

    if let Some(m) = HEADING.captures(body) {
        let len = m.get(0).map_or(0, |g| g.len());
        prefixes.push(Prefix {
            kind: PrefixKind::Heading,
            start: start + rest,
            end: start + rest + len,
        });
        return Line {
            heading_level: m["hashes"].len() as u8,
            ..make(LineKind::Heading, prefixes)
        };
    }

    if RULE.is_match(body) {
        return make(LineKind::Rule, prefixes);
    }

    if let Some(m) = LIST.captures(body)
        && let (Some(marker), Some(space)) = (m.name("marker"), m.name("space"))
    {
        let ordered = marker.as_str().starts_with(|c: char| c.is_ascii_digit());
        prefixes.push(Prefix {
            kind: if ordered {
                PrefixKind::Ordered
            } else {
                PrefixKind::Bullet
            },
            start: start + rest + marker.start(),
            end: start + rest + space.end(),
        });
        return make(LineKind::ListItem, prefixes);
    }

    if body.contains('|')
        && let Some(&(next_start, next_end)) = raw.get(index + 1)
    {
        let next = &text[next_start..next_end];
        let next_body = &next[quote_prefix_lengths(next).iter().sum::<usize>()..];
        if TABLE_SEPARATOR.is_match(next_body) && next_body.contains('|') {
            state.table_separator_at = Some(index + 1);
            state.in_table = true;
            return make(LineKind::TableRow, prefixes);
        }
    }

    make(LineKind::Paragraph, prefixes)
}

/// 表格行 `[start, end)` 中分隔单元格的 | 的位置；行内代码中的 | 和 `\|` 不算。
pub fn table_pipes(text: &str, start: usize, end: usize) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut pipes = Vec::new();
    let mut i = start;
    while i < end {
        match bytes[i] {
            b'`' => {
                let run = bytes[i..end].iter().take_while(|b| **b == b'`').count();
                i += run + find_tick_run(&bytes[i + run..end], run).map_or(0, |close| close + run);
            }
            // \| 是单元格中的 | 字符
            b'\\' if bytes.get(i + 1).filter(|_| i + 1 < end) == Some(&b'|') => i += 2,
            b'|' => {
                pipes.push(i);
                i += 1;
            }
            _ => i += 1,
        }
    }
    pipes
}

fn find_tick_run(bytes: &[u8], run: usize) -> Option<usize> {
    let mut j = 0;
    while j < bytes.len() {
        if bytes[j] != b'`' {
            j += 1;
            continue;
        }
        let len = bytes[j..].iter().take_while(|b| **b == b'`').count();
        if len == run {
            return Some(j);
        }
        j += len;
    }
    None
}

// ───────────── 行内 ─────────────

struct InlineParser<'a> {
    /// 行内文字；已处理的元素被 [`MASKED`] 遮盖，字节偏移与原文相同。
    masked: String,
    offset: usize,
    spans: &'a mut Vec<Span>,
}

type Captures<'t> = fancy_regex::Captures<'t>;

impl InlineParser<'_> {
    // 对当前的遮盖文字逐个匹配；被拒绝的匹配从下一个字符继续找（与服务器一致：拒绝后文字留给其它模式）
    fn run(
        &mut self,
        pattern: &fancy_regex::Regex,
        atomic: bool,
        build: impl Fn(&Captures<'_>, &str) -> Option<Span>,
    ) {
        let mut pos = 0;
        while pos <= self.masked.len() {
            let text = self.masked.clone();
            let Ok(Some(m)) = pattern.captures_from_pos(&text, pos) else {
                return;
            };
            let Some(whole) = m.get(0) else {
                return;
            };
            let Some(span) = build(&m, &text) else {
                pos = whole.start() + char_after(&text, whole.start()).map_or(1, char::len_utf8);
                continue;
            };
            if atomic {
                self.mask(span.start, span.end);
            } else {
                self.mask(span.start, span.content_start);
                self.mask(span.content_end, span.end);
            }
            pos = span.end;
            self.push(span);
        }
    }

    fn mask(&mut self, start: usize, end: usize) {
        self.masked
            .replace_range(start..end, &MASKED.repeat(end - start));
    }

    fn push(&mut self, span: Span) {
        let by = self.offset;
        self.spans.push(Span {
            start: span.start + by,
            end: span.end + by,
            content_start: span.content_start + by,
            content_end: span.content_end + by,
            ..span
        });
    }
}

fn group_range(m: &Captures<'_>, name: &str) -> (usize, usize) {
    m.name(name).map_or((0, 0), |g| (g.start(), g.end()))
}

fn group_str<'t>(m: &Captures<'t>, name: &str) -> &'t str {
    m.name(name).map_or("", |g| g.as_str())
}

fn whole(kind: SpanKind, m: &Captures<'_>, data: Option<String>) -> Option<Span> {
    let g = m.get(0)?;
    Some(Span {
        kind,
        start: g.start(),
        end: g.end(),
        content_start: g.start(),
        content_end: g.end(),
        data,
    })
}

fn with_body(kind: SpanKind, m: &Captures<'_>) -> Option<Span> {
    let g = m.get(0)?;
    let (content_start, content_end) = group_range(m, "body");
    Some(Span {
        kind,
        start: g.start(),
        end: g.end(),
        content_start,
        content_end,
        data: None,
    })
}

fn emphasis(kind: SpanKind, m: &Captures<'_>, marker: usize) -> Option<Span> {
    let g = m.get(0)?;
    Some(Span {
        kind,
        start: g.start(),
        end: g.end(),
        content_start: g.start() + marker,
        content_end: g.end() - marker,
        data: None,
    })
}

fn split_id(name: &str) -> (&str, Option<i64>) {
    if let Some((plain, id)) = name.rsplit_once('|')
        && !plain.is_empty()
        && let Some(id) = parse_digits(id)
    {
        return (plain, Some(id));
    }
    (name, None)
}

// 去掉网址结尾的标点与不配对的右括号
fn trim_url(mut url: &str) -> &str {
    while let Some(last) = url.chars().next_back() {
        let unmatched = last == ')' && url.matches('(').count() < url.matches(')').count();
        if ".,;:!?'\"".contains(last) || unmatched {
            url = &url[..url.len() - last.len_utf8()];
        } else {
            break;
        }
    }
    url
}

/// 按 Python-Markdown 的行内处理实现：按优先级从高到低逐个模式匹配。原子元素整个遮盖，
/// 加粗等只遮盖两侧符号，内容留给优先级更低的模式（服务器处理子节点时从下一个模式开始）。
fn parse_inline(
    text: &str,
    start: usize,
    end: usize,
    resolver: &dyn MarkdownResolver,
    spans: &mut Vec<Span>,
) {
    if end <= start {
        return;
    }
    let mut p = InlineParser {
        masked: text[start..end].to_owned(),
        offset: start,
        spans,
    };
    let channel = |m: &Captures<'_>, _: &str| {
        let stream = group_str(m, "stream");
        if !resolver.has_channel(stream) {
            return None;
        }
        let data = match m.name("topic") {
            Some(topic) => format!("#{stream} > {}", topic.as_str()),
            None => format!("#{stream}"),
        };
        whole(SpanKind::ChannelLink, m, Some(data))
    };

    p.run(&CODE, true, |m, _| with_body(SpanKind::Code, m));
    p.run(&STRONG_EM, false, |m, _| emphasis(SpanKind::StrongEm, m, 3));
    p.run(&USER_MENTION, true, |m, _| {
        let name = group_str(m, "name");
        if !WILDCARDS.iter().any(|w| w.eq_ignore_ascii_case(name)) {
            let (plain, id) = split_id(name);
            if !resolver.has_user(plain, id) {
                return None;
            }
        }
        let silent = !group_str(m, "silent").is_empty();
        whole(
            SpanKind::UserMention,
            m,
            Some(mention::display(name, silent, false)),
        )
    });
    p.run(&MATH, true, |m, s| {
        let g = m.get(0)?;
        let adjacent = char_before(s, g.start()).is_some_and(char::is_alphanumeric)
            || char_after(s, g.end()).is_some_and(char::is_alphanumeric);
        if adjacent {
            None
        } else {
            with_body(SpanKind::Math, m)
        }
    });
    p.run(&CHANNEL_MESSAGE, true, channel);
    p.run(&CHANNEL_TOPIC, true, channel);
    p.run(&CHANNEL, true, channel);
    p.run(&TIME, true, |m, _| {
        let value = group_str(m, "time");
        time::parse_datetime(value)?;
        whole(SpanKind::Time, m, Some(value.to_owned()))
    });
    p.run(&GROUP_MENTION, true, |m, _| {
        let name = group_str(m, "name");
        if !resolver.has_group(name) {
            return None;
        }
        let silent = !group_str(m, "silent").is_empty();
        whole(
            SpanKind::GroupMention,
            m,
            Some(mention::display(name, silent, true)),
        )
    });
    scan_links(&mut p);
    p.run(&URL, true, |m, _| {
        let group = m.name("url")?;
        let url = trim_url(group.as_str());
        (!url.is_empty()).then(|| Span {
            kind: SpanKind::Url,
            start: group.start(),
            end: group.start() + url.len(),
            content_start: group.start(),
            content_end: group.start() + url.len(),
            data: Some(url.to_owned()),
        })
    });
    p.run(&STRONG, false, |m, _| emphasis(SpanKind::Strong, m, 2));
    p.run(&EM, false, |m, _| emphasis(SpanKind::Em, m, 1));
    p.run(&DEL, false, |m, _| emphasis(SpanKind::Del, m, 2));
    p.run(&EMOJI, true, |m, _| {
        let name = group_str(m, "name");
        if resolver.has_emoji(name) {
            whole(SpanKind::Emoji, m, Some(name.to_owned()))
        } else {
            None
        }
    });
}

/// 链接 `[文字](地址)` 与图片 `![文字](地址)`：方括号可以嵌套，地址可以用 &lt;&gt; 包围或含配对的圆括号。
/// 上传的图片为 UploadImage，其它上传文件（视频等，有文字且不是 `![…]`）为 UploadFile，
/// `[Uploading 名字…]()` 为 UploadPlaceholder。链接文字中的其它格式不识别。
fn scan_links(p: &mut InlineParser<'_>) {
    let mut i = 0;
    while i < p.masked.len() {
        if p.masked.as_bytes()[i] != b'[' {
            i += 1;
            continue;
        }
        let Some((close, target_start, target_end, end)) = link_bounds(p.masked.as_bytes(), i)
        else {
            i += 1;
            continue;
        };

        let span = {
            let masked = p.masked.as_str();
            let mut target = masked[target_start..target_end].trim();
            if target.starts_with('<') {
                target = target.trim_matches(['<', '>']);
            }
            let address = target.split([' ', '\t']).next().unwrap_or("");
            let label = &masked[i + 1..close];
            let is_image = i > 0 && masked.as_bytes()[i - 1] == b'!';
            let start = if is_image { i - 1 } else { i };
            let upload = html::normalize_upload_path(address);
            let blank_label = is_blank(label);
            let placeholder = label
                .strip_prefix("Uploading ")
                .and_then(|l| l.strip_suffix('…'));

            let (kind, content, data) =
                if let (true, Some(name)) = (address.is_empty(), placeholder) {
                    (SpanKind::UploadPlaceholder, (start, end), name)
                } else if upload.as_deref().is_some_and(html::is_image_path) {
                    (SpanKind::UploadImage, (start, end), address)
                } else if !is_image && !blank_label && upload.is_some() {
                    (SpanKind::UploadFile, (i + 1, close), address)
                } else if blank_label {
                    (SpanKind::Link, (i + 1, i + 1), address)
                } else {
                    (SpanKind::Link, (i + 1, close), address)
                };
            Span {
                kind,
                start,
                end,
                content_start: content.0,
                content_end: content.1,
                data: Some(data.to_owned()),
            }
        };
        p.mask(span.start, span.end);
        p.push(span);
        i = end;
    }
}

/// 从 `[` 开始的链接的边界：`]` 的位置、地址的区间、整个元素的结尾。
fn link_bounds(bytes: &[u8], open: usize) -> Option<(usize, usize, usize, usize)> {
    let mut depth = 0;
    let close = (open..bytes.len()).find(|&j| {
        match bytes[j] {
            b'[' => depth += 1,
            b']' => depth -= 1,
            _ => return false,
        }
        depth == 0
    })?;
    if bytes.get(close + 1) != Some(&b'(') {
        return None;
    }
    let target_start = close + 2;
    if bytes.get(target_start) == Some(&b'<') {
        let gt = target_start + bytes[target_start..].iter().position(|b| *b == b'>')?;
        if bytes.get(gt + 1) != Some(&b')') {
            return None;
        }
        return Some((close, target_start, gt + 1, gt + 2));
    }
    let mut paren = 1;
    let target_end = (target_start..bytes.len()).find(|&j| {
        match bytes[j] {
            b'(' => paren += 1,
            b')' => paren -= 1,
            _ => return false,
        }
        paren == 0
    })?;
    Some((close, target_start, target_end, target_end + 1))
}

// ───────────── 正则（来自 zerver/lib/markdown） ─────────────

static_regex!(
    FENCE = r"^(?<fence>~{3,}|`{3,})[ ]*(?:\{?\.?(?<lang>[a-zA-Z0-9_+\-./#]+)[ ]*(?<header>[^ ~`][^~`]*)?\}?)?$"
);
static_regex!(HEADING = r"^(?<hashes>#{1,6})[ \t]");
// 只有符号和空格的行也是列表项（换行续写出的空项要显示成圆点）
static_regex!(LIST = r"^(?<indent> *)(?<marker>[*+\-]|\d+\.)(?<space> +)(?:\S|$)");
static_regex!(RULE = r"^ {0,3}(?:(?:-[ ]*){3,}|(?:\*[ ]*){3,}|(?:_[ ]*){3,})$");
static_regex!(TABLE_SEPARATOR = r"^ *\|? *:?-+:? *(?:\| *:?-+:? *)+\|? *$");

static_fancy_regex!(CODE = r"(?<!`)(?<tick>`+)(?<body>.+?)(?<!`)\k<tick>(?!`)");
static_fancy_regex!(STRONG_EM = r"(\*\*\*)(?!\s)([^\*^\n]+)(?<!\s)\*\*\*");
static_fancy_regex!(USER_MENTION = r#"(?<![^\s'"\(\{\[/<])@(?<silent>_?)\*\*(?<name>[^\*]+)\*\*"#);
static_fancy_regex!(MATH = r"(?<!\$)\$\$(?<body>[^\n_$](?:\\\$|[^$\n])*)\$\$(?!\$)");
static_fancy_regex!(CHANNEL_MESSAGE = r"#\*\*(?<stream>[^\*>]+)>(?<topic>[^\*]*)@(?<msg>\d+)\*\*");
static_fancy_regex!(CHANNEL_TOPIC = r"#\*\*(?<stream>[^\*>]+)>(?<topic>[^\*]*)\*\*");
static_fancy_regex!(CHANNEL = r"#\*\*(?<stream>[^\*]+)\*\*");
static_fancy_regex!(TIME = r"<time:(?<time>[^>]*?)>");
static_fancy_regex!(GROUP_MENTION = r#"(?<![^\s'"\(\{\[/<])@(?<silent>_?)\*(?<name>[^\*]+)\*"#);
static_fancy_regex!(
    URL = r#"(?<![^\s'"(,:<\x{80}-\x{10FFFF}])(?<url>(?:https?|ftp)://[^\s<>\x02]+)"#
);
static_fancy_regex!(STRONG = r"(\*\*)([^\n]+?)\*\*");
static_fancy_regex!(EM = r"(\*)(?!\s)([^\*^\n]+)(?<!\s)\*");
static_fancy_regex!(DEL = r"(?<!~)(~~)([^~\n]+?)(~~)(?!~)");
static_fancy_regex!(EMOJI = r":(?<name>[\w\-\+]+):");

#[cfg(test)]
pub(crate) mod test_support {
    use super::MarkdownResolver;

    pub struct FakeResolver;

    impl MarkdownResolver for FakeResolver {
        fn has_user(&self, name: &str, id: Option<i64>) -> bool {
            match id {
                None => name == "甲",
                Some(id) => id == 1,
            }
        }
        fn has_group(&self, name: &str) -> bool {
            name == "开发"
        }
        fn has_channel(&self, name: &str) -> bool {
            name == "综合"
        }
        fn has_emoji(&self, name: &str) -> bool {
            name == "smile"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeResolver;
    use super::*;

    fn p(text: &str) -> MarkdownModel {
        parse(text, &FakeResolver)
    }

    fn kinds(text: &str) -> String {
        p(text)
            .spans
            .iter()
            .map(|s| format!("{:?}:{}", s.kind, &text[s.start..s.end]))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn single(text: &str) -> Span {
        let model = p(text);
        assert_eq!(model.spans.len(), 1, "{text}");
        model.spans[0].clone()
    }

    fn content<'a>(text: &'a str, s: &Span) -> &'a str {
        &text[s.content_start..s.content_end]
    }

    fn line_kinds(model: &MarkdownModel) -> Vec<LineKind> {
        model.lines.iter().map(|l| l.kind).collect()
    }

    #[test]
    fn inline_emphasis() {
        for (text, expected) in [
            ("**粗**", "Strong:**粗**"),
            ("*斜*", "Em:*斜*"),
            ("***粗斜***", "StrongEm:***粗斜***"),
            ("~~删~~", "Del:~~删~~"),
            ("`代码`", "Code:`代码`"),
            ("_x_", ""),
            ("__x__", ""),
            ("**a\nb**", ""),
            ("const char *x = (char *)y", ""),
            ("`a **b** c`", "Code:`a **b** c`"),
            ("`@**甲|1**`", "Code:`@**甲|1**`"),
            ("```\n@**甲|1**\n```", ""),
            ("[a **b**](https://a.com)", "Link:[a **b**](https://a.com)"),
        ] {
            assert_eq!(kinds(text), expected, "{text}");
        }
    }

    #[test]
    fn inline_em_contains_strong() {
        let text = "*a **b** c*";
        let spans = p(text).spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].kind, SpanKind::Em);
        assert_eq!((spans[0].start, spans[0].end), (0, 11));
        assert_eq!(spans[1].kind, SpanKind::Strong);
        assert_eq!(content(text, &spans[1]), "b");
    }

    #[test]
    fn inline_code_content() {
        let text = "x ``a`b`` y";
        assert_eq!(content(text, &single(text)), "a`b");
    }

    #[test]
    fn inline_atomic() {
        for (text, kind, data) in [
            ("@**甲|1**", SpanKind::UserMention, "@甲"),
            ("@**甲**", SpanKind::UserMention, "@甲"),
            ("@_**甲|1**", SpanKind::UserMention, "甲"),
            ("@**all**", SpanKind::UserMention, "@全体成员"),
            ("@**topic**", SpanKind::UserMention, "@话题参与者"),
            ("@*开发*", SpanKind::GroupMention, "@开发"),
            ("#**综合**", SpanKind::ChannelLink, "#综合"),
            ("#**综合>话题**", SpanKind::ChannelLink, "#综合 > 话题"),
            ("#**综合>话题@12**", SpanKind::ChannelLink, "#综合 > 话题"),
            (":smile:", SpanKind::Emoji, "smile"),
            (
                "<time:2026-10-24T15:00:00+08:00>",
                SpanKind::Time,
                "2026-10-24T15:00:00+08:00",
            ),
        ] {
            let span = single(text);
            assert_eq!(span.kind, kind, "{text}");
            assert_eq!((span.start, span.end), (0, text.len()));
            assert_eq!(span.data.as_deref(), Some(data));
            assert!(kind.is_atomic());
        }
    }

    #[test]
    fn inline_unknown_user_is_strong() {
        let text = "@**乙**";
        let span = single(text);
        assert_eq!(span.kind, SpanKind::Strong);
        assert_eq!(content(text, &span), "乙");
    }

    #[test]
    fn inline_not_atomic() {
        for text in [
            "a@**甲|1**",
            "#**未知**",
            "$$_$$",
            "<time:abc>",
            ":unknown:",
        ] {
            assert!(!p(text).spans.iter().any(|s| s.kind.is_atomic()), "{text}");
        }
    }

    #[test]
    fn inline_math() {
        let text = "x $$x^2$$ y";
        let span = single(text);
        assert_eq!(span.kind, SpanKind::Math);
        assert_eq!(content(text, &span), "x^2");
    }

    #[test]
    fn inline_link() {
        let text = "看 [文字](https://a.com/x) 吧";
        let span = single(text);
        assert_eq!(span.kind, SpanKind::Link);
        assert_eq!(&text[span.start..span.end], "[文字](https://a.com/x)");
        assert_eq!(content(text, &span), "文字");
        assert_eq!(span.data.as_deref(), Some("https://a.com/x"));
    }

    #[test]
    fn inline_link_nested_brackets() {
        let text = "[a [b] c](u)";
        let span = single(text);
        assert_eq!(content(text, &span), "a [b] c");
        assert_eq!(span.data.as_deref(), Some("u"));
    }

    #[test]
    fn inline_link_empty_label() {
        let span = single("[](https://a.com)");
        assert_eq!(span.kind, SpanKind::Link);
        assert_eq!(span.content_start, span.content_end);
        assert_eq!(span.data.as_deref(), Some("https://a.com"));
    }

    #[test]
    fn inline_upload_image() {
        for text in [
            "![图](/user_uploads/1/ab/x.png)",
            "[图](/user_uploads/1/ab/x.png)",
        ] {
            let span = single(text);
            assert_eq!(span.kind, SpanKind::UploadImage);
            assert_eq!((span.start, span.end), (0, text.len()));
            assert_eq!(span.data.as_deref(), Some("/user_uploads/1/ab/x.png"));
        }
    }

    #[test]
    fn inline_upload_file() {
        for (text, address, label) in [
            (
                "[a.zip](/user_uploads/1/ab/a.zip)",
                "/user_uploads/1/ab/a.zip",
                "a.zip",
            ),
            (
                "[a.mp4](/user_uploads/1/ab/a.mp4)",
                "/user_uploads/1/ab/a.mp4",
                "a.mp4",
            ),
        ] {
            let span = single(text);
            assert_eq!(span.kind, SpanKind::UploadFile);
            assert_eq!((span.start, span.end), (0, text.len()));
            assert_eq!(span.data.as_deref(), Some(address));
            assert_eq!(content(text, &span), label);
        }
    }

    #[test]
    fn inline_upload_file_without_label_is_link() {
        assert_eq!(single("[](/user_uploads/1/ab/a.zip)").kind, SpanKind::Link);
    }

    #[test]
    fn pipes_in_code_or_escaped_are_not_separators() {
        let text = "| `a|b` | c |";
        assert_eq!(table_pipes(text, 0, text.len()), [0, 8, 12]);
        let text = "| c\\|d | e |";
        assert_eq!(table_pipes(text, 0, text.len()), [0, 7, 11]);
    }

    #[test]
    fn link_markdown_keeps_the_link_valid_with_brackets() {
        assert_eq!(link_markdown("a.txt", "/u/a.txt"), "[a.txt](/u/a.txt)");
        for name in ["[a].txt", "a].txt", "[a.txt", "]a[.txt"] {
            let text = link_markdown(name, "/user_uploads/1/ab/a.txt");
            assert_eq!(text, "[a.txt](/user_uploads/1/ab/a.txt)");
            assert_eq!(single(&text).kind, SpanKind::UploadFile, "{text}");
        }
    }

    #[test]
    fn inline_upload_placeholder() {
        let span = single("x [Uploading 截图.png…]() y");
        assert_eq!(span.kind, SpanKind::UploadPlaceholder);
        assert_eq!(span.data.as_deref(), Some("截图.png"));
    }

    #[test]
    fn inline_url() {
        for (text, url) in [
            ("看 https://a.com/x).", "https://a.com/x"),
            ("(https://a.com/x(1))", "https://a.com/x(1)"),
            ("中文https://a.com/x 后", "https://a.com/x"),
        ] {
            let span = single(text);
            assert_eq!(span.kind, SpanKind::Url);
            assert_eq!(span.data.as_deref(), Some(url));
            assert_eq!(&text[span.start..span.end], url);
        }
    }

    #[test]
    fn inline_url_inside_link_is_not_url() {
        assert_eq!(
            single("[https://a.com](https://a.com)").kind,
            SpanKind::Link
        );
    }

    #[test]
    fn inline_second_line_offsets() {
        let text = "第一行\n**粗**";
        let span = single(text);
        assert_eq!(&text[span.start..span.end], "**粗**");
        assert_eq!(span.end, text.len());
        assert_eq!(content(text, &span), "粗");
    }

    #[test]
    fn mention_spans_only_mentions() {
        let text = "@**甲|1** **x** @*开发*";
        let spans = mention_spans(&p(text), text);
        let markdown: Vec<&str> = spans.iter().map(|s| s.markdown.as_str()).collect();
        assert_eq!(markdown, ["@**甲|1**", "@*开发*"]);
    }

    #[test]
    fn block_heading() {
        let line = &p("## 标题").lines[0];
        assert_eq!(line.kind, LineKind::Heading);
        assert_eq!(line.heading_level, 2);
        assert_eq!(
            line.prefixes,
            [Prefix {
                kind: PrefixKind::Heading,
                start: 0,
                end: 3
            }]
        );
        assert_eq!(line.content_start, 3);
        assert_eq!(p("#标题").lines[0].kind, LineKind::Paragraph);
    }

    #[test]
    fn block_quote() {
        let model = p("> 引用");
        let line = &model.lines[0];
        assert_eq!(line.kind, LineKind::Paragraph);
        assert_eq!(line.quote_depth, 1);
        assert_eq!(line.content_start, 2);

        let model = p("> > 两层");
        let two = &model.lines[0];
        assert_eq!(two.quote_depth, 2);
        assert_eq!(two.prefixes.len(), 2);
        assert_eq!(two.content_start, 4);

        let model = p(">");
        assert_eq!(model.lines[0].quote_depth, 0);
        assert!(model.lines[0].prefixes.is_empty());
    }

    #[test]
    fn block_list() {
        for (text, kind, start, end) in [
            ("- a", PrefixKind::Bullet, 0, 2),
            ("* a", PrefixKind::Bullet, 0, 2),
            ("  1. a", PrefixKind::Ordered, 2, 5),
            // 空的列表项
            ("- ", PrefixKind::Bullet, 0, 2),
            ("2. ", PrefixKind::Ordered, 0, 3),
        ] {
            let model = p(text);
            assert_eq!(model.lines[0].kind, LineKind::ListItem);
            assert_eq!(model.lines[0].prefixes, [Prefix { kind, start, end }]);
        }
    }

    #[test]
    fn block_rule() {
        assert_eq!(p("---").lines[0].kind, LineKind::Rule);
        assert_eq!(p("* * *").lines[0].kind, LineKind::Rule);
    }

    #[test]
    fn block_code_fence() {
        let model = p("```python\n**x**\n```\n**y**");
        assert_eq!(
            line_kinds(&model),
            [
                LineKind::FenceOpen,
                LineKind::FenceBody,
                LineKind::FenceClose,
                LineKind::Paragraph
            ]
        );
        assert_eq!(model.lines[0].fence_info.as_deref(), Some("python"));
        assert_eq!(model.lines[1].fence, Some(FenceKind::Code));
        // 只有围栏外的 **y** 是加粗
        assert_eq!(model.spans.len(), 1);
        assert_eq!(model.spans[0].start, 20);
    }

    #[test]
    fn block_quote_fence() {
        let model = p("```quote\n**x**\n```");
        assert_eq!(model.lines[0].fence, Some(FenceKind::Quote));
        assert_eq!(model.lines[1].kind, LineKind::Paragraph);
        assert_eq!(model.lines[1].quote_depth, 1);
        assert_eq!(model.lines[1].fence, Some(FenceKind::Quote));
        assert_eq!(model.spans.len(), 1);
        assert_eq!(model.spans[0].kind, SpanKind::Strong);
        assert_eq!(model.lines[2].kind, LineKind::FenceClose);
    }

    #[test]
    fn block_spoiler_fence() {
        let model = p("```spoiler 标题\n内容\n```");
        assert_eq!(model.lines[0].fence, Some(FenceKind::Spoiler));
        assert_eq!(model.lines[0].fence_info.as_deref(), Some("标题"));
    }

    #[test]
    fn block_unclosed_fence() {
        assert_eq!(
            line_kinds(&p("```\na\nb")),
            [
                LineKind::FenceOpen,
                LineKind::FenceBody,
                LineKind::FenceBody
            ]
        );
    }

    #[test]
    fn block_indented_code() {
        assert_eq!(p("a\n\n    code").lines[2].kind, LineKind::IndentedCode);
        assert_eq!(p("    code").lines[0].kind, LineKind::IndentedCode);
        assert_ne!(p("- a\n\n    b").lines[2].kind, LineKind::IndentedCode);
        assert_ne!(p("a\n    b").lines[1].kind, LineKind::IndentedCode);
    }

    #[test]
    fn block_table() {
        let model = p("a | `x|y`\n--- | ---\n**1** | 2\n\nc");
        assert_eq!(
            line_kinds(&model),
            [
                LineKind::TableRow,
                LineKind::TableSeparator,
                LineKind::TableRow,
                LineKind::Blank,
                LineKind::Paragraph
            ]
        );
        assert!(model.spans.iter().any(|s| s.kind == SpanKind::Code));
        assert!(model.spans.iter().any(|s| s.kind == SpanKind::Strong));
    }

    #[test]
    fn line_index_at() {
        let model = p("ab\ncd\n");
        assert_eq!(model.line_index_at(0), Some(0));
        assert_eq!(model.line_index_at(2), Some(0));
        assert_eq!(model.line_index_at(3), Some(1));
        assert_eq!(model.line_index_at(6), Some(2));
        assert_eq!(MarkdownModel::default().line_index_at(0), None);
    }
}
