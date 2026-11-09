//! 编辑器的内容模型：Markdown 原文、选区、撤销记录、表格单元格与引用的状态。没有界面依赖。
//! “当前编辑器”是光标所在的单元格或者整个文档，格式、插入、补全都作用于它。偏移都是字节。

use super::render::{Block, Lookup, RenderCtx, quote_panel};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use zulip_core::markdown::format::{self, FormatKind, MarkdownEdit};
use zulip_core::markdown::quote::{self, QuoteBlock};
use zulip_core::markdown::table::{self, MarkdownTable, TableAlign, TableEditor};
use zulip_core::markdown::{self, MarkdownModel, PrefixKind, parse};
use zulip_core::markdown::{code_language, markup};
use zulip_core::offsets;

const UNDO_LIMIT: usize = 200;
/// 连续输入合并成一步撤销。
const TYPING_MERGE: Duration = Duration::from_millis(800);
const BLOCK_PADDING: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPos {
    pub table: usize,
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sel {
    pub start: usize,
    pub end: usize,
    pub cell: Option<CellPos>,
}

impl Sel {
    pub fn caret(at: usize) -> Self {
        Self {
            start: at,
            end: at,
            cell: None,
        }
    }

    pub fn is_collapsed(&self) -> bool {
        self.start == self.end
    }
}

#[derive(Clone)]
struct Snapshot {
    text: String,
    sel: Sel,
}

pub struct EditorCore {
    text: String,
    pub sel: Sel,
    model: MarkdownModel,
    tables: Vec<MarkdownTable>,
    quotes: Vec<QuoteBlock>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_typing: Option<Instant>,
    /// 递增表示程序改了内容：JS 还没收到之前发来的同步要作废。
    pub rev: u64,
    pub source: bool,
    pub lookup: Lookup,
    /// 正在输入的单元格的文字：两端的空格在原文中只是排版，解析出来会丢。
    live_cell: Option<(CellPos, String)>,
    /// 展开的引用（按原文）与发送后的样子。
    expanded: HashSet<String>,
    pub rendered_quotes: HashMap<String, String>,
    /// 正在编辑的引用的开头。
    edit_quote: Option<usize>,
}

impl EditorCore {
    pub fn new(text: String, source: bool, lookup: Lookup) -> Self {
        let mut core = Self {
            text,
            sel: Sel::default(),
            model: MarkdownModel::default(),
            tables: Vec::new(),
            quotes: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_typing: None,
            rev: 1,
            source,
            lookup,
            live_cell: None,
            expanded: HashSet::new(),
            rendered_quotes: HashMap::new(),
            edit_quote: None,
        };
        core.reparse();
        let end = core.text.len();
        core.sel = Sel::caret(end);
        core.settle();
        core
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn model(&self) -> &MarkdownModel {
        &self.model
    }

    pub fn reparse(&mut self) {
        self.model = parse(&self.text, &self.lookup);
        self.tables = table::find_tables(&self.model);
        self.quotes = quote::find(&self.model, &self.text);
    }

    // ───────── 当前编辑器 ─────────

    fn table_editor(&self, index: usize) -> Option<TableEditor<'_>> {
        self.tables
            .get(index)
            .map(|t| TableEditor::new(&self.model, &self.text, *t))
    }

    /// 单元格当前的文字。
    pub fn cell_value(&self, pos: CellPos) -> String {
        if let Some((live, value)) = &self.live_cell
            && *live == pos
        {
            return value.clone();
        }
        self.table_editor(pos.table)
            .and_then(|t| t.cell_values().get(pos.row)?.get(pos.col).cloned())
            .unwrap_or_default()
    }

    /// 光标所在编辑器的文字。
    pub fn active_text(&self) -> String {
        match self.sel.cell {
            Some(pos) => self.cell_value(pos),
            None => self.text.clone(),
        }
    }

    pub fn active_model(&self, text: &str) -> MarkdownModel {
        match self.sel.cell {
            Some(_) => parse(text, &self.lookup),
            None => self.model.clone(),
        }
    }

    pub fn selected_text(&self) -> String {
        let text = self.active_text();
        text.get(self.sel.start..self.sel.end)
            .unwrap_or_default()
            .to_owned()
    }

    fn remember(&mut self) {
        self.undo.push(Snapshot {
            text: self.text.clone(),
            sel: self.sel,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// 对当前编辑器做一次修改（一步撤销）。
    pub fn edit_active(
        &mut self,
        make: impl FnOnce(&str, &MarkdownModel, usize, usize) -> MarkdownEdit,
    ) {
        let before = self.break_off_atomic();
        let active = self.active_text();
        let model = self.active_model(&active);
        let start = self.sel.start.min(active.len());
        let end = self.sel.end.min(active.len());
        let edit = make(&active, &model, start, end);
        self.apply_active(&edit, &active);
        if let Some(before) = before {
            self.undo.pop();
            self.undo.push(before);
        }
    }

    // 光标在表格、收起的引用紧邻的空位上（显示上是单独一行，原文里还没有这一行）：先补出这一行
    fn break_off_atomic(&mut self) -> Option<Snapshot> {
        if self.source || self.sel.cell.is_some() || !self.sel.is_collapsed() {
            return None;
        }
        let caret = self.sel.start;
        let ends = |model: &MarkdownModel, t: &MarkdownTable| {
            (
                model.lines[t.header_line].start,
                model.lines[t.last_line].end,
            )
        };
        let at_start = self.tables.iter().any(|t| ends(&self.model, t).0 == caret)
            || self
                .quotes
                .iter()
                .any(|q| q.start == caret && Some(q.start) != self.edit_quote);
        let at_end = self.tables.iter().any(|t| ends(&self.model, t).1 == caret)
            || self
                .quotes
                .iter()
                .any(|q| q.end == caret && Some(q.start) != self.edit_quote);
        if !at_start && !at_end {
            return None;
        }
        let before = Snapshot {
            text: self.text.clone(),
            sel: self.sel,
        };
        self.text.insert(caret, '\n');
        if at_end {
            self.sel = Sel::caret(caret + 1);
        }
        self.reparse();
        Some(before)
    }

    fn apply_active(&mut self, edit: &MarkdownEdit, active: &str) {
        match self.sel.cell {
            Some(pos) => {
                let value = edit.apply(active);
                let Some(doc_edit) = self
                    .table_editor(pos.table)
                    .map(|t| t.set_cell(pos.row, pos.col, &value))
                else {
                    return;
                };
                self.remember();
                self.text = doc_edit.apply(&self.text);
                self.sel = Sel {
                    start: edit.selection_start,
                    end: edit.selection_end,
                    cell: Some(pos),
                };
                self.live_cell = Some((pos, value));
                self.changed();
            }
            None => self.apply(edit),
        }
    }

    /// 修改文档。
    pub fn apply(&mut self, edit: &MarkdownEdit) {
        self.remember();
        self.text = edit.apply(&self.text);
        self.sel = Sel {
            start: edit.selection_start,
            end: edit.selection_end,
            cell: None,
        };
        self.live_cell = None;
        self.shift_edit_quote(edit);
        self.changed();
        self.settle();
    }

    fn changed(&mut self) {
        self.rev += 1;
        self.last_typing = None;
        self.reparse();
    }

    fn shift_edit_quote(&mut self, edit: &MarkdownEdit) {
        if let Some(anchor) = self.edit_quote
            && edit.start <= anchor
        {
            self.edit_quote = Some(
                anchor + edit.replacement.len() - edit.len.min(anchor - edit.start + edit.len),
            );
        }
    }

    /// 光标落到了显示成整块的内容上：表格进入单元格，收起的引用移到较近的一端。
    fn settle(&mut self) {
        if self.source || self.sel.cell.is_some() || !self.sel.is_collapsed() {
            return;
        }
        let caret = self.sel.start;
        for (index, t) in self.tables.iter().enumerate() {
            let (start, end) = (
                self.model.lines[t.header_line].start,
                self.model.lines[t.last_line].end,
            );
            if caret >= start && caret <= end {
                let editor = TableEditor::new(&self.model, &self.text, *t);
                let (row, col) = editor.cell_at(caret);
                let pos = CellPos {
                    table: index,
                    row,
                    col,
                };
                let len = self.cell_value(pos).len();
                self.sel = Sel {
                    start: len,
                    end: len,
                    cell: Some(pos),
                };
                return;
            }
        }
        if let Some(q) = self
            .quotes
            .iter()
            .find(|q| q.start < caret && caret < q.end && Some(q.start) != self.edit_quote)
        {
            let at = if caret - q.start <= q.end - caret {
                q.start
            } else {
                q.end
            };
            self.sel = Sel::caret(at);
        }
    }

    // ───────── 来自 DOM 的修改 ─────────

    /// 编辑区的文字变了。返回选区是否被挪动（要告诉 JS）。
    pub fn sync(&mut self, text: String, sel: Sel) -> bool {
        self.live_cell = None;
        if text != self.text {
            self.remember_typing();
            let (prefix, removed, inserted) = diff(&self.text, &text);
            self.text = text;
            if let Some(anchor) = self.edit_quote
                && prefix <= anchor
            {
                self.edit_quote = Some(anchor + inserted - removed.min(anchor - prefix + removed));
            }
            self.reparse();
        }
        self.sel = sel;
        let before = self.sel;
        self.settle();
        before != self.sel
    }

    pub fn sync_cell(&mut self, pos: CellPos, value: String, start: usize, end: usize) {
        let Some(edit) = self
            .table_editor(pos.table)
            .map(|t| t.set_cell(pos.row, pos.col, &value))
        else {
            return;
        };
        if self.cell_value(pos) != value {
            self.remember_typing();
            self.text = edit.apply(&self.text);
            self.reparse();
        }
        self.live_cell = Some((pos, value));
        self.sel = Sel {
            start,
            end,
            cell: Some(pos),
        };
    }

    fn remember_typing(&mut self) {
        let now = Instant::now();
        if self
            .last_typing
            .is_none_or(|at| now.duration_since(at) > TYPING_MERGE)
        {
            self.remember();
        } else {
            self.redo.clear();
        }
        self.last_typing = Some(now);
    }

    /// 选区移动。光标离开正在编辑的引用时结束编辑，这时返回 true（要重新渲染）。
    pub fn set_sel(&mut self, sel: Sel) -> bool {
        if sel.cell.is_none_or(|p| p.table < self.tables.len()) {
            if self.sel.cell != sel.cell {
                self.live_cell = None;
            }
            self.sel = sel;
        }
        self.last_typing = None;
        if let Some(anchor) = self.edit_quote
            && !self
                .quotes
                .iter()
                .any(|q| q.start == anchor && q.start < sel.start && sel.end < q.end)
        {
            self.edit_quote = None;
            return true;
        }
        false
    }

    /// 切换富文本和源码。光标留在原来的内容上。
    pub fn set_source(&mut self, source: bool) {
        if source && let Some(pos) = self.sel.cell {
            let at = self
                .table_editor(pos.table)
                .map_or(self.text.len(), |t| t.caret_of(pos.row, pos.col));
            self.sel = Sel::caret(at);
        }
        self.source = source;
        self.live_cell = None;
        self.edit_quote = None;
        self.rev += 1;
        self.settle();
    }

    /// 全选当前编辑器。
    pub fn select_all(&mut self) {
        self.sel.start = 0;
        self.sel.end = self.active_text().len();
        self.rev += 1;
    }

    /// 用户、频道、表情的数据变了。
    pub fn set_lookup(&mut self, lookup: Lookup) {
        self.lookup = lookup;
        self.reparse();
    }

    // ───────── 撤销 ─────────

    pub fn undo(&mut self) -> bool {
        let Some(last) = self.undo.pop() else {
            return false;
        };
        self.redo.push(Snapshot {
            text: std::mem::replace(&mut self.text, last.text),
            sel: self.sel,
        });
        self.sel = last.sel;
        self.after_history();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(Snapshot {
            text: std::mem::replace(&mut self.text, next.text),
            sel: self.sel,
        });
        self.sel = next.sel;
        self.after_history();
        true
    }

    fn after_history(&mut self) {
        self.live_cell = None;
        self.changed();
        if self.sel.cell.is_some_and(|p| p.table >= self.tables.len()) {
            self.sel = Sel::caret(self.text.len());
        }
        self.settle();
    }

    /// 整体替换（载入草稿、发送后清空、开始 / 结束编辑消息）：清空撤销记录。
    pub fn reset(&mut self, text: String) {
        self.text = text;
        self.undo.clear();
        self.redo.clear();
        self.live_cell = None;
        self.edit_quote = None;
        self.changed();
        self.sel = Sel::caret(self.text.len());
        self.settle();
    }

    // ───────── 格式 ─────────

    pub fn format(&mut self, kind: FormatKind) {
        self.edit_active(|text, model, s, e| format::apply(text, s, e, kind, Some(model)));
    }

    pub fn heading(&mut self, level: u8) {
        if self.sel.cell.is_none() {
            self.edit_active(|text, _, s, e| format::set_heading(text, s, e, level));
        }
    }

    pub fn is_active(&self, kind: FormatKind) -> bool {
        let text = self.active_text();
        let model = self.active_model(&text);
        format::is_active(&model, self.sel.start, kind)
    }

    pub fn heading_level(&self) -> u8 {
        if self.sel.cell.is_some() {
            return 0;
        }
        format::heading_level_at(&self.model, self.sel.start)
    }

    /// 在光标处插入（替换选中部分）。
    pub fn insert(&mut self, markdown: &str) {
        let markdown = markdown.to_owned();
        self.edit_active(|_, _, s, e| {
            let caret = s + markdown.len();
            MarkdownEdit::new(s, e - s, markdown, caret, caret)
        });
    }

    /// 替换全部内容（一步撤销）。光标在表格单元格或引用中时也替换整个内容。
    pub fn replace_all(&mut self, markdown: &str) {
        self.live_cell = None;
        self.edit_quote = None;
        self.sel = Sel {
            start: 0,
            end: self.text.len(),
            cell: None,
        };
        self.insert(markdown);
    }

    /// 粘贴、常用语放入的内容：多行时结尾补换行。
    pub fn insert_block(&mut self, markdown: &str) {
        self.insert(&format::with_trailing_line(markdown));
    }

    /// 插入文字并在需要时前后加空格（提及、插件填入）；多行内容作为独立段落。
    pub fn insert_smart(&mut self, markdown: &str) {
        let active = self.active_text();
        let (start, end) = (
            self.sel.start.min(active.len()),
            self.sel.end.min(active.len()),
        );
        if markdown.contains('\n') && self.sel.cell.is_none() {
            let block = format::with_trailing_line(markdown);
            let padded = format::pad_block(
                &active,
                start,
                end,
                block.trim_end_matches('\n'),
                BLOCK_PADDING,
            );
            self.insert(&padded);
            return;
        }
        let before = active[..start]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace());
        let after = !markdown.ends_with(char::is_whitespace)
            && active[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_whitespace());
        let text = format!(
            "{}{markdown}{}",
            if before { " " } else { "" },
            if after { " " } else { "" }
        );
        self.insert(&text);
    }

    /// 把占位换成上传结果。用户已删掉占位时不动。
    pub fn replace_placeholder(&mut self, placeholder: &str, replacement: &str) {
        let Some(start) = self.text.find(placeholder) else {
            return;
        };
        // 删除占位时连同插入时补的空格
        let spaced = format!("{placeholder} ");
        let placeholder = if replacement.is_empty() && self.text[start..].starts_with(&spaced) {
            spaced.as_str()
        } else {
            placeholder
        };
        let shift = |at: usize| {
            if at <= start {
                at
            } else if at >= start + placeholder.len() {
                at + replacement.len() - placeholder.len()
            } else {
                start + replacement.len()
            }
        };
        let sel = self.sel;
        let edit = MarkdownEdit::new(start, placeholder.len(), replacement.to_owned(), 0, 0);
        self.remember();
        self.text = edit.apply(&self.text);
        self.changed();
        self.sel = match sel.cell {
            Some(_) => sel,
            None => Sel {
                start: shift(sel.start),
                end: shift(sel.end),
                cell: None,
            },
        };
    }

    /// 删除文档中的一段（紧挨表格的空行）。
    pub fn delete_range(&mut self, start: usize, end: usize) {
        if start < end && end <= self.text.len() {
            self.apply(&MarkdownEdit::new(
                start,
                end - start,
                String::new(),
                start,
                start,
            ));
        }
    }

    /// 换行：列表、引用续写行首符号（见 [`markup::newline`]）。
    pub fn newline(&mut self) {
        if self.sel.cell.is_some() {
            return;
        }
        self.edit_active(markup::newline);
    }

    /// 在没有结束的开头围栏（` ``` ` 或 ` ```语言 `）行末按回车：补成代码块。不在这样的位置时返回 false。
    pub fn complete_fence(&mut self) -> bool {
        if self.sel.cell.is_some() || !self.sel.is_collapsed() {
            return false;
        }
        match code_language::complete_fence(&self.text, self.sel.start, &self.model) {
            Some(edit) => {
                self.apply(&edit);
                true
            }
            None => false,
        }
    }

    /// 第 `open_line` 行开头围栏写的语言（没有写时为空）。
    pub fn code_language(&self, open_line: usize) -> String {
        self.model
            .lines
            .get(open_line)
            .and_then(|l| l.fence_info.as_deref())
            .and_then(|info| info.split_whitespace().next())
            .unwrap_or_default()
            .to_owned()
    }

    /// 把第 `open_line` 行开头围栏的代码块语言换成 `language`。
    pub fn set_code_language(&mut self, open_line: usize, language: &str) {
        if let Some(edit) =
            code_language::set_language(&self.text, &self.model, open_line, language)
        {
            self.apply(&edit);
        }
    }

    /// 行首的引用、标题、列表符号是一个整体：光标在它后面退格时删掉最里层的一个。
    /// 光标不在符号后面时是普通的退格（编辑区按文字判断后发来，最终以这里的解析为准）。
    pub fn remove_prefix(&mut self) {
        if self.sel.cell.is_some() {
            return;
        }
        let caret = self.sel.start;
        if let Some(edit) = markup::delete_markup_backward(&self.text, &self.model, caret) {
            self.apply(&edit);
            return;
        }
        let Some(line) = self
            .model
            .line_index_at(caret)
            .map(|i| self.model.lines[i].clone())
        else {
            return;
        };
        let Some(prefix) = line
            .prefixes
            .iter()
            .rev()
            .find(|p| p.kind != PrefixKind::Ordered && p.end <= caret)
        else {
            if let Some(edit) = markup::delete_char_backward(&self.text, caret) {
                self.apply(&edit);
            }
            return;
        };
        let (start, len) = (prefix.start, prefix.end - prefix.start);
        self.apply(&MarkdownEdit::new(
            start,
            len,
            String::new(),
            caret - len,
            caret - len,
        ));
    }

    // ───────── 表格 ─────────

    pub fn active_table_size(&self) -> Option<(usize, usize)> {
        let pos = self.sel.cell?;
        let t = self.tables.get(pos.table)?;
        let editor = TableEditor::new(&self.model, &self.text, *t);
        Some((editor.column_count(), t.row_count()))
    }

    pub fn active_align(&self) -> TableAlign {
        self.sel
            .cell
            .and_then(|p| self.table_editor(p.table)?.aligns().get(p.col).copied())
            .unwrap_or_default()
    }

    pub fn active_table_source(&self) -> Option<String> {
        let pos = self.sel.cell?;
        Some(self.table_editor(pos.table)?.source().to_owned())
    }

    /// 修改光标所在的表格，之后光标在目标单元格的末尾。
    pub fn table_op(&mut self, op: impl FnOnce(&TableEditor<'_>, usize) -> Option<MarkdownEdit>) {
        let Some(pos) = self.sel.cell else {
            return;
        };
        let Some(edit) = self.table_editor(pos.table).and_then(|t| {
            let caret = t.caret_of(pos.row, pos.col);
            op(&t, caret)
        }) else {
            return;
        };
        self.apply(&edit);
    }

    /// 在光标所在表格的上方或下方插入空段落。
    pub fn paragraph_beside_table(&mut self, above: bool) {
        let Some(t) = self
            .sel
            .cell
            .and_then(|p| self.tables.get(p.table).copied())
        else {
            return;
        };
        let at = if above {
            self.model.lines[t.header_line].start
        } else {
            self.model.lines[t.last_line].end
        };
        let caret = if above { at } else { at + 2 };
        self.apply(&MarkdownEdit::new(at, 0, "\n\n".to_owned(), caret, caret));
    }

    // ───────── 引用 ─────────

    fn quote_src(&self, q: &QuoteBlock) -> String {
        self.text[q.start..q.end].to_owned()
    }

    pub fn toggle_quote(&mut self, index: usize) {
        if let Some(q) = self.quotes.get(index) {
            let src = self.quote_src(q);
            if !self.expanded.remove(&src) {
                self.expanded.insert(src);
            }
            self.rev += 1;
        }
    }

    pub fn edit_quote(&mut self, index: usize) {
        if let Some(q) = self.quotes.get(index) {
            self.edit_quote = Some(q.start);
            self.sel = Sel::caret(q.body_start);
            self.rev += 1;
        }
    }

    pub fn end_quote_edit(&mut self) {
        if let Some(q) = self
            .edit_quote
            .and_then(|at| self.quotes.iter().find(|q| q.start == at))
        {
            self.sel = Sel::caret(q.end);
        }
        self.edit_quote = None;
        self.rev += 1;
    }

    pub fn delete_quote(&mut self, index: usize) {
        let Some(q) = self.quotes.get(index) else {
            return;
        };
        let (mut start, mut end) = (q.start, q.end);
        if self.text.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        } else if start > 0 && self.text.as_bytes()[start - 1] == b'\n' {
            start -= 1;
        }
        self.expanded.remove(&self.quote_src(q));
        self.edit_quote = None;
        self.apply(&MarkdownEdit::new(
            start,
            end - start,
            String::new(),
            start,
            start,
        ));
    }

    pub fn quote_message_id(&self, index: usize) -> Option<i64> {
        self.quotes.get(index)?.message_id
    }

    /// 展开的引用渲染好的 HTML；还没渲染时为 `None`。
    pub fn rendered_quote(&self, index: usize) -> Option<&str> {
        let quote = self.quotes.get(index)?;
        self.rendered_quotes
            .get(&self.quote_src(quote))
            .map(String::as_str)
    }

    /// 展开了、还没有发送后样子的引用：（整段原文，引用的内容）。
    pub fn quotes_to_render(&self) -> Vec<(String, String)> {
        self.quotes
            .iter()
            .map(|q| {
                (
                    self.quote_src(q),
                    self.text[q.body_start..q.body_end].to_owned(),
                )
            })
            .filter(|(src, _)| {
                self.expanded.contains(src) && !self.rendered_quotes.contains_key(src)
            })
            .collect()
    }

    // ───────── 渲染 ─────────

    fn cell_blocks(&self, table: usize, row: usize, col: usize, value: &str) -> Vec<Block> {
        let pos = CellPos { table, row, col };
        let live = self
            .live_cell
            .as_ref()
            .filter(|(p, v)| *p == pos && v.trim() == value)
            .map_or(value, |(_, v)| v.as_str());
        let model = parse(live, &self.lookup);
        let ctx = RenderCtx {
            text: live,
            model: &model,
            lookup: &self.lookup,
            source: false,
        };
        (0..model.lines.len().max(1))
            .map(|i| {
                if model.lines.is_empty() {
                    Block::line(r#"<div class="zl"><br></div>"#.to_owned())
                } else {
                    Block::line(ctx.line_html(i, ""))
                }
            })
            .collect()
    }

    pub fn blocks(&self) -> Vec<Block> {
        let ctx = RenderCtx {
            text: &self.text,
            model: &self.model,
            lookup: &self.lookup,
            source: self.source,
        };
        let mut blocks: Vec<(Block, bool)> = Vec::new();
        let lines = self.model.lines.len();
        let mut i = 0;
        while i < lines {
            if !self.source {
                if let Some((index, t)) = self
                    .tables
                    .iter()
                    .enumerate()
                    .find(|(_, t)| t.header_line == i)
                {
                    let block = ctx.table_block(*t, index, |r, c, value| {
                        self.cell_blocks(index, r, c, value)
                    });
                    blocks.push((block, true));
                    i = t.last_line + 1;
                    continue;
                }
                if let Some((q, quote)) = self
                    .quotes
                    .iter()
                    .enumerate()
                    .find(|(_, q)| q.header_line == i)
                {
                    let preview = self.quote_preview(quote);
                    if Some(quote.start) == self.edit_quote {
                        let line = &self.model.lines[i];
                        let header = &self.text[line.start..line.end];
                        let html = quote_panel(q, quote, header, &preview, None, true);
                        blocks.push((Block::line(html), true));
                        for k in i + 1..=quote.close_line {
                            blocks.push((Block::line(ctx.line_html(k, "zl-qcard")), false));
                        }
                    } else {
                        let src = self.quote_src(quote);
                        let expanded = self.expanded.contains(&src).then(|| {
                            self.rendered_quotes.get(&src).cloned().unwrap_or_else(|| {
                                format!(
                                    r#"<div class="zq-plain">{}</div>"#,
                                    zulip_core::html::encode(&self.quote_body(quote))
                                )
                            })
                        });
                        let html =
                            quote_panel(q, quote, &src, &preview, expanded.as_deref(), false);
                        blocks.push((Block::line(html), true));
                    }
                    i = quote.close_line + 1;
                    continue;
                }
            }
            blocks.push((Block::line(ctx.line_html(i, "")), false));
            i += 1;
        }
        if blocks.is_empty() {
            blocks.push((
                Block::line(r#"<div class="zl"><br></div>"#.to_owned()),
                false,
            ));
        }
        // 整块的两边要有能放光标的地方
        let mut out = Vec::with_capacity(blocks.len() + 2);
        let phantom = || Block::line(r#"<div class="zl zphantom"><br></div>"#.to_owned());
        let mut previous_atomic = true;
        for (block, atomic) in blocks {
            if atomic && previous_atomic {
                out.push(phantom());
            }
            previous_atomic = atomic;
            out.push(block);
        }
        if previous_atomic {
            out.push(phantom());
        }
        out
    }

    fn quote_body(&self, q: &QuoteBlock) -> String {
        let body = &self.text[q.body_start..q.body_end];
        markdown::to_plain_text(body, &self.lookup, &|name| {
            self.lookup.emoji.get(name).and_then(|e| e.text.clone())
        })
        .trim_matches('\n')
        .to_owned()
    }

    fn quote_preview(&self, q: &QuoteBlock) -> String {
        self.quote_body(q)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_owned()
    }

    /// 显示的文字（复制到其它程序）；源码模式下显示的就是原文。
    pub fn plain_text(&self, markdown: &str) -> String {
        if self.source {
            return markdown.to_owned();
        }
        markdown::to_plain_text(markdown, &self.lookup, &|name| {
            self.lookup.emoji.get(name).and_then(|e| e.text.clone())
        })
    }

    /// 选区的 UTF-16 偏移（JS 用）。
    pub fn sel_utf16(&self) -> (usize, usize) {
        let text = self.active_text();
        (
            offsets::to_utf16(&text, self.sel.start),
            offsets::to_utf16(&text, self.sel.end),
        )
    }

    /// JS 发来的选区转成字节偏移。
    pub fn sel_from_utf16(&self, s: usize, e: usize, cell: Option<CellPos>) -> Sel {
        let text = match cell {
            Some(pos) => std::borrow::Cow::Owned(self.cell_value(pos)),
            None => std::borrow::Cow::Borrowed(self.text.as_str()),
        };
        Sel {
            start: offsets::from_utf16(&text, s),
            end: offsets::from_utf16(&text, e),
            cell,
        }
    }
}

// 两段文字不同的部分：相同前缀的长度、删掉的长度、新增的长度
fn diff(old: &str, new: &str) -> (usize, usize, usize) {
    let prefix = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map_or(old.len().min(new.len()), |((i, _), _)| i);
    let suffix: usize = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    (
        prefix,
        old.len() - prefix - suffix,
        new.len() - prefix - suffix,
    )
}

/// 去掉上次没有完成的上传留下的占位（`[Uploading 名字…]()`，后面可以有一个空格）。
pub fn strip_stale_uploads(text: &str) -> String {
    const OPEN: &str = "[Uploading ";
    const CLOSE: &str = "…]()";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let body = &rest[start + OPEN.len()..];
        let end = body
            .find(CLOSE)
            .filter(|&e| !body[..e].contains(['\n', ']']));
        out.push_str(&rest[..start]);
        match end {
            Some(end) => {
                let after = &body[end + CLOSE.len()..];
                rest = after.strip_prefix(' ').unwrap_or(after);
            }
            None => {
                out.push_str(OPEN);
                rest = body;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 上传占位（与网页版相同）；同名文件加序号，保证唯一。
pub fn upload_placeholder(name: &str, exists: impl Fn(&str) -> bool) -> String {
    let clean: String = name.chars().filter(|c| !matches!(c, '[' | ']')).collect();
    let make = |n: &str| format!("[Uploading {n}…]()");
    let mut placeholder = make(&clean);
    let (stem, ext) = clean
        .rfind('.')
        .filter(|&i| i > 0)
        .map_or((clean.as_str(), ""), |i| clean.split_at(i));
    let mut k = 2;
    while exists(&placeholder) {
        placeholder = make(&format!("{stem} ({k}){ext}"));
        k += 1;
    }
    placeholder
}

/// 投票消息：问题一行，每个非空选项一行。换行会被服务器当成新的选项，所以合并成一行。
pub fn poll_message(question: &str, options: &[String]) -> String {
    let one_line = |text: &str| {
        text.split(['\n', '\r'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut lines = vec![format!("/poll {}", one_line(question))];
    lines.extend(
        options
            .iter()
            .map(|o| one_line(o))
            .filter(|o| !o.is_empty()),
    );
    lines.join("\n")
}

/// 任务列表（`/todo`）：整条消息要以它开头，所以加在最前面，光标停在标题处。
pub fn todo_edit(text: &str) -> MarkdownEdit {
    const PREFIX: &str = "/todo ";
    if text.starts_with(PREFIX.trim_end()) {
        let end = text.find('\n').unwrap_or(text.len());
        return MarkdownEdit::new(0, 0, String::new(), end, end);
    }
    MarkdownEdit::new(0, 0, format!("{PREFIX}\n"), PREFIX.len(), PREFIX.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(text: &str) -> EditorCore {
        EditorCore::new(text.to_owned(), false, Lookup::default())
    }

    #[test]
    fn replace_all_is_one_undo_step() {
        let mut c = core("旧内容");
        c.sel = Sel::caret(3);
        c.replace_all("新");
        assert_eq!(c.text(), "新");
        assert!(c.undo());
        assert_eq!(c.text(), "旧内容");
    }

    #[test]
    fn typing_merges_into_one_undo_step() {
        let mut c = core("");
        c.sync("a".into(), Sel::caret(1));
        c.sync("ab".into(), Sel::caret(2));
        assert!(c.undo());
        assert_eq!(c.text(), "");
        assert!(c.redo());
        assert_eq!(c.text(), "ab");
    }

    #[test]
    fn format_is_its_own_undo_step() {
        let mut c = core("abc");
        c.sel = Sel {
            start: 0,
            end: 3,
            cell: None,
        };
        c.format(FormatKind::Bold);
        assert_eq!(c.text(), "**abc**");
        c.undo();
        assert_eq!(c.text(), "abc");
        assert_eq!(
            c.sel,
            Sel {
                start: 0,
                end: 3,
                cell: None
            }
        );
    }

    #[test]
    fn reset_clears_history() {
        let mut c = core("a");
        c.sync("ab".into(), Sel::caret(2));
        c.reset("新".into());
        assert!(!c.undo());
        assert_eq!(c.sel, Sel::caret(3));
    }

    #[test]
    fn caret_on_table_enters_cell() {
        let mut c = core("");
        c.edit_active(|text, _, s, e| format::insert_table(text, s, e, 2, 2));
        assert_eq!(
            c.sel.cell,
            Some(CellPos {
                table: 0,
                row: 0,
                col: 0
            })
        );
    }

    #[test]
    fn cell_edit_keeps_trailing_space_while_typing() {
        let mut c = core("| a | b |\n| --- | --- |\n| c | d |");
        let pos = CellPos {
            table: 0,
            row: 1,
            col: 0,
        };
        c.sync_cell(pos, "c ".into(), 2, 2);
        assert_eq!(c.cell_value(pos), "c ");
        let blocks = c.blocks();
        let table = blocks.iter().find_map(|b| b.table.as_ref()).unwrap();
        assert!(table.rows[1][0][0].html.as_ref().unwrap().contains("c "));
        c.sync_cell(pos, "c x".into(), 3, 3);
        assert!(c.text().contains("| c x |"));
    }

    #[test]
    fn format_in_cell_targets_cell_value() {
        let mut c = core("| a | b |\n| --- | --- |\n| c | d |");
        c.sel = Sel {
            start: 0,
            end: 1,
            cell: Some(CellPos {
                table: 0,
                row: 1,
                col: 1,
            }),
        };
        c.format(FormatKind::Bold);
        assert!(c.text().ends_with("| **d** |"), "{}", c.text());
        assert_eq!(c.sel.cell.unwrap().col, 1);
        c.undo();
        assert!(c.text().ends_with("| d |"));
    }

    #[test]
    fn table_ops_keep_caret_in_cell() {
        let mut c = core("| a | b |\n| --- | --- |\n| c | d |");
        c.sel = Sel {
            start: 0,
            end: 0,
            cell: Some(CellPos {
                table: 0,
                row: 1,
                col: 0,
            }),
        };
        c.table_op(|t, caret| t.insert_row(caret, true));
        assert_eq!(
            c.sel.cell,
            Some(CellPos {
                table: 0,
                row: 2,
                col: 0
            })
        );
        c.table_op(|t, _| Some(t.delete_table()));
        assert_eq!(c.text(), "");
        assert_eq!(c.sel.cell, None);
    }

    #[test]
    fn utf16_selection_converts_to_bytes() {
        let c = core("a中😄b");
        assert_eq!(
            c.sel_from_utf16(2, 4, None),
            Sel {
                start: 4,
                end: 8,
                cell: None
            }
        );
    }

    #[test]
    fn atomic_blocks_get_room_for_caret() {
        let c = core("| a | b |\n| --- | --- |");
        let blocks = c.blocks();
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].html.as_ref().unwrap().contains("zphantom"));
        assert!(blocks[1].table.is_some());
        assert!(blocks[2].html.as_ref().unwrap().contains("zphantom"));
    }

    #[test]
    fn source_mode_shows_table_lines() {
        let mut c = core("| a | b |\n| --- | --- |");
        c.source = true;
        let blocks = c.blocks();
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|b| b.table.is_none()));
    }

    const QUOTE: &str =
        "@_**张三|5** [发言道](https://z.test/#narrow/near/9)：\n```quote\n你好\n```";

    #[test]
    fn quote_is_one_block_until_edited() {
        let mut c = core(QUOTE);
        let blocks = c.blocks();
        assert_eq!(blocks.len(), 3);
        assert!(blocks[1].html.as_ref().unwrap().contains("张三：你好"));
        c.edit_quote(0);
        assert!(c.blocks().len() > 3);
        assert!(c.set_sel(Sel::caret(0)));
        assert_eq!(c.blocks().len(), 3);
    }

    #[test]
    fn caret_inside_collapsed_quote_moves_to_near_end() {
        let mut c = core(QUOTE);
        c.apply(&MarkdownEdit::new(0, 0, String::new(), 3, 3));
        assert_eq!(c.sel, Sel::caret(0));
    }

    #[test]
    fn delete_quote_removes_its_newline() {
        let mut c = core(&format!("{QUOTE}\n后面"));
        c.delete_quote(0);
        assert_eq!(c.text(), "后面");
    }

    #[test]
    fn insert_beside_quote_starts_new_line() {
        let mut c = core(QUOTE);
        c.sel = Sel::caret(QUOTE.len());
        c.insert_block("文字");
        assert_eq!(c.text(), format!("{QUOTE}\n文字"));
    }

    #[test]
    fn prefix_removed_as_one() {
        let mut c = core("## 标题");
        c.sel = Sel::caret(3);
        c.remove_prefix();
        assert_eq!(c.text(), "标题");
        assert_eq!(c.sel, Sel::caret(0));
    }

    #[test]
    fn placeholder_replacement_shifts_selection() {
        let mut c = core("[Uploading a.png…]() 后");
        let len = c.text().len();
        c.sel = Sel::caret(len);
        c.replace_placeholder("[Uploading a.png…]()", "[a](u)");
        assert_eq!(c.text(), "[a](u) 后");
        assert_eq!(c.sel, Sel::caret("[a](u) 后".len()));
    }

    #[test]
    fn smart_insert_adds_spaces() {
        let mut c = core("你好");
        c.insert_smart("@**甲|1**");
        assert_eq!(c.text(), "你好 @**甲|1** ");
    }

    #[test]
    fn smart_insert_pads_blocks() {
        let mut c = core("前");
        c.insert_smart("a\nb");
        assert_eq!(c.text(), "前\n\na\nb\n\n");
    }

    #[test]
    fn stale_uploads_are_removed() {
        assert_eq!(strip_stale_uploads("a[Uploading x.png…]() b"), "ab");
        assert_eq!(strip_stale_uploads("[Uploading x"), "[Uploading x");
    }

    #[test]
    fn placeholders_stay_unique() {
        let existing = "[Uploading a.png…]()";
        assert_eq!(
            upload_placeholder("a.png", |p| p == existing),
            "[Uploading a (2).png…]()"
        );
        assert_eq!(
            upload_placeholder("[b].png", |_| false),
            "[Uploading b.png…]()"
        );
    }

    #[test]
    fn poll_keeps_each_option_on_one_line() {
        assert_eq!(
            poll_message("问\n题", &["甲".into(), " ".into(), "乙\n丙".into()]),
            "/poll 问 题\n甲\n乙 丙"
        );
    }

    #[test]
    fn todo_goes_first() {
        let edit = todo_edit("x");
        assert_eq!(edit.apply("x"), "/todo \nx");
        assert_eq!(edit.selection_start, 6);
        assert_eq!(
            todo_edit("/todo 标题\ny").selection_start,
            "/todo 标题".len()
        );
    }
}
