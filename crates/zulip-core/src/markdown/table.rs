//! 输入框中的表格：查找、列宽、单元格编码与整表修改。
//! 修改时解析成单元格，改完整个表格重新写出（一步撤销），光标放在目标单元格文字的末尾。
//! 行号 `row` 不算分隔行（0 为标题行），列号 `column` 从 0 起。
//! 操作不了时（已经在边上、最后的两列）返回 `None`，不修改。

use super::format::MarkdownEdit;
use super::{Line, LineKind, MarkdownModel, table_pipes};
use crate::util::is_blank;

/// Zulip 的表格至少两列（分隔行只有一列时不是表格）。
pub const MIN_COLUMNS: usize = 2;
pub const MAX_COLUMNS: usize = 20;
pub const MIN_ROWS: usize = 1;
pub const MAX_ROWS: usize = 50;

/// 单元格中的换行（Markdown 表格的一行不能真的换行）。
pub const LINE_BREAK: &str = "<br />";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TableAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// `header_line` 为标题行，紧接着是分隔行 `separator_line`，到 `last_line` 结束（行号都从 0 起）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkdownTable {
    pub header_line: usize,
    pub separator_line: usize,
    pub last_line: usize,
}

impl MarkdownTable {
    /// 行数（含标题行）。
    pub fn row_count(&self) -> usize {
        self.last_line - self.separator_line + 1
    }
}

/// 找出解析结果中的表格。引用（"> " 或 quote 围栏）中的表格不算，按原文显示。
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

/// 输入框中的列宽：各列平分 `available`；文字放不下平分宽度的列取文字需要的宽度，其它列平分剩下的。
/// 全部加起来放不下时，各列按能缩小的量同比例缩小（文字自动换行），但不小于 `minimum`（最长的不能断开的一段；
/// `None` 时与 `required` 相同）；到了最小宽度还放不下时每列取最小宽度（总宽度超过 `available`，表格横向滚动）。
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

/// 单元格的原文 → 编辑时的文字：[`LINE_BREAK`] 是真的换行，`\|` 是 `|`。
pub fn decode_cell(source: &str) -> String {
    source.replace(LINE_BREAK, "\n").replace("\\|", "|")
}

/// 编辑时的文字 → 单元格的原文（[`decode_cell`] 的逆操作）。
pub fn encode_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', LINE_BREAK)
}

/// 新表格的 Markdown（第一行为标题，单元格都是空的）。
pub fn empty_table(columns: usize, rows: usize) -> String {
    let columns = columns.clamp(MIN_COLUMNS, MAX_COLUMNS);
    let rows = rows.clamp(MIN_ROWS, MAX_ROWS);
    let data = Data {
        rows: vec![vec![String::new(); columns]; rows],
        aligns: vec![TableAlign::Left; columns],
    };
    serialize(&data, false, 0, 0).0
}

/// 由单元格的 Markdown 写出表格（第一行为标题）。`padded` 为 true 时同一列补空格对齐。
/// 单元格不能含竖线和换行：转义为 `\|` 和 `<br />`。列数以最多的一行为准。
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

/// 一个表格的读取与修改。
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

    /// 行首有不是 | 开头的单元格。
    fn has_lead(&self, line: &Line, pipes: &[usize]) -> bool {
        pipes
            .first()
            .is_some_and(|&first| !is_blank(&self.text[line.content_start..first]))
    }

    /// 每个单元格编辑时的文字（行、列）；列数不够的行补空。
    pub fn cell_values(&self) -> Vec<Vec<String>> {
        self.parse_data()
            .rows
            .iter()
            .map(|cells| cells.iter().map(|c| decode_cell(c)).collect())
            .collect()
    }

    /// 放在这个单元格中的光标位置（[`cell_at`](Self::cell_at) 的逆操作）：编辑表格的操作用它指定单元格。
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

    /// 修改一个单元格：`value` 是编辑时的文字，其它单元格的原文不变。
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
        // 这一行没有这一格，或者不是 | 开头结尾：整个表格重新写出
        let mut data = self.parse_data();
        data.rows[row][column] = encoded;
        self.write(&data, row, column, false)
    }

    /// 光标所在的单元格（行、列）。
    pub fn cell_at(&self, caret: usize) -> (usize, usize) {
        let table = &self.table;
        let index = self
            .model
            .line_index_at(caret)
            .unwrap_or(0)
            .clamp(table.header_line, table.last_line);
        // 分隔行不显示，光标不会停在上面；万一在，算作标题行
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

    /// 每列的对齐方式（来自分隔行）。
    pub fn aligns(&self) -> Vec<TableAlign> {
        self.row_cells(&self.model.lines[self.table.separator_line])
            .iter()
            .map(|c| parse_align(c))
            .collect()
    }

    pub fn column_count(&self) -> usize {
        self.parse_data().columns()
    }

    /// 调整大小：缩小时多出来的内容直接丢弃，放大时补空单元格。
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

    /// 删除光标所在的行；标题行删除后下一行成为标题；只剩一行时删除整个表格。
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

    /// 删除整个表格（连同它后面的换行；在文字末尾时连同前面的换行）。
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

    /// 格式化源码：同一列补空格对齐（中文按两格）。
    pub fn format(&self, caret: usize) -> MarkdownEdit {
        let (row, column) = self.cell_at(caret);
        self.write(&self.parse_data(), row, column, true)
    }

    /// 整个表格的原文。
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
        // 列数以最多的一行为准；不够的补空
        let columns = rows.iter().map(Vec::len).fold(aligns.len(), usize::max);
        aligns.resize(columns, TableAlign::Left);
        for cells in &mut rows {
            cells.resize(columns, String::new());
        }
        Data { rows, aligns }
    }

    // 一行的单元格原文（含两端空白）：第一个 | 之前和最后一个 | 之后只有空白时不是单元格
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

// 写出表格；同时返回 (row, column) 单元格文字末尾在写出的文字中的位置
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

    // 把光标放在 marker 之后操作，返回修改后的文字，光标用 « 标出；没有修改返回 None
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
        // 缩小时丢弃内容
        assert_eq!(
            run_ok(
                "| a | b | c |\n| --- | --- | --- |\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |",
                "a",
                |e, c| e.resize(c, 2, 2)
            ),
            "| a« | b |\n| --- | --- |\n| 1 | 2 |"
        );
        // 放大时补空单元格
        assert_eq!(
            run_ok(SAMPLE, "a", |e, c| e.resize(c, 3, 4)),
            "| a« | b |  |\n| --- | --- | --- |\n| 1 | 2 |  |\n| 3 | 4 |  |\n|  |  |  |"
        );
        // 至少两列
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
