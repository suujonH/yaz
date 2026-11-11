//! Reply assistant (modeled on jev-chat-jarvis's "understand first, then reply"): the message
//! context menu item "Analyze this message" -> on a worker thread, first uses the analysis model
//! (jev, OpenRouter decisions interface) to analyze the other side's messages by scene, then sends
//! the earlier chat, this message and the analysis conclusion to the reply-writing model, and shows
//! below the message the analysis, intent, risk, what the other side needs, suggestions, and
//! candidate reply buttons (clicking fills the composer; the user sends it themselves). Both models
//! can be used on their own. The scene is set per topic and the identity per friend, in the topic
//! profile panel / user profile card, and saved together with the analysis result in the plugin's
//! own database, and the analysis result is re-attached when the message is shown again.

mod analysis;
mod catalog;
mod scenes;
mod store;

use zulip_plugin_sdk::abi::{LOG_ERROR, LOG_INFO, LOG_WARNING};
use zulip_plugin_sdk::serde_json::{self, Value, json};
use zulip_plugin_sdk::{
    Message, MessagesShown, PanelRequest, Plugin, annotate, clear_annotation, context_messages,
    export_plugin, extract_json, format_time, log, model_center, post, reply_panel, select_account,
    set_annotation, settings, str_of,
};

use crate::analysis::{Findings, analyze};
use crate::catalog::CATALOG;
use crate::scenes::{SceneChoice, identity_options, parse_identity, resolve, scene_options};

const MENU_ANALYZE: i32 = 1;
// Header buttons of the result card (not in plugin.json's menus).
const MENU_REFRESH: i32 = 2;
const MENU_REMOVE: i32 = 3;
const MAX_REPLIES: usize = 3;
const MAX_PREPARE: usize = 4;
const MAX_CONTEXT: usize = 50;
const DEFAULT_CONTEXT: usize = 10;
const SCENE_FIELD: &str = "scene";
const IDENTITY_FIELD: &str = "identity";
const ANALYSIS_MODEL_FIELD: &str = "analysis_model";
const MODEL_FIELD: &str = "model";

struct Request {
    message: Message,
    settings: Value,
    analysis_model: Option<Value>,
    model: Option<Value>,
    prompt: String,
    background: String,
    context_count: usize,
}

struct ReplyAssistant;

impl Plugin for ReplyAssistant {
    // Analyze: shows the saved result directly when there is one (also after it was removed),
    // otherwise requests. The card's refresh button always requests again; its close button (X) only
    // removes the card (the result stays saved).
    fn on_message_menu(&self, menu_id: i32, message: &Message) {
        match menu_id {
            MENU_ANALYZE => {
                store::set_hidden(message.account_id, message.id, false);
                match saved_annotation(message.account_id, message.id) {
                    Some(saved) => annotate(message.id, &with_tools(saved)),
                    None => start_analysis(message),
                }
            }
            MENU_REFRESH => {
                store::set_hidden(message.account_id, message.id, false);
                start_analysis(message);
            }
            MENU_REMOVE => {
                store::set_hidden(message.account_id, message.id, true);
                if let Err(error) = clear_annotation(message.id) {
                    log(LOG_WARNING, &format!("移除分析结果失败：{error}"));
                }
            }
            _ => {}
        }
    }

    // Re-attach the saved analysis result when a message is shown (the callback's account is the
    // message's account); removed ones are not.
    fn on_messages_shown(&self, shown: &MessagesShown) {
        for (message_id, content) in store::analyses(shown.account_id, &shown.message_ids, false) {
            if let Some(annotation) = parse_annotation(&content) {
                annotate(message_id, &with_tools(annotation));
            }
        }
    }

    // The topic profile panel (scene) and user profile card (identity), and the list in plugin
    // settings.
    fn on_panel(&self, request: &PanelRequest) {
        let settings = settings();
        let reply = match request.action.as_str() {
            "list" => json!({"entries": entries(request.is_user)}),
            "remove" => {
                save_panel_value(request, "");
                return;
            }
            "set" => {
                let value = request.value.as_str().unwrap_or_default();
                if is_valid_panel_value(request, value) {
                    save_panel_value(request, value);
                }
                json!({"fields": [panel_field(request, &settings)]})
            }
            "get" => json!({"fields": [panel_field(request, &settings)]}),
            _ => return,
        };
        if let Err(error) = reply_panel(&reply) {
            log(LOG_WARNING, &format!("回复面板失败：{error}"));
        }
    }
}

// Requests the analysis on a worker thread, showing progress below the message.
fn start_analysis(message: &Message) {
    let settings = settings();
    let analysis_model = model_center::selection(&settings, ANALYSIS_MODEL_FIELD);
    let model = model_center::selection(&settings, MODEL_FIELD);
    if analysis_model.is_none() && model.is_none() {
        annotate_text(
            message.id,
            "请先在“设置 > 插件 > 回复助手 > 设置”中选择分析模型或写回复模型",
        );
        return;
    }
    let request = Request {
        message: message.clone(),
        analysis_model,
        model,
        prompt: str_of(&settings, "prompt").to_owned(),
        background: str_of(&settings, "background").trim().to_owned(),
        context_count: str_of(&settings, "context")
            .parse::<usize>()
            .map_or(DEFAULT_CONTEXT, |n| n.min(MAX_CONTEXT)),
        settings,
    };
    annotate_text(message.id, "正在分析…");
    if !post(move || run_analysis(&request)) {
        annotate_text(message.id, "待分析的消息过多，请稍后再试");
    }
}

fn parse_annotation(content: &str) -> Option<Value> {
    serde_json::from_str::<Value>(content)
        .ok()
        .filter(Value::is_object)
}

fn saved_annotation(account_id: i64, message_id: i64) -> Option<Value> {
    parse_annotation(&store::analysis(account_id, message_id)?)
}

/// Adds the refresh and close buttons to the card header (not saved with the result).
fn with_tools(mut annotation: Value) -> Value {
    annotation["tools"] = json!([
        {"menu": MENU_REFRESH, "icon": "refresh", "tooltip": "重新获取"},
        {"menu": MENU_REMOVE, "icon": "close", "tooltip": "关闭（再次分析时直接显示，不重新请求）"},
    ]);
    annotation
}

fn annotate_text(message_id: i64, text: &str) {
    annotate(message_id, &json!({ "text": text }));
}

fn is_valid_panel_value(request: &PanelRequest, value: &str) -> bool {
    if request.is_user {
        request.key == IDENTITY_FIELD && (value.is_empty() || parse_identity(value).is_some())
    } else {
        request.key == SCENE_FIELD && (value.is_empty() || CATALOG.scene(value).is_some())
    }
}

fn save_panel_value(request: &PanelRequest, value: &str) {
    if request.is_user {
        store::set_identity(request.account_id, request.user_id, &request.name, value);
    } else {
        store::set_topic_scene(
            request.account_id,
            request.stream_id,
            &request.topic,
            &request.name,
            value,
        );
    }
}

fn panel_field(request: &PanelRequest, settings: &Value) -> Value {
    if request.is_user {
        json!({
            "key": IDENTITY_FIELD, "label": "身份", "type": "select", "options": identity_options(),
            "value": store::get_identity(request.account_id, request.user_id).unwrap_or_default(),
        })
    } else {
        json!({
            "key": SCENE_FIELD, "label": "分析场景", "type": "select", "options": scene_options(settings),
            "value": store::get_topic_scene(request.account_id, request.stream_id, &request.topic)
                .unwrap_or_default(),
        })
    }
}

fn entries(users: bool) -> Vec<Value> {
    if users {
        store::identities()
            .into_iter()
            .map(|i| {
                let text = parse_identity(&i.identity)
                    .map(|c| c.label())
                    .unwrap_or_default();
                json!({
                    "target": {"account": i.account_id, "user": i.user_id, "name": i.name},
                    "text": text,
                })
            })
            .collect()
    } else {
        store::topic_scenes()
            .into_iter()
            .map(|t| {
                let text = CATALOG
                    .scene(&t.scene)
                    .map(|s| s.label.clone())
                    .unwrap_or_default();
                json!({
                    "target": {"account": t.account_id, "stream": t.stream_id, "topic": t.topic, "name": t.name},
                    "text": text,
                })
            })
            .collect()
    }
}

// Worker thread.
fn run_analysis(request: &Request) {
    let message = &request.message;
    if select_account(message.account_id).is_err() {
        return;
    }
    let context = context_messages(message, request.context_count).unwrap_or_default();

    let mut card = Card::default();
    let mut findings: Option<Findings> = None;
    let mut choice: Option<SceneChoice> = None;
    let mut analysis_failed = false;
    if let Some(analysis_model) = &request.analysis_model {
        let scene = resolve(message, &request.settings);
        card.scene = scene.label();
        match analyze(analysis_model, &scene, message, &context) {
            Ok(result) => {
                if !result.doing.is_empty() {
                    card.title = format!("对方在：{}", result.doing.join("、"));
                }
                card.confidence = result.confidence;
                card.danger = result.friction_level.map(friction_danger);
                findings = Some(result);
            }
            Err(error) => {
                log(
                    LOG_ERROR,
                    &format!("jev 分析消息 {} 失败：{error}", message.id),
                );
                card.notes.push(format!("jev 分析失败：{error}"));
                analysis_failed = true;
            }
        }
        choice = Some(scene);
        if request.model.is_none() {
            show(message, &card.to_json(), !analysis_failed);
            return;
        }
        let mut progress = card.clone();
        progress.notes.push("正在生成候选回复…".to_owned());
        annotate(message.id, &progress.to_json());
    }
    let Some(model) = &request.model else {
        return;
    };

    let situation = findings.as_ref().map(|f| f.lines().join("；"));
    let relationship = choice.map(|c| c.label()).unwrap_or_default();
    let user = user_prompt(
        request,
        &context,
        situation.as_deref().map(|s| (s, relationship.as_str())),
    );
    match model_center::ask(model, &request.prompt, &user) {
        Ok(reply) => {
            add_reply(&mut card, &reply);
            show(message, &card.to_json(), !analysis_failed);
        }
        Err(error) => {
            log(LOG_ERROR, &format!("分析消息 {} 失败：{error}", message.id));
            card.notes.push(format!("生成回复失败：{error}"));
            show(message, &card.to_json(), false);
        }
    }
}

/// The analysis result shown below the message, kept short: the danger level (the only color),
/// the headline (intent), how sure the judgment is and what to prepare (plain text), notes
/// (failures, progress), candidate replies.
#[derive(Clone, Default)]
struct Card {
    /// (level, tag tone).
    danger: Option<(&'static str, &'static str)>,
    /// Scene used by jev (shown on hovering the danger level).
    scene: String,
    title: String,
    /// 0 to 1.
    confidence: Option<f64>,
    prepare: String,
    notes: Vec<String>,
    replies: Vec<Value>,
}

impl Card {
    fn to_json(&self) -> Value {
        let mut card = json!({});
        if let Some((level, tone)) = self.danger {
            let tip = if self.scene.is_empty() {
                String::new()
            } else {
                format!(
                    "分析场景：{}（可在话题资料面板、用户资料卡中修改）",
                    self.scene
                )
            };
            card["tags"] = json!([{"text": format!("危险 {level}"), "tone": tone, "tip": tip}]);
        }
        if !self.title.is_empty() {
            card["title"] = json!(self.title);
        }
        let mut fields = Vec::new();
        if let Some(confidence) = self.confidence {
            fields.push(json!({"label": "把握", "value": format!("{:.0}%", confidence * 100.0)}));
        }
        if !self.prepare.is_empty() {
            fields.push(json!({"label": "需要准备", "value": self.prepare}));
        }
        if !fields.is_empty() {
            card["fields"] = json!(fields);
        }
        if !self.notes.is_empty() {
            card["text"] = json!(self.notes.join(
                "
"
            ));
        }
        if !self.replies.is_empty() {
            card["replies_title"] = json!("候选回复");
            card["replies"] = json!(self.replies);
        }
        card
    }
}

// jev's conflict level 0 to 5 as the danger level.
fn friction_danger(level: usize) -> (&'static str, &'static str) {
    match level {
        0 | 1 => ("低", "success"),
        2 => ("中", "warning"),
        _ => ("高", "danger"),
    }
}

const RISK_SEPARATORS: &[char] = &['：', ':', '，', ',', '、', '。', ' ', '-', '—'];

// "中：原因" / "风险高，原因" / "低" -> (level, tag tone, reason); None when there is no level at the
// start.
fn risk_level(risk: &str) -> Option<(&'static str, &'static str, &str)> {
    let rest = risk.strip_prefix("风险").unwrap_or(risk);
    let mut chars = rest.trim_start_matches(RISK_SEPARATORS).chars();
    let (level, tone) = match chars.next()? {
        '低' => ("低", "success"),
        '中' => ("中", "warning"),
        '高' => ("高", "danger"),
        _ => return None,
    };
    let reason = chars.as_str();
    let reason = reason.strip_prefix("风险").unwrap_or(reason);
    Some((level, tone, reason.trim_start_matches(RISK_SEPARATORS)))
}

fn speaker(message: &Message) -> String {
    if message.is_outgoing() {
        format!("我（{}）", message.sender_name)
    } else {
        message.sender_name.clone()
    }
}

// situation: jev's conclusion and the relationship.
fn user_prompt(request: &Request, context: &[Message], situation: Option<(&str, &str)>) -> String {
    let message = &request.message;
    let mut user = format!("会话：{}\n", message.conv.describe());
    if !request.background.is_empty() {
        user += &format!("关于我：{}\n", request.background);
    }
    if !context.is_empty() {
        let history: Vec<String> = context
            .iter()
            .map(|m| format!("[{}] {}：{}", format_time(m.timestamp), speaker(m), m.text))
            .collect();
        user += &format!("此前的聊天：\n{}\n", history.join("\n"));
    }
    if let Some((situation, relationship)) = situation {
        user += &format!("分析模型（jev）对这条消息的结论（关系：{relationship}）：{situation}\n");
    }
    user += &format!(
        "需要分析的消息（[{}] {}）：\n{}",
        format_time(message.timestamp),
        speaker(message),
        message.text
    );
    user
}

// save: saves the result, re-attached when the message is shown again (failed results are not
// saved).
fn show(message: &Message, annotation: &Value, save: bool) {
    if save {
        store::set_analysis(message.account_id, message.id, &annotation.to_string());
    }
    match set_annotation(message.id, &with_tools(annotation.clone())) {
        Err(error) => log(LOG_WARNING, &format!("显示分析结果失败：{error}")),
        Ok(()) => log(LOG_INFO, &format!("已分析消息 {}", message.id)),
    }
}

// When the model returns JSON: the intent becomes the headline, its risk level the danger level
// (over jev's), its confidence is used when jev gave none, prepare (or else advice) the plain text
// of what to prepare, and the replies candidates; when it is not JSON, it is shown whole as a note.
fn add_reply(card: &mut Card, reply: &str) {
    let Some(parsed) = extract_json(reply) else {
        card.notes.push(reply.trim().to_owned());
        return;
    };
    let intent = str_of(&parsed, "intent").trim();
    if !intent.is_empty() {
        card.title = format!("对方意图：{intent}");
    }
    if let Some((level, tone, _)) = risk_level(str_of(&parsed, "risk").trim()) {
        card.danger = Some((level, tone));
    }
    if card.confidence.is_none() {
        card.confidence = parsed["confidence"]
            .as_f64()
            .map(|c| if c > 1.0 { c / 100.0 } else { c })
            .filter(|c| (0.0..=1.0).contains(c));
    }
    let prepare: Vec<&str> = parsed["prepare"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(MAX_PREPARE)
        .collect();
    card.prepare = if prepare.is_empty() {
        str_of(&parsed, "advice").trim().to_owned()
    } else {
        prepare.join(" · ")
    };
    card.replies = parsed["replies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(MAX_REPLIES)
        .enumerate()
        .map(|(i, text)| json!({"label": format!("#{}", i + 1), "fill": text}))
        .collect();
}

export_plugin!(ReplyAssistant, "回复助手");

#[cfg(test)]
mod tests {
    use zulip_plugin_sdk::Conversation;
    use zulip_plugin_sdk::abi::MSG_OUTGOING;

    use super::*;

    #[test]
    fn card_from_json_reply() {
        let reply = r#"结果：{"intent": " 想约时间 ", "risk": "中：要加班", "confidence": 72, "prepare": ["查日程", " ", "想好替代时间"], "advice": "x", "replies": ["好", " ", "可以", "行", "嗯"]}"#;
        let mut card = Card {
            scene: "工作".into(),
            danger: Some(friction_danger(0)),
            ..Default::default()
        };
        add_reply(&mut card, reply);
        let json = card.to_json();
        assert_eq!(json["title"], "对方意图：想约时间");
        assert_eq!(json["tags"][0]["text"], "危险 中");
        assert_eq!(json["tags"][0]["tone"], "warning");
        assert_eq!(
            json["fields"],
            json!([{"label": "把握", "value": "72%"}, {"label": "需要准备", "value": "查日程 · 想好替代时间"}])
        );
        assert!(json.get("text").is_none());
        let replies = json["replies"].as_array().unwrap();
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[1], json!({"label": "#2", "fill": "可以"}));
    }

    #[test]
    fn card_keeps_jev_confidence_and_falls_back_to_advice() {
        let mut card = Card {
            confidence: Some(0.384),
            ..Default::default()
        };
        add_reply(
            &mut card,
            r#"{"confidence": 0.9, "advice": " 先确认范围 "}"#,
        );
        let json = card.to_json();
        assert_eq!(json["fields"][0]["value"], "38%");
        assert_eq!(json["fields"][1]["value"], "先确认范围");
        assert!(json.get("tags").is_none());
        assert_eq!(friction_danger(3), ("高", "danger"));
    }

    #[test]
    fn card_from_plain_reply() {
        let mut card = Card::default();
        add_reply(&mut card, " 普通文字 ");
        assert_eq!(card.to_json(), json!({"text": "普通文字"}));
    }

    #[test]
    fn risk_level_is_read_from_start() {
        assert_eq!(risk_level("高"), Some(("高", "danger", "")));
        assert_eq!(
            risk_level("风险低，随便聊"),
            Some(("低", "success", "随便聊"))
        );
        assert_eq!(
            risk_level("中风险: 追加工作"),
            Some(("中", "warning", "追加工作"))
        );
        assert_eq!(risk_level("不确定"), None);
        assert_eq!(risk_level(""), None);
    }

    #[test]
    fn panel_values_are_validated() {
        let user = PanelRequest {
            is_user: true,
            key: IDENTITY_FIELD.to_owned(),
            ..Default::default()
        };
        assert!(is_valid_panel_value(&user, ""));
        assert!(is_valid_panel_value(&user, "work/peer"));
        assert!(!is_valid_panel_value(&user, "work"));
        let topic = PanelRequest {
            key: SCENE_FIELD.to_owned(),
            ..Default::default()
        };
        assert!(is_valid_panel_value(&topic, "work"));
        assert!(!is_valid_panel_value(&topic, "work/peer"));
    }

    #[test]
    fn prompt_contains_context_and_situation() {
        let conv = Conversation::Direct { user_ids: vec![7] };
        let message = |text: &str, flags| Message {
            id: 1,
            account_id: 1,
            timestamp: 0,
            flags,
            sender_id: 1,
            sender_name: "甲".to_owned(),
            sender_email: String::new(),
            conv: conv.clone(),
            html: String::new(),
            text: text.to_owned(),
            reactions: Vec::new(),
            uploads: Vec::new(),
            last_edit_timestamp: None,
        };
        let request = Request {
            message: message("目标", 0),
            settings: json!({}),
            analysis_model: None,
            model: None,
            prompt: String::new(),
            background: "我是开发".to_owned(),
            context_count: 1,
        };
        let context = [message("之前", MSG_OUTGOING)];
        let user = user_prompt(&request, &context, Some(("对方在：询问", "工作 / 同事")));
        assert!(user.starts_with("会话：私聊\n关于我：我是开发\n此前的聊天：\n"));
        assert!(user.contains("我（甲）：之前"));
        assert!(user.contains("结论（关系：工作 / 同事）：对方在：询问"));
        assert!(user.ends_with("甲）：\n目标"));
    }
}
