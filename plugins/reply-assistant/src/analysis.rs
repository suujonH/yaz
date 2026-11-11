//! Analyzes the other side's latest few messages the jev way: sends two decisions requests in
//! parallel (behavior and tone, conflict level), and interprets them with jev's thresholds.

use std::collections::HashMap;

use zulip_plugin_sdk::abi::LOG_WARNING;
use zulip_plugin_sdk::serde_json::{Map, Value, json};
use zulip_plugin_sdk::{Conversation, Message, log, model_center, str_of};

use crate::catalog::{CATALOG, Catalog, Question, Scene};
use crate::scenes::SceneChoice;

// jev's threshold: >= HIT counts as present.
const HIT: f64 = 0.7;
// The conflict request carries only these few messages from before the latest round.
const RECENT: usize = 4;
const MAX_LABELS: usize = 3;
const FRICTION_OTHER: &str = "friction.other";
const FRICTION_SELF: &str = "friction.self";

/// Conclusion of the jev analysis: what the other side is doing, signals, tone (each at most 3
/// items, by descending probability), number of unanswered questions, and conflict level.
#[derive(Debug, Default, PartialEq)]
pub struct Findings {
    pub doing: Vec<String>,
    pub cues: Vec<String>,
    pub tones: Vec<String>,
    pub missing: usize,
    /// Text and level (1 to 5); None when below 1 or not answered.
    pub friction: Option<(String, usize)>,
    /// Conflict level 0 to 5; None when the conflict request failed.
    pub friction_level: Option<usize>,
    /// Probability (0 to 1) of the most likely "what the other side is doing" item; None when
    /// nothing reached the threshold.
    pub confidence: Option<f64>,
}

impl Findings {
    /// As text lines (passed to the reply-writing model).
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "对方在：{}",
            if self.doing.is_empty() {
                "没有明确识别到".to_owned()
            } else {
                self.doing.join("、")
            }
        )];
        if !self.cues.is_empty() {
            lines.push(format!("信号：{}", self.cues.join("、")));
        }
        if !self.tones.is_empty() {
            lines.push(format!("语气：{}", self.tones.join("、")));
        }
        if let Some((friction, _)) = &self.friction {
            lines.push(format!("冲突：{friction}"));
        }
        if self.missing > 0 {
            lines.push(format!("有 {} 项没能读出", self.missing));
        }
        lines
    }
}

/// Called on a worker thread (blocks until both requests return). `context` is the messages before
/// this one (in chronological order). Not analyzed when this message was sent by self (jev analyzes
/// only the other side's messages).
pub fn analyze(
    model: &Value,
    choice: &SceneChoice,
    message: &Message,
    context: &[Message],
) -> Result<Findings, String> {
    if message.is_outgoing() {
        return Err("这条是你发送的消息，jev 只分析对方的消息".to_owned());
    }
    let catalog = &*CATALOG;
    let questions = questions_for(choice);
    let (earlier, latest) = split_latest_round(context, message);
    let state_of = |earlier: &[&Message]| {
        json!({
            "relationship_note": choice.note(),
            "earlier_messages": to_state(earlier, message),
            "latest_messages": to_state(&latest, message),
        })
    };
    let state = state_of(&earlier);
    let friction_state = state_of(&earlier[earlier.len().saturating_sub(RECENT)..]);
    let behaviour = behaviour_questions(catalog, &questions);
    let friction = json!({
        FRICTION_OTHER: score_question(catalog, choice.scene, &catalog.friction_other_question),
        FRICTION_SELF: score_question(catalog, choice.scene, &catalog.friction_self_question),
    });

    let (answers, friction_answers) = std::thread::scope(|scope| {
        let friction_task = scope.spawn(|| model_center::decide(model, &friction_state, &friction));
        let answers = model_center::decide(model, &state, &behaviour);
        let friction_answers = friction_task
            .join()
            .unwrap_or_else(|_| Err("分析线程异常退出".to_owned()));
        (answers, friction_answers)
    });
    let answers = answers?;
    let friction_answers = friction_answers
        .inspect_err(|error| log(LOG_WARNING, &format!("jev 冲突程度分析失败：{error}")))
        .ok();
    Ok(read_findings(
        catalog,
        &questions,
        &answers,
        friction_answers.as_ref(),
    ))
}

fn questions_for(choice: &SceneChoice) -> Vec<&'static Question> {
    choice
        .scene
        .questions
        .iter()
        .filter(|q| match &q.relationships {
            None => true,
            Some(allowed) => choice.relationship.is_some_and(|r| allowed.contains(&r.id)),
        })
        .collect()
}

// The latest round: this message and the messages the same person sent consecutively before it.
// Returns (earlier messages, latest round).
fn split_latest_round<'a>(
    context: &'a [Message],
    message: &'a Message,
) -> (Vec<&'a Message>, Vec<&'a Message>) {
    let mut start = context.len();
    while start > 0
        && !context[start - 1].is_outgoing()
        && context[start - 1].sender_name == message.sender_name
    {
        start -= 1;
    }
    let earlier = context[..start].iter().collect();
    let latest = context[start..]
        .iter()
        .chain(std::iter::once(message))
        .collect();
    (earlier, latest)
}

// In group chats jev still only distinguishes "me / the other side"; the speaker is prefixed to the
// text.
fn to_state(messages: &[&Message], target: &Message) -> Value {
    let group = match &target.conv {
        Conversation::Channel { .. } => true,
        Conversation::Direct { user_ids } => user_ids.len() > 1,
    };
    let items: Vec<Value> = messages
        .iter()
        .map(|m| {
            let outgoing = m.is_outgoing();
            let text = if group && !outgoing {
                format!("{}: {}", m.sender_name, m.text)
            } else {
                m.text.clone()
            };
            json!({"from": if outgoing { "user" } else { "other" }, "text": text})
        })
        .collect();
    Value::Array(items)
}

fn behaviour_questions(catalog: &Catalog, questions: &[&Question]) -> Value {
    let mut map = Map::new();
    for q in questions {
        map.insert(
            q.id.clone(),
            json!({
                "type": "noul",
                "instructions": format!("{} {}", q.question, catalog.data_note),
                "criteria": {"true": q.yes, "false": q.no},
            }),
        );
    }
    Value::Object(map)
}

// Conflict level question: 6 levels; examples are not written when empty.
fn score_question(catalog: &Catalog, scene: &Scene, question: &str) -> Value {
    let criteria: Vec<Value> = catalog
        .friction_levels
        .iter()
        .enumerate()
        .map(|(i, level)| {
            let mut criterion = json!({"what": level.what});
            if let Some(examples) = scene.friction_examples.get(i).filter(|e| !e.is_empty()) {
                criterion["examples"] = json!(examples);
            }
            criterion
        })
        .collect();
    json!({
        "type": "score",
        "instructions": format!("{question} {}", catalog.data_note),
        "criteria": criteria,
    })
}

fn read_findings(
    catalog: &Catalog,
    questions: &[&Question],
    answers: &Value,
    friction_answers: Option<&Value>,
) -> Findings {
    let mut probabilities: HashMap<&str, f64> = HashMap::new();
    for q in questions {
        let answer = &answers[q.id.as_str()];
        if str_of(answer, "type") == "noul"
            && let Some(p) = answer["noul"].as_f64()
            && (0.0..=1.0).contains(&p)
        {
            probabilities.insert(&q.id, p);
        }
    }
    // Hits by descending probability (at most MAX_LABELS).
    let hits = |filter: &dyn Fn(&Question) -> bool| {
        let mut hits: Vec<(&Question, f64)> = questions
            .iter()
            .filter(|q| filter(q))
            .filter_map(|q| {
                let p = *probabilities.get(q.id.as_str())?;
                (p >= HIT).then_some((*q, p))
            })
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.truncate(MAX_LABELS);
        hits
    };
    let labels = |hits: &[(&Question, f64)]| hits.iter().map(|(q, _)| q.label.clone()).collect();
    let doing = hits(&|q| q.kind == "behavior" && q.role != "cue");
    Findings {
        doing: labels(&doing),
        cues: labels(&hits(&|q| q.kind == "behavior" && q.role == "cue")),
        tones: labels(&hits(&|q| q.kind == "tone")),
        missing: questions
            .iter()
            .filter(|q| !probabilities.contains_key(q.id.as_str()))
            .count(),
        friction: friction_text(catalog, friction_answers),
        friction_level: friction_level(catalog, friction_answers),
        confidence: doing.first().map(|(_, p)| *p),
    }
}

// The higher of the two sides, 0 to 5 (below 1 counts as 0); None when neither side was answered.
fn friction_level(catalog: &Catalog, answers: Option<&Value>) -> Option<usize> {
    let answers = answers?;
    let other = answers[FRICTION_OTHER]["score"].as_f64();
    let own = answers[FRICTION_SELF]["score"].as_f64();
    if other.is_none() && own.is_none() {
        return None;
    }
    let level = other.unwrap_or(0.0).max(own.unwrap_or(0.0));
    if level < 1.0 {
        return Some(0);
    }
    Some((level.round() as usize).min(catalog.friction_levels.len().saturating_sub(1)))
}

// The higher of the two sides (0 to 5); when >= 1 the level and source are shown; None when there
// is no answer. Returns (text, level index).
fn friction_text(catalog: &Catalog, answers: Option<&Value>) -> Option<(String, usize)> {
    let answers = answers?;
    let other = answers[FRICTION_OTHER]["score"].as_f64();
    let own = answers[FRICTION_SELF]["score"].as_f64();
    let level = other.unwrap_or(0.0).max(own.unwrap_or(0.0));
    if level < 1.0 {
        return None;
    }
    let source = match (
        other.is_some_and(|s| s >= 1.0),
        own.is_some_and(|s| s >= 1.0),
    ) {
        (true, true) => "双方都有",
        (true, false) => "来自对方",
        _ => "来自你",
    };
    let index = (level.round() as usize).min(catalog.friction_levels.len().saturating_sub(1));
    Some((
        format!("{}（{source}）", catalog.friction_levels[index].label),
        index,
    ))
}

#[cfg(test)]
mod tests {
    use zulip_plugin_sdk::abi::MSG_OUTGOING;

    use super::*;
    use crate::scenes::parse_identity;

    fn message(id: i64, sender: &str, flags: u32, conv: Conversation) -> Message {
        Message {
            id,
            account_id: 1,
            timestamp: 0,
            flags,
            sender_id: id,
            sender_name: sender.to_owned(),
            sender_email: String::new(),
            conv,
            html: String::new(),
            text: format!("文字{id}"),
            reactions: Vec::new(),
            uploads: Vec::new(),
            last_edit_timestamp: None,
        }
    }

    fn direct() -> Conversation {
        Conversation::Direct { user_ids: vec![7] }
    }

    #[test]
    fn latest_round_is_consecutive_messages_of_sender() {
        let context = vec![
            message(1, "甲", 0, direct()),
            message(2, "我", MSG_OUTGOING, direct()),
            message(3, "甲", 0, direct()),
            message(4, "甲", 0, direct()),
        ];
        let target = message(5, "甲", 0, direct());
        let (earlier, latest) = split_latest_round(&context, &target);
        assert_eq!(earlier.iter().map(|m| m.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(latest.iter().map(|m| m.id).collect::<Vec<_>>(), [3, 4, 5]);
    }

    #[test]
    fn state_prefixes_sender_in_group_only() {
        let other = message(1, "甲", 0, direct());
        let mine = message(2, "我", MSG_OUTGOING, direct());
        let state = to_state(&[&other, &mine], &other);
        assert_eq!(state[0], json!({"from": "other", "text": "文字1"}));
        assert_eq!(state[1]["from"], "user");

        let channel = message(
            3,
            "乙",
            0,
            Conversation::Channel {
                stream_id: 1,
                stream_name: "s".into(),
                topic: "t".into(),
            },
        );
        let state = to_state(&[&channel, &mine], &channel);
        assert_eq!(state[0]["text"], "乙: 文字3");
        assert_eq!(state[1]["text"], "文字2");
    }

    #[test]
    fn outgoing_message_is_not_analyzed() {
        let mine = message(1, "我", MSG_OUTGOING, direct());
        let choice = parse_identity("work/peer").unwrap();
        assert!(analyze(&json!({}), &choice, &mine, &[]).is_err());
    }

    #[test]
    fn relationship_questions_need_matching_relationship() {
        let scene = &CATALOG.scenes[0];
        let unspecified = SceneChoice {
            scene,
            relationship: None,
        };
        assert!(
            questions_for(&unspecified)
                .iter()
                .all(|q| q.relationships.is_none())
        );
    }

    #[test]
    fn findings_apply_thresholds_and_order() {
        let scene = CATALOG.scene("work").unwrap();
        let behaviours: Vec<&Question> = scene
            .questions
            .iter()
            .filter(|q| q.relationships.is_none() && q.kind == "behavior" && q.role != "cue")
            .take(3)
            .collect();
        let mut answers = Map::new();
        answers.insert(
            behaviours[0].id.clone(),
            json!({"type": "noul", "noul": 0.8}),
        );
        answers.insert(
            behaviours[1].id.clone(),
            json!({"type": "noul", "noul": 0.95}),
        );
        answers.insert(
            behaviours[2].id.clone(),
            json!({"type": "noul", "noul": 0.5}),
        );
        let findings = read_findings(&CATALOG, &behaviours, &Value::Object(answers), None);
        assert_eq!(
            findings.doing,
            vec![behaviours[1].label.clone(), behaviours[0].label.clone()]
        );
        assert_eq!(findings.missing, 0);
        assert_eq!(findings.confidence, Some(0.95));

        let empty = read_findings(&CATALOG, &behaviours, &json!({}), None);
        assert_eq!(empty.missing, 3);
        assert_eq!(empty.lines()[0], "对方在：没有明确识别到");
    }

    #[test]
    fn friction_takes_higher_side() {
        let text = |answers: Value| friction_text(&CATALOG, Some(&answers));
        assert_eq!(friction_text(&CATALOG, None), None);
        assert_eq!(text(json!({FRICTION_OTHER: {"score": 0}})), None);
        let levels = &CATALOG.friction_levels;
        assert_eq!(
            text(json!({FRICTION_OTHER: {"score": 2}, FRICTION_SELF: {"score": 0}})),
            Some((format!("{}（来自对方）", levels[2].label), 2))
        );
        assert_eq!(
            text(json!({FRICTION_OTHER: {"score": 1}, FRICTION_SELF: {"score": 3}})),
            Some((format!("{}（双方都有）", levels[3].label), 3))
        );
        assert_eq!(
            text(json!({FRICTION_SELF: {"score": 9}})),
            Some((format!("{}（来自你）", levels[5].label), 5))
        );
    }

    #[test]
    fn friction_level_and_lines() {
        let level = |answers: Value| friction_level(&CATALOG, Some(&answers));
        assert_eq!(friction_level(&CATALOG, None), None);
        assert_eq!(level(json!({})), None);
        assert_eq!(level(json!({FRICTION_OTHER: {"score": 0.4}})), Some(0));
        assert_eq!(
            level(json!({FRICTION_OTHER: {"score": 1}, FRICTION_SELF: {"score": 3.2}})),
            Some(3)
        );
        let findings = Findings {
            cues: vec!["催促".into(), "追问".into()],
            ..Default::default()
        };
        assert_eq!(
            findings.lines(),
            ["对方在：没有明确识别到", "信号：催促、追问"]
        );
    }

    #[test]
    fn score_question_has_six_levels() {
        let scene = CATALOG.scene("work").unwrap();
        let question = score_question(&CATALOG, scene, "Q");
        assert_eq!(question["criteria"].as_array().unwrap().len(), 6);
        assert_eq!(question["type"], "score");
    }
}
