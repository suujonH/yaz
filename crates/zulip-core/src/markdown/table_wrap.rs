//! 输入框表格中单元格的自动换行：只用于显示，文档不变。只在空格（全角、半角）和
//! 左括号、减号、问号、感叹号、逗号、冒号（全角、半角）、全角句号处断行；半角句号可能是小数点，不断行。

const SPACES: &str = " 　";
const OPEN_BRACKETS: &str = "[(<{［（＜｛";
const BREAK_AFTER: &str = "-－?？!！,，:：。";

/// 断行点的种类，决定断行处的那个字符怎么显示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftBreakKind {
    /// 空格（可以是连续的几个）：留在上一行的末尾，不显示，光标不能停在其中。
    Space,
    /// 符号留在上一行的末尾，在它之后断行。
    After,
    /// 左括号到下一行的开头，在它之前断行。
    Before,
}

/// 可以断行的位置：`[start, start + len)` 是相关的字符（行内字节偏移）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftBreak {
    pub start: usize,
    pub len: usize,
    pub kind: SoftBreakKind,
}

impl SoftBreak {
    /// 上一行显示的文字到这里结束。
    pub fn text_end(&self) -> usize {
        match self.kind {
            SoftBreakKind::After => self.start + self.len,
            _ => self.start,
        }
    }

    /// 下一行显示的文字从这里开始。
    pub fn next_start(&self) -> usize {
        match self.kind {
            SoftBreakKind::Before => self.start,
            _ => self.start + self.len,
        }
    }
}

/// 这一行所有可以断行的位置；`is_protected` 为 true 的字符（代码、链接、提及等的内部）不断行。
pub fn candidates(line: &str, is_protected: impl Fn(usize) -> bool) -> Vec<SoftBreak> {
    let mut result = Vec::new();
    let mut previous = None;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if is_protected(i) {
            previous = Some(c);
            continue;
        }
        if SPACES.contains(c) {
            let mut end = i + c.len_utf8();
            while let Some(&(j, next)) = chars.peek() {
                if !SPACES.contains(next) || is_protected(j) {
                    break;
                }
                end = j + next.len_utf8();
                chars.next();
            }
            // 行首和行尾的空格后面或前面没有东西，在那里断行没有意义
            if i > 0 && end < line.len() {
                result.push(SoftBreak {
                    start: i,
                    len: end - i,
                    kind: SoftBreakKind::Space,
                });
            }
            previous = Some(' ');
            continue;
        }
        if OPEN_BRACKETS.contains(c) {
            // 前面是空格或另一个左括号时，那里已经可以断行或者不该断开
            if previous.is_some_and(|p| !SPACES.contains(p) && !OPEN_BRACKETS.contains(p)) {
                result.push(SoftBreak {
                    start: i,
                    len: c.len_utf8(),
                    kind: SoftBreakKind::Before,
                });
            }
        } else if BREAK_AFTER.contains(c) {
            // 连续的符号（“？！”）和后面的空格、右侧没有内容时不断开
            if chars
                .peek()
                .is_some_and(|&(_, next)| !SPACES.contains(next) && !BREAK_AFTER.contains(next))
            {
                result.push(SoftBreak {
                    start: i,
                    len: c.len_utf8(),
                    kind: SoftBreakKind::After,
                });
            }
        }
        previous = Some(c);
    }
    result
}

/// 实际断行的位置：一行放得下时不断，放不下时在最后一个放得下的位置断开。一段比 `width` 还长时单独占一行。
pub fn choose(
    line: &str,
    candidates: &[SoftBreak],
    measure: impl Fn(&str) -> f64,
    width: f64,
) -> Vec<SoftBreak> {
    let mut chosen = Vec::new();
    let mut start = 0;
    for (i, candidate) in candidates.iter().enumerate() {
        let end = candidates
            .get(i + 1)
            .map_or(line.len(), SoftBreak::text_end);
        if end > start && measure(&line[start..end]) > width && candidate.text_end() > start {
            chosen.push(*candidate);
            start = candidate.next_start();
        }
    }
    chosen
}

/// 断开后最宽的一段；`breaks` 是全部可以断行的位置时，就是这一行最少需要的宽度。
pub fn widest(line: &str, breaks: &[SoftBreak], measure: impl Fn(&str) -> f64) -> f64 {
    let mut widest = 0.0f64;
    let mut start = 0;
    for b in breaks {
        if b.text_end() > start {
            widest = widest.max(measure(&line[start..b.text_end()]));
        }
        start = b.next_start();
    }
    widest.max(measure(&line[start..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 每个字符宽 1
    fn width(text: &str) -> f64 {
        text.chars().count() as f64
    }

    fn wrap(line: &str, max: f64) -> Vec<&str> {
        let mut parts = Vec::new();
        let mut start = 0;
        for b in choose(line, &candidates(line, |_| false), width, max) {
            parts.push(&line[start..b.text_end()]);
            start = b.next_start();
        }
        parts.push(&line[start..]);
        parts
    }

    #[test]
    fn wraps_only_at_allowed_places() {
        let cases: [(&str, f64, &[&str]); 9] = [
            ("aaa bbb", 4.0, &["aaa", "bbb"]),  // 空格留在上一行，不显示
            ("aaa　bbb", 4.0, &["aaa", "bbb"]), // 全角空格
            ("aaa,bbb", 4.0, &["aaa,", "bbb"]), // 符号留在上一行
            ("aa，bb。cc", 3.0, &["aa，", "bb。", "cc"]),
            ("aaa(bbb)", 4.0, &["aaa", "(bbb)"]), // 左括号到下一行
            ("aaa?!bbb", 5.0, &["aaa?!", "bbb"]), // 连续的符号不断开
            ("1.5.5.5", 3.0, &["1.5.5.5"]),       // 半角句号可能是小数点
            ("中文没有断点", 3.0, &["中文没有断点"]), // 太长时横向滚动，不在文字中间断开
            ("aaa bbb", 7.0, &["aaa bbb"]),       // 放得下时不断
        ];
        for (line, max, expected) in cases {
            assert_eq!(wrap(line, max), expected, "{line}");
        }
    }

    #[test]
    fn protected_text_does_not_break() {
        assert!(candidates("a-b c", |i| i < 5).is_empty());
    }

    #[test]
    fn widest_of_all_candidates_is_the_longest_unbreakable_run() {
        let line = "aa bbbb,c";
        assert_eq!(widest(line, &candidates(line, |_| false), width), 5.0);
    }
}
