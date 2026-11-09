//! 输入框相关的纯逻辑：引用、转发文字、邀请邮箱解析。

use std::collections::HashSet;

use zulip_core::markdown::format::unused_fence;

/// 回复引用：`@_**名字|id** [发言道](链接)：` 加 quote 代码块。围栏比原文里的都长。
pub fn quote_markdown(sender_name: &str, sender_id: i64, link: &str, raw: &str) -> String {
    let fence = unused_fence(raw);
    format!("@_**{sender_name}|{sender_id}** [发言道]({link})：\n{fence}quote\n{raw}\n{fence}")
}

/// 邀请框的邮箱：逗号、分号（含全角）和空白都是分隔符，忽略大小写去重，保留第一次的写法。
pub fn parse_emails(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split([',', '，', ';', '；', ' ', '\t', '\r', '\n'])
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.to_lowercase()))
        .map(str::to_owned)
        .collect()
}

/// 消息被删除后的占位文字。
pub fn deleted_text(sender_name: &str) -> String {
    format!("{sender_name}的一条消息已被删除")
}

/// 合并转发的预览。
pub fn forward_preview(count: usize) -> String {
    format!("[聊天记录] 共{count}条")
}

/// 逐条转发的预览。
pub fn forward_each_preview(count: usize) -> String {
    format!("[逐条转发] 共{count}条消息")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_format() {
        assert_eq!(
            quote_markdown("张三", 5, "https://z.test/#narrow/dm/1,5-dm/near/9", "你好"),
            "@_**张三|5** [发言道](https://z.test/#narrow/dm/1,5-dm/near/9)：\n```quote\n你好\n```"
        );
    }

    #[test]
    fn quote_fence_is_longer_than_inner_fence() {
        assert_eq!(
            quote_markdown("A", 1, "l", "```py\nx\n```"),
            "@_**A|1** [发言道](l)：\n````quote\n```py\nx\n```\n````"
        );
    }

    #[test]
    fn emails_split_on_all_separators() {
        assert_eq!(
            parse_emails(" a@x.test, b@x.test；c@x.test\r\n\nd@x.test，"),
            ["a@x.test", "b@x.test", "c@x.test", "d@x.test"]
        );
    }

    #[test]
    fn emails_dedupe_ignoring_case() {
        assert_eq!(parse_emails("A@x.test a@X.test"), ["A@x.test"]);
    }

    #[test]
    fn emails_blank_input_is_empty() {
        assert!(parse_emails(" ,\n ").is_empty());
    }
}
