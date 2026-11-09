//! Tables in the composer: lookup, column widths, cell encoding and whole-table edits. Edits parse
//! the table into cells and rewrite the whole table (one undo step), leaving the cursor at the end
//! of the target cell text. The row number `row` does not count the separator row (0 is the header
//! row); the column number `column` is 0-based. When an operation is impossible (already at the
//! edge, the last two columns) it returns `None` and changes nothing.

use super::format::MarkdownEdit;
use super::{Line, LineKind, MarkdownModel, table_pipes};
use crate::util::is_blank;

/// A Zulip table has at least two columns (a separator row with only one column is not a table).
pub const MIN_COLUMNS: usize = 2;
pub const MAX_COLUMNS: usize = 20;
pub const MIN_ROWS: usize = 1;
pub const MAX_ROWS: usize = 50;

/// A line break inside a cell (a Markdown table row cannot really break).
pub const LINE_BREAK: &str = "<br />";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TableAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// `header_line` is the header row, followed by the separator row `separator_line`, ending at `last_line` (line numbers are all 0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkdownTable {
    pub header_line: usize,
    pub separator_line: usize,
    pub last_line: usize,
}

impl MarkdownTable {
    /// Number of rows (including the header row).
    pub fn row_count(&self) -> usize {
        self.last_line - self.separator_line + 1
    }
}

/// Finds the tables in the parse result. Tables inside a quote ("> " or a quote fence) do not count and are shown as the original text.
pub fn find_tables(model: &MarkdownModel) -> Vec<MarkdownTable> {
    let lines = &model.lines;
    let mut result = Vec::new();
    let mut i = 0;
    while i + 1 < lines.len() {
        if lines[i].kind != LineKind::TableRow || lines[i + 1].kind != LineKind::TableSeparator {
            i += 1;
            continue;
        }
        let mut last = i + 1;
        while lines
            .get(last + 1)
            .is_some_and(|l| l.kind == LineKind::TableRow)
        {
            last += 1;
        }
        if lines[i..=last].iter().all(|l| l.quote_depth == 0) {
            result.push(MarkdownTable {
                header_line: i,
                separator_line: i + 1,
                last_line: last,
            });
        }
        i = last + 1;
    }
    result
}

/// Column widths in the composer: columns share `available` equally; columns whose text does not fit the equal width take the width the text needs, and the other columns share the rest.
/// When the total does not fit, columns shrink proportionally to the amount they can shrink (text wraps automatically), but not below `minimum` (the longest unbreakable segment;
/// same as `required` when `None`); if it still does not fit at the minimum width every column takes its minimum width (the total width exceeds `available` and the table scrolls horizontally).
pub fn fit_columns(required: &[f64], available: f64, minimum: Option<&[f64]>) -> Vec<f64> {
    let minimum = minimum.unwrap_or(required);
    let total_required: f64 = required.iter().sum();
    let total_minimum: f64 = minimum.iter().sum();
    if total_minimum >= available {
        return minimum.to_vec();
    }
    if total_required > available {
        let ratio = (total_required - available) / (total_required - total_minimum);
        return required
            .iter()
            .zip(minimum)
            .map(|(r, m)| r - (r - m) * ratio)
            .collect();
    }
    let mut widths = required.to_vec();
    let mut open: Vec<usize> = (0..required.len()).collect();
    let mut remaining = available;
    while !open.is_empty() {
        let share = remaining / open.len() as f64;
        let (wide, rest): (Vec<usize>, Vec<usize>) =
            open.iter().partition(|&&c| required[c] > share);
        if wide.is_empty() {
            for c in open {
                widths[c] = share;
            }
            break;
        }
        remaining -= wide.iter().map(|&c| required[c]).sum::<f64>();
        open = rest;
    }
    widths
}

/// Original cell text -> text for editing: [`LINE_BREAK`] is a real line break, and `\|` is `|`.
pub fn decode_cell(source: &str) -> String {
    source.replace(LINE_BREAK, "\n").replace("\\|", "|")
}

/// Text for editing -> original cell text (the inverse of [`decode_cell`]).
pub fn encode_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', LINE_BREAK)
}

/// Markdown of a new table (the first row is the header, all cells are empty).
pub fn empty_table(columns: usize, rows: usize) -> String {
    let columns = columns.clamp(MIN_COLUMNS, MAX_COLUMNS);
    let rows = rows.clamp(MIN_ROWS, MAX_ROWS);
    let data = Data {
        rows: vec![vec![String::new(); columns]; rows],
        aligns: vec![TableAlign::Left; columns],
    };
    serialize(&data, false, 0, 0).0
}

/// Writes a table from the Markdown of its cells (the first row is the header). When `padded` is true, cells in the same column are padded with spaces to align.
/// Cells cannot contain a vertical bar or a line break: they are escaped as `\|` and `<br />`. The number of columns follows the row with the most cells.
pub fn table_from_cells(rows: &[Vec<&str>], aligns: &[TableAlign], padded: bool) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty() {
        return String::new();
    }
    let data = Data {
        rows: rows
            .iter()
            .map(|r| {
                let mut row: Vec<String> = r.iter().map(|c| escape_cell(c)).collect();
                row.resize(columns, String::new());
                row
            })
            .collect(),
        aligns: (0..columns)
            .map(|c| aligns.get(c).copied().unwrap_or_default())
            .collect(),
    };
    serialize(&data, padded, 0, 0).0
}

fn escape_cell(cell: &str) -> String {
    let cell = cell
        .trim()
        .replace("\r\n", LINE_BREAK)
        .replace(['\r', '\n'], LINE_BREAK);
    let mut out = String::with_capacity(cell.len());
    let mut previous = None;
    for c in cell.chars() {
        if c == '|' && previous != Some('\\') {
            out.push('\\');
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

struct Data {
    rows: Vec<Vec<String>>,
    aligns: Vec<TableAlign>,
}

impl Data {
    fn columns(&self) -> usize {
        self.aligns.len()
    }
}

/// Reading and modifying one table.
#[derive(Clone, Copy, Debug)]
pub struct TableEditor<'a> {
    model: &'a MarkdownModel,
    text: &'a str,
    table: MarkdownTable,
}

impl<'a> TableEditor<'a> {
    pub fn new(model: &'a MarkdownModel, text: &'a str, table: MarkdownTable) -> Self {
        Self { model, text, table }
    }

    fn row_line(&self, row: usize) -> &'a Line {
        let index = if row == 0 {
            self.table.header_line
        } else {
            self.table.separator_line + row
        };
        &self.model.lines[index]
    }

    /// A cell at the line start that does not begin with |.
    fn has_lead(&self, line: &Line, pipes: &[usize]) -> bool {
        pipes
            .first()
            .is_some_and(|&first| !is_blank(&self.text[line.content_start..first]))
    }

    /// The editing text of each cell (row, column); rows with too few columns are padded with empty cells.
    pub fn cell_values(&self) -> Vec<Vec<String>> {
        self.parse_data()
            .rows
            .iter()
            .map(|cells| cells.iter().map(|c| decode_cell(c)).collect())
            .collect()
    }

    /// Cursor position placed in this cell (the inverse of [`cell_at`](Self::cell_at)): table-editing operations use it to specify a cell.
    pub fn caret_of(&self, row: usize, column: usize) -> usize {
        let line = self.row_line(row);
        let pipes = table_pipes(self.text, line.content_start, line.end);
        let pipe = if self.has_lead(line, &pipes) {
            column.checked_sub(1)
        } else {
            Some(column)
        };
        match pipe {
            Some(pipe) if !pipes.is_empty() => pipes[pipe.min(pipes.len() - 1)] + 1,
            _ => line.content_start,
        }
    }

    /// Modifies one cell: `value` is the editing text, and the original text of the other cells is unchanged.
    pub fn set_cell(&self, row: usize, column: usize, value: &str) -> MarkdownEdit {
        let encoded = encode_cell(value);
        let line = self.row_line(row);
        let pipes = table_pipes(self.text, line.content_start, line.end);
        if !self.has_lead(line, &pipes) && column + 1 < pipes.len() {
            let start = pipes[column] + 1;
            let caret = start + 1 + encoded.len();
            return MarkdownEdit::new(
                start,
                pipes[column + 1] - start,
                format!(" {encoded} "),
                caret,
                caret,
            );
        }
        // This row lacks this cell, or does not begin and end with |: rewrite the whole table
        let mut data = self.parse_data();
        data.rows[row][column] = encoded;
        self.write(&data, row, column, false)
    }

    /// The cell containing the cursor (row, column).
    pub fn cell_at(&self, caret: usize) -> (usize, usize) {
        let table = &self.table;
        let index = self
            .model
            .line_index_at(caret)
            .unwrap_or(0)
            .clamp(table.header_line, table.last_line);
        // The separator row is not displayed and the cursor never stays on it; if it ever does, treat it as the header row
        let row = index.saturating_sub(table.separator_line);
        let line = &self.model.lines[if index == table.separator_line {
            table.header_line
        } else {
            index
        }];
        let pipes = table_pipes(self.text, line.content_start, line.end);
        let before = pipes.iter().filter(|p| **p < caret).count();
        let column = if self.has_lead(line, &pipes) {
            before
        } else {
            before.saturating_sub(1)
        };
        let columns = self.row_cells(line).len();
        (row, column.min(columns.saturating_sub(1)))
    }

    /// Alignment of each column (from the separator row).
    pub fn aligns(&self) -> Vec<TableAlign> {
        self.row_cells(&self.model.lines[self.table.separator_line])
            .iter()
            .map(|c| parse_align(c))
            .collect()
    }

    pub fn column_count(&self) -> usize {
        self.parse_data().columns()
    }

    /// Resize: when shrinking, the extra content is discarded; when growing, empty cells are added.
    pub fn resize(&self, caret: usize, columns: usize, rows: usize) -> MarkdownEdit {
        let columns = columns.clamp(MIN_COLUMNS, MAX_COLUMNS);
        let rows = rows.clamp(MIN_ROWS, MAX_ROWS);
        let mut data = self.parse_data();
        let (row, column) = self.cell_at(caret);
        data.rows.resize(rows, vec![String::new(); data.columns()]);
        data.aligns.resize(columns, TableAlign::Left);
        for cells in &mut data.rows {
            cells.resize(columns, String::new());
        }
        self.write(&data, row.min(rows - 1), column.min(columns - 1), false)
    }

    pub fn set_align(&self, caret: usize, align: TableAlign) -> Option<MarkdownEdit> {
        let mut data = self.parse_data();
        let (row, column) = self.cell_at(caret);
        if data.aligns[column] == align {
            return None;
        }
        data.aligns[column] = align;
        Some(self.write(&data, row, column, false))
    }

    pub fn insert_row(&self, caret: usize, below: bool) -> Option<MarkdownEdit> {
        let mut data = self.parse_data();
        let (row, column) = self.cell_at(caret);
        if data.rows.len() >= MAX_ROWS {
            return None;
        }
        let at = if below { row + 1 } else { row };
        data.rows.insert(at, vec![String::new(); data.columns()]);
        Some(self.write(&data, at, column, false))
    }

    pub fn insert_column(&self, caret: usize, right: bool) -> Option<MarkdownEdit> {
        let mut data = self.parse_data();
        let (row, column) = self.cell_at(caret);
        if data.columns() >= MAX_COLUMNS {
            return None;
        }
        let at = if right { column + 1 } else { column };
        data.aligns.insert(at, TableAlign::Left);
        for cells in &mut data.rows {
            cells.insert(at, String::new());
        }
        Some(self.write(&data, row, at, false))
    }

    pub fn move_row(&self, caret: usize, delta: isize) -> Option<MarkdownEdit> {
        let mut data = self.parse_data();
        let (row, column) = self.cell_at(caret);
        let target = row
            .checked_add_signed(delta)
            .filter(|t| *t < data.rows.len())?;
        data.rows.swap(row, target);
        Some(self.write(&data, target, column, false))
    }

    pub fn move_column(&self, caret: usize, delta: isize) -> Option<MarkdownEdit> {
        let mut data = self.parse_data();
        let (row, column) = self.cell_at(caret);
        let target = column
            .checked_add_signed(delta)
            .filter(|t| *t < data.columns())?;
        data.aligns.swap(column, target);
        for cells in &mut data.rows {
            cells.swap(column, target);
        }
        Some(self.write(&data, row, target, false))
    }

    /// Deletes the row at the cursor; after deleting the header row the next row becomes the header; when only one row remains, the whole table is deleted.
    pub fn delete_row(&self, caret: usize) -> MarkdownEdit {
        let mut data = self.parse_data();
        if data.rows.len() <= 1 {
            return self.delete_table();
        }
        let (row, column) = self.cell_at(caret);
        data.rows.remove(row);
        self.write(&data, row.min(data.rows.len() - 1), column, false)
    }

    pub fn delete_column(&self, caret: usize) -> Option<MarkdownEdit> {
        let mut data = self.parse_data();
        if data.columns() <= MIN_COLUMNS {
            return None;
        }
        let (row, column) = self.cell_at(caret);
        data.aligns.remove(column);
        for cells in &mut data.rows {
            cells.remove(column);
        }
        Some(self.write(&data, row, column.min(data.columns() - 1), false))
    }

    /// Deletes the whole table (together with the newline after it; at the end of the text, together with the newline before it).
    pub fn delete_table(&self) -> MarkdownEdit {
        let (mut start, mut end) = self.range();
        let bytes = self.text.as_bytes();
        if bytes.get(end) == Some(&b'\n') {
            end += 1;
        } else if start > 0 && bytes[start - 1] == b'\n' {
            start -= 1;
        }
        MarkdownEdit::new(start, end - start, String::new(), start, start)
    }

    /// Formats the source: cells in the same column are padded with spaces to align (a Chinese character counts as two columns).
    pub fn format(&self, caret: usize) -> MarkdownEdit {
        let (row, column) = self.cell_at(caret);
        self.write(&self.parse_data(), row, column, true)
    }

    /// The original text of the whole table.
    pub fn source(&self) -> &'a str {
        let (start, end) = self.range();
        &self.text[start..end]
    }

    fn range(&self) -> (usize, usize) {
        (
            self.model.lines[self.table.header_line].start,
            self.model.lines[self.table.last_line].end,
        )
    }

    fn parse_data(&self) -> Data {
        let mut aligns = self.aligns();
        let table = &self.table;
        let mut rows: Vec<Vec<String>> = (table.header_line..=table.last_line)
            .filter(|index| *index != table.separator_line)
            .map(|index| {
                self.row_cells(&self.model.lines[index])
                    .iter()
                    .map(|c| c.trim().to_owned())
                    .collect()
            })
            .collect();
        // The number of columns follows the row with the most cells; rows with too few are padded with empty cells
        let columns = rows.iter().map(Vec::len).fold(aligns.len(), usize::max);
        aligns.resize(columns, TableAlign::Left);
        for cells in &mut rows {
            cells.resize(columns, String::new());
        }
        Data { rows, aligns }
    }

    // The original cell texts of a row (including whitespace at both ends): when only whitespace precedes the first | or follows the last |, it is not a cell
    fn row_cells(&self, line: &Line) -> Vec<&'a str> {
        let mut cells = Vec::new();
        let mut start = line.content_start;
        for pipe in table_pipes(self.text, line.content_start, line.end) {
            cells.push(&self.text[start..pipe]);
            start = pipe + 1;
        }
        cells.push(&self.text[start..line.end]);
        if cells.len() > 1 && cells.last().is_some_and(|c| is_blank(c)) {
            cells.pop();
        }
        if cells.len() > 1 && is_blank(cells[0]) {
            cells.remove(0);
        }
        cells
    }

    fn write(&self, data: &Data, row: usize, column: usize, padded: bool) -> MarkdownEdit {
        let (start, end) = self.range();
        let (replacement, caret) = serialize(data, padded, row, column);
        MarkdownEdit::new(
            start,
            end - start,
            replacement,
            start + caret,
            start + caret,
        )
    }
}

fn parse_align(separator: &str) -> TableAlign {
    let s = separator.trim();
    match (s.starts_with(':'), s.ends_with(':')) {
        (true, true) => TableAlign::Center,
        (false, true) => TableAlign::Right,
        _ => TableAlign::Left,
    }
}

// Writes the table; also returns the position of the end of the (row, column) cell text in the written text
fn serialize(data: &Data, padded: bool, row: usize, column: usize) -> (String, usize) {
    let widths: Vec<usize> = (0..data.columns())
        .map(|c| {
            if padded {
                data.rows
                    .iter()
                    .map(|r| display_width(&r[c]))
                    .fold(3, usize::max)
            } else {
                0
            }
        })
        .collect();

    let mut out = String::new();
    let mut caret = 0;
    let write_row = |cells: &[String], index: usize, out: &mut String, caret: &mut usize| {
        for (c, cell) in cells.iter().enumerate() {
            out.push_str("| ");
            out.push_str(cell);
            if index == row && c == column {
                *caret = out.len();
            }
            out.push_str(&" ".repeat(widths[c].saturating_sub(display_width(cell))));
            out.push(' ');
        }
        out.push('|');
    };

    write_row(&data.rows[0], 0, &mut out, &mut caret);
    out.push('\n');
    for (align, width) in data.aligns.iter().zip(&widths) {
        out.push_str("| ");
        out.push_str(&separator_cell(*align, (*width).max(3)));
        out.push(' ');
    }
    out.push('|');
    for (index, cells) in data.rows.iter().enumerate().skip(1) {
        out.push('\n');
        write_row(cells, index, &mut out, &mut caret);
    }
    (out, caret)
}

fn separator_cell(align: TableAlign, width: usize) -> String {
    match align {
        TableAlign::Center => format!(":{}:", "-".repeat(width.saturating_sub(2).max(1))),
        TableAlign::Right => format!("{}:", "-".repeat(width.saturating_sub(1).max(1))),
        TableAlign::Left => "-".repeat(width),
    }
}

fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c >= '\u{2E80}' { 2 } else { 1 }).sum()
}

#[cfg(test)]
mod tests {
    use super::super::{NoResolver, parse};
    use super::*;

    const SAMPLE: &str = "| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |";
    const SMALL: &str = "| a | b |\n| --- | --- |\n| c | d |";

    fn single(model: &MarkdownModel) -> MarkdownTable {
        let tables = find_tables(model);
        assert_eq!(tables.len(), 1);
        tables[0]
    }

    // Operate with the cursor placed after the marker, and return the edited text with the cursor marked by the guillemet U+00AB; returns None if nothing changed
    fn run(
        text: &str,
        marker: &str,
        edit: impl Fn(TableEditor<'_>, usize) -> Option<MarkdownEdit>,
    ) -> Option<String> {
        let model = parse(text, &NoResolver);
        let editor = TableEditor::new(&model, text, single(&model));
        let caret = text.find(marker).unwrap() + marker.len();
        let change = edit(editor, caret)?;
        let mut result = change.apply(text);
        result.insert(change.selection_start, '«');
        Some(result)
    }

    fn run_ok(
        text: &str,
        marker: &str,
        edit: impl Fn(TableEditor<'_>, usize) -> MarkdownEdit,
    ) -> String {
        run(text, marker, |e, c| Some(edit(e, c))).unwrap()
    }

    fn with_editor<R>(text: &str, f: impl FnOnce(TableEditor<'_>) -> R) -> R {
        let model = parse(text, &NoResolver);
        f(TableEditor::new(&model, text, single(&model)))
    }

    #[test]
    fn finds_rows_after_header_and_separator() {
        let model = parse(
            "前\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n\n后",
            &NoResolver,
        );
        assert_eq!(
            single(&model),
            MarkdownTable {
                header_line: 2,
                separator_line: 3,
                last_line: 5
            }
        );
        assert_eq!(single(&model).row_count(), 3);
    }

    #[test]
    fn ignores_incomplete_or_quoted() {
        for text in ["| a |", "> | a | b |\n> | --- | --- |\n> | 1 | 2 |"] {
            assert!(find_tables(&parse(text, &NoResolver)).is_empty(), "{text}");
        }
    }

    #[test]
    fn cell_at() {
        for (marker, row, column) in [("a", 0, 0), ("b", 0, 1), ("1", 1, 0), ("4", 2, 1)] {
            let caret = SAMPLE.find(marker).unwrap() + 1;
            assert_eq!(
                with_editor(SAMPLE, |e| e.cell_at(caret)),
                (row, column),
                "{marker}"
            );
        }
    }

    #[test]
    fn insert_row_below() {
        assert_eq!(
            run(SAMPLE, "2", |e, c| e.insert_row(c, true)).unwrap(),
            "| a | b |\n| --- | --- |\n| 1 | 2 |\n|  | « |\n| 3 | 4 |"
        );
    }

    #[test]
    fn insert_column_right() {
        assert_eq!(
            run(SAMPLE, "a", |e, c| e.insert_column(c, true)).unwrap(),
            "| a | « | b |\n| --- | --- | --- |\n| 1 |  | 2 |\n| 3 |  | 4 |"
        );
    }

    #[test]
    fn move_row_follows_caret_and_stops_at_edge() {
        assert_eq!(
            run(SAMPLE, "4", |e, c| e.move_row(c, -1)).unwrap(),
            "| a | b |\n| --- | --- |\n| 3 | 4« |\n| 1 | 2 |"
        );
        assert_eq!(run(SAMPLE, "a", |e, c| e.move_row(c, -1)), None);
    }

    #[test]
    fn move_column_moves_alignment_too() {
        assert_eq!(
            run("| a | b |\n| --- | :---: |\n| 1 | 2 |", "b", |e, c| e
                .move_column(c, -1))
            .unwrap(),
            "| b« | a |\n| :-: | --- |\n| 2 | 1 |"
        );
    }

    #[test]
    fn resize() {
        // Discard content when shrinking
        assert_eq!(
            run_ok(
                "| a | b | c |\n| --- | --- | --- |\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |",
                "a",
                |e, c| e.resize(c, 2, 2)
            ),
            "| a« | b |\n| --- | --- |\n| 1 | 2 |"
        );
        // Add empty cells when growing
        assert_eq!(
            run_ok(SAMPLE, "a", |e, c| e.resize(c, 3, 4)),
            "| a« | b |  |\n| --- | --- | --- |\n| 1 | 2 |  |\n| 3 | 4 |  |\n|  |  |  |"
        );
        // At least two columns
        assert_eq!(
            run_ok(SAMPLE, "a", |e, c| e.resize(c, 1, 1)),
            "| a« | b |\n| --- | --- |"
        );
    }

    #[test]
    fn set_align() {
        let text = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        for (align, separator) in [
            (TableAlign::Center, "| --- | :-: |"),
            (TableAlign::Right, "| --- | --: |"),
        ] {
            assert_eq!(
                run(text, "b", |e, c| e.set_align(c, align)).unwrap(),
                format!("| a | b« |\n{separator}\n| 1 | 2 |")
            );
        }
        assert_eq!(
            run(SAMPLE, "b", |e, c| e.set_align(c, TableAlign::Left)),
            None
        );
    }

    #[test]
    fn delete_row() {
        assert_eq!(
            run_ok(SAMPLE, "a", |e, c| e.delete_row(c)),
            "| 1« | 2 |\n| --- | --- |\n| 3 | 4 |"
        );
        assert_eq!(
            run_ok("前\n| a | b |\n| --- | --- |\n后", "a", |e, c| e
                .delete_row(c)),
            "前\n«后"
        );
    }

    #[test]
    fn delete_column_stops_at_two_columns() {
        assert_eq!(run(SAMPLE, "a", |e, c| e.delete_column(c)), None);
        assert_eq!(
            run("| a | b | c |\n| --- | --- | --- |", "b", |e, c| e
                .delete_column(c))
            .unwrap(),
            "| a | c« |\n| --- | --- |"
        );
    }

    #[test]
    fn format_pads_by_display_width() {
        assert_eq!(
            run_ok("| 标题 | b |\n| --- | --- |\n| 1 | 2 |", "题", |e, c| e
                .format(c)),
            "| 标题« | b   |\n| ---- | --- |\n| 1    | 2   |"
        );
    }

    #[test]
    fn empty_clamps_to_minimum() {
        assert_eq!(empty_table(1, 0), "|  |  |\n| --- | --- |");
    }

    #[test]
    fn from_cells() {
        assert_eq!(
            table_from_cells(
                &[vec!["a", "b"], vec!["1"]],
                &[TableAlign::Left, TableAlign::Right],
                false
            ),
            "| a | b |\n| --- | --: |\n| 1 |  |"
        );
        assert_eq!(
            table_from_cells(
                &[vec!["姓名", "备注"], vec!["Alexander", "你好世界"]],
                &[TableAlign::Left, TableAlign::Center],
                true
            ),
            "| 姓名      | 备注     |\n| --------- | :------: |\n| Alexander | 你好世界 |"
        );
        assert_eq!(
            table_from_cells(&[vec!["a|b", "c\nd"], vec![r"e\|f", ""]], &[], false),
            "| a\\|b | c<br />d |\n| --- | --- |\n| e\\|f |  |"
        );
    }

    #[test]
    fn encode_and_decode_are_inverse() {
        for (value, source) in [("a\nb", "a<br />b"), ("c|d", "c\\|d"), ("plain", "plain")] {
            assert_eq!(encode_cell(value), source);
            assert_eq!(decode_cell(source), value);
        }
    }

    #[test]
    fn set_cell_changes_only_that_cell() {
        let edit = with_editor(SMALL, |e| e.set_cell(1, 1, "x\ny|z"));
        assert_eq!(
            edit.apply(SMALL),
            "| a | b |\n| --- | --- |\n| c | x<br />y\\|z |"
        );
    }

    #[test]
    fn set_cell_on_a_short_row_rewrites_the_table() {
        let text = "| a | b |\n| --- | --- |\n| c |";
        let edit = with_editor(text, |e| e.set_cell(1, 1, "x"));
        assert_eq!(edit.apply(text), "| a | b |\n| --- | --- |\n| c | x |");
    }

    #[test]
    fn cell_values_decode_and_fill_missing_cells() {
        let values = with_editor("| a<br />b | c\\|d |\n| --- | --- |\n| e |", |e| {
            e.cell_values()
        });
        assert_eq!(values[0], ["a\nb", "c|d"]);
        assert_eq!(values[1], ["e", ""]);
    }

    #[test]
    fn caret_of_is_the_inverse_of_cell_at() {
        for (row, column) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            let found = with_editor(SMALL, |e| e.cell_at(e.caret_of(row, column)));
            assert_eq!(found, (row, column));
        }
    }

    #[test]
    fn source_and_column_count() {
        let text = format!("前\n\n{SMALL}\n\n后");
        with_editor(&text, |e| {
            assert_eq!(e.source(), SMALL);
            assert_eq!(e.column_count(), 2);
        });
    }

    #[test]
    fn columns_share_the_width_equally_when_everything_fits() {
        assert_eq!(
            fit_columns(&[20.0, 50.0, 100.0], 300.0, None),
            [100.0, 100.0, 100.0]
        );
    }

    #[test]
    fn wide_column_takes_what_it_needs_and_the_others_share_the_rest() {
        assert_eq!(
            fit_columns(&[200.0, 20.0, 20.0], 300.0, None),
            [200.0, 50.0, 50.0]
        );
    }

    #[test]
    fn columns_keep_their_required_width_when_the_table_overflows() {
        assert_eq!(fit_columns(&[300.0, 200.0], 300.0, None), [300.0, 200.0]);
    }

    #[test]
    fn columns_shrink_towards_their_minimum_so_text_wraps() {
        assert_eq!(
            fit_columns(&[300.0, 100.0], 300.0, Some(&[100.0, 100.0])),
            [200.0, 100.0]
        );
    }

    #[test]
    fn columns_stop_at_their_minimum_and_the_table_scrolls() {
        assert_eq!(
            fit_columns(&[400.0, 100.0], 300.0, Some(&[250.0, 100.0])),
            [250.0, 100.0]
        );
    }
}
