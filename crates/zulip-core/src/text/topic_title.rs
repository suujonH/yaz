//! 话题会话的显示标题模板。可用变量 `{Channel}`（频道名）与 `{Topic}`（话题名，有备注时为备注），
//! 不区分大小写；其它文字原样保留。

use regex::Captures;

use crate::util::{is_blank, static_regex};

/// 默认模板：只显示话题名。
pub const DEFAULT: &str = "{Topic}";

static_regex!(VARIABLE = r"(?i)\{(channel|topic)\}");

/// 按模板生成标题；频道名、话题名中恰好含有变量文字时不会被再次替换。
/// 模板或结果为空白时返回话题名。
pub fn format(template: Option<&str>, channel: &str, topic: &str) -> String {
    let Some(template) = template.filter(|t| !is_blank(t)) else {
        return topic.to_owned();
    };
    let replaced = VARIABLE.replace_all(template, |m: &Captures<'_>| {
        if m[1].eq_ignore_ascii_case("channel") {
            channel
        } else {
            topic
        }
    });
    match replaced.trim() {
        "" => topic.to_owned(),
        title => title.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        for (template, expected) in [
            ("{Topic}", "人事通知"),
            ("{Channel} / {Topic}", "公告 / 人事通知"),
            ("[{channel}] {TOPIC}", "[公告] 人事通知"),
            ("#{Channel}", "#公告"),
            ("{Topic}{Topic}", "人事通知人事通知"),
            ("{Unknown} {Topic}", "{Unknown} 人事通知"),
            ("  {Topic}  ", "人事通知"),
        ] {
            assert_eq!(format(Some(template), "公告", "人事通知"), expected);
        }
    }

    #[test]
    fn blank_template_uses_topic() {
        for template in [None, Some(""), Some("   ")] {
            assert_eq!(format(template, "公告", "人事通知"), "人事通知");
        }
    }

    #[test]
    fn blank_result_uses_topic() {
        assert_eq!(format(Some("{Channel}"), "", "人事通知"), "人事通知");
    }

    #[test]
    fn values_are_not_expanded() {
        assert_eq!(
            format(Some("{Channel} / {Topic}"), "{Topic}", "x"),
            "{Topic} / x"
        );
    }
}
