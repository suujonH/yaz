//! 补全：判断光标前正在输入什么，从候选中筛选排序；输入框的候选来自会话，弹层的显示和接受也在这里。

use super::control::Ctl;
use dioxus::prelude::*;
use zulip_core::ConversationKey;
use zulip_core::markdown::format;
use zulip_core::markdown::{self, MarkdownModel, code_language};
use zulip_core::text::mention::{self, match_rank};
use zulip_session::Session;

pub const MAX_ITEMS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mention,
    Channel,
    Emoji,
    Language,
    Snippet,
}

/// 光标前正在输入的内容：`[start, caret)` 会被替换。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: Kind,
    pub start: usize,
    pub query: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Item {
    pub title: String,
    pub subtitle: Option<String>,
    /// 替换进文字的内容；语言补全是语言名。
    pub insert: String,
    pub avatar_url: Option<String>,
    pub avatar_text: Option<String>,
    /// 表情的字符或图片。
    pub glyph: Option<String>,
    /// 排序用的名字。
    pub sort: String,
}

/// 向前找到 `mark`：它在开头或空白之后，且到光标之间没有空白。
fn marked_query(text: &str, caret: usize, mark: char) -> Option<(usize, &str)> {
    let (at, _) = text[..caret]
        .char_indices()
        .rev()
        .take_while(|(_, c)| !c.is_whitespace())
        .find(|(_, c)| *c == mark)?;
    if text[..at]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace())
    {
        return None;
    }
    Some((at, &text[at + mark.len_utf8()..caret]))
}

pub fn detect(text: &str, caret: usize, model: &MarkdownModel) -> Option<Trigger> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    // 只有 ``` 时不补全：回车直接补成没有语言的代码块
    if let Some((start, query)) = code_language::query_at(text, caret, model)
        && !query.is_empty()
    {
        return Some(Trigger {
            kind: Kind::Language,
            start,
            query: query.to_owned(),
        });
    }
    let line = model.line_index_at(caret).and_then(|i| model.lines.get(i));
    // 代码里不补全；已经完成的提及、表情等不再触发
    if line.is_some_and(|l| {
        matches!(
            l.kind,
            markdown::LineKind::FenceBody | markdown::LineKind::IndentedCode
        ) && l.fence != Some(markdown::FenceKind::Quote)
    }) {
        return None;
    }
    if model
        .spans
        .iter()
        .any(|s| s.kind == markdown::SpanKind::Code && s.start < caret && caret <= s.end)
    {
        return None;
    }
    let masked = mention::mask(text, &markdown::mention_spans(model, text));
    if let Some((start, query)) = mention::try_get_query(&masked, caret) {
        return Some(Trigger {
            kind: Kind::Mention,
            start,
            query: query.to_owned(),
        });
    }
    if let Some((start, query)) = marked_query(text, caret, '#')
        && !query.starts_with(['*', '#'])
    {
        return Some(Trigger {
            kind: Kind::Channel,
            start,
            query: query.to_owned(),
        });
    }
    if let Some((start, query)) = marked_query(text, caret, ':')
        && !query.is_empty()
        && !query.contains(':')
    {
        return Some(Trigger {
            kind: Kind::Emoji,
            start,
            query: query.to_owned(),
        });
    }
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
    if let Some(query) = text[line_start..caret].strip_prefix('/')
        && !query.contains(char::is_whitespace)
    {
        return Some(Trigger {
            kind: Kind::Snippet,
            start: line_start,
            query: query.to_owned(),
        });
    }
    None
}

/// 按匹配程度和名字排序，取前几个。`rank` 为 `None` 的候选去掉。
pub fn rank_items(mut items: Vec<(u8, Item)>) -> Vec<Item> {
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.sort.cmp(&b.1.sort)));
    items
        .into_iter()
        .take(MAX_ITEMS)
        .map(|(_, item)| item)
        .collect()
}

pub fn language_items(query: &str) -> Vec<Item> {
    code_language::search(query, MAX_ITEMS)
        .into_iter()
        .map(|language| Item {
            title: language.to_owned(),
            insert: language.to_owned(),
            ..Item::default()
        })
        .collect()
}

#[derive(Clone, PartialEq)]
pub struct Popup {
    pub kind: Kind,
    pub start: usize,
    pub query: String,
    pub items: Vec<Item>,
    pub selected: usize,
    pub x: f64,
    pub y: f64,
}

impl Ctl {
    pub(super) fn load_subscribers(self) {
        match self.key() {
            ConversationKey::Topic(topic) => {
                let session = self.session();
                self.spawn(async move {
                    if let Ok(ids) = session.load_subscribers(topic.stream_id).await {
                        *self.subscribers.write_unchecked() = Some(ids.into_iter().collect());
                    }
                });
            }
            ConversationKey::Dm(dm) => {
                *self.subscribers.write_unchecked() =
                    Some(dm.other_user_ids().iter().copied().collect());
            }
        }
    }

    pub(super) fn close_popup(self) {
        let mut popup = self.popup;
        if popup.peek().is_some() {
            popup.set(None);
            self.send_options();
        }
    }

    pub(super) fn dismiss_popup(self) {
        let start = self.popup.peek().as_ref().map(|p| p.start);
        *self.dismissed.write_unchecked() = start;
        self.close_popup();
    }

    pub(super) fn move_popup(self, delta: isize) {
        let mut popup = self.popup;
        if let Some(p) = popup.write().as_mut() {
            let len = p.items.len() as isize;
            p.selected = (p.selected as isize + delta).rem_euclid(len) as usize;
        }
    }

    /// 按光标处的触发字符更新补全。`may_open` 为 false（只移动了光标，如打开会话时聚焦到草稿末尾）时
    /// 不打开新的补全，只更新或关闭已打开的。
    pub(super) fn refresh_popup(self, may_open: bool) {
        if !may_open && self.popup.peek().is_none() {
            return;
        }
        let trigger = {
            let core = self.core.peek();
            if !*self.focused.peek() || !core.sel.is_collapsed() {
                None
            } else {
                let text = core.active_text();
                let model = core.active_model(&text);
                detect(&text, core.sel.start, &model)
            }
        };
        let Some(trigger) = trigger else {
            *self.dismissed.write_unchecked() = None;
            self.close_popup();
            return;
        };
        if *self.dismissed.peek() == Some(trigger.start) {
            return;
        }
        let items = self.items_for(&trigger);
        if items.is_empty() {
            self.close_popup();
            return;
        }
        let (x, y) = *self.caret.peek();
        let mut popup = self.popup;
        let was_open = popup.peek().is_some();
        let selected = popup
            .peek()
            .as_ref()
            .filter(|p| p.start == trigger.start && p.query == trigger.query)
            .map_or(0, |p| p.selected.min(items.len() - 1));
        let next = Popup {
            kind: trigger.kind,
            start: trigger.start,
            query: trigger.query,
            items,
            selected,
            x,
            y,
        };
        if popup.peek().as_ref() != Some(&next) {
            popup.set(Some(next));
        }
        if !was_open {
            self.send_options();
        }
    }

    pub(super) fn items_for(self, trigger: &Trigger) -> Vec<Item> {
        let session = self.session();
        let query = trigger.query.as_str();
        match trigger.kind {
            Kind::Language => language_items(query),
            Kind::Mention => self.mention_items(&session, query),
            Kind::Channel => rank_items(
                session
                    .subscribed_channels()
                    .into_iter()
                    .filter_map(|channel| {
                        let rank = match_rank(query, &[Some(&channel.name)])?;
                        Some((
                            rank,
                            Item {
                                insert: format!("#**{}** ", channel.name),
                                sort: channel.name.to_lowercase(),
                                title: channel.name,
                                ..Item::default()
                            },
                        ))
                    })
                    .collect(),
            ),
            Kind::Emoji => session
                .emoji_catalog()
                .search(query)
                .into_iter()
                .take(MAX_ITEMS)
                .map(|entry| Item {
                    title: format!(":{}:", entry.name),
                    insert: format!(":{}: ", entry.name),
                    glyph: entry.text.clone(),
                    avatar_url: entry.image_url.clone(),
                    ..Item::default()
                })
                .collect(),
            Kind::Snippet => rank_items(
                session
                    .snippets()
                    .into_iter()
                    .filter_map(|snippet| {
                        let rank = match_rank(query, &[Some(&snippet.title)])?;
                        let first_line = snippet
                            .content
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        Some((
                            rank,
                            Item {
                                insert: format::with_trailing_line(&snippet.content),
                                subtitle: Some(first_line),
                                sort: snippet.title.to_lowercase(),
                                title: snippet.title,
                                ..Item::default()
                            },
                        ))
                    })
                    .collect(),
            ),
        }
    }

    pub(super) fn mention_items(self, session: &Session, query: &str) -> Vec<Item> {
        let mut items = Vec::new();
        if session.is_wildcard_allowed(&self.key.peek()) {
            let mut wildcard = |title: &str, name: &str| {
                if let Some(rank) = match_rank(query, &[Some(title), Some(name)]) {
                    items.push((
                        rank,
                        Item {
                            title: title.to_owned(),
                            subtitle: Some(name.to_owned()),
                            insert: format!("@**{name}** "),
                            avatar_text: Some(title.to_owned()),
                            sort: name.to_owned(),
                            ..Item::default()
                        },
                    ));
                }
            };
            wildcard("全体成员", "all");
            if matches!(*self.key.peek(), ConversationKey::Topic(_)) {
                wildcard("话题参与者", "topic");
            }
        }
        let me = session.self_id();
        let allowed = self.subscribers.peek();
        items.extend(session.users().into_iter().filter_map(|user| {
            if user.id == me || !user.is_active {
                return None;
            }
            if allowed.as_ref().is_some_and(|ids| !ids.contains(&user.id)) {
                return None;
            }
            let rank = match_rank(query, &[Some(&user.full_name), Some(&user.email)])?;
            Some((
                rank,
                Item {
                    insert: format!("@**{}|{}** ", user.full_name, user.id),
                    subtitle: Some(user.email.clone()),
                    avatar_url: user.avatar_url.clone(),
                    avatar_text: Some(user.full_name.clone()),
                    sort: user.full_name.to_lowercase(),
                    title: user.full_name,
                    ..Item::default()
                },
            ))
        }));
        rank_items(items)
    }

    /// 接受补全：`index` 为空时是当前选中的一项。
    pub fn accept_popup(self, index: Option<usize>) {
        let Some(popup) = self.popup.peek().clone() else {
            return;
        };
        let Some(item) = popup.items.get(index.unwrap_or(popup.selected)).cloned() else {
            return;
        };
        self.edit(|core| {
            core.edit_active(|text, model, caret, _| match popup.kind {
                Kind::Language => zulip_core::markdown::code_language::accept(
                    text,
                    caret,
                    popup.start,
                    &item.insert,
                    model,
                ),
                _ => {
                    let end = popup.start + item.insert.len();
                    format::MarkdownEdit::new(
                        popup.start,
                        caret - popup.start,
                        item.insert.clone(),
                        end,
                        end,
                    )
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zulip_core::markdown::{NoResolver, parse};

    fn at_end(text: &str) -> Option<(Kind, String)> {
        let model = parse(text, &NoResolver);
        detect(text, text.len(), &model).map(|t| (t.kind, t.query))
    }

    #[test]
    fn triggers() {
        assert_eq!(at_end("你好 @阿"), Some((Kind::Mention, "阿".into())));
        assert_eq!(at_end("#综"), Some((Kind::Channel, "综".into())));
        assert_eq!(at_end("a :smi"), Some((Kind::Emoji, "smi".into())));
        assert_eq!(at_end("```py"), Some((Kind::Language, "py".into())));
        assert_eq!(at_end("x\n/常"), Some((Kind::Snippet, "常".into())));
    }

    #[test]
    fn no_trigger() {
        for text in [
            "mail@example",
            "a#b",
            "12:30",
            "a /b",
            ":",
            "`@a`",
            "```\n@a",
            "```",
            "/a b",
        ] {
            assert_eq!(at_end(text), None, "{text}");
        }
    }

    #[test]
    fn items_sorted_by_rank_then_name() {
        let item = |sort: &str| Item {
            sort: sort.into(),
            ..Item::default()
        };
        let sorted = rank_items(vec![(1, item("a")), (0, item("c")), (0, item("b"))]);
        let names: Vec<&str> = sorted.iter().map(|i| i.sort.as_str()).collect();
        assert_eq!(names, ["b", "c", "a"]);
    }

    #[test]
    fn languages_filtered() {
        assert_eq!(language_items("rus")[0].title, "rust");
    }
}
