//! `/api/v1/*` 的同步部分：参数解析后按方法和路径分发。长轮询和文件传输在 `lib.rs`。

use std::collections::HashMap;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::state::{Chan, Msg, State, Target, highlight};
use crate::{ME_EMAIL, ME_ID, now};

pub struct ApiFail {
    pub status: StatusCode,
    pub code: &'static str,
    pub msg: String,
    pub extra: Value,
}

impl ApiFail {
    pub fn new(status: StatusCode, code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status,
            code,
            msg: msg.into(),
            extra: json!({}),
        }
    }

    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", msg)
    }
}

type ApiResult = Result<Value, ApiFail>;

/// 查询串与表单体合并后的参数。
pub struct Params(pub HashMap<String, String>);

impl Params {
    pub fn parse(query: Option<&str>, body: &[u8]) -> Self {
        let mut map: HashMap<String, String> = HashMap::new();
        if let Some(query) = query {
            map.extend(url::form_urlencoded::parse(query.as_bytes()).into_owned());
        }
        map.extend(url::form_urlencoded::parse(body).into_owned());
        Self(map)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn i64(&self, key: &str) -> Option<i64> {
        self.str(key)?.parse().ok()
    }

    fn require(&self, key: &str) -> Result<&str, ApiFail> {
        self.str(key)
            .ok_or_else(|| ApiFail::bad(format!("缺少参数 {key}")))
    }

    fn json(&self, key: &str) -> Option<Value> {
        serde_json::from_str(self.str(key)?).ok()
    }

    fn ids(&self, key: &str) -> Vec<i64> {
        self.json(key)
            .and_then(|v| {
                v.as_array()
                    .map(|a| a.iter().filter_map(Value::as_i64).collect())
            })
            .unwrap_or_default()
    }

    fn flag(&self, key: &str) -> bool {
        self.str(key) == Some("true")
    }
}

fn message_mut(s: &mut State, id: i64) -> Result<&mut Msg, ApiFail> {
    s.messages
        .get_mut(&id)
        .ok_or_else(|| ApiFail::bad("消息不存在"))
}

pub fn dispatch(
    s: &mut State,
    method: &Method,
    path: &[&str],
    p: &Params,
    realm: &str,
) -> ApiResult {
    let get = method == Method::GET;
    let post = method == Method::POST;
    let patch = method == Method::PATCH;
    let delete = method == Method::DELETE;
    let id_of = |seg: &str| {
        seg.parse::<i64>()
            .map_err(|_| ApiFail::bad(format!("无效的 id：{seg}")))
    };
    match path {
        ["register"] if post => Ok(s.register_json(realm)),
        ["events"] if delete => {
            if let Some(q) = p.str("queue_id") {
                s.queues.remove(q);
            }
            Ok(json!({}))
        }
        ["messages"] if get => get_messages(s, p),
        ["messages"] if post => send_message(s, p),
        ["messages", "render"] if post => Ok(json!({"rendered": s.render(p.require("content")?)})),
        ["messages", "flags"] if post => update_flags(s, p),
        ["messages", "flags", "narrow"] if post => flags_narrow(s, p),
        ["messages", id]
            if get
                && (s.fail_message_fetch
                    || id.parse().is_ok_and(|id| s.failing_messages.contains(&id))) =>
        {
            Err(ApiFail::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "模拟的服务器错误",
            ))
        }
        ["messages", id] if get => {
            let m = s
                .messages
                .get(&id_of(id)?)
                .ok_or_else(|| ApiFail::bad("消息不存在"))?;
            let mut message = s.message_json(m);
            message["raw_content"] = json!(m.raw);
            // apply_markdown=false 时 content 是原始 Markdown
            if p.str("apply_markdown") == Some("false") {
                message["content"] = json!(m.raw);
            }
            Ok(json!({"message": message, "raw_content": m.raw}))
        }
        ["messages", id] if patch => update_message(s, id_of(id)?, p),
        ["messages", id] if delete => {
            let id = id_of(id)?;
            let m = s
                .messages
                .remove(&id)
                .ok_or_else(|| ApiFail::bad("消息不存在"))?;
            s.emit(delete_event(&[id], &m));
            Ok(json!({}))
        }
        ["messages", id, "history"] if get => {
            let m = s
                .messages
                .get(&id_of(id)?)
                .ok_or_else(|| ApiFail::bad("消息不存在"))?;
            let mut history = m.history.clone();
            history.push(json!({
                "topic": m.topic, "content": m.raw, "rendered_content": m.html,
                "user_id": m.sender_id, "timestamp": m.last_edit.unwrap_or(m.timestamp),
            }));
            Ok(json!({"message_history": history}))
        }
        ["messages", id, "read_receipts"] if get => {
            let m = s
                .messages
                .get(&id_of(id)?)
                .ok_or_else(|| ApiFail::bad("消息不存在"))?;
            let readers: Vec<i64> = m
                .dm_users
                .iter()
                .copied()
                .filter(|u| *u != m.sender_id)
                .collect();
            Ok(json!({"user_ids": readers}))
        }
        ["messages", id, "reactions"] if post || delete => {
            let id = id_of(id)?;
            let name = p.require("emoji_name")?.to_owned();
            let code = p.str("emoji_code").unwrap_or_default().to_owned();
            let kind = p.str("reaction_type").unwrap_or("unicode_emoji").to_owned();
            let m = message_mut(s, id)?;
            if post {
                m.reactions.push(json!({
                    "emoji_name": name, "emoji_code": code, "reaction_type": kind, "user_id": ME_ID,
                }));
            } else {
                m.reactions.retain(|r| {
                    !(r["emoji_name"] == name.as_str() && r["user_id"].as_i64() == Some(ME_ID))
                });
            }
            s.emit(json!({
                "type": "reaction", "op": if post { "add" } else { "remove" }, "message_id": id,
                "user_id": ME_ID, "emoji_name": name, "emoji_code": code, "reaction_type": kind,
            }));
            Ok(json!({}))
        }
        ["submessage"] if post => {
            let message_id = p
                .i64("message_id")
                .ok_or_else(|| ApiFail::bad("缺少 message_id"))?;
            let id = s.alloc_id();
            let sub = json!({
                "id": id, "message_id": message_id, "sender_id": ME_ID,
                "msg_type": p.str("msg_type").unwrap_or("widget"),
                "content": p.require("content")?,
            });
            message_mut(s, message_id)?.submessages.push(sub.clone());
            let mut event = sub;
            event["type"] = json!("submessage");
            event["submessage_id"] = json!(id);
            s.emit(event);
            Ok(json!({"submessage_id": id}))
        }
        ["typing"] if post => Ok(json!({})),
        ["streams", id, "members"] if get => {
            id_of(id)?;
            let ids: Vec<i64> = s
                .users
                .iter()
                .filter(|u| !u.is_bot && u.is_active)
                .map(|u| u.id)
                .collect();
            Ok(json!({"subscribers": ids}))
        }
        ["streams", id, "delete_topic"] if post => {
            let stream = id_of(id)?;
            let topic = p.require("topic_name")?.to_owned();
            let ids: Vec<i64> = s
                .messages
                .values()
                .filter(|m| m.stream_id == Some(stream) && m.topic.eq_ignore_ascii_case(&topic))
                .map(|m| m.id)
                .collect();
            for id in &ids {
                s.messages.remove(id);
            }
            s.emit(json!({
                "type": "delete_message", "message_ids": ids, "message_type": "stream",
                "stream_id": stream, "topic": topic,
            }));
            Ok(json!({"complete": true}))
        }
        ["streams", id] if patch => update_channel(s, id_of(id)?, p),
        ["users", "me", "subscriptions"] if post => subscribe(s, p, true),
        ["users", "me", "subscriptions"] if delete => subscribe(s, p, false),
        ["users", "me", "subscriptions", "properties"] if post => {
            for change in p
                .json("subscription_data")
                .unwrap_or(json!([]))
                .as_array()
                .into_iter()
                .flatten()
            {
                apply_subscription_property(s, change);
            }
            Ok(json!({}))
        }
        ["users", "me", "muted_users", id] if post || delete => {
            let target = id_of(id)?;
            s.muted_users.retain(|m| m["id"].as_i64() != Some(target));
            if post {
                s.muted_users
                    .push(json!({"id": target, "timestamp": now()}));
            }
            let event = json!({"type": "muted_users", "muted_users": s.muted_users});
            s.emit(event);
            Ok(json!({}))
        }
        ["users", "me", "presence"] if post => {
            s.presence_last_update_id += 1;
            Ok(json!({
                "presences": s.presences,
                "presence_last_update_id": s.presence_last_update_id,
                "server_timestamp": now() as f64,
            }))
        }
        ["users", "me", "status"] if post => {
            let text = p.str("status_text").unwrap_or_default();
            s.user_status
                .insert(ME_ID.to_string(), json!({"status_text": text}));
            s.emit(json!({"type": "user_status", "user_id": ME_ID, "status_text": text}));
            Ok(json!({}))
        }
        ["users", "me", "alert_words"] if post || delete => {
            let words: Vec<String> = p
                .json("alert_words")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            s.alert_words.retain(|w| !words.contains(w));
            if post {
                s.alert_words.extend(words);
            }
            let event = json!({"type": "alert_words", "alert_words": s.alert_words});
            s.emit(event);
            Ok(json!({"alert_words": s.alert_words}))
        }
        ["users", "me", id, "topics"] if get => {
            let stream = id_of(id)?;
            let mut topics: HashMap<&str, i64> = HashMap::new();
            for m in s.messages.values().filter(|m| m.stream_id == Some(stream)) {
                topics.insert(&m.topic, m.id);
            }
            let mut list: Vec<(&str, i64)> = topics.into_iter().collect();
            list.sort_by_key(|(_, id)| -id);
            Ok(json!({"topics": list.iter()
                .map(|(name, max)| json!({"name": name, "max_id": max}))
                .collect::<Vec<_>>()}))
        }
        ["users", id] if patch => {
            let target = id_of(id)?;
            let user = s
                .users
                .iter_mut()
                .find(|u| u.id == target)
                .ok_or_else(|| ApiFail::bad("用户不存在"))?;
            let mut person = json!({"user_id": target});
            if let Some(name) = p.str("full_name") {
                user.name = name.to_owned();
                person["full_name"] = json!(name);
            }
            if let Some(role) = p.i64("role") {
                user.role = role as i32;
                person["role"] = json!(role);
            }
            s.emit(json!({"type": "realm_user", "op": "update", "person": person}));
            Ok(json!({}))
        }
        ["user_topics"] if post => {
            let stream = p.i64("stream_id").unwrap_or_default();
            let topic = p.require("topic")?.to_owned();
            let policy = p.i64("visibility_policy").unwrap_or_default();
            s.user_topics.retain(|t| {
                !(t["stream_id"].as_i64() == Some(stream) && t["topic_name"] == topic.as_str())
            });
            let entry = json!({
                "stream_id": stream, "topic_name": topic,
                "visibility_policy": policy, "last_updated": now(),
            });
            if policy != 0 {
                s.user_topics.push(entry.clone());
            }
            let mut event = entry;
            event["type"] = json!("user_topic");
            s.emit(event);
            Ok(json!({}))
        }
        ["channels", "create"] if post => {
            let id = s.alloc_id();
            let chan = Chan {
                id,
                name: p.require("name")?.to_owned(),
                description: p.str("description").unwrap_or_default().to_owned(),
                rendered_description: None,
                invite_only: p.flag("invite_only"),
                color: "#c2c2c2".to_owned(),
                subscribed: true,
                is_muted: false,
                properties: serde_json::Map::new(),
                date_created: now(),
            };
            let stream = s.stream_json(&chan);
            let subscription = s.subscription_json(&chan);
            s.channels.push(chan);
            s.emit(json!({"type": "stream", "op": "create", "streams": [stream]}));
            s.emit(json!({"type": "subscription", "op": "add", "subscriptions": [subscription]}));
            Ok(json!({"id": id}))
        }
        ["scheduled_messages"] if post => {
            let id = s.alloc_id();
            let kind = if p.str("type") == Some("direct") {
                "private"
            } else {
                "stream"
            };
            let to = p.json("to").unwrap_or(Value::Null);
            let content = p.require("content")?;
            let item = json!({
                "scheduled_message_id": id, "type": kind, "to": to,
                "topic": p.str("topic").unwrap_or_default(), "content": content,
                "rendered_content": s.render(content),
                "scheduled_delivery_timestamp": p.i64("scheduled_delivery_timestamp").unwrap_or_default(),
                "failed": false,
            });
            s.scheduled.insert(id, item.clone());
            s.emit(
                json!({"type": "scheduled_messages", "op": "add", "scheduled_messages": [item]}),
            );
            Ok(json!({"scheduled_message_id": id}))
        }
        ["scheduled_messages", id] if delete => {
            let id = id_of(id)?;
            s.scheduled.remove(&id);
            s.emit(
                json!({"type": "scheduled_messages", "op": "remove", "scheduled_message_id": id}),
            );
            Ok(json!({}))
        }
        ["reminders"] if post => {
            let id = s.alloc_id();
            let target = p
                .i64("message_id")
                .ok_or_else(|| ApiFail::bad("缺少 message_id"))?;
            let item = json!({
                "reminder_id": id, "type": "private", "to": [ME_ID],
                "content": format!("提醒 {target}"), "rendered_content": format!("<p>提醒 {target}</p>"),
                "scheduled_delivery_timestamp": p.i64("scheduled_delivery_timestamp").unwrap_or_default(),
                "failed": false, "reminder_target_message_id": target,
            });
            s.reminders.insert(id, item.clone());
            s.emit(json!({"type": "reminders", "op": "add", "reminders": [item]}));
            Ok(json!({"reminder_id": id}))
        }
        ["reminders", id] if delete => {
            let id = id_of(id)?;
            s.reminders.remove(&id);
            s.emit(json!({"type": "reminders", "op": "remove", "reminder_id": id}));
            Ok(json!({}))
        }
        ["saved_snippets"] if post => {
            let id = s.alloc_id();
            let item = json!({
                "id": id, "title": p.require("title")?, "content": p.require("content")?,
                "date_created": now(),
            });
            s.snippets.insert(id, item.clone());
            s.emit(json!({"type": "saved_snippets", "op": "add", "saved_snippet": item}));
            Ok(json!({"saved_snippet_id": id}))
        }
        ["saved_snippets", id] if delete => {
            let id = id_of(id)?;
            s.snippets.remove(&id);
            s.emit(json!({"type": "saved_snippets", "op": "remove", "saved_snippet_id": id}));
            Ok(json!({}))
        }
        ["invites"] if post => Ok(json!({})),
        ["attachments"] if get => {
            let mut used = 0;
            let list: Vec<Value> = s
                .uploads
                .iter()
                .map(|(path, u)| {
                    used += u.bytes.len();
                    let refs: Vec<i64> = s
                        .messages
                        .values()
                        .filter(|m| m.raw.contains(path.as_str()))
                        .map(|m| m.id)
                        .chain(u.dangling_refs.iter().copied())
                        .collect();
                    json!({
                        "id": u.attachment_id, "name": u.name, "path_id": path,
                        "size": u.bytes.len(), "create_time": u.created_at, "message_ids": refs,
                    })
                })
                .collect();
            Ok(json!({"attachments": list, "upload_space_used": used}))
        }
        ["attachments", id] if delete => {
            let id = id_of(id)?;
            s.uploads.retain(|_, u| u.attachment_id != id);
            s.emit(json!({"type": "attachment", "op": "remove", "attachment": {"id": id}}));
            Ok(json!({}))
        }
        ["drafts"] if get => Ok(json!({"drafts": s.drafts.values().collect::<Vec<_>>()})),
        ["drafts"] if post => {
            let mut ids = Vec::new();
            let mut items = Vec::new();
            for draft in p
                .json("drafts")
                .unwrap_or(json!([]))
                .as_array()
                .into_iter()
                .flatten()
            {
                let id = s.alloc_id();
                let mut item = draft.clone();
                item["id"] = json!(id);
                if item.get("timestamp").is_none_or(Value::is_null) {
                    item["timestamp"] = json!(now());
                }
                s.drafts.insert(id, item.clone());
                ids.push(id);
                items.push(item);
            }
            s.emit(json!({"type": "drafts", "op": "add", "drafts": items}));
            Ok(json!({"ids": ids}))
        }
        ["drafts", id] if patch => {
            let id = id_of(id)?;
            let mut item = p.json("draft").unwrap_or(json!({}));
            item["id"] = json!(id);
            if item.get("timestamp").is_none_or(Value::is_null) {
                item["timestamp"] = json!(now());
            }
            s.drafts.insert(id, item.clone());
            s.emit(json!({"type": "drafts", "op": "update", "draft": item}));
            Ok(json!({}))
        }
        ["drafts", id] if delete => {
            let id = id_of(id)?;
            s.drafts.remove(&id);
            s.emit(json!({"type": "drafts", "op": "remove", "draft_id": id}));
            Ok(json!({}))
        }
        ["settings"] if patch => {
            for (key, raw) in &p.0 {
                let value: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
                s.settings.insert(key.clone(), value.clone());
                s.emit(json!({
                    "type": "user_settings", "op": "update", "property": key, "value": value,
                }));
            }
            Ok(json!({}))
        }
        ["realm"] if patch => {
            for (key, raw) in &p.0 {
                let value: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
                s.realm.insert(format!("realm_{key}"), value.clone());
                s.emit(json!({
                    "type": "realm", "op": "update", "property": key, "value": value,
                }));
            }
            Ok(json!({}))
        }
        _ => Err(ApiFail::new(
            StatusCode::NOT_FOUND,
            "NOT_IMPLEMENTED",
            format!("未实现的接口：{method} {}", path.join("/")),
        )),
    }
}

pub fn fetch_api_key(p: &Params) -> ApiResult {
    if p.str("username") != Some(ME_EMAIL) {
        return Err(ApiFail::new(
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_FAILED",
            "Your username or password is incorrect",
        ));
    }
    Ok(json!({"api_key": crate::API_KEY, "email": ME_EMAIL, "user_id": ME_ID}))
}

pub fn server_settings(realm: &str) -> Value {
    json!({
        "zulip_feature_level": 445,
        "zulip_version": "11.0-test",
        "realm_name": "测试组织",
        "realm_icon": "/avatar/1.png",
        "realm_url": realm,
        "email_auth_enabled": true,
        "require_email_format_usernames": true,
    })
}

fn delete_event(ids: &[i64], m: &Msg) -> Value {
    let mut event = json!({"type": "delete_message", "message_ids": ids});
    if let Some(stream) = m.stream_id {
        event["message_type"] = json!("stream");
        event["stream_id"] = json!(stream);
        event["topic"] = json!(m.topic);
    } else {
        event["message_type"] = json!("private");
    }
    event
}

fn send_message(s: &mut State, p: &Params) -> ApiResult {
    let content = p.require("content")?;
    let target = match p.require("type")? {
        "direct" | "private" => Target::Dm(p.ids("to")),
        _ => {
            let stream = p.i64("to").ok_or_else(|| ApiFail::bad("缺少频道"))?;
            if s.channel(stream).is_none() {
                return Err(ApiFail::bad("频道不存在"));
            }
            Target::Stream(stream, p.str("topic").unwrap_or_default().to_owned())
        }
    };
    let local = p
        .str("queue_id")
        .zip(p.str("local_id"))
        .map(|(q, l)| (q.to_owned(), l.to_owned()));
    let id = s.send(ME_ID, target, content, now(), local);
    Ok(json!({"id": id}))
}

fn update_flags(s: &mut State, p: &Params) -> ApiResult {
    let ids = p.ids("messages");
    let flag = p.require("flag")?.to_owned();
    let op = p.require("op")?.to_owned();
    let mut changed = Vec::new();
    for id in &ids {
        if let Some(m) = s.messages.get_mut(id) {
            let did = if op == "add" {
                m.flags.insert(flag.clone())
            } else {
                m.flags.remove(&flag)
            };
            if did {
                changed.push(*id);
            }
        }
    }
    let event = flags_event(s, &op, &flag, &changed);
    s.emit(event);
    Ok(json!({"messages": changed}))
}

fn flags_event(s: &State, op: &str, flag: &str, ids: &[i64]) -> Value {
    let mut event = json!({
        "type": "update_message_flags", "op": op, "flag": flag, "messages": ids, "all": false,
    });
    if op == "remove" && flag == "read" {
        let details: serde_json::Map<String, Value> = ids
            .iter()
            .filter_map(|id| s.messages.get(id))
            .map(|m| {
                let mut d = match m.stream_id {
                    Some(stream) => {
                        json!({"type": "stream", "stream_id": stream, "topic": m.topic})
                    }
                    None => json!({
                        "type": "private",
                        "user_ids": m.dm_users.iter().filter(|u| **u != ME_ID).collect::<Vec<_>>(),
                    }),
                };
                if m.flags.contains("mentioned") {
                    d["mentioned"] = json!(true);
                }
                (m.id.to_string(), d)
            })
            .collect();
        event["message_details"] = Value::Object(details);
    }
    event
}

fn flags_narrow(s: &mut State, p: &Params) -> ApiResult {
    let narrow = p.json("narrow").unwrap_or(json!([]));
    let (selected, found_oldest, found_newest, _) = select(
        s,
        &narrow,
        p.require("anchor")?,
        None,
        p.i64("num_before").unwrap_or(0) as usize,
        p.i64("num_after").unwrap_or(0) as usize,
        p.flag("include_anchor"),
    );
    let flag = p.require("flag")?.to_owned();
    let op = p.require("op")?.to_owned();
    let mut changed = Vec::new();
    for id in &selected {
        if let Some(m) = s.messages.get_mut(id) {
            let did = if op == "add" {
                m.flags.insert(flag.clone())
            } else {
                m.flags.remove(&flag)
            };
            if did {
                changed.push(*id);
            }
        }
    }
    let event = flags_event(s, &op, &flag, &changed);
    s.emit(event);
    Ok(json!({
        "processed_count": selected.len(),
        "updated_count": changed.len(),
        "first_processed_id": selected.first(),
        "last_processed_id": selected.last(),
        "found_oldest": found_oldest,
        "found_newest": found_newest,
    }))
}

fn matches_term(m: &Msg, term: &Value) -> bool {
    let operand = &term["operand"];
    let text = operand.as_str().unwrap_or_default();
    let hit = match term["operator"].as_str().unwrap_or_default() {
        "channel" | "stream" => m.stream_id == operand.as_i64(),
        "topic" => m.stream_id.is_some() && m.topic.to_lowercase() == text.to_lowercase(),
        "dm" | "pm-with" => {
            let mut want: Vec<i64> = operand
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default();
            want.retain(|id| *id != ME_ID);
            want.sort_unstable();
            let others: Vec<i64> = m
                .dm_users
                .iter()
                .copied()
                .filter(|id| *id != ME_ID)
                .collect();
            m.stream_id.is_none() && others == want
        }
        "sender" => Some(m.sender_id) == operand.as_i64(),
        "search" => {
            let q = text.to_lowercase();
            m.raw.to_lowercase().contains(&q) || m.topic.to_lowercase().contains(&q)
        }
        "is" => match text {
            "unread" => !m.flags.contains("read"),
            "starred" => m.flags.contains("starred"),
            "mentioned" => m.flags.contains("mentioned"),
            "dm" | "private" => m.stream_id.is_none(),
            _ => false,
        },
        "has" => match text {
            "attachment" => m.raw.contains("/user_uploads/"),
            "image" => m.html.contains("message_inline_image"),
            "link" => m.raw.contains("](") || m.raw.contains("http"),
            _ => false,
        },
        _ => true,
    };
    hit != term["negated"].as_bool().unwrap_or(false)
}

/// 返回 `(选中的 id, found_oldest, found_newest, found_anchor)`。
fn select(
    s: &State,
    narrow: &Value,
    anchor: &str,
    anchor_date: Option<&str>,
    before: usize,
    after: usize,
    include_anchor: bool,
) -> (Vec<i64>, bool, bool, bool) {
    let terms: &[Value] = narrow.as_array().map_or(&[], Vec::as_slice);
    let ids: Vec<i64> = s
        .messages
        .values()
        .filter(|m| terms.iter().all(|t| matches_term(m, t)))
        .map(|m| m.id)
        .collect();
    let anchor_id = match anchor {
        "newest" => i64::MAX,
        "oldest" => 0,
        "first_unread" => ids
            .iter()
            .copied()
            .find(|id| !s.messages[id].flags.contains("read"))
            .unwrap_or(i64::MAX),
        "date" => {
            let at = anchor_date
                .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                .map_or(i64::MAX, |d| d.timestamp());
            ids.iter()
                .copied()
                .find(|id| s.messages[id].timestamp >= at)
                .unwrap_or(i64::MAX)
        }
        number => number.parse().unwrap_or(i64::MAX),
    };
    let split = ids.partition_point(|id| *id < anchor_id);
    let found_anchor = ids.get(split) == Some(&anchor_id);
    let start = split.saturating_sub(before);
    let mut end = split;
    if found_anchor && include_anchor {
        end += 1;
    } else if found_anchor {
        // 不含锚点时，锚点之后的消息从锚点下一条算起
        let tail = &ids[split + 1..];
        let take = tail.len().min(after);
        let mut selected: Vec<i64> = ids[start..split].to_vec();
        selected.extend_from_slice(&tail[..take]);
        return (
            selected,
            start == 0 && (before > 0 || anchor_id == 0),
            take == tail.len() && (after > 0 || anchor_id == i64::MAX),
            found_anchor,
        );
    }
    let last = (end + after).min(ids.len());
    (
        ids[start..last].to_vec(),
        start == 0 && (before > 0 || anchor_id == 0),
        last == ids.len() && (after > 0 || anchor_id == i64::MAX),
        found_anchor,
    )
}

fn get_messages(s: &State, p: &Params) -> ApiResult {
    let narrow = p.json("narrow").unwrap_or(json!([]));
    let (selected, found_oldest, found_newest, found_anchor) = select(
        s,
        &narrow,
        p.require("anchor")?,
        p.str("anchor_date"),
        p.i64("num_before").unwrap_or(0) as usize,
        p.i64("num_after").unwrap_or(0) as usize,
        p.str("include_anchor") != Some("false"),
    );
    let search = narrow.as_array().and_then(|terms| {
        terms
            .iter()
            .find(|t| t["operator"] == "search")
            .and_then(|t| t["operand"].as_str())
    });
    let raw = p.str("apply_markdown") == Some("false");
    let messages: Vec<Value> = selected
        .iter()
        .map(|id| {
            let m = &s.messages[id];
            let mut value = s.message_json(m);
            if raw {
                value["content"] = json!(m.raw);
            }
            if let Some(q) = search {
                value["match_content"] = json!(highlight(&m.html, q));
                value["match_subject"] = json!(highlight(&m.topic, q));
            }
            value
        })
        .collect();
    Ok(json!({
        "anchor": selected.first().copied().unwrap_or_default(),
        "found_newest": found_newest,
        "found_oldest": found_oldest,
        "found_anchor": found_anchor,
        "history_limited": false,
        "messages": messages,
    }))
}

fn update_message(s: &mut State, id: i64, p: &Params) -> ApiResult {
    let at = now();
    if let Some(content) = p.str("content") {
        let html = s.render(content);
        let m = message_mut(s, id)?;
        let event = json!({
            "type": "update_message", "message_id": id, "message_ids": [id], "user_id": ME_ID,
            "edit_timestamp": at, "stream_id": m.stream_id, "topic": m.topic,
            "orig_content": m.raw, "orig_rendered_content": m.html,
            "content": content, "rendered_content": html,
            "flags": m.flags, "rendering_only": false,
        });
        m.history.push(json!({
            "topic": m.topic, "content": m.raw, "rendered_content": m.html,
            "user_id": m.sender_id, "timestamp": m.last_edit.unwrap_or(m.timestamp),
        }));
        m.raw = content.to_owned();
        m.html = html;
        m.last_edit = Some(at);
        s.emit(event);
    }
    let new_topic = p.str("topic").map(str::to_owned);
    let new_stream = p.i64("stream_id");
    if new_topic.is_none() && new_stream.is_none() {
        return Ok(json!({}));
    }
    let origin = s
        .messages
        .get(&id)
        .ok_or_else(|| ApiFail::bad("消息不存在"))?;
    let (orig_stream, orig_topic) = (origin.stream_id, origin.topic.clone());
    let mode = p.str("propagate_mode").unwrap_or("change_one").to_owned();
    let in_topic: Vec<i64> = s
        .messages
        .values()
        .filter(|m| m.stream_id == orig_stream && m.topic == orig_topic)
        .map(|m| m.id)
        .collect();
    let ids: Vec<i64> = match mode.as_str() {
        "change_all" => in_topic.clone(),
        "change_later" => in_topic.iter().copied().filter(|m| *m >= id).collect(),
        _ => vec![id],
    };
    if let Some(first_allowed) = s.move_limit_first_id
        && ids.iter().any(|m| *m < first_allowed)
    {
        let allowed = in_topic.iter().filter(|m| **m >= first_allowed).count();
        let mut fail = ApiFail::new(
            StatusCode::BAD_REQUEST,
            "MOVE_MESSAGES_TIME_LIMIT_EXCEEDED",
            "超出移动时限",
        );
        fail.extra = json!({
            "total_messages_allowed_to_move": allowed,
            "total_messages_in_topic": in_topic.len(),
            "first_message_id_allowed_to_move": first_allowed,
        });
        return Err(fail);
    }
    for mid in &ids {
        let m = message_mut(s, *mid)?;
        if let Some(topic) = &new_topic {
            m.topic.clone_from(topic);
        }
        if let Some(stream) = new_stream {
            m.stream_id = Some(stream);
        }
        m.last_edit = Some(at);
    }
    let mut event = json!({
        "type": "update_message", "message_id": id, "message_ids": ids, "user_id": ME_ID,
        "edit_timestamp": at, "stream_id": orig_stream, "orig_subject": orig_topic,
        "propagate_mode": mode, "flags": [], "rendering_only": false,
    });
    if let Some(topic) = new_topic {
        event["subject"] = json!(topic);
    }
    if let Some(stream) = new_stream {
        event["new_stream_id"] = json!(stream);
    }
    s.emit(event);
    Ok(json!({}))
}

fn update_channel(s: &mut State, id: i64, p: &Params) -> ApiResult {
    let name = s
        .channel(id)
        .map(|c| c.name.clone())
        .ok_or_else(|| ApiFail::bad("频道不存在"))?;
    for (key, raw) in &p.0 {
        if let Some(old) = serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|v| v.get("old").cloned())
        {
            let current = s
                .channel(id)
                .map(|c| s.stream_json(c)[key.as_str()].clone());
            if current.as_ref() != Some(&old) {
                return Err(ApiFail::new(
                    StatusCode::BAD_REQUEST,
                    "EXPECTATION_MISMATCH",
                    "'old' value does not match the expected value.",
                ));
            }
        }
    }
    for (key, raw) in &p.0 {
        let value: Value = match key.as_str() {
            "new_name" | "description" => json!(raw),
            _ => serde_json::from_str(raw).unwrap_or_else(|_| json!(raw)),
        };
        // 权限是 {"new": …, "old": …}，事件和频道数据中只有新值
        let value = value.get("new").cloned().unwrap_or(value);
        if let Some(chan) = s.channels.iter_mut().find(|c| c.id == id) {
            match key.as_str() {
                "new_name" => chan.name.clone_from(raw),
                "description" => {
                    chan.description.clone_from(raw);
                    chan.rendered_description = None;
                }
                "is_private" => chan.invite_only = raw == "true",
                _ => {
                    chan.properties.insert(key.clone(), value.clone());
                }
            }
        }
        let property = match key.as_str() {
            "new_name" => "name",
            "is_private" => "invite_only",
            other => other,
        };
        let mut event = json!({
            "type": "stream", "op": "update", "stream_id": id, "name": name,
            "property": property, "value": value,
        });
        if key == "description" {
            event["rendered_description"] = json!(s.render(raw));
        }
        s.emit(event);
    }
    Ok(json!({}))
}

fn subscribe(s: &mut State, p: &Params, add: bool) -> ApiResult {
    let requested = p.json("subscriptions").unwrap_or(json!([]));
    let names: Vec<String> = requested
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.get("name").or(Some(v)).and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let others = p.json("principals").is_some_and(|v| {
        v.as_array()
            .is_some_and(|a| a.iter().any(|u| u.as_i64() != Some(ME_ID)))
    });
    if others {
        return Ok(json!({"subscribed": {}, "already_subscribed": {}}));
    }
    let mut affected = Vec::new();
    for chan in s.channels.iter_mut().filter(|c| names.contains(&c.name)) {
        chan.subscribed = add;
        affected.push(chan.id);
    }
    let event = if add {
        let subscriptions: Vec<Value> = affected
            .iter()
            .filter_map(|id| s.channel(*id))
            .map(|c| s.subscription_json(c))
            .collect();
        json!({"type": "subscription", "op": "add", "subscriptions": subscriptions})
    } else {
        let subscriptions: Vec<Value> = affected
            .iter()
            .filter_map(|id| s.channel(*id))
            .map(|c| json!({"stream_id": c.id, "name": c.name}))
            .collect();
        json!({"type": "subscription", "op": "remove", "subscriptions": subscriptions})
    };
    s.emit(event);
    Ok(json!({"subscribed": {}, "already_subscribed": {}, "removed": names, "not_removed": []}))
}

fn apply_subscription_property(s: &mut State, change: &Value) {
    let (Some(id), Some(property)) = (change["stream_id"].as_i64(), change["property"].as_str())
    else {
        return;
    };
    let value = change["value"].clone();
    if let Some(chan) = s.channels.iter_mut().find(|c| c.id == id) {
        match property {
            "is_muted" => chan.is_muted = value.as_bool().unwrap_or(false),
            "color" => chan.color = value.as_str().unwrap_or_default().to_owned(),
            _ => {
                chan.properties.insert(property.to_owned(), value.clone());
            }
        }
    }
    s.emit(json!({
        "type": "subscription", "op": "update", "stream_id": id,
        "property": property, "value": value,
    }));
}
