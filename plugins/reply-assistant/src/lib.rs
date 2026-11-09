//! 回复助手（参考 jev-chat-jarvis 的“先读懂，再回复”）：消息右键菜单“分析这条消息”→
//! 在工作线程上先用分析模型（jev，OpenRouter decisions 接口）按场景分析对方的消息，
//! 再把此前的聊天、这条消息与分析结论发给写回复模型，在消息下方显示分析、意图、风险、对方需要、建议，
//! 以及候选回复按钮（点击填入输入框，由用户自己发送）。两个模型都可以单独使用。
//! 场景按话题、身份按好友在话题资料面板 / 用户资料卡中设置，与分析结果一起保存在插件自己的数据库中，
//! 消息再次显示时重新附加分析结果。

mod analysis;
mod catalog;
mod scenes;
mod store;

use zulip_plugin_sdk::abi::{LOG_ERROR, LOG_INFO, LOG_WARNING};
use zulip_plugin_sdk::serde_json::{self, Value, json};
use zulip_plugin_sdk::{
    Message, MessagesShown, PanelRequest, Plugin, annotate, context_messages, export_plugin,
    extract_json, format_time, log, model_center, post, reply_panel, select_account,
    set_annotation, settings, str_of,
};

use crate::analysis::{Findings, analyze};
use crate::catalog::CATALOG;
use crate::scenes::{SceneChoice, identity_options, parse_identity, resolve, scene_options};

const MENU_ANALYZE: i32 = 1;
const MAX_REPLIES: usize = 3;
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
    fn on_message_menu(&self, menu_id: i32, message: &Message) {
        if menu_id != MENU_ANALYZE {
            return;
        }
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

    // 消息显示时重新附加保存的分析结果（回调所属的账号即消息所属的账号）。
    fn on_messages_shown(&self, shown: &MessagesShown) {
        for (message_id, content) in store::analyses(shown.account_id, &shown.message_ids) {
            if let Ok(annotation) = serde_json::from_str::<Value>(&content)
                && annotation.is_object()
            {
                annotate(message_id, &annotation);
            }
        }
    }

    // 话题资料面板（场景）与用户资料卡（身份），以及插件设置中的列表。
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

// 工作线程。
fn run_analysis(request: &Request) {
    let message = &request.message;
    if select_account(message.account_id).is_err() {
        return;
    }
    let context = context_messages(message, request.context_count).unwrap_or_default();

    let mut lines = Vec::new();
    let mut findings: Option<Findings> = None;
    let mut choice: Option<SceneChoice> = None;
    let mut analysis_failed = false;
    if let Some(analysis_model) = &request.analysis_model {
        let scene = resolve(message, &request.settings);
        lines.push(format!(
            "分析场景：{}（可在话题资料面板、用户资料卡中修改）",
            scene.label()
        ));
        match analyze(analysis_model, &scene, message, &context) {
            Ok(result) => {
                lines.extend(result.lines());
                findings = Some(result);
            }
            Err(error) => {
                log(
                    LOG_ERROR,
                    &format!("jev 分析消息 {} 失败：{error}", message.id),
                );
                lines.push(format!("jev 分析失败：{error}"));
                analysis_failed = true;
            }
        }
        choice = Some(scene);
        if request.model.is_none() {
            show(
                message,
                &json!({"text": lines.join("\n")}),
                !analysis_failed,
            );
            return;
        }
        let mut progress = lines.clone();
        progress.push("正在生成候选回复…".to_owned());
        annotate_text(message.id, &progress.join("\n"));
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
        Ok(reply) => show(message, &to_annotation(&reply, &lines), !analysis_failed),
        Err(error) => {
            log(LOG_ERROR, &format!("分析消息 {} 失败：{error}", message.id));
            lines.push(format!("生成回复失败：{error}"));
            show(message, &json!({"text": lines.join("\n")}), false);
        }
    }
}

fn speaker(message: &Message) -> String {
    if message.is_outgoing() {
        format!("我（{}）", message.sender_name)
    } else {
        message.sender_name.clone()
    }
}

// situation：jev 的结论与关系。
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

// save：保存结果，消息再次显示时重新附加（失败的结果不保存）。
fn show(message: &Message, annotation: &Value, save: bool) {
    if save {
        store::set_analysis(message.account_id, message.id, &annotation.to_string());
    }
    match set_annotation(message.id, annotation) {
        Err(error) => log(LOG_WARNING, &format!("显示分析结果失败：{error}")),
        Ok(()) => log(LOG_INFO, &format!("已分析消息 {}", message.id)),
    }
}

// 模型返回 JSON 时整理为几行说明与候选回复按钮；不是 JSON 时整段显示。prefix 为 jev 分析的几行。
fn to_annotation(reply: &str, prefix: &[String]) -> Value {
    let mut lines = prefix.to_vec();
    let Some(parsed) = extract_json(reply) else {
        lines.push(reply.trim().to_owned());
        return json!({"text": lines.join("\n")});
    };
    for (label, key) in [
        ("意图", "intent"),
        ("风险", "risk"),
        ("对方需要", "needs"),
        ("建议", "advice"),
    ] {
        let value = str_of(&parsed, key).trim();
        if !value.is_empty() {
            lines.push(format!("{label}：{value}"));
        }
    }
    let actions: Vec<Value> = parsed["replies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(MAX_REPLIES)
        .enumerate()
        .map(|(i, text)| json!({"label": format!("填入回复 {}", i + 1), "fill": text}))
        .collect();
    json!({"text": lines.join("\n"), "actions": actions})
}

export_plugin!(ReplyAssistant, "回复助手");

#[cfg(test)]
mod tests {
    use zulip_plugin_sdk::Conversation;
    use zulip_plugin_sdk::abi::MSG_OUTGOING;

    use super::*;

    #[test]
    fn annotation_from_json_reply() {
        let reply = r#"结果：{"intent": " 想约时间 ", "risk": "", "replies": ["好", " ", "可以", "行", "嗯"]}"#;
        let annotation = to_annotation(reply, &["分析场景：工作".to_owned()]);
        assert_eq!(annotation["text"], "分析场景：工作\n意图：想约时间");
        let actions = annotation["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[1], json!({"label": "填入回复 2", "fill": "可以"}));
    }

    #[test]
    fn annotation_from_plain_reply() {
        assert_eq!(to_annotation(" 普通文字 ", &[])["text"], "普通文字");
        assert!(to_annotation(" 普通文字 ", &[]).get("actions").is_none());
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
