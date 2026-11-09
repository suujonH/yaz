//! 本地备注（用户、频道、话题）的显示格式。管理用的列表同时显示备注和原名，消息、会话标题等只显示备注。

use crate::util::is_blank;

/// “备注(原名称)”；没有备注或备注与原名称相同时只显示原名称。
pub fn with_original(remark: Option<&str>, original: &str) -> String {
    match remark {
        Some(remark) if !is_blank(remark) && remark != original => format!("{remark}({original})"),
        _ => original.to_owned(),
    }
}

/// 去掉首尾空白；空白视为清除。
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
