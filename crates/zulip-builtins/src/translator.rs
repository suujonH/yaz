//! Translation: the message context menu item "翻译" first shows "正在翻译…" below the message,
//! translates through the model center on a worker thread, then on success temporarily replaces the
//! bubble's displayed content with the translation (the host provides a "show original" toggle and
//! does not modify the message's original text). The translation is saved in translator.db in the
//! data directory and re-attached when the message is shown again. Next to the toggle are refresh
//! (translate again) and close (remove the translation from the message, it stays saved); clicking
//! "翻译" when a translation is saved shows it without a new request.

use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use zulip_plugin_host::{BuiltinContext, BuiltinMessage, BuiltinPlugin};
use zulip_plugin_sdk::abi::{LOG_ERROR, LOG_INFO, LOG_WARNING};
use zulip_plugin_sdk::annotation_store::AnnotationStore;
use zulip_plugin_sdk::str_of;

pub const ID: &str = "com.yaz.translator";
pub const MANIFEST: &str = include_str!("translator.json");

const MENU_TRANSLATE: i32 = 1;
// Buttons next to the translation (not in the manifest's menus).
const MENU_REFRESH: i32 = 2;
const MENU_REMOVE: i32 = 3;
const MODEL_FIELD: &str = "model";
const TEST_BUTTON: &str = "test";
const TEST_TEXT: &str = "你好";
const DATABASE_FILE: &str = "translator.db";
const TABLE: &str = "translation";

// Target language and the prompt after substitution.
fn prompt_of(settings: &Value) -> (String, String) {
    let language = match str_of(settings, "language").trim() {
        "" => "简体中文",
        language => language,
    }
    .to_owned();
    let prompt = str_of(settings, "prompt").replace("{language}", &language);
    (language, prompt)
}

/// The model chosen in the settings, if any.
fn selection(settings: &Value) -> Option<Value> {
    let value = settings.get(MODEL_FIELD)?;
    (!str_of(value, "resource").is_empty()).then(|| value.clone())
}

/// Chat request: a system prompt (may be empty) plus one user message; returns the reply text.
fn ask(ctx: &BuiltinContext, model: &Value, system: &str, user: &str) -> Result<String, String> {
    let mut messages = Vec::new();
    if !system.trim().is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.push(json!({"role": "user", "content": user}));
    let reply = ctx.request_resource(model, &json!({"messages": messages}))?;
    reply["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "模型中心返回的格式无法识别".to_owned())
}

type Database = Arc<OnceLock<Option<AnnotationStore>>>;

#[derive(Default)]
pub struct Translator {
    // Opened on first use; shared with the worker threads that save translations.
    database: Database,
}

impl BuiltinPlugin for Translator {
    fn on_message_menu(&self, ctx: &BuiltinContext, menu_id: i32, message: &BuiltinMessage) {
        let (account, id) = (message.account_id, message.id);
        match menu_id {
            MENU_TRANSLATE => {
                self.set_hidden(ctx, account, id, false);
                match self.saved(ctx, account, &[id], true).pop() {
                    Some((_, translated)) => {
                        ctx.set_annotation(account, id, Some(&translation(&translated)));
                    }
                    None => self.start_translation(ctx, message),
                }
            }
            MENU_REFRESH => {
                self.set_hidden(ctx, account, id, false);
                self.start_translation(ctx, message);
            }
            MENU_REMOVE => {
                self.set_hidden(ctx, account, id, true);
                ctx.set_annotation(account, id, None);
            }
            _ => {}
        }
    }

    // Re-attach the saved translation when a message is shown; removed ones are not.
    fn on_messages_shown(&self, ctx: &BuiltinContext, account_id: i64, message_ids: &[i64]) {
        for (message_id, translated) in self.saved(ctx, account_id, message_ids, false) {
            ctx.set_annotation(account_id, message_id, Some(&translation(&translated)));
        }
    }

    // "Test" in settings: values is the form's current value (not yet saved).
    fn on_settings_button(&self, ctx: &BuiltinContext, button: &Value) {
        if str_of(button, "key") != TEST_BUTTON {
            return;
        }
        let values = &button["values"];
        let Some(model) = selection(values) else {
            ctx.settings_text(TEST_BUTTON, None, "请先选择模型");
            return;
        };
        let (language, prompt) = prompt_of(values);
        ctx.settings_text(
            TEST_BUTTON,
            None,
            &format!("正在把“{TEST_TEXT}”翻译为{language}…"),
        );
        let worker = ctx.clone();
        ctx.spawn(move || {
            let text = match ask(&worker, &model, &prompt, TEST_TEXT) {
                Ok(reply) if !reply.trim().is_empty() => format!("成功：{}", reply.trim()),
                Ok(_) => "失败：模型没有返回文字".to_owned(),
                Err(error) => format!("失败：{error}"),
            };
            worker.settings_text(TEST_BUTTON, None, &text);
        });
    }
}

/// The refresh and close buttons.
fn tools() -> Value {
    json!([
        {"menu": MENU_REFRESH, "icon": "refresh", "tooltip": "重新翻译"},
        {"menu": MENU_REMOVE, "icon": "close", "tooltip": "关闭译文（再次翻译时直接显示，不重新请求）"},
    ])
}

fn translation(translated: &str) -> Value {
    json!({"replace": translated, "tools": tools()})
}

impl Translator {
    // Shows progress below the message and translates on a worker thread.
    fn start_translation(&self, ctx: &BuiltinContext, message: &BuiltinMessage) {
        let (account, id) = (message.account_id, message.id);
        let note = |text: String| ctx.set_annotation(account, id, Some(&json!({"text": text})));
        if message.text.trim().is_empty() {
            note("这条消息没有可翻译的文字".to_owned());
            return;
        }
        let settings = ctx.settings();
        let Some(model) = selection(&settings) else {
            note("请先在“设置 > 翻译”中选择模型".to_owned());
            return;
        };
        let (language, prompt) = prompt_of(&settings);
        note(format!("正在翻译为{language}…"));
        let (worker, text, database) = (ctx.clone(), message.text.clone(), self.database.clone());
        ctx.spawn(move || {
            let result = ask(&worker, &model, &prompt, &text);
            if let Err(error) = &result {
                worker.log(LOG_ERROR, &format!("翻译消息 {id} 失败：{error}"));
            }
            let translated = result.as_deref().map(str::trim).unwrap_or_default();
            let annotation = if translated.is_empty() {
                let reason = result
                    .as_ref()
                    .err()
                    .map_or("模型没有返回文字", String::as_str);
                json!({"text": format!("翻译失败：{reason}"), "tools": tools()})
            } else {
                let saved = database.get_or_init(|| open(&worker)).as_ref();
                if let Some(Err(error)) = saved.map(|db| db.save(account, id, translated)) {
                    worker.log(LOG_WARNING, &format!("保存译文失败：{error}"));
                }
                worker.log(LOG_INFO, &format!("已翻译消息 {id}"));
                translation(translated)
            };
            worker.set_annotation(account, id, Some(&annotation));
        });
    }

    fn database(&self, ctx: &BuiltinContext) -> Option<&AnnotationStore> {
        self.database.get_or_init(|| open(ctx)).as_ref()
    }

    // Saved translations: (message id, translation). `with_hidden`: also those the user removed.
    fn saved(
        &self,
        ctx: &BuiltinContext,
        account: i64,
        message_ids: &[i64],
        with_hidden: bool,
    ) -> Vec<(i64, String)> {
        if message_ids.is_empty() {
            return Vec::new();
        }
        let Some(db) = self.database(ctx) else {
            return Vec::new();
        };
        let rows = if with_hidden {
            db.load(account, message_ids)
        } else {
            db.load_visible(account, message_ids)
        };
        rows.unwrap_or_else(|error| {
            ctx.log(LOG_WARNING, &format!("读取译文失败：{error}"));
            Vec::new()
        })
    }

    fn set_hidden(&self, ctx: &BuiltinContext, account: i64, message_id: i64, hidden: bool) {
        if let Some(db) = self.database(ctx)
            && let Err(error) = db.set_hidden(account, message_id, hidden)
        {
            ctx.log(LOG_WARNING, &format!("保存译文失败：{error}"));
        }
    }
}

fn open(ctx: &BuiltinContext) -> Option<AnnotationStore> {
    let path = ctx.data_dir()?.join(DATABASE_FILE);
    AnnotationStore::open_at(&path, TABLE, "")
        .inspect_err(|error| ctx.log(LOG_WARNING, &format!("打开数据库失败：{error}")))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_replaces_language() {
        let (language, prompt) =
            prompt_of(&json!({"language": " 英语 ", "prompt": "译为{language}：{language}"}));
        assert_eq!(language, "英语");
        assert_eq!(prompt, "译为英语：英语");
        assert_eq!(prompt_of(&json!({})).0, "简体中文");
    }
}
