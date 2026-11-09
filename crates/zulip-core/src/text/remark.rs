//! Display format of local remarks (users, channels, topics). Management lists show the remark and the original name together; messages, conversation titles, etc. show only the remark.

use crate::util::is_blank;

/// "remark(original name)"; when there is no remark or it equals the original name, only the original name is shown.
pub fn with_original(remark: Option<&str>, original: &str) -> String {
    match remark {
        Some(remark) if !is_blank(remark) && remark != original => format!("{remark}({original})"),
        _ => original.to_owned(),
    }
}

/// Trims leading and trailing whitespace; blank is treated as clearing.
pub fn normalize(remark: Option<&str>) -> Option<String> {
    remark
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_shown_with_remark() {
        for (remark, original, expected) in [
            (Some("小王"), "王大锤", "小王(王大锤)"),
            (None, "王大锤", "王大锤"),
            (Some(""), "王大锤", "王大锤"),
            (Some("  "), "王大锤", "王大锤"),
            (Some("王大锤"), "王大锤", "王大锤"),
            (Some("发布"), "release (v2)", "发布(release (v2))"),
        ] {
            assert_eq!(with_original(remark, original), expected);
        }
    }

    #[test]
    fn normalized() {
        assert_eq!(normalize(Some(" 备注 ")).as_deref(), Some("备注"));
        for input in [Some(""), Some(" \t"), None] {
            assert_eq!(normalize(input), None);
        }
    }
}
