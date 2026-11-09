macro_rules! static_regex {
    ($name:ident = $pattern:expr) => {
        static $name: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new($pattern).expect("正则表达式无效"));
    };
}

macro_rules! static_fancy_regex {
    ($name:ident = $pattern:expr) => {
        static $name: std::sync::LazyLock<fancy_regex::Regex> =
            std::sync::LazyLock::new(|| fancy_regex::Regex::new($pattern).expect("正则表达式无效"));
    };
}

pub(crate) use {static_fancy_regex, static_regex};

/// Case folding for per-character comparison: only one-to-one mappings are used, so the match length corresponds one-to-one with the characters of the original.
fn fold(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

fn fold_eq(a: char, b: char) -> bool {
    a == b || fold(a) == fold(b)
}

pub(crate) fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count() && a.chars().zip(b.chars()).all(|(x, y)| fold_eq(x, y))
}

/// When the start of `haystack` matches `needle`, returns the byte length of the matched part in `haystack`.
fn match_prefix(haystack: &str, needle: &str) -> Option<usize> {
    let mut chars = haystack.char_indices();
    let mut end = 0;
    for n in needle.chars() {
        let (i, h) = chars.next()?;
        if !fold_eq(h, n) {
            return None;
        }
        end = i + h.len_utf8();
    }
    Some(end)
}

pub fn starts_with_ignore_case(haystack: &str, needle: &str) -> bool {
    match_prefix(haystack, needle).is_some()
}

pub(crate) fn ends_with_ignore_case(haystack: &str, needle: &str) -> bool {
    let n = needle.chars().count();
    let start = haystack
        .char_indices()
        .rev()
        .nth(n.wrapping_sub(1))
        .map_or(if n == 0 { haystack.len() } else { usize::MAX }, |(i, _)| i);
    start <= haystack.len() && eq_ignore_case(&haystack[start..], needle)
}

/// Searches from `from` and returns the byte range of the matched part.
pub(crate) fn find_ignore_case(
    haystack: &str,
    needle: &str,
    from: usize,
) -> Option<(usize, usize)> {
    haystack[from..]
        .char_indices()
        .map(|(i, _)| from + i)
        .chain(std::iter::once(haystack.len()))
        .find_map(|i| match_prefix(&haystack[i..], needle).map(|len| (i, i + len)))
}

/// Whether `haystack` contains `needle`, case-insensitive. Always true when `needle` is empty.
pub fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    find_ignore_case(haystack, needle, 0).is_some()
}

pub(crate) fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// A non-negative integer made only of digits (signs and whitespace are not accepted).
pub(crate) fn parse_digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Extension of a file path (including the dot); empty when there is none.
pub(crate) fn file_extension(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(dot) if dot + 1 < name.len() => &name[dot..],
        _ => "",
    }
}

pub(crate) fn char_before(text: &str, index: usize) -> Option<char> {
    text[..index].chars().next_back()
}

pub(crate) fn char_after(text: &str, index: usize) -> Option<char> {
    text[index..].chars().next()
}

/// Clamps to `[0, text.len()]` and aligns to a character boundary.
pub(crate) fn clamp_offset(text: &str, offset: usize) -> usize {
    text.floor_char_boundary(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_case_search_reports_original_range() {
        assert_eq!(find_ignore_case("Dev 开发 dev", "DEV", 1), Some((11, 14)));
        assert_eq!(find_ignore_case("abc", "", 3), Some((3, 3)));
        assert_eq!(find_ignore_case("abc", "x", 0), None);
    }

    #[test]
    fn ends_with_handles_short_haystack() {
        assert!(ends_with_ignore_case("张三 MODERATION", " moderation"));
        assert!(!ends_with_ignore_case("x", " moderation"));
        assert!(ends_with_ignore_case("x", ""));
    }

    #[test]
    fn extension() {
        assert_eq!(file_extension("/a.b/c.PNG"), ".PNG");
        assert_eq!(file_extension("/a.b/c"), "");
        assert_eq!(file_extension("c."), "");
    }
}
