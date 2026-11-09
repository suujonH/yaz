//! Notification Bot 的消息：按发送者、频道和话题名分类，并把事件提示与消息提醒翻译为中文。

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use scraper::{ElementRef, Node};

use crate::html::{self, element_with_text, node_html};
use crate::util::{ends_with_ignore_case, eq_ignore_case, static_regex};

/// 提醒私信的开头（服务器只有英文，见 zerver/actions/reminders.py）。
pub const REMINDER_MARKER: &str = "You requested a reminder for the following";

// 服务器按组织默认语言翻译话题名；zh_Hans 的 "moderation" 译文本身就是“{name} 提到了您：”
const NEW_CHANNELS_TOPICS: [&str; 2] = ["new channels", "新频道"];
const SIGNUPS_TOPICS: [&str; 2] = ["signups", "注册"];
const ZULIP_UPDATES_TOPICS: [&str; 1] = ["Zulip updates"];
const MODERATION_SUFFIXES: [&str; 2] = [" moderation", " 提到了您："];

const NOTE_PREFIX: &str = " Note:";

/// Notification Bot 消息的显示方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SystemNoticeKind {
    /// 不是 Notification Bot 的消息，按普通消息显示。
    #[default]
    None,
    /// 话题中的事件提示：在聊天中显示为居中灰色小字。
    Inline,
    /// 公告或机器人私信：只在“系统通知”中显示。
    Notice,
    /// 消息提醒到时间：只在“消息提醒”窗口中显示。
    Reminder,
}

/// 系统通知的分类（筛选用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemNoticeCategory {
    Other,
    NewChannels,
    Signups,
    ZulipUpdates,
    Moderation,
    Direct,
}

impl SystemNoticeCategory {
    pub fn text(self) -> &'static str {
        match self {
            Self::NewChannels => "新频道",
            Self::Signups => "新成员",
            Self::ZulipUpdates => "Zulip 更新",
            Self::Moderation => "举报",
            Self::Direct => "通知",
            Self::Other => "公告",
        }
    }
}

/// 不是 Notification Bot 发的为 `None`；私信为系统通知或提醒；公告频道中的固定公告话题为系统通知；
/// 其它频道消息为灰色提示条。
pub fn classify(
    bot_id: Option<i64>,
    announcement_channel_ids: &HashSet<i64>,
    sender_id: i64,
    stream_id: Option<i64>,
    topic: &str,
    html: &str,
) -> SystemNoticeKind {
    if bot_id != Some(sender_id) {
        return SystemNoticeKind::None;
    }
    match stream_id {
        None if html.contains(REMINDER_MARKER) => SystemNoticeKind::Reminder,
        None => SystemNoticeKind::Notice,
        Some(id) if announcement_channel_ids.contains(&id) && is_announcement_topic(topic) => {
            SystemNoticeKind::Notice
        }
        Some(_) => SystemNoticeKind::Inline,
    }
}

/// 服务器发公告时使用的话题名；频道只允许空话题时公告发在空话题。
pub fn is_announcement_topic(topic: &str) -> bool {
    topic.is_empty()
        || matches_any(&NEW_CHANNELS_TOPICS, topic)
        || matches_any(&SIGNUPS_TOPICS, topic)
        || matches_any(&ZULIP_UPDATES_TOPICS, topic)
        || is_moderation(topic)
}

pub fn category_of(stream_id: Option<i64>, topic: &str) -> SystemNoticeCategory {
    if stream_id.is_none() {
        SystemNoticeCategory::Direct
    } else if matches_any(&NEW_CHANNELS_TOPICS, topic) {
        SystemNoticeCategory::NewChannels
    } else if matches_any(&SIGNUPS_TOPICS, topic) {
        SystemNoticeCategory::Signups
    } else if matches_any(&ZULIP_UPDATES_TOPICS, topic) {
        SystemNoticeCategory::ZulipUpdates
    } else if is_moderation(topic) {
        SystemNoticeCategory::Moderation
    } else {
        SystemNoticeCategory::Other
    }
}

fn matches_any(names: &[&str], topic: &str) -> bool {
    names.iter().any(|n| eq_ignore_case(n, topic))
}

fn is_moderation(topic: &str) -> bool {
    MODERATION_SUFFIXES
        .iter()
        .any(|s| ends_with_ignore_case(topic, s))
}

// ───────────── 翻译 ─────────────

type Rules = Vec<(Regex, &'static str)>;

fn compile(rules: &[(&str, &'static str)]) -> Rules {
    rules
        .iter()
        .map(|(pattern, template)| (Regex::new(pattern).expect("正则表达式无效"), *template))
        .collect()
}

// 句子中的元素用 ⟦序号⟧ 代替后再匹配（见 zerver/actions/message_edit.py）。
// 模板中 {name} 为句中对应的部分，{name=文字} 同时把该元素（链接）的文字换掉
static EVENT_RULES: LazyLock<Rules> = LazyLock::new(|| {
    compile(&[
        (
            r"^This topic was moved here from (?<from>⟦\d+⟧) by (?<user>⟦\d+⟧)\.$",
            "{user} 将此话题从 {from} 移到了这里。",
        ),
        (
            r"^This topic was moved to (?<to>⟦\d+⟧) by (?<user>⟦\d+⟧)\.$",
            "{user} 将此话题移到了 {to}。",
        ),
        (
            r"^(?<count>A message|\d+ messages|⟦\d+⟧) (?:was|were) moved here from (?<from>⟦\d+⟧) by (?<user>⟦\d+⟧)\.$",
            "{user} 从 {from} 移来了 {count}。",
        ),
        (
            r"^(?<count>A message|\d+ messages|⟦\d+⟧) (?:was|were) moved from this topic to (?<to>⟦\d+⟧) by (?<user>⟦\d+⟧)\.$",
            "{user} 将 {count} 移到了 {to}。",
        ),
        (
            r"^(?<user>⟦\d+⟧) has marked this topic as resolved\.$",
            "{user} 将此话题标记为已解决。",
        ),
        (
            r"^(?<user>⟦\d+⟧) has marked this topic as unresolved\.$",
            "{user} 将此话题标记为未解决。",
        ),
    ])
});

// 消息提醒（见 zerver/lib/reminders.py）：第一段说明，第二段是被提醒消息的出处，之后是引用的原文
static REMINDER_RULES: LazyLock<Rules> = LazyLock::new(|| {
    compile(&[
        (
            r"(?s)^You requested a reminder for the following (?<kind>message|direct message)\.(?<note> Note:.*)?$",
            "你为以下{kind}设置了提醒。{note}",
        ),
        (
            r"^(?<user>⟦\d+⟧) (?<said>⟦\d+⟧) in (?<topic>⟦\d+⟧):$",
            "{user} 在 {topic} 中{said=说}：",
        ),
        (
            r"^(?<user>⟦\d+⟧) (?<said>⟦\d+⟧) to (?<to>⟦\d+⟧(?:(?:, |,? and )⟦\d+⟧)*):$",
            "{user} 对 {to} {said=说}：",
        ),
        (
            r"^(?<user>⟦\d+⟧) (?<sent>⟦\d+⟧) a (?<widget>poll|todo list) in (?<topic>⟦\d+⟧)\.$",
            "{user} 在 {topic} 中{sent=发起}了{widget}。",
        ),
        (
            r"^(?<user>⟦\d+⟧) (?<sent>⟦\d+⟧) a (?<widget>poll|todo list) to (?<to>⟦\d+⟧(?:(?:, |,? and )⟦\d+⟧)*)\.$",
            "{user} 向 {to} {sent=发起}了{widget}。",
        ),
        (
            r"^You (?<sent>⟦\d+⟧) yourself a (?<widget>poll|todo list)\.$",
            "你给自己{sent=发起}了{widget}。",
        ),
        (
            r"^You (?<said>⟦\d+⟧) a note to yourself:$",
            "你给自己{said=写}了一条笔记：",
        ),
    ])
});

static_regex!(PLACEHOLDER = r"\{(\w+)(?:=([^}]*))?\}");
static_regex!(LIST_PART = r"(⟦\d+⟧)|,? and |, ");
static_regex!(INLINE_ELEMENT = r"⟦(\d+)⟧");
static_regex!(SINGLE_ELEMENT = r"^⟦(\d+)⟧$");
static_regex!(COUNT = r"^(\d+) messages$");
static_regex!(WHITESPACE = r"\s+");

/// 翻译事件提示的 HTML；只翻译只有一个段落且句子认识的提示，其它原样返回。
pub fn localize(source: &str) -> String {
    let doc = html::parse_fragment(source);
    let mut children = doc.root_element().child_elements();
    let translated = match (children.next(), children.next()) {
        (Some(p), None) if p.value().name() == "p" => translate(p, &EVENT_RULES),
        _ => None,
    };
    translated.map_or_else(|| source.to_owned(), |inner| format!("<p>{inner}</p>"))
}

/// 翻译消息提醒的 HTML：各段落中认识的句子译为中文，引用的原文等其它内容不变。
pub fn localize_reminder(source: &str) -> String {
    let doc = html::parse_fragment(source);
    let mut changed = false;
    let mut out = String::new();
    for node in doc.root_element().children() {
        let translated = ElementRef::wrap(node)
            .filter(|e| e.value().name() == "p")
            .and_then(|p| translate(p, &REMINDER_RULES));
        match translated {
            Some(inner) => {
                out.push_str(&format!("<p>{inner}</p>"));
                changed = true;
            }
            None => out.push_str(&node_html(node)),
        }
    }
    if changed { out } else { source.to_owned() }
}

// 段落的句子认识时返回译文（段落内部的 HTML）
fn translate(p: ElementRef<'_>, rules: &Rules) -> Option<String> {
    let mut elements = Vec::new();
    let mut sentence = String::new();
    for node in p.children() {
        if let Some(e) = ElementRef::wrap(node) {
            sentence.push_str(&format!("⟦{}⟧", elements.len()));
            elements.push(e);
        } else if let Node::Text(text) = node.value() {
            sentence.push_str(text);
        }
    }
    let text = WHITESPACE.replace_all(&sentence, " ");
    let text = text.trim();

    rules.iter().find_map(|(pattern, template)| {
        let m = pattern.captures(text)?;
        let rendered = PLACEHOLDER.replace_all(template, |t: &Captures<'_>| {
            let value = m.name(&t[1]).map_or("", |g| g.as_str());
            part(value, t.get(2).map(|g| g.as_str()), &elements)
        });
        Some(rendered.into_owned())
    })
}

fn element_at<'a>(elements: &[ElementRef<'a>], index: &str) -> Option<ElementRef<'a>> {
    index
        .parse::<usize>()
        .ok()
        .and_then(|i| elements.get(i).copied())
}

// 占位的元素换回元素自身的 HTML；消息条数（可能是指向消息的链接）译为“N 条消息”，几个人译为“A、B 和 C”
fn part(value: &str, link_text: Option<&str>, elements: &[ElementRef<'_>]) -> String {
    if let Some(m) = SINGLE_ELEMENT.captures(value)
        && let Some(e) = element_at(elements, &m[1])
    {
        let text: String = e.text().collect();
        return match link_text
            .map(str::to_owned)
            .or_else(|| count_text(text.trim()))
        {
            Some(replacement) => element_with_text(e, &replacement),
            None => e.html(),
        };
    }
    // 提醒的备注：一般是下面的引用段落，与说明在同一段落时原样放在“备注：”后
    if let Some(note) = value.strip_prefix(NOTE_PREFIX) {
        let encoded = html::encode(note.trim());
        let note = INLINE_ELEMENT.replace_all(&encoded, |m: &Captures<'_>| {
            element_at(elements, &m[1]).map_or_else(String::new, |e| e.html())
        });
        return format!("备注：{note}");
    }
    if value.contains('⟦') {
        return LIST_PART
            .replace_all(value, |m: &Captures<'_>| {
                if m.get(1).is_some() {
                    part(&m[0], None, elements)
                } else if m[0].contains("and") {
                    " 和 ".to_owned()
                } else {
                    "、".to_owned()
                }
            })
            .into_owned();
    }
    let translated = match value {
        "message" => Some("消息".to_owned()),
        "direct message" => Some("私信".to_owned()),
        "poll" => Some("一个投票".to_owned()),
        "todo list" => Some("一个待办清单".to_owned()),
        _ => count_text(value),
    };
    html::encode(translated.as_deref().unwrap_or(value))
}

fn count_text(value: &str) -> Option<String> {
    if value == "A message" {
        return Some("1 条消息".to_owned());
    }
    COUNT.captures(value).map(|m| format!("{} 条消息", &m[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOT: i64 = 6;
    const USER: &str = "<span class=\"user-mention silent\" data-user-id=\"8\">群主大大</span>";
    const USER2: &str = "<span class=\"user-mention silent\" data-user-id=\"9\">小王</span>";
    const USER3: &str = "<span class=\"user-mention silent\" data-user-id=\"10\">小李</span>";
    const TOPIC: &str = "<a class=\"stream-topic\" data-stream-id=\"4\" href=\"/#narrow/channel/4/topic/x\">#开发 &gt; 发布计划</a>";
    const SAID: &str = "<a href=\"http://x/#narrow/near/9\">said</a>";
    const SENT: &str = "<a href=\"http://x/#narrow/near/9\">sent</a>";
    const QUOTE: &str = "<blockquote>\n<p>原文, and more</p>\n</blockquote>";

    fn kind(sender: i64, stream: Option<i64>, topic: &str, html: &str) -> SystemNoticeKind {
        classify(Some(BOT), &HashSet::from([3]), sender, stream, topic, html)
    }

    #[test]
    fn other_sender_is_none() {
        assert_eq!(
            kind(9, Some(3), "new channels", "<p>x</p>"),
            SystemNoticeKind::None
        );
        assert_eq!(
            classify(None, &HashSet::new(), BOT, None, "", "<p>x</p>"),
            SystemNoticeKind::None
        );
    }

    #[test]
    fn direct_messages() {
        assert_eq!(kind(BOT, None, "", "<p>x</p>"), SystemNoticeKind::Notice);
        assert_eq!(
            kind(
                BOT,
                None,
                "",
                "<p>You requested a reminder for the following message.</p>"
            ),
            SystemNoticeKind::Reminder
        );
    }

    #[test]
    fn announcement_topic_is_notice() {
        for topic in [
            "new channels",
            "新频道",
            "Signups",
            "Zulip updates",
            "张三 moderation",
            "张三 提到了您：",
            "",
        ] {
            assert_eq!(
                kind(BOT, Some(3), topic, "<p>x</p>"),
                SystemNoticeKind::Notice,
                "{topic}"
            );
        }
    }

    #[test]
    fn other_channel_messages_are_inline() {
        for (stream, topic) in [(3, "随便聊"), (4, "new channels")] {
            assert_eq!(
                kind(BOT, Some(stream), topic, "<p>x</p>"),
                SystemNoticeKind::Inline
            );
        }
    }

    #[test]
    fn categories() {
        for (stream, topic, expected) in [
            (None, "", SystemNoticeCategory::Direct),
            (Some(3), "New Channels", SystemNoticeCategory::NewChannels),
            (Some(3), "注册", SystemNoticeCategory::Signups),
            (Some(3), "Zulip updates", SystemNoticeCategory::ZulipUpdates),
            (Some(3), "张三 moderation", SystemNoticeCategory::Moderation),
            (Some(3), "", SystemNoticeCategory::Other),
        ] {
            assert_eq!(category_of(stream, topic), expected);
        }
        assert_eq!(SystemNoticeCategory::Other.text(), "公告");
    }

    #[test]
    fn known_notices_translated() {
        for (source, expected) in [
            (
                format!("<p>This topic was moved here from {TOPIC} by {USER}.</p>"),
                format!("<p>{USER} 将此话题从 {TOPIC} 移到了这里。</p>"),
            ),
            (
                format!("<p>This topic was moved to {TOPIC} by {USER}.</p>"),
                format!("<p>{USER} 将此话题移到了 {TOPIC}。</p>"),
            ),
            (
                format!("<p>A message was moved here from {TOPIC} by {USER}.</p>"),
                format!("<p>{USER} 从 {TOPIC} 移来了 1 条消息。</p>"),
            ),
            (
                format!("<p>3 messages were moved from this topic to {TOPIC} by {USER}.</p>"),
                format!("<p>{USER} 将 3 条消息 移到了 {TOPIC}。</p>"),
            ),
            (
                format!("<p>{USER} has marked this topic as resolved.</p>"),
                format!("<p>{USER} 将此话题标记为已解决。</p>"),
            ),
            (
                format!("<p>{USER} has marked this topic as unresolved.</p>"),
                format!("<p>{USER} 将此话题标记为未解决。</p>"),
            ),
        ] {
            assert_eq!(localize(&source), expected);
        }
    }

    #[test]
    fn linked_count_translates_link_text() {
        assert_eq!(
            localize(&format!(
                "<p><a href=\"/#narrow/near/5\">2 messages</a> were moved here from {TOPIC} by {USER}.</p>"
            )),
            format!("<p>{USER} 从 {TOPIC} 移来了 <a href=\"/#narrow/near/5\">2 条消息</a>。</p>")
        );
    }

    #[test]
    fn unknown_unchanged() {
        for source in ["<p>Some other notice.</p>", "<p>a</p><p>b</p>"] {
            assert_eq!(localize(source), source);
        }
        assert_eq!(
            localize_reminder("<p>Hello, and bye</p>"),
            "<p>Hello, and bye</p>"
        );
    }

    #[test]
    fn reminder_channel_message() {
        assert_eq!(
            localize_reminder(&format!(
                "<p>You requested a reminder for the following message.</p>\n<p>{USER} {SAID} in {TOPIC}:</p>\n{QUOTE}"
            )),
            format!(
                "<p>你为以下消息设置了提醒。</p>\n<p>{USER} 在 {TOPIC} 中<a href=\"http://x/#narrow/near/9\">说</a>：</p>\n{QUOTE}"
            )
        );
    }

    #[test]
    fn reminder_direct_message_with_note() {
        assert_eq!(
            localize_reminder(&format!(
                "<p>You requested a reminder for the following direct message. Note:</p>\n<blockquote>\n<p>记得回复</p>\n</blockquote>\n<p>{USER} {SAID} to {USER2}, {USER3}, and {USER}:</p>"
            )),
            format!(
                "<p>你为以下私信设置了提醒。备注：</p>\n<blockquote>\n<p>记得回复</p>\n</blockquote>\n<p>{USER} 对 {USER2}、{USER3} 和 {USER} <a href=\"http://x/#narrow/near/9\">说</a>：</p>"
            )
        );
    }

    #[test]
    fn reminder_context() {
        let link = |text: &str| format!("<a href=\"http://x/#narrow/near/9\">{text}</a>");
        for (source, expected) in [
            (
                format!("<p>{USER} {SENT} a poll in {TOPIC}.</p>"),
                format!("<p>{USER} 在 {TOPIC} 中{}了一个投票。</p>", link("发起")),
            ),
            (
                format!("<p>{USER} {SENT} a todo list to {USER2} and {USER3}.</p>"),
                format!(
                    "<p>{USER} 向 {USER2} 和 {USER3} {}了一个待办清单。</p>",
                    link("发起")
                ),
            ),
            (
                format!("<p>You {SENT} yourself a poll.</p>"),
                format!("<p>你给自己{}了一个投票。</p>", link("发起")),
            ),
            (
                format!("<p>You {SAID} a note to yourself:</p>"),
                format!("<p>你给自己{}了一条笔记：</p>", link("写")),
            ),
        ] {
            assert_eq!(localize_reminder(&source), expected);
        }
    }
}
