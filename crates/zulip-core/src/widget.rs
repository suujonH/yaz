//! Widgets in messages (polls, to-dos): sub-messages are aggregated following the Zulip web app's widget protocol.
//!
//! The first sub-message is the definition: `{"widget_type":"poll","extra_data":{"question","options"}}` or
//! `{"widget_type":"todo","extra_data":{"task_list_title","tasks":[{"task","desc"}]}}`; after that each operation is applied in id order:
//! - Poll: `new_option` (key is `sender,idx`, preset options are `canned,number`), `vote` (1 to vote, -1 to withdraw),
//!   `question` (only the message sender can change it).
//! - To-do: `new_task` (key is `key,sender`, preset tasks are `number,canned`), `strike` (toggles completion),
//!   `new_task_list_title` (only the message sender can change it).
//!
//! Options / tasks with the same name keep only the first.

use serde_json::{Value, json};

use crate::model::Submessage;

const WIDGET_MSG_TYPE: &str = "widget";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Widget {
    Poll(PollWidget),
    Todo(TodoWidget),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollWidget {
    pub question: String,
    pub options: Vec<PollOption>,
}

/// One poll option and its voters. `key` is the key referenced when voting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollOption {
    pub key: String,
    pub text: String,
    pub voters: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoWidget {
    pub title: String,
    pub tasks: Vec<TodoTask>,
}

/// A to-do task. `key` is the key referenced when checking it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoTask {
    pub key: String,
    pub text: String,
    pub description: String,
    pub completed: bool,
}

/// When the format is not recognized returns `None`, and the message is shown as a normal message.
pub fn parse(submessages: &[Submessage], message_sender_id: i64) -> Option<Widget> {
    let mut ordered: Vec<&Submessage> = submessages
        .iter()
        .filter(|s| s.msg_type == WIDGET_MSG_TYPE)
        .collect();
    ordered.sort_by_key(|s| s.id);
    let (definition, events) = ordered.split_first()?;
    let root: Value = serde_json::from_str(&definition.content).ok()?;
    let extra = root
        .get("extra_data")
        .filter(|e| e.is_object())
        .unwrap_or(&Value::Null);
    match root.get("widget_type")?.as_str()? {
        "poll" => Some(Widget::Poll(parse_poll(extra, events, message_sender_id))),
        "todo" => Some(Widget::Todo(parse_todo(extra, events, message_sender_id))),
        _ => None,
    }
}

fn parse_poll(extra: &Value, events: &[&Submessage], message_sender_id: i64) -> PollWidget {
    let mut question = string_field(extra, "question").to_owned();
    let mut options: Vec<PollOption> = Vec::new();
    let add_option = |options: &mut Vec<PollOption>, key: String, text: &str| {
        if !text.is_empty() && !options.iter().any(|o| o.key == key || o.text == text) {
            options.push(PollOption {
                key,
                text: text.to_owned(),
                voters: Vec::new(),
            });
        }
    };

    if let Some(canned) = extra.get("options").and_then(Value::as_array) {
        for (i, option) in canned.iter().enumerate() {
            add_option(
                &mut options,
                format!("canned,{i}"),
                option.as_str().unwrap_or(""),
            );
        }
    }

    for (submessage, event) in events
        .iter()
        .filter_map(|s| parse_event(s).map(|e| (*s, e)))
    {
        let sender = submessage.sender_id;
        match string_field(&event, "type") {
            "new_option" => {
                if let Some(idx) = event.get("idx").and_then(Value::as_i64) {
                    add_option(
                        &mut options,
                        format!("{sender},{idx}"),
                        string_field(&event, "option"),
                    );
                }
            }
            "vote" => {
                let key = string_field(&event, "key");
                if let Some(vote) = event.get("vote").and_then(Value::as_i64)
                    && let Some(option) = options.iter_mut().find(|o| o.key == key)
                {
                    option.voters.retain(|v| *v != sender);
                    if vote == 1 {
                        option.voters.push(sender);
                    }
                }
            }
            "question" if sender == message_sender_id => {
                question = string_field(&event, "question").to_owned()
            }
            _ => {}
        }
    }
    PollWidget { question, options }
}

fn parse_todo(extra: &Value, events: &[&Submessage], message_sender_id: i64) -> TodoWidget {
    let mut title = string_field(extra, "task_list_title").to_owned();
    let mut tasks: Vec<TodoTask> = Vec::new();
    let add_task = |tasks: &mut Vec<TodoTask>, key: String, source: &Value, completed: bool| {
        let text = string_field(source, "task");
        if !text.is_empty() && !tasks.iter().any(|t| t.key == key || t.text == text) {
            tasks.push(TodoTask {
                key,
                text: text.to_owned(),
                description: string_field(source, "desc").to_owned(),
                completed,
            });
        }
    };

    if let Some(canned) = extra.get("tasks").and_then(Value::as_array) {
        for (i, task) in canned.iter().enumerate() {
            add_task(&mut tasks, format!("{i},canned"), task, false);
        }
    }

    for (submessage, event) in events
        .iter()
        .filter_map(|s| parse_event(s).map(|e| (*s, e)))
    {
        let sender = submessage.sender_id;
        match string_field(&event, "type") {
            "new_task" => {
                if let Some(key) = event.get("key").and_then(Value::as_i64) {
                    let completed = event.get("completed") == Some(&Value::Bool(true));
                    add_task(&mut tasks, format!("{key},{sender}"), &event, completed);
                }
            }
            "strike" => {
                let key = string_field(&event, "key");
                if let Some(task) = tasks.iter_mut().find(|t| t.key == key) {
                    task.completed = !task.completed;
                }
            }
            "new_task_list_title" if sender == message_sender_id => {
                title = string_field(&event, "title").to_owned()
            }
            _ => {}
        }
    }
    TodoWidget { title, tasks }
}

/// Number for one's own next new option / task (the poll's `idx`, the to-do's `key`): one more than the largest number one has used, starting from 1.
/// The key is made of "sender + number" and the same sender cannot repeat it (the rule of `my_idx` in the web app).
pub fn next_own_index(submessages: &[Submessage], self_id: i64) -> i64 {
    submessages
        .iter()
        .filter(|s| s.msg_type == WIDGET_MSG_TYPE && s.sender_id == self_id)
        .filter_map(parse_event)
        .filter_map(|event| {
            let field = match string_field(&event, "type") {
                "new_option" => "idx",
                "new_task" => "key",
                _ => return None,
            };
            event.get(field).and_then(Value::as_i64)
        })
        .fold(1, |next, idx| next.max(idx + 1))
}

// JSON of one operation; malformed operations are skipped (without affecting other operations)
fn parse_event(submessage: &Submessage) -> Option<Value> {
    serde_json::from_str(&submessage.content)
        .ok()
        .filter(Value::is_object)
}

fn string_field<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or("")
}

// Poll / to-do operations (the content of POST /submessage), in the same format as the web app's poll_data.ts / todo_data.ts.
// The server only validates the format; the result comes back with the following submessage event.

/// Vote (when `vote` is true) or withdraw a vote.
pub fn vote_event(option_key: &str, vote: bool) -> String {
    json!({ "type": "vote", "key": option_key, "vote": if vote { 1 } else { -1 } }).to_string()
}

/// Adds a poll option; `idx` comes from [`next_own_index`].
pub fn new_option_event(idx: i64, option: &str) -> String {
    json!({ "type": "new_option", "idx": idx, "option": option }).to_string()
}

/// Adds a to-do task; `key` comes from [`next_own_index`].
pub fn new_task_event(key: i64, task: &str, desc: &str) -> String {
    json!({ "type": "new_task", "key": key, "task": task, "desc": desc, "completed": false })
        .to_string()
}

/// Toggles the completion state of a task.
pub fn strike_event(task_key: &str) -> String {
    json!({ "type": "strike", "key": task_key }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: i64 = 8;

    // Numbered in the order passed in
    fn subs(items: &[(i64, &str)]) -> Vec<Submessage> {
        items
            .iter()
            .enumerate()
            .map(|(i, (sender, content))| Submessage {
                id: i as i64 + 1,
                message_id: 100,
                sender_id: *sender,
                msg_type: WIDGET_MSG_TYPE.to_owned(),
                content: (*content).to_owned(),
            })
            .collect()
    }

    fn poll(items: &[(i64, &str)]) -> PollWidget {
        match parse(&subs(items), OWNER) {
            Some(Widget::Poll(poll)) => poll,
            other => panic!("不是投票：{other:?}"),
        }
    }

    fn todo(items: &[(i64, &str)]) -> TodoWidget {
        match parse(&subs(items), OWNER) {
            Some(Widget::Todo(todo)) => todo,
            other => panic!("不是待办：{other:?}"),
        }
    }

    #[test]
    fn unrecognized_returns_none() {
        assert_eq!(parse(&[], OWNER), None);
        for content in [
            r#"{"widget_type":"zform","extra_data":{}}"#,
            "{",
            r#"{"widget_type":1}"#,
        ] {
            assert_eq!(parse(&subs(&[(OWNER, content)]), OWNER), None, "{content}");
        }
    }

    #[test]
    fn non_integer_fields_are_ignored() {
        let result = poll(&[
            (
                OWNER,
                r#"{"widget_type":"poll","extra_data":{"question":"Q","options":["A"]}}"#,
            ),
            (9, r#"{"type":"new_option","idx":1.5,"option":"B"}"#),
            (9, r#"{"type":"vote","key":"canned,0","vote":"1"}"#),
            (9, r#"{"type":"vote","key":"canned,0","vote":1e40}"#),
        ]);
        assert_eq!(result.options.len(), 1);
        assert!(result.options[0].voters.is_empty());
    }

    #[test]
    fn poll_counts_votes_and_new_options() {
        let result = poll(&[
            (
                OWNER,
                r#"{"widget_type":"poll","extra_data":{"question":"午饭","options":["面条","米饭"]}}"#,
            ),
            (9, r#"{"type":"vote","key":"canned,1","vote":1}"#),
            (OWNER, r#"{"type":"vote","key":"canned,1","vote":1}"#),
            (9, r#"{"type":"new_option","idx":1,"option":"饺子"}"#),
            (10, r#"{"type":"vote","key":"9,1","vote":1}"#),
            (OWNER, r#"{"type":"vote","key":"canned,1","vote":-1}"#),
            (10, r#"{"type":"vote","key":"canned,9","vote":1}"#),
            (10, "not json"),
        ]);
        assert_eq!(result.question, "午饭");
        let texts: Vec<&str> = result.options.iter().map(|o| o.text.as_str()).collect();
        assert_eq!(texts, ["面条", "米饭", "饺子"]);
        let keys: Vec<&str> = result.options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, ["canned,0", "canned,1", "9,1"]);
        assert!(result.options[0].voters.is_empty());
        assert_eq!(result.options[1].voters, [9]);
        assert_eq!(result.options[2].voters, [10]);
    }

    #[test]
    fn repeated_vote_counts_once_and_duplicate_option_ignored() {
        let result = poll(&[
            (
                OWNER,
                r#"{"widget_type":"poll","extra_data":{"question":"Q","options":["A"]}}"#,
            ),
            (9, r#"{"type":"vote","key":"canned,0","vote":1}"#),
            (9, r#"{"type":"vote","key":"canned,0","vote":1}"#),
            (9, r#"{"type":"new_option","idx":1,"option":"A"}"#),
        ]);
        assert_eq!(result.options.len(), 1);
        assert_eq!(result.options[0].voters, [9]);
    }

    #[test]
    fn only_sender_can_change_question() {
        let result = poll(&[
            (
                OWNER,
                r#"{"widget_type":"poll","extra_data":{"question":"旧","options":[]}}"#,
            ),
            (9, r#"{"type":"question","question":"别人改的"}"#),
            (OWNER, r#"{"type":"question","question":"新"}"#),
        ]);
        assert_eq!(result.question, "新");
    }

    #[test]
    fn applies_in_id_order() {
        let mut items = subs(&[
            (
                OWNER,
                r#"{"widget_type":"poll","extra_data":{"question":"Q","options":["A"]}}"#,
            ),
            (9, r#"{"type":"vote","key":"canned,0","vote":1}"#),
            (9, r#"{"type":"vote","key":"canned,0","vote":-1}"#),
        ]);
        items.reverse();
        let Some(Widget::Poll(result)) = parse(&items, OWNER) else {
            panic!("不是投票");
        };
        assert!(result.options[0].voters.is_empty());
    }

    #[test]
    fn todo_tasks_and_strike() {
        let result = todo(&[
            (
                OWNER,
                r#"{"widget_type":"todo","extra_data":{"task_list_title":"发布","tasks":[{"task":"打包","desc":"zip"},{"task":"测试","desc":""}]}}"#,
            ),
            (
                9,
                r#"{"type":"new_task","key":2,"task":"公告","desc":"写说明","completed":false}"#,
            ),
            (9, r#"{"type":"strike","key":"0,canned"}"#),
            (OWNER, r#"{"type":"strike","key":"2,9"}"#),
            (OWNER, r#"{"type":"strike","key":"2,9"}"#),
            (9, r#"{"type":"new_task_list_title","title":"别人改的"}"#),
        ]);
        assert_eq!(result.title, "发布");
        let texts: Vec<&str> = result.tasks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["打包", "测试", "公告"]);
        let keys: Vec<&str> = result.tasks.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["0,canned", "1,canned", "2,9"]);
        assert_eq!(result.tasks[0].description, "zip");
        let completed: Vec<bool> = result.tasks.iter().map(|t| t.completed).collect();
        assert_eq!(completed, [true, false, false]);
    }

    #[test]
    fn sender_changes_todo_title() {
        let result = todo(&[
            (
                OWNER,
                r#"{"widget_type":"todo","extra_data":{"task_list_title":"","tasks":[]}}"#,
            ),
            (OWNER, r#"{"type":"new_task_list_title","title":"本周"}"#),
        ]);
        assert_eq!(result.title, "本周");
        assert!(result.tasks.is_empty());
    }

    #[test]
    fn events_round_trip_through_parser() {
        let result = poll(&[
            (
                OWNER,
                r#"{"widget_type":"poll","extra_data":{"question":"Q","options":["A","B"]}}"#,
            ),
            (9, &vote_event("canned,1", true)),
            (10, &vote_event("canned,1", true)),
            (10, &vote_event("canned,1", false)),
            (9, &new_option_event(1, "饺子")),
        ]);
        assert_eq!(result.options[1].voters, [9]);
        assert_eq!(
            (
                result.options[2].key.as_str(),
                result.options[2].text.as_str()
            ),
            ("9,1", "饺子")
        );

        let result = todo(&[
            (
                OWNER,
                r#"{"widget_type":"todo","extra_data":{"task_list_title":"","tasks":[{"task":"A","desc":""}]}}"#,
            ),
            (9, &strike_event("0,canned")),
            (9, &new_task_event(1, "写说明", "")),
        ]);
        assert!(result.tasks[0].completed);
        assert_eq!(result.tasks[1].key, "1,9");
        assert_eq!(result.tasks[1].text, "写说明");
        assert!(!result.tasks[1].completed);
    }

    #[test]
    fn events_match_web_client_format() {
        for (actual, expected) in [
            (
                vote_event("canned,0", true),
                r#"{"type":"vote","key":"canned,0","vote":1}"#,
            ),
            (
                vote_event("9,1", false),
                r#"{"type":"vote","key":"9,1","vote":-1}"#,
            ),
            (strike_event("2,9"), r#"{"type":"strike","key":"2,9"}"#),
            (
                new_option_event(3, "A"),
                r#"{"type":"new_option","idx":3,"option":"A"}"#,
            ),
            (
                new_task_event(2, "T", ""),
                r#"{"type":"new_task","key":2,"task":"T","desc":"","completed":false}"#,
            ),
        ] {
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn next_index_after_own_options_and_tasks() {
        assert_eq!(next_own_index(&[], 9), 1);
        let items = subs(&[
            (
                OWNER,
                r#"{"widget_type":"poll","extra_data":{"question":"Q","options":["A"]}}"#,
            ),
            (9, r#"{"type":"new_option","idx":5,"option":"B"}"#),
            (9, r#"{"type":"new_option","idx":2,"option":"C"}"#),
            (10, r#"{"type":"new_option","idx":40,"option":"D"}"#),
            (9, r#"{"type":"vote","key":"10,40","vote":1}"#),
        ]);
        assert_eq!(next_own_index(&items, 9), 6);
        let items = subs(&[(
            9,
            r#"{"type":"new_task","key":3,"task":"T","desc":"","completed":false}"#,
        )]);
        assert_eq!(next_own_index(&items, 9), 4);
    }
}
