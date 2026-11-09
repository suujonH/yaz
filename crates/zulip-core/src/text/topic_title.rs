//! Display title template of a topic conversation. Available variables are `{Channel}` (channel name) and `{Topic}` (topic name, the remark if there is one),
//! case-insensitive; other text is kept as is.

use regex::Captures;

use crate::util::{is_blank, static_regex};

/// Default template: shows only the topic name.
pub const DEFAULT: &str = "{Topic}";

static_regex!(VARIABLE = r"(?i)\{(channel|topic)\}");

/// Generates the title from the template; if the channel or topic name happens to contain variable text, it is not replaced again.
/// No template gives the topic name; a blank template is the user's choice of an empty title. When
/// a template with variables gives a blank result (the names are empty), the topic name is returned.
pub fn format(template: Option<&str>, channel: &str, topic: &str) -> String {
    let Some(template) = template else {
        return topic.to_owned();
    };
    if is_blank(template) {
        return String::new();
    }
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
    fn no_template_uses_topic() {
        assert_eq!(format(None, "公告", "人事通知"), "人事通知");
    }

    #[test]
    fn blank_template_gives_empty_title() {
        for template in ["", "   "] {
            assert_eq!(format(Some(template), "公告", "人事通知"), "");
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
