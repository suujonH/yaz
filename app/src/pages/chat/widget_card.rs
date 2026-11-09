//! 投票、待办卡片。

use super::store::Chat;
use crate::ui::{Avatar, Checkbox, Icon, IconButton, IconName, OverlapStack, TextInput};
use dioxus::prelude::*;
use zulip_core::widget::{PollWidget, TodoWidget, Widget};
use zulip_session::WidgetAction;

const VOTER_PREVIEW: usize = 5;

fn run(chat: Chat, id: i64, action: WidgetAction) {
    chat.spawn(async move {
        chat.overlays
            .attempt("操作失败", chat.session().widget_action(id, action))
            .await;
    });
}

/// 去掉空白后不重复的新项文字；重复时提示。
fn checked_new_item<'a>(
    chat: Chat,
    text: &'a str,
    existing: &[&str],
    duplicate: &str,
) -> Option<&'a str> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if existing.iter().any(|e| e.trim() == text) {
        chat.overlays.toast(duplicate);
        return None;
    }
    Some(text)
}

pub fn percent(votes: usize, total: usize) -> usize {
    (votes * 100 + total / 2).checked_div(total).unwrap_or(0)
}

#[component]
pub fn WidgetCard(id: i64, widget: Widget) -> Element {
    match widget {
        Widget::Poll(poll) => rsx! { PollCard { id, poll } },
        Widget::Todo(todo) => rsx! { TodoCard { id, todo } },
    }
}

#[component]
fn CardHead(icon: IconName, title: String, label: String, onadd: EventHandler<()>) -> Element {
    rsx! {
        div { class: "widget-head",
            Icon { name: icon, size: 16.0 }
            span { class: "widget-kind", "{label}" }
            span { class: "widget-title selectable", "{title}" }
            IconButton {
                icon: IconName::Add,
                size: 22.0,
                icon_size: 12.0,
                onclick: move |e: MouseEvent| {
                    e.stop_propagation();
                    onadd.call(());
                },
            }
        }
    }
}

#[component]
fn AddRow(placeholder: String, oncommit: EventHandler<String>) -> Element {
    let mut text = use_signal(String::new);
    rsx! {
        div { class: "widget-add", onclick: |e: MouseEvent| e.stop_propagation(),
            TextInput {
                value: text(),
                placeholder,
                autofocus: true,
                oninput: move |v| text.set(v),
                onsubmit: move |v: String| {
                    text.set(String::new());
                    oncommit.call(v);
                },
            }
        }
    }
}

#[component]
fn PollCard(id: i64, poll: PollWidget) -> Element {
    let chat = use_context::<Chat>();
    let mut adding = use_signal(|| false);
    let session = chat.session();
    let self_id = session.self_id();
    let total: usize = poll.options.iter().map(|o| o.voters.len()).sum();
    let existing: Vec<String> = poll.options.iter().map(|o| o.text.clone()).collect();
    rsx! {
        div { class: "widget-card",
            CardHead {
                icon: IconName::Poll,
                label: "投票",
                title: poll.question.clone(),
                onadd: move |()| adding.toggle(),
            }
            if poll.options.is_empty() {
                div { class: "widget-empty", "暂无内容" }
            }
            for option in poll.options.iter().cloned() {
                {
                    let votes = option.voters.len();
                    let mine = option.voters.contains(&self_id);
                    let share = percent(votes, total);
                    let key = option.key.clone();
                    let names: Vec<String> = option
                        .voters
                        .iter()
                        .map(|v| session.display_name(*v))
                        .collect();
                    let tip = names.join("、");
                    rsx! {
                        div {
                            key: "{option.key}",
                            class: if mine { "poll-option mine" } else { "poll-option" },
                            title: "{tip}",
                            onclick: move |e: MouseEvent| {
                                e.stop_propagation();
                                run(chat, id, WidgetAction::Vote { option_key: key.clone(), vote: !mine });
                            },
                            div { class: "poll-bar", style: "width:{share}%" }
                            span { class: "poll-text", "{option.text}" }
                            if !names.is_empty() {
                                OverlapStack {
                                    items: names
                                        .iter()
                                        .take(VOTER_PREVIEW)
                                        .map(|n| rsx! { Avatar { text: n.clone(), size: 18.0 } })
                                        .collect::<Vec<Element>>(),
                                    item_size: 18.0,
                                    step: 12.0,
                                }
                            }
                            span { class: "poll-count", "{votes}票({share}%)" }
                        }
                    }
                }
            }
            if adding() {
                AddRow {
                    placeholder: "新选项，回车添加",
                    oncommit: move |text: String| {
                        let list: Vec<&str> = existing.iter().map(String::as_str).collect();
                        if let Some(text) = checked_new_item(chat, &text, &list, "已有相同的选项") {
                            run(chat, id, WidgetAction::AddPollOption { option: text.to_owned() });
                            adding.set(false);
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn TodoCard(id: i64, todo: TodoWidget) -> Element {
    let chat = use_context::<Chat>();
    let mut adding = use_signal(|| false);
    let existing: Vec<String> = todo.tasks.iter().map(|t| t.text.clone()).collect();
    rsx! {
        div { class: "widget-card",
            CardHead {
                icon: IconName::CheckCircle,
                label: "待办",
                title: todo.title.clone(),
                onadd: move |()| adding.toggle(),
            }
            if todo.tasks.is_empty() {
                div { class: "widget-empty", "暂无内容" }
            }
            for task in todo.tasks.iter().cloned() {
                {
                    let key = task.key.clone();
                    rsx! {
                        div {
                            key: "{task.key}",
                            class: if task.completed { "todo-task done" } else { "todo-task" },
                            onclick: |e: MouseEvent| e.stop_propagation(),
                            Checkbox {
                                checked: task.completed,
                                onchange: move |_| run(chat, id, WidgetAction::StrikeTodoTask { task_key: key.clone() }),
                            }
                            div { class: "todo-text",
                                span { "{task.text}" }
                                if !task.description.is_empty() {
                                    span { class: "todo-desc", "{task.description}" }
                                }
                            }
                        }
                    }
                }
            }
            if adding() {
                AddRow {
                    placeholder: "新任务，回车添加",
                    oncommit: move |text: String| {
                        let list: Vec<&str> = existing.iter().map(String::as_str).collect();
                        if let Some(text) = checked_new_item(chat, &text, &list, "已有相同的任务") {
                            run(chat, id, WidgetAction::AddTodoTask { task: text.to_owned(), desc: String::new() });
                            adding.set(false);
                        }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::percent;

    #[test]
    fn percent_rounds_and_handles_zero() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(2, 3), 67);
        assert_eq!(percent(3, 3), 100);
    }
}
