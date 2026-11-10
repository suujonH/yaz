//! Markdown -> editing area HTML. Invariant: the text of each line (`data-src` counts as its
//! source) joined together is that line's Markdown, so JS can read the DOM back into source without
//! understanding the syntax. Markers are hidden with `.zm`; `data-s` and `data-e` of `.zr` are the
//! range of markers shown when the cursor enters.

use super::language::label as language_label;
use crate::ui::{IconName, icon_html};
use crate::widgets::file_icon;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use zulip_core::emoji::EmojiCatalog;
use zulip_core::html::{encode, encode_into};
use zulip_core::markdown::quote::QuoteBlock;
use zulip_core::markdown::table::{MarkdownTable, TableAlign, TableEditor};
use zulip_core::markdown::{
    FenceKind, Line, LineKind, MarkdownModel, MarkdownResolver, PrefixKind, Span, SpanKind,
};
use zulip_core::text::time;

/// Decide whether mentions, channels and emoji exist, and provide the data for display. A snapshot
/// from the conversation.
#[derive(Clone, Default)]
pub struct Lookup {
    pub users: HashMap<i64, String>,
    pub user_names: HashSet<String>,
    pub groups: HashSet<String>,
    pub channels: HashSet<String>,
    pub emoji: Arc<EmojiCatalog>,
    /// Remote image -> local address.
    pub images: HashMap<String, String>,
}

impl MarkdownResolver for Lookup {
    fn has_user(&self, name: &str, id: Option<i64>) -> bool {
        let name = name.to_lowercase();
        match id {
            Some(id) => self
                .users
                .get(&id)
                .is_some_and(|n| n.to_lowercase() == name),
            None => self.user_names.contains(&name),
        }
    }

    fn has_group(&self, name: &str) -> bool {
        self.groups.contains(&name.to_lowercase())
    }

    fn has_channel(&self, name: &str) -> bool {
        self.channels.contains(&name.to_lowercase())
    }

    fn has_emoji(&self, name: &str) -> bool {
        self.emoji.get(&name.to_lowercase()).is_some()
    }
}

/// One block of the editing area; JS uses `key` to decide whether to update.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Block {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<TableBlock>,
}

impl Block {
    pub fn line(html: String) -> Self {
        Self {
            key: html.clone(),
            html: Some(html),
            table: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TableBlock {
    pub index: usize,
    pub src: String,
    pub aligns: Vec<&'static str>,
    /// Row -> column -> rows inside a cell.
    pub rows: Vec<Vec<Vec<Block>>>,
}

pub struct RenderCtx<'a> {
    pub text: &'a str,
    pub model: &'a MarkdownModel,
    pub lookup: &'a Lookup,
    pub source: bool,
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Only code, links, bold and italic show their markers when the cursor enters.
fn reveals(kind: SpanKind) -> bool {
    matches!(
        kind,
        SpanKind::Code
            | SpanKind::Link
            | SpanKind::Url
            | SpanKind::Strong
            | SpanKind::Em
            | SpanKind::StrongEm
    )
}

fn span_class(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::Code => "zk",
        SpanKind::StrongEm => "zB zI",
        SpanKind::Strong => "zB",
        SpanKind::Em => "zI",
        SpanKind::Del => "zD",
        SpanKind::Math => "zX",
        SpanKind::Link | SpanKind::Url => "zu",
        _ => "",
    }
}

impl RenderCtx<'_> {
    /// HTML of line `index`. `extra` is an extra class.
    pub fn line_html(&self, index: usize, extra: &str) -> String {
        let line = &self.model.lines[index];
        let mut class = String::from("zl");
        let mut attrs = String::new();
        let len = utf16_len(&self.text[line.start..line.end]);
        let unclosed = self.in_unclosed_fence(index);
        let mut body = if unclosed {
            self.unclosed_fence_body(line, &mut class, &mut attrs)
        } else {
            self.line_body(line, len, &mut class, &mut attrs)
        };
        if line.kind == LineKind::FenceClose && !self.source {
            self.language_button(&mut body, index);
        }
        if !extra.is_empty() {
            class.push(' ');
            class.push_str(extra);
        }
        if !unclosed {
            self.block_classes(index, &mut class);
        }
        // The fence lines of a code block cannot be edited: whole blocks to the editor
        let code_fence = !self.source
            && (line.kind == LineKind::FenceOpen && line.fence == Some(FenceKind::Code)
                || self.code_block_open(index).is_some());
        if code_fence && !unclosed {
            attrs.push_str(&format!(
                r#" data-src="{}""#,
                encode(&self.text[line.start..line.end])
            ));
        }
        format!(r#"<div class="{class}"{attrs}>{body}<br></div>"#)
    }

    /// Line `index` is the opening fence or a body line of a fence that is never closed: the fence
    /// is still being typed (` ``` ` without Enter yet), so it is shown as plain text; Enter at the
    /// end of the opening fence completes it into a code block. Not in source mode, where every
    /// line is plain anyway.
    fn in_unclosed_fence(&self, index: usize) -> bool {
        let lines = &self.model.lines;
        if self.source || !matches!(lines[index].kind, LineKind::FenceOpen | LineKind::FenceBody) {
            return false;
        }
        !lines[index + 1..]
            .iter()
            .find(|l| l.kind != LineKind::FenceBody)
            .is_some_and(|l| l.kind == LineKind::FenceClose)
    }

    /// Plain text of a line of an unclosed fence (see [`Self::in_unclosed_fence`]).
    fn unclosed_fence_body(&self, line: &Line, class: &mut String, attrs: &mut String) -> String {
        let mut out = String::new();
        if line.kind == LineKind::FenceOpen {
            encode_into(&mut out, &self.text[line.start..line.end]);
        } else {
            self.prefixes(&mut out, line, class, attrs);
            encode_into(
                &mut out,
                &self.text[line.content_start.min(line.end)..line.end],
            );
        }
        out
    }

    fn line_body(&self, line: &Line, len: usize, class: &mut String, attrs: &mut String) -> String {
        let mut out = String::new();
        let whole = &self.text[line.start..line.end];
        match line.kind {
            LineKind::FenceOpen | LineKind::FenceClose | LineKind::Rule if !self.source => {
                class.push_str(match line.kind {
                    // A code block shows neither the fence nor the language at its top (the
                    // language is in the box below it)
                    LineKind::FenceOpen if line.fence == Some(FenceKind::Code) => {
                        " zr zl-fence zl-code-open"
                    }
                    LineKind::FenceOpen => " zr zl-fence",
                    LineKind::FenceClose => " zr zl-close",
                    _ => " zr zl-rule",
                });
                attrs.push_str(&format!(r#" data-s="0" data-e="{len}""#));
                out.push_str(r#"<span class="zm">"#);
                encode_into(&mut out, whole);
                out.push_str("</span>");
                if line.kind == LineKind::FenceOpen {
                    out.push_str(&format!(
                        r#"<span class="zf" data-label="{}"></span>"#,
                        encode(&fence_label(line))
                    ));
                }
                return out;
            }
            LineKind::FenceOpen
            | LineKind::FenceClose
            | LineKind::Rule
            | LineKind::TableSeparator => {
                class.push_str(" zl-dim");
                encode_into(&mut out, whole);
                return out;
            }
            LineKind::FenceBody
                if !self.source
                    && matches!(line.fence, Some(FenceKind::Code | FenceKind::Math)) =>
            {
                class.push_str(" zl-mono");
                self.prefixes(&mut out, line, class, attrs);
                encode_into(
                    &mut out,
                    &self.text[line.content_start.min(line.end)..line.end],
                );
                return out;
            }
            LineKind::IndentedCode if !self.source => {
                class.push_str(" zl-mono");
                encode_into(&mut out, whole);
                return out;
            }
            LineKind::Heading if !self.source => {
                class.push_str(&format!(" zl-h{}", line.heading_level.clamp(1, 6)));
            }
            _ => {}
        }
        self.prefixes(&mut out, line, class, attrs);
        // A line without list symbols starts with its indentation (a continuation line)
        let mut from = line.content_start;
        if line.prefixes.iter().all(|p| p.kind == PrefixKind::Quote) {
            let rest = &self.text[from..line.end];
            let lead = rest.len() - rest.trim_start_matches(' ').len();
            self.indentation(&mut out, &rest[..lead]);
            from += lead;
        }
        let spans: Vec<&Span> = self
            .model
            .spans
            .iter()
            .filter(|s| s.start >= from && s.end <= line.end)
            .collect();
        self.inline(&mut out, line, from, line.end, &spans);
        out
    }

    /// Opening fence line of the code block closed by line `index`, when that block is a code
    /// block (not math / spoiler / quote) and not in source mode: such a closing line shows the
    /// language box below the block, at the right, instead of the fence.
    fn code_block_open(&self, index: usize) -> Option<usize> {
        let line = &self.model.lines[index];
        if self.source || line.kind != LineKind::FenceClose || line.quote_depth > 0 {
            return None;
        }
        let open = self.model.lines[..index]
            .iter()
            .rposition(|l| l.kind == LineKind::FenceOpen)?;
        (self.model.lines[open].fence == Some(FenceKind::Code)).then_some(open)
    }

    // Below a code block at the right (on its closing fence line, takes no source text): a text box
    // for the language, showing the current one. Typing in it lists matching languages below it.
    fn language_button(&self, out: &mut String, index: usize) {
        let Some(open) = self.code_block_open(index) else {
            return;
        };
        let language = self.model.lines[open]
            .fence_info
            .as_deref()
            .and_then(|info| info.split_whitespace().next())
            .map(language_label)
            .unwrap_or_default();
        out.push_str(&format!(
            r#"<span class="zlang" contenteditable="false" data-src=""><input class="zlang-input" data-open="{open}" value="{}" placeholder="选择语言" spellcheck="false" autocomplete="off"></span>"#,
            encode(&language),
        ));
    }

    // Line-start marker: hidden in render mode, and the bullet is replaced by a bullet character
    // (U+2022). The text after it up to the content (indentation) is kept as is
    fn prefixes(&self, out: &mut String, line: &Line, class: &mut String, attrs: &mut String) {
        let mut pos = line.start;
        let mut hidden_to = 0;
        for prefix in &line.prefixes {
            self.indentation(out, &self.text[pos..prefix.start]);
            let raw = &self.text[prefix.start..prefix.end];
            // List symbols are as wide as their characters, like indentation (see `indentation`)
            let width = raw.chars().count();
            if self.source {
                out.push_str(r#"<span class="zs">"#);
                encode_into(out, raw);
                out.push_str("</span>");
            } else if prefix.kind == PrefixKind::Ordered {
                // The number in its box, the spaces after it as indentation (so the cursor after
                // them is at the content column)
                let number = raw.trim_end_matches(' ');
                let width = number.chars().count();
                out.push_str(&format!(r#"<span class="zo" style="width:{width}ch">"#));
                encode_into(out, number);
                out.push_str("</span>");
                self.indentation(out, &raw[number.len()..]);
            } else {
                out.push_str(r#"<span class="zm">"#);
                encode_into(out, raw);
                out.push_str("</span>");
                if prefix.kind == PrefixKind::Bullet {
                    out.push_str(&format!(
                        r#"<span class="zb" style="width:{width}ch"></span>"#
                    ));
                }
                hidden_to = prefix.end;
            }
            pos = prefix.end;
        }
        encode_into(out, &self.text[pos..line.content_start.max(pos)]);
        if !self.source
            && line.quote_depth > 0
            && line.prefixes.iter().any(|p| p.kind == PrefixKind::Quote)
        {
            class.push_str(" zl-bar");
        }
        if hidden_to > 0 {
            attrs.push_str(&format!(
                r#" data-cs="{}""#,
                utf16_len(&self.text[line.start..hidden_to])
            ));
        }
    }

    // Indentation (spaces before a list symbol, or of a continuation line in a list item): shown one
    // character column (`ch`) per space, the same unit as list symbols, so the content of a list
    // item and its continuation lines line up. Other text, and source mode, as it is.
    fn indentation(&self, out: &mut String, text: &str) {
        if self.source || text.is_empty() || !text.bytes().all(|b| b == b' ') {
            encode_into(out, text);
            return;
        }
        out.push_str(&format!(
            r#"<span class="zi" style="width:{}ch">"#,
            text.len()
        ));
        out.push_str(text);
        out.push_str("</span>");
    }

    // Nested inline elements (parse results do not cross; if they ever do, the later one is shown
    // as text)
    fn inline(&self, out: &mut String, line: &Line, from: usize, to: usize, spans: &[&Span]) {
        let mut pos = from;
        let mut i = 0;
        while i < spans.len() {
            let span = spans[i];
            if span.start < pos || span.end > to {
                i += 1;
                continue;
            }
            encode_into(out, &self.text[pos..span.start]);
            let inner_end = spans[i + 1..]
                .iter()
                .position(|s| s.start >= span.end)
                .map_or(spans.len(), |n| i + 1 + n);
            self.span(out, line, span, &spans[i + 1..inner_end]);
            pos = span.end;
            i = inner_end;
        }
        encode_into(out, &self.text[pos..to]);
    }

    fn span(&self, out: &mut String, line: &Line, span: &Span, inner: &[&Span]) {
        let raw = &self.text[span.start..span.end];
        if span.kind.is_atomic() {
            if self.source {
                out.push_str(r#"<span class="zs">"#);
                encode_into(out, raw);
                out.push_str("</span>");
            } else {
                out.push_str(&format!(
                    r#"<span class="za {}" contenteditable="false" data-src="{}">"#,
                    atom_class(span.kind),
                    encode(raw)
                ));
                self.atom(out, span);
                out.push_str("</span>");
            }
            return;
        }
        // Source mode shows the source: font and size unchanged, only the markers are colored
        let class = if self.source {
            ""
        } else {
            span_class(span.kind)
        };
        let (mark, reveal) = if self.source {
            ("zs", false)
        } else {
            ("zm", reveals(span.kind))
        };
        // `data-href`: the address opened by Ctrl + click. Rich mode: the whole link; source mode:
        // only the address (a bare address is all address)
        let href = matches!(span.kind, SpanKind::Link | SpanKind::Url)
            .then(|| span.data.clone().unwrap_or_default());
        let href_attr = match &href {
            Some(href) if !self.source || span.kind == SpanKind::Url => {
                format!(r#" data-href="{}""#, encode(href))
            }
            _ => String::new(),
        };
        if reveal {
            // `data-ie`: the end of the text inside the markers (Up / Down into the span lands there)
            out.push_str(&format!(
                r#"<span class="zr {class}" data-s="{}" data-e="{}" data-ie="{}"{href_attr}>"#,
                utf16_len(&self.text[line.start..span.start]),
                utf16_len(&self.text[line.start..span.end]),
                utf16_len(&self.text[line.start..span.content_end]),
            ));
        } else {
            out.push_str(&format!(r#"<span class="{class}"{href_attr}>"#));
        }
        // A link with no text shows its address
        let empty_link =
            span.kind == SpanKind::Link && span.content_start == span.content_end && !self.source;
        let write_mark = |out: &mut String, text: &str| {
            if !text.is_empty() {
                out.push_str(&format!(r#"<span class="{mark}">"#));
                encode_into(out, text);
                out.push_str("</span>");
            }
        };
        write_mark(out, &self.text[span.start..span.content_start]);
        if empty_link {
            let label = span.data.clone().unwrap_or_default();
            out.push_str(&format!(
                r#"<span class="zf" data-label="{}"></span>"#,
                encode(&label)
            ));
        }
        self.inline(out, line, span.content_start, span.content_end, inner);
        let close = &self.text[span.content_end..span.end];
        // Source mode: the address inside `](...)` of a link is its own element for Ctrl + click
        let address = href
            .as_deref()
            .filter(|href| self.source && span.kind == SpanKind::Link && !href.is_empty())
            .and_then(|href| close.find(href).map(|at| (at, href)));
        match address {
            Some((at, href)) => {
                write_mark(out, &close[..at]);
                out.push_str(&format!(
                    r#"<span class="{mark}" data-href="{}">"#,
                    encode(href)
                ));
                encode_into(out, href);
                out.push_str("</span>");
                write_mark(out, &close[at + href.len()..]);
            }
            None => write_mark(out, close),
        }
        out.push_str("</span>");
    }

    fn atom(&self, out: &mut String, span: &Span) {
        let data = span.data.as_deref().unwrap_or_default();
        match span.kind {
            SpanKind::Emoji => match self.lookup.emoji.get(&data.to_lowercase()) {
                Some(entry) => match (&entry.text, &entry.image_url) {
                    (Some(text), None) => encode_into(out, text),
                    (_, Some(url)) => match self.lookup.images.get(url) {
                        Some(local) => out.push_str(&format!(
                            r#"<img class="zemoji" src="{}" alt=":{}:">"#,
                            encode(local),
                            encode(data)
                        )),
                        None => encode_into(out, &format!(":{data}:")),
                    },
                    _ => encode_into(out, &format!(":{data}:")),
                },
                None => encode_into(out, &format!(":{data}:")),
            },
            SpanKind::Time => {
                let label = time::parse_datetime(data)
                    .map_or_else(|| data.to_owned(), |t| time::local_label(&t));
                encode_into(out, &label);
            }
            SpanKind::UploadImage => match self.lookup.images.get(data) {
                Some(local) => out.push_str(&format!(
                    r#"<img class="zthumb" src="{}" alt="">"#,
                    encode(local)
                )),
                None => out.push_str(r#"<span class="zthumb zthumb-empty"></span>"#),
            },
            SpanKind::UploadFile => {
                out.push_str(&icon_html(file_icon(data), 16));
                out.push_str("<span>");
                encode_into(out, &self.text[span.content_start..span.content_end]);
                out.push_str("</span>");
            }
            SpanKind::UploadPlaceholder => {
                encode_into(out, &format!("上传中：{data}…"));
            }
            _ => encode_into(out, data),
        }
    }

    // Consecutive quote or code lines are joined into one block: mark the first and last line
    fn block_classes(&self, index: usize, class: &mut String) {
        if self.source {
            return;
        }
        // The closing line of a code block holds the language box below the block
        let kind_at = |i: usize| {
            if self.code_block_open(i).is_some() {
                None
            } else {
                block_kind(&self.model.lines[i])
            }
        };
        let Some(kind) = kind_at(index) else {
            return;
        };
        class.push_str(if kind == BlockKind::Quote {
            " zl-bq"
        } else {
            " zl-bc"
        });
        let same = |i: Option<usize>| {
            i.filter(|i| *i < self.model.lines.len())
                .is_some_and(|i| kind_at(i) == Some(kind))
        };
        if !same(index.checked_sub(1)) {
            class.push_str(" zl-first");
        }
        if !same(Some(index + 1)) {
            class.push_str(" zl-last");
        }
    }

    pub fn table_block(
        &self,
        table: MarkdownTable,
        index: usize,
        cell_lines: impl Fn(usize, usize, &str) -> Vec<Block>,
    ) -> Block {
        let editor = TableEditor::new(self.model, self.text, table);
        let aligns = editor
            .aligns()
            .into_iter()
            .map(|a| match a {
                TableAlign::Left => "left",
                TableAlign::Center => "center",
                TableAlign::Right => "right",
            })
            .collect();
        let rows = editor
            .cell_values()
            .iter()
            .enumerate()
            .map(|(r, cells)| {
                cells
                    .iter()
                    .enumerate()
                    .map(|(c, value)| cell_lines(r, c, value))
                    .collect()
            })
            .collect();
        let block = TableBlock {
            index,
            src: editor.source().to_owned(),
            aligns,
            rows,
        };
        Block {
            key: serde_json::to_string(&block).unwrap_or_default(),
            html: None,
            table: Some(block),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Quote,
    Code,
}

fn block_kind(line: &Line) -> Option<BlockKind> {
    if line.quote_depth > 0 {
        return Some(BlockKind::Quote);
    }
    (line.kind == LineKind::IndentedCode
        || matches!(
            line.fence,
            Some(FenceKind::Code | FenceKind::Math | FenceKind::Spoiler)
        ))
    .then_some(BlockKind::Code)
}

fn atom_class(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::UserMention | SpanKind::GroupMention | SpanKind::ChannelLink => "zn",
        SpanKind::Time => "zw",
        SpanKind::Emoji => "ze",
        SpanKind::UploadImage => "zp",
        SpanKind::UploadFile => "zfile-chip",
        _ => "zh",
    }
}

fn fence_label(line: &Line) -> String {
    let info = line.fence_info.as_deref().unwrap_or_default();
    match line.fence {
        Some(FenceKind::Quote) => "引用".to_owned(),
        Some(FenceKind::Math) => "公式".to_owned(),
        Some(FenceKind::Spoiler) if info.is_empty() => "折叠".to_owned(),
        Some(FenceKind::Spoiler) => format!("折叠 {info}"),
        _ if info.is_empty() => "代码".to_owned(),
        _ => format!("代码 {info}"),
    }
}

/// A collapsed or expanded quote panel. The whole source is in `data-src`; while editing, `src` has
/// only the quote lines.
pub fn quote_panel(
    q: usize,
    quote: &QuoteBlock,
    src: &str,
    preview: &str,
    expanded: Option<&str>,
    editing: bool,
) -> String {
    let mut out = format!(
        r#"<div class="zq{}" contenteditable="false" data-src="{}" data-q="{q}">"#,
        if editing { " editing" } else { "" },
        encode(src),
    );
    let open = editing || expanded.is_some();
    out.push_str(&format!(
        r#"<div class="zq-head"><div class="zq-title{}" data-act="{}"><span class="zq-chevron">{}</span><span class="zq-text">{}：{}</span></div>"#,
        if open { " open" } else { "" },
        if editing { "end-edit" } else { "toggle" },
        icon_html(IconName::ChevronRight, 12),
        encode(&quote.sender_name),
        encode(preview),
    ));
    let button = |act: &str, tip: &str, icon: IconName| {
        format!(
            r#"<button class="icon-btn" type="button" title="{tip}" data-act="{act}" style="width:22px;height:22px">{}</button>"#,
            icon_html(icon, 14)
        )
    };
    if !editing {
        out.push_str(&button("edit", "编辑引用", IconName::Edit));
    }
    if quote.message_id.is_some() {
        out.push_str(&button("jump", "跳转到原消息", IconName::ArrowRightUp));
    }
    out.push_str(&button("delete", "删除引用", IconName::Close));
    out.push_str("</div>");
    if let Some(body) = expanded {
        // The body is the message HTML, using the message content styles (code blocks, tables,
        // quotes)
        out.push_str(&format!(
            r#"<div class="zq-body msg-html selectable">{body}</div>"#
        ));
    }
    out.push_str("</div>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zulip_core::markdown::{NoResolver, parse};

    /// The text after removing tags (`data-src` counts as source) must equal this line's Markdown.
    fn text_of(html: &str) -> String {
        let mut out = String::new();
        let mut rest = html;
        while let Some(open) = rest.find('<') {
            out.push_str(&rest[..open]);
            let close = rest[open..].find('>').map_or(rest.len(), |i| open + i);
            let tag = &rest[open..=close.min(rest.len() - 1)];
            rest = &rest[(close + 1).min(rest.len())..];
            if let Some(start) = tag.find("data-src=\"") {
                let value = &tag[start + 10..];
                let value = &value[..value.find('"').unwrap_or(0)];
                out.push_str(value);
                // Skip the displayed text inside elements
                let end = rest.find("</span>").unwrap_or(0);
                rest = &rest[end + 7..];
            }
        }
        out.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&nbsp;", "\u{a0}")
            .replace("&amp;", "&")
    }

    #[test]
    fn source_mode_keeps_one_font_size() {
        let html = render("# 标题\n`代码`\n```\n围栏\n```\n    缩进", true).join("\n");
        for class in ["zl-h", "zl-mono", "zk", "zl-fence"] {
            assert!(!html.contains(class), "{class}: {html}");
        }
    }

    #[test]
    fn unclosed_fence_stays_plain_until_completed() {
        for text in [
            "```",
            "```py",
            "x
```
code",
        ] {
            let html = render(text, false).join(
                "
",
            );
            for class in ["zl-fence", "zl-mono", "zl-bc"] {
                assert!(!html.contains(class), "{text} {class}: {html}");
            }
        }
        let closed = render(
            "```
code
```",
            false,
        )
        .join(
            "
",
        );
        assert!(
            closed.contains("zl-fence") && closed.contains("zl-mono"),
            "{closed}"
        );
    }

    fn render(text: &str, source: bool) -> Vec<String> {
        let model = parse(text, &NoResolver);
        let lookup = Lookup::default();
        let ctx = RenderCtx {
            text,
            model: &model,
            lookup: &lookup,
            source,
        };
        (0..model.lines.len())
            .map(|i| ctx.line_html(i, ""))
            .collect()
    }

    #[test]
    fn text_round_trips() {
        let doc = "# 标题\n**粗体** *斜* `代码` ~~删~~ [链接](http://a.b)\n> 引用\n- 列表\n1. 有序\n```py\nx < 1 & y\n```\n---\n<普通> \"引号\" '单引号' 不换行\u{a0}空格";
        for source in [false, true] {
            let lines = render(doc, source);
            let joined: Vec<String> = lines.iter().map(|l| text_of(l)).collect();
            assert_eq!(joined.join("\n"), doc, "source={source}");
        }
    }

    #[test]
    fn marks_hidden_only_in_rich_mode() {
        let rich = render("**a**", false);
        assert!(
            rich[0].contains(r#"class="zr zB" data-s="0" data-e="5""#),
            "{}",
            rich[0]
        );
        assert!(rich[0].contains(r#"<span class="zm">**</span>"#));
        let source = render("**a**", true);
        assert!(source[0].contains(r#"<span class="zs">**</span>"#));
        assert!(!source[0].contains("zm"));
    }

    #[test]
    fn reveal_range_counts_utf16() {
        let html = render("😀 **a**", false);
        assert!(html[0].contains(r#"data-s="3" data-e="8""#), "{}", html[0]);
    }

    #[test]
    fn hidden_prefix_marks_content_start() {
        let html = render("## 标题", false);
        assert!(html[0].contains(r#"data-cs="3""#), "{}", html[0]);
        assert!(html[0].contains("zl-h2"));
        assert!(!render("## 标题", true)[0].contains("data-cs"));
    }

    #[test]
    fn code_lines_form_one_block() {
        let html = render("```\na\n```", false);
        assert!(html[0].contains("zl-first") && !html[0].contains("zl-last"));
        assert!(html[1].contains("zl-mono") && html[1].contains("zl-last"));
        // The closing line sits below the block and holds the language box
        assert!(!html[2].contains("zl-bc") && html[2].contains("zlang-input"));
    }
}
