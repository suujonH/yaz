//! 搜索结果中标出命中的部分。

use crate::util::find_ignore_case;

/// 按查询词（不区分大小写）把文字切成普通段和命中段（`true`）。
pub fn split<'a>(text: &'a str, query: Option<&str>) -> Vec<(&'a str, bool)> {
    let mut result = Vec::new();
    let mut start = 0;
    if let Some(query) = query.filter(|q| !q.is_empty()) {
        while let Some((from, to)) = find_ignore_case(text, query, start) {
            if from > start {
                result.push((&text[start..from], false));
            }
            result.push((&text[from..to], true));
            start = to;
        }
    }
    if start < text.len() {
        result.push((&text[start..], false));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_every_match_ignoring_case() {
        assert_eq!(
            split("Dev 开发 dev", Some("DEV")),
            [("Dev", true), (" 开发 ", false), ("dev", true)]
        );
    }

    #[test]
    fn no_query_or_no_match_returns_whole_text() {
        assert_eq!(split("阿杰", None), [("阿杰", false)]);
        assert_eq!(split("阿杰", Some("雨")), [("阿杰", false)]);
        assert!(split("", Some("a")).is_empty());
    }
}
