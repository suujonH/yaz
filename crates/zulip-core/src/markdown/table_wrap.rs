//! Automatic wrapping of cells in composer tables: for display only, the document is unchanged. Lines break only at spaces (fullwidth and halfwidth) and after
//! left brackets, hyphens, question marks, exclamation marks, commas, colons (fullwidth and halfwidth) and the fullwidth full stop; a halfwidth period may be a decimal point, so it does not break.

const SPACES: &str = " 　";
const OPEN_BRACKETS: &str = "[(<{［（＜｛";
const BREAK_AFTER: &str = "-－?？!！,，:：。";

/// Kind of break point; it decides how the character at the break is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftBreakKind {
    /// Spaces (possibly several in a row): stay at the end of the previous line, are not shown, and the cursor cannot stop in them.
    Space,
    /// The symbol stays at the end of the previous line, and the break comes after it.
    After,
    /// The left bracket goes to the start of the next line, and the break comes before it.
    Before,
}

/// A position where a break is possible: `[start, start + len)` are the related characters (byte offsets within the line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftBreak {
    pub start: usize,
    pub len: usize,
    pub kind: SoftBreakKind,
}

impl SoftBreak {
    /// The text shown on the previous line ends here.
    pub fn text_end(&self) -> usize {
        match self.kind {
            SoftBreakKind::After => self.start + self.len,
            _ => self.start,
        }
    }

    /// The text shown on the next line starts here.
    pub fn next_start(&self) -> usize {
        match self.kind {
            SoftBreakKind::Before => self.start,
            _ => self.start + self.len,
        }
    }
}

/// All positions in this line where a break is possible; characters with `is_protected` true (inside code, links, mentions, etc.) are not broken.
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
            // Nothing follows or precedes the spaces at the line start and end, so breaking there is pointless
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
            // When preceded by a space or another left bracket, it can already break there or should not be broken
            if previous.is_some_and(|p| !SPACES.contains(p) && !OPEN_BRACKETS.contains(p)) {
                result.push(SoftBreak {
                    start: i,
                    len: c.len_utf8(),
                    kind: SoftBreakKind::Before,
                });
            }
        } else if BREAK_AFTER.contains(c) {
            // Consecutive symbols (like a question mark followed by an exclamation mark), and spaces after them with no content on the right, do not break
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

/// Positions where lines actually break: no break when the line fits, otherwise break at the last position that fits. A segment longer than `width` takes a line by itself.
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

/// The widest segment after breaking; when `breaks` is all possible break positions, it is the minimum width this line needs.
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

    // Each character is 1 wide
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
            ("aaa bbb", 4.0, &["aaa", "bbb"]), // Spaces stay on the previous line and are not shown
            ("aaa　bbb", 4.0, &["aaa", "bbb"]), // Fullwidth space
            ("aaa,bbb", 4.0, &["aaa,", "bbb"]), // The symbol stays on the previous line
            ("aa，bb。cc", 3.0, &["aa，", "bb。", "cc"]),
            ("aaa(bbb)", 4.0, &["aaa", "(bbb)"]), // The left bracket goes to the next line
            ("aaa?!bbb", 5.0, &["aaa?!", "bbb"]), // Consecutive symbols do not break
            ("1.5.5.5", 3.0, &["1.5.5.5"]),       // A halfwidth period may be a decimal point
            ("中文没有断点", 3.0, &["中文没有断点"]), // When too long, scroll horizontally and do not break in the middle of the text
            ("aaa bbb", 7.0, &["aaa bbb"]),           // No break when it fits
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
