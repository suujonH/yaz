//! Content model of the editor: Markdown source, selection, undo history, and the state of table
//! cells and quotes. No UI dependencies. The "current editor" is the cell holding the cursor, or
//! the whole document; formatting, insertion and completion all act on it. All offsets are bytes.

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
/// Consecutive typing is merged into one undo step.
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
    /// An increase means the program changed the content: any sync JS sent earlier but not yet
    /// received is void.
    pub rev: u64,
    pub source: bool,
    pub lookup: Lookup,
    /// Text of the cell being typed in: spaces at both ends are only layout in the source and are
    /// lost on parsing.
    live_cell: Option<(CellPos, String)>,
    /// Expanded quotes (by source) and how they look after sending.
    expanded: HashSet<String>,
    pub rendered_quotes: HashMap<String, String>,
    /// Start of the quote being edited.
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

    // --------- Current editor ---------

    fn table_editor(&self, index: usize) -> Option<TableEditor<'_>> {
        self.tables
            .get(index)
            .map(|t| TableEditor::new(&self.model, &self.text, *t))
    }

    /// Current text of the cell.
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

    /// Text of the editor holding the cursor.
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

    /// Make one modification to the current editor (one undo step).
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

    // The cursor is in the empty slot next to a table or a collapsed quote (displayed as its own
    // line, but that line does not exist in the source yet): create that line first
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
            || self.model.line_index_at(caret).is_some_and(|i| {
                self.code_fence_role(i) == Some(false)
                    && self.model.lines[i].end == caret
                    && (i + 1 == self.model.lines.len() || self.starts_atomic_block(i + 1))
            })
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

    /// Modify the document.
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

    /// The cursor landed on content displayed as a block: a table enters a cell, a collapsed quote
    /// moves to the nearer end.
    fn settle(&mut self) {
        if self.source || self.sel.cell.is_some() || !self.sel.is_collapsed() {
            return;
        }
        if self.skip_code_fences(None) {
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

    // --------- Changes from the DOM ---------

    /// The editor text changed. Returns whether the selection was moved (JS must be told).
    pub fn sync(&mut self, text: String, sel: Sel) -> bool {
        self.live_cell = None;
        if text != self.text {
            self.remember_typing();
            let (prefix, removed, inserted) = diff(&self.text, &text);
            let old_model = std::mem::replace(&mut self.model, parse(&text, &self.lookup));
            self.text = text;
            if let Some(anchor) = self.edit_quote
                && prefix <= anchor
            {
                self.edit_quote = Some(anchor + inserted - removed.min(anchor - prefix + removed));
            }
            self.reparse();
            if !self.source
                && let Some(edits) = lengthen_outer_fence(
                    &old_model,
                    &self.model,
                    &self.text,
                    sel.start,
                    inserted,
                    removed,
                )
            {
                // Inserted at the end of the fence runs, the later one first; the opening fence is
                // before the cursor, so the cursor moves by the characters added there
                let mut sel = sel;
                for (at, extra) in edits.iter().rev() {
                    self.text.insert_str(*at, extra);
                }
                sel.start += edits[0].1.len();
                sel.end += edits[0].1.len();
                self.reparse();
                self.sel = sel;
                self.settle();
                return true;
            }
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

    /// Selection moved. Editing ends when the cursor leaves the quote being edited, and then
    /// returns true (a re-render is needed).
    pub fn set_sel(&mut self, sel: Sel) -> bool {
        let from = self.sel.start;
        if sel.cell.is_none_or(|p| p.table < self.tables.len()) {
            if self.sel.cell != sel.cell {
                self.live_cell = None;
            }
            self.sel = sel;
        }
        self.last_typing = None;
        if self.skip_code_fences(Some(from)) {
            return true;
        }
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

    /// Toggle between rich text and source. The cursor stays on the same content.
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

    /// Select all in the current editor.
    pub fn select_all(&mut self) {
        self.sel.start = 0;
        self.sel.end = self.active_text().len();
        self.rev += 1;
    }

    /// User, channel or emoji data changed.
    pub fn set_lookup(&mut self, lookup: Lookup) {
        self.lookup = lookup;
        self.reparse();
    }

    // --------- Undo ---------

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

    /// Whole replacement (loading a draft, clearing after sending, starting / ending message
    /// editing): clears the undo history.
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

    // --------- Formatting ---------

    /// Formatting is not applied where it would break the structure: nothing in code, and no
    /// block formats (quote, lists, blocks, rule) in a table cell.
    pub fn format(&mut self, kind: FormatKind) {
        if !self.format_allowed(kind) {
            return;
        }
        self.edit_active(|text, model, s, e| format::apply(text, s, e, kind, Some(model)));
    }

    /// Whether `kind` can be applied at the cursor (see [`Self::format`]).
    pub fn format_allowed(&self, kind: FormatKind) -> bool {
        if self.caret_in_code() {
            return false;
        }
        self.sel.cell.is_none() || !is_block_format(kind)
    }

    pub fn heading(&mut self, level: u8) {
        if self.sel.cell.is_none() && !self.caret_in_code() {
            self.edit_active(|text, _, s, e| format::set_heading(text, s, e, level));
        }
    }

    pub fn is_active(&self, kind: FormatKind) -> bool {
        let text = self.active_text();
        let model = self.active_model(&text);
        format::is_active(&model, self.sel.start, kind)
    }

    /// Role of line `index` in a closed code block whose fence lines show nothing (rendered
    /// mode): `Some(true)` for the opening fence, `Some(false)` for the closing fence (it holds the
    /// language box).
    fn code_fence_role(&self, index: usize) -> Option<bool> {
        use markdown::{FenceKind, LineKind};
        let lines = &self.model.lines;
        let line = lines.get(index)?;
        match line.kind {
            LineKind::FenceOpen if line.fence == Some(FenceKind::Code) => lines[index + 1..]
                .iter()
                .find(|l| l.kind != LineKind::FenceBody)
                .is_some_and(|l| l.kind == LineKind::FenceClose)
                .then_some(true),
            LineKind::FenceClose if line.quote_depth == 0 => {
                let open = lines[..index]
                    .iter()
                    .rposition(|l| l.kind == LineKind::FenceOpen)?;
                (lines[open].fence == Some(FenceKind::Code)).then_some(false)
            }
            _ => None,
        }
    }

    /// Line `index` starts a block shown whole (table, collapsed quote, code block opening fence):
    /// an empty slot is shown before it when the previous block is shown whole too.
    fn starts_atomic_block(&self, index: usize) -> bool {
        self.tables.iter().any(|t| t.header_line == index)
            || self
                .quotes
                .iter()
                .any(|q| q.header_line == index && Some(q.start) != self.edit_quote)
            || self.code_fence_role(index) == Some(true)
    }

    /// The cursor never rests on the fence lines of a code block (they show nothing): it moves on
    /// in the direction it came from (`from`: the previous cursor offset; `None` keeps it inside
    /// the block). Returns whether it moved.
    fn skip_code_fences(&mut self, from: Option<usize>) -> bool {
        if self.source || self.sel.cell.is_some() || !self.sel.is_collapsed() {
            return false;
        }
        let Some(index) = self.model.line_index_at(self.sel.start) else {
            return false;
        };
        let Some(open) = self.code_fence_role(index) else {
            return false;
        };
        let lines = &self.model.lines;
        let line = &lines[index];
        let before = index.checked_sub(1).map(|i| lines[i].end);
        let after = lines.get(index + 1).map(|l| l.start);
        // The end of the closing fence is the place right after the block: the next line's
        // start, or the empty slot shown when nothing editable follows (the text ends, or a
        // table / quote / code block follows). The editor shows that slot for this offset, so
        // the cursor stays there; moving it on would bounce between the two forever.
        if !open && self.sel.start == line.end {
            return match after {
                Some(start) if !self.starts_atomic_block(index + 1) => {
                    self.sel = Sel::caret(start);
                    true
                }
                _ => false,
            };
        }
        let target = if open {
            // Coming up from inside the block: on to the line above it
            match (from.is_some_and(|f| f > line.end), before, after) {
                (true, Some(end), _) | (false, Some(end), None) => end,
                (_, _, Some(start)) => start,
                (_, None, None) => return false,
            }
        } else {
            // Coming down from inside the block: on to the line below it
            match (from.is_some_and(|f| f < line.start), after, before) {
                (true, Some(start), _) | (false, Some(start), None) => start,
                (_, _, Some(end)) => end,
                (_, None, None) => return false,
            }
        };
        self.sel = Sel::caret(target);
        true
    }

    /// Backspace at the start of the line right after a code block, or in the empty slot shown
    /// after a block that ends the text: the line break before it is kept (removing it would join
    /// the line to the closing fence); the cursor moves to the end of the block's last line
    /// instead.
    pub fn back_into_code(&mut self) {
        let Some(index) = self.model.line_index_at(self.sel.start) else {
            return;
        };
        let close = if self.code_fence_role(index) == Some(false) {
            index
        } else if index >= 1 && self.code_fence_role(index - 1) == Some(false) {
            index - 1
        } else {
            return;
        };
        if let Some(last) = close.checked_sub(1) {
            self.sel = Sel::caret(self.model.lines[last].end);
        }
    }

    /// Backspace at the start of the first line of a code block: remove the block's opening and
    /// closing fences together, keeping its content as plain lines (an empty block leaves one
    /// empty line). The cursor stays at the start of that content.
    pub fn unwrap_code_block(&mut self) {
        use markdown::LineKind;
        if self.source || self.sel.cell.is_some() || !self.sel.is_collapsed() {
            return;
        }
        let lines = &self.model.lines;
        let Some(index) = self.model.line_index_at(self.sel.start) else {
            return;
        };
        let Some(open) = index.checked_sub(1) else {
            return;
        };
        if lines[index].kind != LineKind::FenceBody || self.code_fence_role(open) != Some(true) {
            return;
        }
        let close = index
            + lines[index..]
                .iter()
                .position(|l| l.kind == LineKind::FenceClose)
                .unwrap_or(0);
        let start = lines[open].start;
        let body = self.text[lines[index].start..lines[close - 1].end].to_owned();
        let edit = MarkdownEdit::new(start, lines[close].end - start, body, start, start);
        self.apply(&edit);
    }

    /// Ctrl + Enter in a code block (rendered mode): the cursor leaves the block for the empty line
    /// right below it, adding that line when the next line is not empty (or there is none).
    pub fn exit_code_block(&mut self) {
        use markdown::LineKind;
        if self.source || self.sel.cell.is_some() {
            return;
        }
        let lines = &self.model.lines;
        let Some(index) = self.model.line_index_at(self.sel.end) else {
            return;
        };
        if lines[index].kind != LineKind::FenceBody {
            return;
        }
        let Some(close) = (index + 1..lines.len()).find(|&i| lines[i].kind != LineKind::FenceBody)
        else {
            return;
        };
        if self.code_fence_role(close) != Some(false) {
            return;
        }
        match lines.get(close + 1) {
            Some(next) if next.start == next.end && !self.starts_atomic_block(close + 1) => {
                self.sel = Sel::caret(next.start);
            }
            _ => {
                let at = lines[close].end;
                self.apply(&MarkdownEdit::new(
                    at,
                    0,
                    "
"
                    .to_owned(),
                    at + 1,
                    at + 1,
                ));
            }
        }
    }

    /// The cursor is in a code block or math block (its fences included) or indented code, where
    /// nothing inserted is formatted. Never in source mode or a table cell.
    pub fn caret_in_code(&self) -> bool {
        !self.source && self.caret_line_is_code()
    }

    /// The cursor's line is code (see [`Self::caret_in_code`]), in either mode: the toolbar keeps
    /// the format buttons collapsed there, so switching back from source mode does not expand
    /// them first.
    pub fn caret_line_is_code(&self) -> bool {
        use markdown::{FenceKind, LineKind};
        if self.sel.cell.is_some() {
            return false;
        }
        let Some(line) = self
            .model
            .line_index_at(self.sel.start)
            .and_then(|i| self.model.lines.get(i))
        else {
            return false;
        };
        line.kind == LineKind::IndentedCode
            || (matches!(
                line.kind,
                LineKind::FenceOpen | LineKind::FenceBody | LineKind::FenceClose
            ) && matches!(line.fence, Some(FenceKind::Code | FenceKind::Math)))
    }

    pub fn heading_level(&self) -> u8 {
        if self.sel.cell.is_some() {
            return 0;
        }
        format::heading_level_at(&self.model, self.sel.start)
    }

    /// Insert at the cursor (replacing the selected part).
    pub fn insert(&mut self, markdown: &str) {
        let markdown = markdown.to_owned();
        self.edit_active(|_, _, s, e| {
            let caret = s + markdown.len();
            MarkdownEdit::new(s, e - s, markdown, caret, caret)
        });
    }

    /// Replace all content (one undo step). Also replaces the whole content when the cursor is in a
    /// table cell or a quote.
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

    /// A candidate reply: first removes `previous` (the candidate appended before, its last
    /// occurrence; nothing when the user has changed it), then appends `text` to the end unless the
    /// content already contains it. One undo step; the cursor goes to the end.
    pub fn append_candidate(&mut self, previous: Option<&str>, text: &str) {
        let mut content = self.text.clone();
        if let Some(previous) = previous.filter(|p| !p.is_empty())
            && let Some(at) = content.rfind(previous)
        {
            content.replace_range(at..at + previous.len(), "");
        }
        if !content.contains(text) {
            content.push_str(text);
        }
        if content != self.text {
            self.replace_all(&content);
        } else {
            let end = self.text.len();
            self.set_sel(Sel::caret(end));
        }
    }

    /// Content put in by paste or canned replies: a line break is appended at the end for
    /// multi-line content.
    pub fn insert_block(&mut self, markdown: &str) {
        self.insert(&format::with_trailing_line(markdown));
    }

    /// Insert text, adding spaces before and after when needed (mentions, plugin fill-ins);
    /// multi-line content becomes its own paragraph.
    pub fn insert_smart(&mut self, markdown: &str) {
        // Into code nothing is inserted (it would only be code text, and a fence in it could end
        // the block); into a table cell nothing multi-line (it would break the row): such content
        // goes below the block instead
        let redirect = self.caret_in_code() || (self.sel.cell.is_some() && markdown.contains('\n'));
        if redirect && self.insert_below_block(markdown) {
            return;
        }
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

    /// Images, files and the like: inserted at the cursor like [`Self::insert_smart`], except in
    /// code or a table, where they go below the code block / table.
    pub fn insert_attachment(&mut self, markdown: &str) {
        if !self.insert_below_block(markdown) {
            self.insert_smart(markdown);
        }
    }

    /// Range `[start, end)` of the structure around the cursor that inserted content must not go
    /// into, and whether it is a table: the table of the cell being edited, or the code / math
    /// block (fences included; an unclosed one runs to the end) or run of indented code holding
    /// the cursor. `None` elsewhere and in source mode.
    fn protected_block(&self) -> Option<(usize, usize, bool)> {
        use markdown::LineKind;
        if self.source {
            return None;
        }
        if let Some(pos) = self.sel.cell {
            let t = self.tables.get(pos.table)?;
            return Some((
                self.model.lines[t.header_line].start,
                self.model.lines[t.last_line].end,
                true,
            ));
        }
        if !self.caret_in_code() {
            return None;
        }
        let lines = &self.model.lines;
        let index = self.model.line_index_at(self.sel.start)?;
        let (first, last) = if lines[index].kind == LineKind::IndentedCode {
            let same = |i: &usize| lines[*i].kind == LineKind::IndentedCode;
            let first = (0..index).rev().take_while(same).last().unwrap_or(index);
            let last = (index + 1..lines.len())
                .take_while(same)
                .last()
                .unwrap_or(index);
            (first, last)
        } else {
            let first = if lines[index].kind == LineKind::FenceOpen {
                index
            } else {
                lines[..index]
                    .iter()
                    .rposition(|l| l.kind == LineKind::FenceOpen)
                    .unwrap_or(index)
            };
            let last = if lines[index].kind == LineKind::FenceClose {
                index
            } else {
                lines[index..]
                    .iter()
                    .position(|l| l.kind == LineKind::FenceClose)
                    .map_or(lines.len() - 1, |p| index + p)
            };
            (first, last)
        };
        Some((lines[first].start, lines[last].end, false))
    }

    /// Insert `markdown` on its own line right below the protected structure around the cursor
    /// (see [`Self::protected_block`]): a line break is added when there is none; below a table an
    /// empty line too, so the content does not become a table row. The cursor stays where it was
    /// (in the block or cell). Returns false (nothing done) when the cursor is not in such a
    /// structure.
    fn insert_below_block(&mut self, markdown: &str) -> bool {
        let Some((_, end, table)) = self.protected_block() else {
            return false;
        };
        let block = markdown.trim_end_matches('\n');
        let mut padded = format::pad_block(&self.text, end, end, block, if table { 2 } else { 1 });
        if end == self.text.len() {
            padded.truncate(padded.trim_end_matches('\n').len());
        }
        // Inserted after the block, so the cursor's offset (or cell) is still valid
        let (sel, live_cell) = (self.sel, self.live_cell.take());
        self.apply(&MarkdownEdit::new(end, 0, padded, sel.start, sel.end));
        self.sel = sel;
        self.live_cell = live_cell;
        true
    }

    /// Replace the placeholder with the upload result. Does nothing if the user already deleted the
    /// placeholder.
    pub fn replace_placeholder(&mut self, placeholder: &str, replacement: &str) {
        let Some(start) = self.text.find(placeholder) else {
            return;
        };
        // When deleting the placeholder, also delete the spaces added at insertion
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

    /// Delete a span of the document (a blank line next to a table).
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

    /// Line break: lists and quotes continue the line-start marker (see [`markup::newline`]).
    pub fn newline(&mut self) {
        if self.sel.cell.is_some() {
            return;
        }
        self.edit_active(markup::newline);
    }

    /// Enter at the end of an unclosed opening fence line (` ``` ` or ` ```language `): complete it
    /// into a code block. Returns false when not at such a position.
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

    /// Change the code block language on the opening fence of line `open_line` to `language`.
    pub fn set_code_language(&mut self, open_line: usize, language: &str) {
        if let Some(edit) =
            code_language::set_language(&self.text, &self.model, open_line, language)
        {
            self.apply(&edit);
        }
    }

    /// The quote, heading and list markers at the start of a line are one unit: Backspace after
    /// them deletes the innermost one. When the cursor is not after a marker it is an ordinary
    /// Backspace (the editor decides from the text and sends it; the parse here is final).
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

    // --------- Tables ---------

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

    /// Modify the table holding the cursor; afterwards the cursor is at the end of the target cell.
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

    /// Insert an empty paragraph above or below the table holding the cursor.
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

    // --------- Quotes ---------

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

    /// Rendered HTML of an expanded quote; `None` if not rendered yet.
    pub fn rendered_quote(&self, index: usize) -> Option<&str> {
        let quote = self.quotes.get(index)?;
        self.rendered_quotes
            .get(&self.quote_src(quote))
            .map(String::as_str)
    }

    /// Quotes that are expanded but have no after-sending look yet: (the whole source, the quote
    /// content).
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

    // --------- Rendering ---------

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
            // Fence lines of a code block are whole blocks: the cursor goes beside them (an
            // empty slot is shown after a block that ends the text)
            let atomic = !self.source && self.code_fence_role(i).is_some();
            blocks.push((Block::line(ctx.line_html(i, "")), atomic));
            i += 1;
        }
        if blocks.is_empty() {
            blocks.push((
                Block::line(r#"<div class="zl"><br></div>"#.to_owned()),
                false,
            ));
        }
        // Blocks need places on both sides where the cursor can go
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

    /// Displayed text (for copying to other programs); in source mode what is displayed is the
    /// source.
    pub fn plain_text(&self, markdown: &str) -> String {
        if self.source {
            return markdown.to_owned();
        }
        markdown::to_plain_text(markdown, &self.lookup, &|name| {
            self.lookup.emoji.get(name).and_then(|e| e.text.clone())
        })
    }

    /// UTF-16 offsets of the selection (for JS).
    pub fn sel_utf16(&self) -> (usize, usize) {
        let text = self.active_text();
        (
            offsets::to_utf16(&text, self.sel.start),
            offsets::to_utf16(&text, self.sel.end),
        )
    }

    /// Convert the selection sent by JS to byte offsets.
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

/// The fence run at the start of a fence line: byte offset of its end, the fence character and
/// its length. `None` when the line does not start with ``` or ~~~.
fn fence_run(text: &str, line: &markdown::Line) -> Option<(usize, char, usize)> {
    let content = &text[line.content_start.min(line.end)..line.end];
    let trimmed = content.trim_start_matches(' ');
    let mark = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = trimmed.chars().take_while(|c| *c == mark).count();
    let end = line.end - trimmed.len() + len;
    (len >= 3).then_some((end, mark, len))
}

/// A line typed inside a code block that now ends the block early (` ``` ` typed as code): it was a
/// body line of a code / math fence before the change and closes that fence after it, which would
/// turn the old closing fence into the start of a new block. Returns the insertions that lengthen
/// the outer fence (opening and closing runs, in text order) to one more than the typed run, so the
/// typed line stays code. `inserted` / `removed` are the byte lengths of the change before `caret`'s
/// line ends. `None` when nothing needs to change.
fn lengthen_outer_fence(
    old: &MarkdownModel,
    new: &MarkdownModel,
    text: &str,
    caret: usize,
    inserted: usize,
    removed: usize,
) -> Option<Vec<(usize, String)>> {
    use markdown::{FenceKind, LineKind};
    let index = new.line_index_at(caret)?;
    let line = &new.lines[index];
    if line.kind != LineKind::FenceClose {
        return None;
    }
    let old_index = old.line_index_at(line.start)?;
    let old_line = &old.lines[old_index];
    if old_line.kind != LineKind::FenceBody
        || !matches!(old_line.fence, Some(FenceKind::Code | FenceKind::Math))
    {
        return None;
    }
    let open = old.lines[..old_index]
        .iter()
        .rposition(|l| l.kind == LineKind::FenceOpen)?;
    let close = old_index
        + 1
        + old.lines[old_index + 1..]
            .iter()
            .position(|l| l.kind == LineKind::FenceClose)?;
    // The opening fence is before the change (same offsets); the closing one after it (shifted)
    let open_line = &new.lines[new.line_index_at(old.lines[open].start)?];
    let (open_end, mark, outer) = fence_run(text, open_line)?;
    let (_, inner_mark, inner) = fence_run(text, line)?;
    let close_start = (old.lines[close].start + inserted).checked_sub(removed)?;
    let close_line = &new.lines[new.line_index_at(close_start)?];
    let (close_end, close_mark, _) = fence_run(text, close_line)?;
    if inner_mark != mark || close_mark != mark || inner < outer {
        return None;
    }
    let extra: String = std::iter::repeat_n(mark, inner + 1 - outer).collect();
    Some(vec![(open_end, extra.clone()), (close_end, extra)])
}

/// Block formats: they change whole lines, so they cannot go into a table cell.
fn is_block_format(kind: FormatKind) -> bool {
    matches!(
        kind,
        FormatKind::Quote
            | FormatKind::BulletedList
            | FormatKind::NumberedList
            | FormatKind::CodeBlock
            | FormatKind::MathBlock
            | FormatKind::QuoteBlock
            | FormatKind::Spoiler
            | FormatKind::Rule
    )
}

// The differing part of two texts: length of the common prefix, length removed, length added
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

/// Remove the placeholder left by an upload that did not finish last time (`[Uploading name...]()`,
/// optionally followed by one space).
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

/// Upload placeholder (same as the web app); files with the same name get a number so it stays
/// unique.
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

/// Poll message: the question on one line, each non-empty option on one line. A line break would be
/// treated by the server as a new option, so they are merged into one line.
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

/// Task list (`/todo`): the whole message must start with it, so it is added at the very front,
/// with the cursor at the title.
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
    fn candidate_replaces_previous_and_is_not_repeated() {
        let mut c = core("我想说：");
        c.append_candidate(None, "好的");
        assert_eq!(c.text(), "我想说：好的");
        assert_eq!(c.sel, Sel::caret(c.text().len()));
        c.append_candidate(Some("好的"), "好的");
        assert_eq!(c.text(), "我想说：好的");
        c.append_candidate(Some("好的"), "明天再看");
        assert_eq!(c.text(), "我想说：明天再看");
        // The user changed the earlier candidate: it is kept.
        let mut c = core("明天再看看");
        c.append_candidate(Some("明天再看吧"), "收到");
        assert_eq!(c.text(), "明天再看看收到");
        assert!(c.undo());
        assert_eq!(c.text(), "明天再看看");
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
    fn inserts_go_below_code_and_tables() {
        // In a code block: below the closing fence, on a new line
        let mut c = EditorCore::new("```\nab\n```".into(), false, Lookup::default());
        c.set_sel(Sel::caret(5));
        c.insert_attachment("![a](u)");
        assert_eq!(c.text(), "```\nab\n```\n![a](u)");
        // The cursor stays in the block
        assert_eq!(c.sel.start, 5);
        // A quote typed into a code block goes below it too, and the text after it stays apart
        let mut c = EditorCore::new("```\nab\n```\nnext".into(), false, Lookup::default());
        c.set_sel(Sel::caret(5));
        c.insert_smart("> q\n> r");
        assert_eq!(c.text(), "```\nab\n```\n> q\n> r\nnext");
        // Formatting does nothing in code
        c.set_sel(Sel::caret(5));
        c.format(FormatKind::Bold);
        c.heading(1);
        assert_eq!(c.text(), "```\nab\n```\n> q\n> r\nnext");
        // In a table cell: below the table, after an empty line
        let mut c = EditorCore::new("|a|b|\n|-|-|\n|1|2|".into(), false, Lookup::default());
        c.set_sel(Sel {
            start: 0,
            end: 0,
            cell: Some(CellPos {
                table: 0,
                row: 1,
                col: 0,
            }),
        });
        c.insert_attachment("![a](u)");
        assert_eq!(c.text(), "|a|b|\n|-|-|\n|1|2|\n\n![a](u)");
        // The cursor stays in the cell
        assert_eq!(
            c.sel.cell,
            Some(CellPos {
                table: 0,
                row: 1,
                col: 0
            })
        );
    }

    #[test]
    fn slot_between_code_block_and_quote_holds_the_cursor() {
        let quote = "@_**A|1** [said](http://x/near/1):\n```quote\nhi\n```";
        let mut c = EditorCore::new("```\nab\n```".into(), false, Lookup::default());
        c.set_sel(Sel::caret(5));
        // A quote inserted in the block goes below it, the cursor stays in the block
        c.insert_smart(quote);
        assert_eq!(c.text(), format!("```\nab\n```\n{quote}"));
        assert_eq!(c.sel.start, 5);
        // The slot between the block and the quote keeps the cursor (no bouncing)
        assert!(!c.set_sel(Sel::caret(10)));
        assert_eq!(c.sel.start, 10);
        // Typing there starts its own line
        c.insert("x");
        assert_eq!(c.text(), format!("```\nab\n```\nx\n{quote}"));
        // Back into the block is still possible
        assert!(!c.set_sel(Sel::caret(5)));
        assert_eq!(c.sel.start, 5);
    }

    #[test]
    fn slot_after_code_block_is_not_text() {
        // Nothing is added to the text for the slot shown after a block ending it
        let mut c = EditorCore::new("```\nab\n```".into(), false, Lookup::default());
        assert_eq!(c.text(), "```\nab\n```");
        // The cursor may rest in that slot (the end of the text)
        c.set_sel(Sel::caret(6));
        c.set_sel(Sel::caret(10));
        assert_eq!(c.sel.start, 10);
        // Inserting there starts a new line
        c.insert("x");
        assert_eq!(c.text(), "```\nab\n```\nx");
        // Backspace in the slot goes back into the block
        let mut c = EditorCore::new("```\nab\n```".into(), false, Lookup::default());
        c.set_sel(Sel::caret(10));
        c.back_into_code();
        assert_eq!(c.sel.start, 6);
    }

    #[test]
    fn caret_skips_code_fences() {
        let mut c = EditorCore::new("a\n```\nb\n```\nc".into(), false, Lookup::default());
        // Up from the code line onto the opening fence: on to the line above
        c.set_sel(Sel::caret(6));
        c.set_sel(Sel::caret(3));
        assert_eq!(c.sel.start, 1);
        // Down from the line above onto the opening fence: into the block
        c.set_sel(Sel::caret(3));
        assert_eq!(c.sel.start, 6);
        // Down from the code line onto the closing fence: on to the line below
        c.set_sel(Sel::caret(9));
        assert_eq!(c.sel.start, 12);
        // Up from the line below onto the closing fence: into the block
        c.set_sel(Sel::caret(9));
        assert_eq!(c.sel.start, 7);
    }

    #[test]
    fn ctrl_enter_leaves_code_block() {
        // No line after the block: one is added and the cursor goes there
        let mut c = EditorCore::new(
            "```
ab
```"
            .into(),
            false,
            Lookup::default(),
        );
        c.set_sel(Sel::caret(5));
        c.exit_code_block();
        assert_eq!(
            c.text(),
            "```
ab
```
"
        );
        assert_eq!(c.sel.start, 11);
        // An empty line after the block is used as it is
        let mut c = EditorCore::new(
            "```
ab
```

x"
            .into(),
            false,
            Lookup::default(),
        );
        c.set_sel(Sel::caret(5));
        c.exit_code_block();
        assert_eq!(
            c.text(),
            "```
ab
```

x"
        );
        assert_eq!(c.sel.start, 11);
        // Text right after the block: an empty line is added before it
        let mut c = EditorCore::new(
            "```
ab
```
x"
            .into(),
            false,
            Lookup::default(),
        );
        c.set_sel(Sel::caret(5));
        c.exit_code_block();
        assert_eq!(
            c.text(),
            "```
ab
```

x"
        );
        assert_eq!(c.sel.start, 11);
    }

    #[test]
    fn backspace_unwraps_code_block() {
        let mut c = EditorCore::new("a\n```\nb\nc\n```\nd".into(), false, Lookup::default());
        c.set_sel(Sel::caret(6));
        c.unwrap_code_block();
        assert_eq!(c.text(), "a\nb\nc\nd");
        assert_eq!(c.sel.start, 2);
        let mut c = EditorCore::new("```\n\n```".into(), false, Lookup::default());
        c.set_sel(Sel::caret(4));
        c.unwrap_code_block();
        assert_eq!(c.text(), "");
    }

    #[test]
    fn fence_typed_inside_code_stays_code() {
        let mut c = EditorCore::new(
            "```
ab
```"
            .into(),
            false,
            Lookup::default(),
        );
        // Type the third backtick on the body line
        c.sync(
            "```
``
ab
```"
            .into(),
            Sel::caret(6),
        );
        c.sync(
            "```
```
ab
```"
            .into(),
            Sel::caret(7),
        );
        assert_eq!(
            c.text(),
            "````
```
ab
````"
        );
        assert_eq!(c.sel.start, 8);
        // A closed block's own closing fence is left alone
        let mut c = EditorCore::new(
            "```
ab
``"
            .into(),
            false,
            Lookup::default(),
        );
        c.sync(
            "```
ab
```"
            .into(),
            Sel::caret(9),
        );
        assert_eq!(
            c.text(),
            "```
ab
```"
        );
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
