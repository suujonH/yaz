//! Example plugin. When a DM receives ping it replies with the content from settings; messages
//! containing echo:block are intercepted, when sending, the content after echo:rewrite replaces the
//! whole message, and echo:slow demonstrates a callback timeout. Notification-type events are
//! logged; the context menu sets a status bar hint; the toolbar button replaces the composer
//! content, and when the composer content is `echo:call <action> <JSON args>` it calls the matching
//! host function and writes the result to the log (see [`call`]). When settings have
//! `login_dialog`, a dialog pops up after login (after `login_flood` log entries and toasts).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use zulip_plugin_sdk::abi::{LOG_INFO, LOG_WARNING};
use zulip_plugin_sdk::serde_json::{Value, json};
use zulip_plugin_sdk::{
    ChannelsChanged, ComposerClick, ConnectionChanged, ContextMenuClick, ContextTarget,
    Conversation, ConversationOpened, DraftChanged, Event, Message, MessageEdited, MessagesDeleted,
    MutedUsersChanged, Outgoing, OutgoingKind, PanelRequest, Plugin, PresenceChanged,
    ReactionChanged, ReadStateChanged, RealmChanged, RemindersChanged, Result, ScheduledChanged,
    SettingsButton, TopicMoved, TypingChanged, UserChanged, UserTopicChanged, add_reaction,
    alert_words, all_channels, api_request, cancel_reminder, cancel_scheduled_message,
    conversation_level, conversations, create_channel, create_reminder, current_account,
    current_conversation, data_dir, delete_topic, download_attachment, download_attachment_ex,
    drafts, edit_history, export_plugin, fill_composer, fill_composer_to, last_error, log,
    mark_unread, message, message_actions, message_source, move_topic, muted_users,
    open_conversation, presence, read_receipts, realm_info, reminders, remove_reaction,
    reply_panel, save_draft, schedule_message, scheduled_messages, search_messages,
    send_direct_message, set_alert_word, set_annotation, set_conversation_level,
    set_conversation_pinned, set_outgoing_content, set_own_status, set_settings, set_status_text,
    set_subscribed, set_subscription_property, set_thread_account, set_topic_resolved,
    set_topic_visibility, set_user_muted, settings, settings_text, show_dialog, show_notification,
    show_toast, str_of, subscribers, topic_visibility, upload_file_ex, user_groups,
};

const BLOCK: &str = "echo:block";
const REWRITE: &str = "echo:rewrite ";
const SLOW: &str = "echo:slow";
const SLOW_FOR: Duration = Duration::from_millis(2500);
const CALL: &str = "echo:call ";

#[derive(Default)]
struct Echo {
    enabled: AtomicBool,
    // Names shown in the panel: account id, user id and content, kept only in memory.
    nicknames: Mutex<Vec<(i64, i64, String)>>,
}

impl Echo {
    fn nickname(&self, user_id: i64) -> String {
        let nicknames = self.nicknames.lock().unwrap_or_else(|e| e.into_inner());
        nicknames
            .iter()
            .find(|(_, id, _)| *id == user_id)
            .map_or_else(String::new, |(_, _, text)| text.clone())
    }

    fn set_nickname(&self, account_id: i64, user_id: i64, text: &str) {
        let mut nicknames = self.nicknames.lock().unwrap_or_else(|e| e.into_inner());
        nicknames.retain(|(_, id, _)| *id != user_id);
        if !text.is_empty() {
            nicknames.push((account_id, user_id, text.to_owned()));
        }
    }

    fn panel_fields(&self, user_id: i64, info: &str) -> Value {
        json!({"fields": [{
            "key": "nickname", "label": "称呼", "type": "select", "value": self.nickname(user_id),
            "options": [
                {"value": "", "label": "无"},
                {"value": "friend", "label": "朋友"},
                {"value": "colleague", "label": "同事"},
                {"value": SLOW, "label": "删除时超时"},
            ],
        },
        {"key": "note", "label": "备注", "type": "text", "value": "", "placeholder": "写点什么"},
        {"key": "ping", "label": "打招呼", "type": "button"},
        {"key": "info", "label": "说明", "type": "info", "value": info}]})
    }
}

fn int(args: &Value, key: &str) -> i64 {
    args[key].as_i64().unwrap_or_default()
}

/// Calls a host function and returns the result to write to the log.
fn call(op: &str, args: &Value) -> Result<String> {
    let id = int(args, "id");
    let stream = int(args, "stream");
    let topic = str_of(args, "topic");
    let on = args["on"].as_bool().unwrap_or(true);
    let done = |result: Result<()>| result.map(|()| "ok".to_owned());
    let conv = || {
        Conversation::from_json(&args["conv"]).unwrap_or(Conversation::Direct {
            user_ids: Vec::new(),
        })
    };
    Ok(match op {
        "message" => {
            let m = message(id)?;
            format!(
                "回应 {} 上传 {} 编辑 {}",
                m.reactions.len(),
                m.uploads.first().map_or("", |u| u.file_name.as_str()),
                m.last_edit_timestamp.is_some()
            )
        }
        "search" => {
            let page = search_messages(args)?;
            format!("{} 条，下一页 {:?}", page.messages.len(), page.next_cursor)
        }
        "conversations" => conversations()?.to_string(),
        "current" => current_conversation()?.map_or("无".to_owned(), |c| c.to_json().to_string()),
        "presence" => presence(id)?.to_string(),
        "groups" => user_groups()?.to_string(),
        "all_channels" => {
            let channels = all_channels()?;
            let unsubscribed = channels.iter().filter(|c| !c.is_subscribed).count();
            format!("{} 个，未订阅 {unsubscribed}", channels.len())
        }
        "visibility" => topic_visibility(stream, topic)?.to_string(),
        "history" => edit_history(id)?.to_string(),
        "react" => add_reaction(id, str_of(args, "emoji")).map(|()| "ok".to_owned())?,
        "unreact" => remove_reaction(id, str_of(args, "emoji")).map(|()| "ok".to_owned())?,
        "mark_unread" => mark_unread(id).map(|()| "ok".to_owned())?,
        "move_topic" => move_topic(
            stream,
            topic,
            int(args, "new_stream"),
            str_of(args, "new_topic"),
        )
        .map(|()| "ok".to_owned())?,
        "resolve" => set_topic_resolved(stream, topic, args["resolved"].as_bool().unwrap_or(true))
            .map(|()| "ok".to_owned())?,
        "delete_topic" => delete_topic(stream, topic).map(|()| "ok".to_owned())?,
        "open" => open_conversation(&conv(), (id > 0).then_some(id)).map(|()| "ok".to_owned())?,
        "schedule" => schedule_message(&conv(), str_of(args, "text"), int(args, "at"))?.to_string(),
        "toast" => show_toast(str_of(args, "text")).map(|()| "ok".to_owned())?,
        "notify" => show_notification(str_of(args, "title"), str_of(args, "text"))
            .map(|()| "ok".to_owned())?,
        "download" => download_attachment(str_of(args, "path"))?,
        "api" => api_request(
            str_of(args, "method"),
            str_of(args, "path"),
            &args["params"],
        )?
        .to_string(),
        "send_self" => send_direct_message(&[], str_of(args, "text"))?.to_string(),
        "last_error" => last_error()?,
        "source" => message_source(id)?,
        "scheduled" => scheduled_messages()?.to_string(),
        "cancel_scheduled" => done(cancel_scheduled_message(id))?,
        "set_settings" => done(set_settings(&args["values"]))?,
        "annotate" => done(set_annotation(id, &args["annotation"]))?,
        "fill_to" => done(fill_composer_to(&conv(), str_of(args, "text"), on))?,
        "set_visibility" => done(set_topic_visibility(stream, topic, int(args, "v") as i32))?,
        "subscribed" => done(set_subscribed(stream, on))?,
        "subscription_property" => done(set_subscription_property(
            stream,
            str_of(args, "name"),
            &args["value"],
        ))?,
        "create_channel" => create_channel(args)?.to_string(),
        "subscribers" => subscribers(stream)?.to_string(),
        "remind" => create_reminder(id, int(args, "at"))?.to_string(),
        "cancel_reminder" => done(cancel_reminder(id))?,
        "reminders" => reminders()?.to_string(),
        "drafts" => drafts()?.to_string(),
        "save_draft" => done(save_draft(&conv(), str_of(args, "text")))?,
        "level" => conversation_level(&conv())?.to_string(),
        "set_level" => done(set_conversation_level(&conv(), int(args, "level") as i32))?,
        "pin" => done(set_conversation_pinned(&conv(), on))?,
        "mute" => done(set_user_muted(id, on))?,
        "alert_word" => done(set_alert_word(str_of(args, "word"), on))?,
        "status" => done(set_own_status(str_of(args, "text")))?,
        "muted" => muted_users()?.to_string(),
        "alert_words" => alert_words()?.to_string(),
        "receipts" => read_receipts(id)?.to_string(),
        "actions" => message_actions(id)?.to_string(),
        "realm" => realm_info()?.to_string(),
        "download_ex" => {
            let mut calls = 0;
            let path = download_attachment_ex(str_of(args, "path"), |_, _| calls += 1)?;
            format!("{path} 进度 {}", calls > 0)
        }
        "upload_ex" => {
            let mut last = (0, 0);
            let url = upload_file_ex(str_of(args, "path"), |done, total| last = (done, total))?;
            format!("{url} 进度 {} {}", last.0, last.1)
        }
        // The following actions run on separate threads (not allowed or not applicable on the
        // plugin thread), and the results are written to the log.
        "dialog" => {
            let spec = args.clone();
            let delay = args["delay_ms"].as_u64().unwrap_or_default();
            let flood = args["flood"].as_u64().unwrap_or_default();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(delay));
                flood_events(flood);
                let result = show_dialog(&spec);
                log(LOG_INFO, &format!("对话框结果：{result:?}"));
            });
            "已发起".to_owned()
        }
        "dialog_here" => show_dialog(args)?.to_string(),
        "thread_account" => {
            let account = int(args, "account");
            let result = thread::spawn(move || {
                let before = current_account();
                let bound = set_thread_account(account);
                let after = current_account();
                let unbound = set_thread_account(0);
                let last = current_account();
                format!("{before:?} {bound:?} {after:?} {unbound:?} {last:?}")
            })
            .join();
            result.unwrap_or_default()
        }
        "bind_here" => {
            let account = int(args, "account");
            let bound = set_thread_account(account);
            let current = current_account();
            set_thread_account(0)?;
            format!("{bound:?} {current:?}")
        }
        "bind_wait" => {
            let account = int(args, "account");
            thread::spawn(move || {
                let bound = set_thread_account(account);
                thread::sleep(Duration::from_millis(800));
                log(
                    LOG_INFO,
                    &format!("绑定后账号：{bound:?} {:?}", current_account()),
                );
            });
            "已发起".to_owned()
        }
        "after_disable" => {
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(300));
                let toast = show_toast("停用后");
                let dir = data_dir().map(|_| ());
                let values = settings();
                log(
                    LOG_INFO,
                    &format!("停用后调用：{toast:?} {dir:?} {}", values.is_object()),
                );
            });
            "已发起".to_owned()
        }
        _ => "未知操作".to_owned(),
    })
}

/// Lots of logs and toasts: pushes the host's event channel into falling behind.
fn flood_events(count: u64) {
    for i in 0..count {
        log(LOG_INFO, &format!("洪水 {i}"));
        // When a toast fails, the next one still continues
        let _ = show_toast(&format!("洪水 {i}"));
    }
}

impl Plugin for Echo {
    fn on_enable(&self) {
        self.enabled.store(true, Ordering::Release);
        log(LOG_INFO, "已开启");
    }

    fn on_disable(&self) {
        self.enabled.store(false, Ordering::Release);
        log(
            LOG_INFO,
            &format!("停用回调中：{:?}", realm_info().map(|_| ())),
        );
        log(LOG_INFO, "已停用");
    }

    fn on_account_login(&self, account_id: i64) {
        log(LOG_INFO, &format!("账号 {account_id} 已登录"));
        let values = settings();
        let title = str_of(&values, "login_dialog").to_owned();
        if !title.is_empty() {
            let flood = str_of(&values, "login_flood").parse().unwrap_or_default();
            // A dialog cannot be shown on the plugin thread
            thread::spawn(move || {
                flood_events(flood);
                let result = show_dialog(&json!({"kind": "confirm", "title": title}));
                log(LOG_INFO, &format!("登录对话框结果：{result:?}"));
            });
        }
    }

    fn on_account_logout(&self, account_id: i64) {
        log(LOG_INFO, &format!("账号 {account_id} 已退出"));
    }

    fn on_message(&self, message: &Message) -> Event {
        if message.is_outgoing() || message.text.trim() != "ping" {
            return Event::Ignore;
        }
        if let Conversation::Direct { user_ids } = &message.conv
            && user_ids.len() == 1
        {
            let reply = settings();
            let text = match str_of(&reply, "reply") {
                "" => "pong",
                text => text,
            };
            if let Err(error) = send_direct_message(user_ids, text) {
                log(LOG_WARNING, &format!("回复失败：{error}"));
            }
        }
        Event::Ignore
    }

    fn filter_incoming(&self, message: &Message) -> Event {
        if message.text.contains(BLOCK) {
            Event::Block
        } else {
            Event::Ignore
        }
    }

    fn filter_outgoing(&self, outgoing: &Outgoing) -> Event {
        if outgoing.content.contains(BLOCK) {
            return Event::Block;
        }
        if outgoing.kind != OutgoingKind::Text {
            log(LOG_INFO, &format!("发送种类 {:?}", outgoing.kind));
        }
        if outgoing.content == SLOW {
            std::thread::sleep(SLOW_FOR);
        }
        if let Some(rest) = outgoing.content.strip_prefix(REWRITE)
            && let Err(error) = set_outgoing_content(rest)
        {
            log(LOG_WARNING, &format!("改写失败：{error}"));
        }
        Event::Ignore
    }

    fn on_menu(&self, menu_id: i32) {
        log(LOG_INFO, &format!("点击了菜单 {menu_id}"));
    }

    fn on_message_menu(&self, _menu_id: i32, message: &Message) {
        let annotation = json!({
            "text": format!("**回声**：{}", message.text),
            "actions": [{"label": "填入", "fill": message.text, "style": "primary"}],
        });
        if let Err(error) = set_annotation(message.id, &annotation) {
            log(LOG_WARNING, &format!("附加失败：{error}"));
        }
    }

    fn on_context_menu(&self, menu_id: i32, click: &ContextMenuClick) {
        let target = match &click.target {
            Some(ContextTarget::Conversation(conv)) => conv.describe(),
            Some(ContextTarget::User { name, .. }) => format!("用户 {name}"),
            Some(ContextTarget::Channel { name, .. }) => format!("频道 {name}"),
            None => return,
        };
        log(LOG_INFO, &format!("右键菜单 {menu_id}：{target}"));
        if let Err(error) = set_status_text(&format!("回声：{target}")) {
            log(LOG_WARNING, &format!("状态栏提示失败：{error}"));
        }
    }

    fn on_composer_button(&self, _button_id: i32, click: &ComposerClick) {
        if let Some(rest) = click.text.strip_prefix(CALL) {
            let (op, args) = rest.split_once(' ').unwrap_or((rest, "{}"));
            let args = zulip_plugin_sdk::serde_json::from_str(args).unwrap_or(Value::Null);
            match call(op, &args) {
                Ok(result) => log(LOG_INFO, &format!("调用 {op}：{result}")),
                Err(error) => log(LOG_WARNING, &format!("调用 {op} 失败：{error}")),
            }
            return;
        }
        let replaced = format!("回声：{}", click.text);
        if let Err(error) = fill_composer(&replaced, true) {
            log(LOG_WARNING, &format!("填入失败：{error}"));
        }
    }

    fn on_message_edited(&self, event: &MessageEdited) {
        log(
            LOG_INFO,
            &format!("编辑 {}：{}", event.message_id, event.text),
        );
    }

    fn on_message_deleted(&self, event: &MessagesDeleted) {
        log(LOG_INFO, &format!("删除 {:?}", event.message_ids));
    }

    fn on_reaction_changed(&self, event: &ReactionChanged) {
        log(
            LOG_INFO,
            &format!(
                "回应 {} {} {}",
                event.message_id, event.reaction.emoji_name, event.added
            ),
        );
    }

    fn on_read_state_changed(&self, event: &ReadStateChanged) {
        log(
            LOG_INFO,
            &format!("已读 {:?} {}", event.message_ids, event.read),
        );
    }

    fn on_topic_moved(&self, event: &TopicMoved) {
        let describe = |conv: &Option<Conversation>| match conv {
            Some(Conversation::Channel {
                stream_id, topic, ..
            }) => format!("{stream_id}/{topic}"),
            _ => String::new(),
        };
        log(
            LOG_INFO,
            &format!("移动 {} -> {}", describe(&event.old), describe(&event.new)),
        );
    }

    fn on_channel_changed(&self, event: &ChannelsChanged) {
        log(
            LOG_INFO,
            &format!("频道 {:?} {}", event.stream_ids, event.op),
        );
    }

    fn on_user_changed(&self, event: &UserChanged) {
        log(LOG_INFO, &format!("用户 {}", event.user_id));
    }

    fn on_presence_changed(&self, event: &PresenceChanged) {
        log(LOG_INFO, &format!("在线 {:?}", event.user_ids));
    }

    fn on_conversation_opened(&self, event: &ConversationOpened) {
        if let Some(conv) = &event.conv {
            log(LOG_INFO, &format!("切换到 {}", conv.describe()));
        }
    }

    fn on_connection_changed(&self, event: &ConnectionChanged) {
        log(LOG_INFO, &format!("连接 {}", event.state));
    }

    fn on_typing_changed(&self, event: &TypingChanged) {
        log(
            LOG_INFO,
            &format!("输入 {} {}", event.user_id, event.typing),
        );
    }

    fn on_scheduled_changed(&self, _event: &ScheduledChanged) {
        log(LOG_INFO, "定时消息变化");
    }

    fn on_reminders_changed(&self, _event: &RemindersChanged) {
        log(LOG_INFO, "提醒变化");
    }

    fn on_draft_changed(&self, event: &DraftChanged) {
        let conv = event
            .conv
            .as_ref()
            .map_or_else(String::new, Conversation::describe);
        log(LOG_INFO, &format!("草稿变化 {conv}"));
    }

    fn on_user_topic_changed(&self, event: &UserTopicChanged) {
        log(LOG_INFO, &format!("话题关注 {}", event.visibility));
    }

    fn on_muted_users_changed(&self, event: &MutedUsersChanged) {
        log(LOG_INFO, &format!("静音 {:?}", event.user_ids));
    }

    fn on_realm_changed(&self, event: &RealmChanged) {
        log(LOG_INFO, &format!("组织 {:?}", event.properties));
    }

    fn on_settings_changed(&self) {
        log(LOG_INFO, "设置已变化");
    }

    fn on_settings_button(&self, button: &SettingsButton) {
        let text = format!("回复内容：{}", str_of(&button.values, "reply"));
        if let Err(error) = settings_text(&button.key, button.row, &text) {
            log(LOG_WARNING, &format!("回复设置窗口失败：{error}"));
        }
    }

    fn on_panel(&self, request: &PanelRequest) {
        let reply = match request.action.as_str() {
            "get" => self.panel_fields(request.user_id, ""),
            "set" => {
                log(
                    LOG_INFO,
                    &format!("面板修改 {}：{}", request.key, request.value),
                );
                if request.key == "nickname" {
                    self.set_nickname(
                        request.account_id,
                        request.user_id,
                        request.value.as_str().unwrap_or_default(),
                    );
                }
                self.panel_fields(request.user_id, "")
            }
            "click" => {
                if request.key == SLOW {
                    thread::sleep(SLOW_FOR);
                }
                self.panel_fields(request.user_id, &format!("点击了 {}", request.key))
            }
            "remove" => {
                if self.nickname(request.user_id) == SLOW {
                    thread::sleep(SLOW_FOR);
                }
                self.set_nickname(request.account_id, request.user_id, "");
                return;
            }
            _ => {
                let nicknames = self.nicknames.lock().unwrap_or_else(|e| e.into_inner());
                let entries: Vec<Value> = nicknames
                    .iter()
                    // A list request has no target; the account must be the one recorded when the
                    // settings were set
                    .map(|(account, id, text)| {
                        json!({
                            "target": {"account": account, "user": id, "name": id.to_string()},
                            "text": text,
                        })
                    })
                    .collect();
                json!({ "entries": entries })
            }
        };
        if let Err(error) = reply_panel(&reply) {
            log(LOG_WARNING, &format!("回复面板失败：{error}"));
        }
    }
}

export_plugin!(Echo::default(), "回声示例");
