//! Composer controller: connects the editor JS, the content model and the conversation. Sending,
//! drafts and typing notifications are here; the JS protocol is in `bridge`, uploads in `upload`,
//! completion in `complete`.

use super::ComposerHandle;
use super::bridge::TableRect;
use super::complete::Popup;
use super::core::{EditorCore, strip_stale_uploads};
use super::dialogs;
use super::language::LangSearch;
use super::render::Lookup;
use super::upload::{PastedFile, UploadRow, screenshot_name, write_paste_file};
use crate::pages::chat::quote_preview_html;
use crate::services::{plugins, services, write_account_setting};
use crate::ui::{Overlays, PopoverHandle, TextPrompt};
use crate::widgets::ask_future_time;
use chrono::{Local, TimeZone};
use dioxus::dioxus_core::{Runtime, current_scope_id, spawn_forever};
use dioxus::prelude::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zulip_core::ConversationKey;
use zulip_core::html::to_preview;
use zulip_core::markdown::format::{self, FormatKind};
use zulip_core::markdown::table::TableAlign;
use zulip_core::markdown::{SpanKind, quote};
use zulip_core::text::time::datetime_label;
use zulip_session::{DeleteTarget, Session};
use zulip_store::AccountSettings;

const DRAFT_DELAY: Duration = Duration::from_secs(1);
const ACTIVE_KINDS: [FormatKind; 7] = [
    FormatKind::Bold,
    FormatKind::Italic,
    FormatKind::Code,
    FormatKind::Link,
    FormatKind::Quote,
    FormatKind::NumberedList,
    FormatKind::BulletedList,
];

/// State needed by the toolbar and the send button.
#[derive(Clone, Default, PartialEq)]
pub struct View {
    pub blank: bool,
    pub quote_only: bool,
    pub active: Vec<FormatKind>,
    pub heading: u8,
    pub table: Option<(usize, usize)>,
    pub align: TableAlign,
    /// The cursor's line is code (see [`EditorCore::caret_line_is_code`]): nothing can be
    /// formatted or inserted there.
    pub in_code: bool,
}

#[derive(Clone, PartialEq)]
pub struct EditState {
    pub id: i64,
    pub preview: String,
    pub original: String,
    stash: String,
}

type DeferredEdit = Box<dyn FnOnce(Ctl)>;

#[derive(Clone, Copy)]
pub struct Ctl {
    pub core: CopyValue<EditorCore>,
    pub(super) session: CopyValue<Arc<Session>>,
    pub(super) key: CopyValue<ConversationKey>,
    pub overlays: Overlays,
    pub handle: ComposerHandle,
    pub dom_id: CopyValue<String>,
    pub(super) scope: ScopeId,
    pub(super) tx: CopyValue<UnboundedSender<Value>>,
    pub(super) rx: CopyValue<Option<UnboundedReceiver<Value>>>,
    /// Sequence number of the last input from JS: included in replies so JS can discard stale
    /// patches.
    pub(super) seq: CopyValue<u64>,
    pub(super) composing: CopyValue<bool>,
    pub(super) focused: CopyValue<bool>,
    /// Async changes that arrive during IME composition (upload completion and so on), applied
    /// after composition ends.
    pub(super) deferred: CopyValue<Vec<DeferredEdit>>,
    pub(super) caret: CopyValue<(f64, f64)>,
    pub(super) dismissed: CopyValue<Option<usize>>,
    pub(super) draft_gen: CopyValue<u64>,
    pub(super) fetching: CopyValue<HashSet<String>>,
    pub(super) pasting: CopyValue<HashMap<u64, PastedFile>>,
    pub(super) subscribers: CopyValue<Option<HashSet<i64>>>,
    pub(super) next_upload: CopyValue<u64>,
    pub view: Signal<View>,
    pub popup: Signal<Option<Popup>>,
    pub table_rect: Signal<Option<TableRect>>,
    pub uploads: Signal<Vec<UploadRow>>,
    pub sending: Signal<bool>,
    pub edit: Signal<Option<EditState>>,
    pub source: Signal<bool>,
    /// Text typed in the language box under a code block, while its language list is shown.
    pub(super) lang: Signal<Option<LangSearch>>,
    pub(super) lang_popover: CopyValue<Option<PopoverHandle>>,
}

impl PartialEq for Ctl {
    fn eq(&self, other: &Self) -> bool {
        self.core == other.core
    }
}

fn next_dom_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("composer-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn build_lookup(session: &Session, images: HashMap<String, String>) -> Lookup {
    let users = session.users();
    Lookup {
        user_names: users.iter().map(|u| u.full_name.to_lowercase()).collect(),
        users: users.into_iter().map(|u| (u.id, u.full_name)).collect(),
        groups: HashSet::new(),
        channels: session
            .channels()
            .iter()
            .map(|c| c.name.to_lowercase())
            .collect(),
        emoji: session.emoji_catalog(),
        images,
    }
}

/// Create the controller. Call it only once, inside the component.
pub fn use_ctl(
    session: Arc<Session>,
    key: ConversationKey,
    overlays: Overlays,
    handle: ComposerHandle,
) -> Ctl {
    let view = use_signal(View::default);
    let popup = use_signal(|| None);
    let table_rect = use_signal(|| None);
    let uploads = use_signal(Vec::new);
    let sending = use_signal(|| false);
    let edit = use_signal(|| None);
    let source_signal = use_signal(|| false);
    let lang = use_signal(|| None);
    use_hook(move || {
        let (tx, rx) = unbounded_channel();
        let source =
            AccountSettings::new(&services().db, session.account_id()).composer_source_mode();
        let draft = session
            .draft(&key)
            .map(|d| strip_stale_uploads(&d.content))
            .unwrap_or_default();
        let mut initial_source = source_signal;
        initial_source.set(source);
        let core = EditorCore::new(draft, source, build_lookup(&session, HashMap::new()));
        let ctl = Ctl {
            core: CopyValue::new(core),
            session: CopyValue::new(session),
            key: CopyValue::new(key),
            overlays,
            handle,
            dom_id: CopyValue::new(next_dom_id()),
            scope: current_scope_id(),
            tx: CopyValue::new(tx),
            rx: CopyValue::new(Some(rx)),
            seq: CopyValue::new(0),
            composing: CopyValue::new(false),
            focused: CopyValue::new(false),
            deferred: CopyValue::new(Vec::new()),
            caret: CopyValue::new((0.0, 0.0)),
            dismissed: CopyValue::new(None),
            draft_gen: CopyValue::new(0),
            fetching: CopyValue::new(HashSet::new()),
            pasting: CopyValue::new(HashMap::new()),
            subscribers: CopyValue::new(None),
            next_upload: CopyValue::new(0),
            lang,
            lang_popover: CopyValue::new(None),
            view,
            popup,
            table_rect,
            uploads,
            sending,
            edit,
            source: source_signal,
        };
        ctl.update_view();
        ctl.load_subscribers();
        ctl.fetch_assets();
        ctl
    })
}

impl Ctl {
    pub fn session(self) -> Arc<Session> {
        self.session.peek().clone()
    }

    pub fn key(self) -> ConversationKey {
        self.key.peek().clone()
    }

    pub fn spawn(self, task: impl Future<Output = ()> + 'static) {
        let scope = self.scope;
        Runtime::current().in_scope(scope, || {
            spawn(task);
        });
    }

    pub(super) fn is_editing(self) -> bool {
        self.edit.peek().is_some()
    }

    /// When the component is destroyed: detach JS, save the draft, end typing.
    pub fn shutdown(self) {
        let id = self.dom_id.peek().clone();
        document::eval(&format!(
            "window.zulipComposer && window.zulipComposer.detach({id:?});"
        ));
        let session = self.session();
        let key = self.key();
        session.stop_typing();
        let text = match self.edit.peek().as_ref() {
            Some(edit) => edit.stash.clone(),
            None => self.core.peek().text().to_owned(),
        };
        spawn_forever(async move {
            if let Err(error) = session.save_draft(&key, &text).await {
                log::warn!("保存草稿失败：{error}");
            }
        });
    }

    /// A change caused by a user action.
    pub fn edit(self, change: impl FnOnce(&mut EditorCore)) {
        change(&mut self.core.write_unchecked());
        self.push(true, true, false);
        self.after_change(true);
    }

    /// A plugin fills in text and focuses: replace all content, or insert at the cursor.
    pub fn fill(self, text: String, replace: bool) {
        self.when_idle(move |ctl| {
            ctl.edit(|core| {
                if replace {
                    core.replace_all(&text);
                } else {
                    core.insert_smart(&text);
                }
            });
        });
    }

    /// A change that completes asynchronously: hold it during IME composition, otherwise it would
    /// interrupt the input method.
    pub(super) fn when_idle(self, change: impl FnOnce(Ctl) + 'static) {
        if *self.composing.peek() {
            self.deferred.write_unchecked().push(Box::new(change));
        } else {
            change(self);
        }
    }

    pub(super) fn after_change(self, text_changed: bool) {
        self.update_view();
        if text_changed {
            self.fetch_assets();
            if !self.is_editing() {
                let session = self.session();
                let has_text = !self.core.peek().text().is_empty();
                session.on_compose_input(&self.key.peek(), has_text);
                self.schedule_draft();
            }
        }
        self.refresh_popup(text_changed);
    }

    pub(super) fn update_view(self) {
        let core = self.core.peek();
        let text = core.text();
        let view = View {
            blank: text.trim().is_empty(),
            quote_only: quote::has_only_quotes(text),
            active: if core.source {
                Vec::new()
            } else {
                ACTIVE_KINDS
                    .into_iter()
                    .filter(|k| core.is_active(*k))
                    .collect()
            },
            heading: core.heading_level(),
            table: core.active_table_size(),
            align: core.active_align(),
            in_code: core.caret_line_is_code(),
        };
        drop(core);
        let mut signal = self.view;
        if *signal.peek() != view {
            signal.set(view);
        }
    }

    // An expanded quote shows how it will look after sending
    pub(super) fn render_quotes(self) {
        for (src, body) in self.core.peek().quotes_to_render() {
            let session = self.session();
            self.spawn(async move {
                if let Ok(html) = session.render_preview(&body).await {
                    let html = quote_preview_html(&session, &html).await;
                    self.core
                        .write_unchecked()
                        .rendered_quotes
                        .insert(src, html);
                    self.when_idle(|ctl| {
                        ctl.core.write_unchecked().rev += 1;
                        ctl.push(true, false, false);
                    });
                }
            });
        }
    }

    // --------- Copy and paste ---------

    pub fn has_selection(self) -> bool {
        !self.core.peek().sel.is_collapsed()
    }

    pub fn copy(self) {
        let core = self.core.peek();
        let markdown = core.selected_text();
        if markdown.is_empty() {
            return;
        }
        let plain = core.plain_text(&markdown);
        drop(core);
        crate::platform::remember_markdown(&plain, &markdown);
        self.overlays.copy_text(&plain);
    }

    pub fn cut(self) {
        if self.has_selection() {
            self.copy();
            self.edit(|c| c.insert(""));
        }
    }

    /// Copy the source of a table.
    pub fn copy_table(self) {
        if let Some(source) = self.core.peek().active_table_source() {
            crate::platform::remember_markdown(&source, &source);
            self.overlays.copy_text(&source);
        }
    }

    pub(super) fn paste_text(self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.is_empty() {
            return;
        }
        let copied = crate::platform::copied_markdown(&text);
        let selected = self.core.peek().selected_text();
        self.edit(|core| match copied {
            Some(markdown) => core.insert_block(&markdown),
            None if is_url(&text) && !selected.is_empty() && !selected.contains('\n') => {
                core.insert(&format!("[{selected}]({text})"));
            }
            None => core.insert_block(&text),
        });
    }

    /// Paste from the context menu: check for an image first, then text.
    pub fn paste_from_clipboard(self) {
        if let Some(png) = crate::platform::paste_image().and_then(|image| image.to_png()) {
            let name = screenshot_name();
            match write_paste_file(&name, &png) {
                Ok(path) => self.upload(path, name),
                Err(error) => self.overlays.report_error("粘贴失败", error),
            }
            return;
        }
        match crate::platform::paste_text() {
            Some(text) => self.paste_text(&text),
            None => self.refocus(),
        }
    }

    // Images to show in the editor: re-render after downloading locally
    fn fetch_assets(self) {
        let core = self.core.peek();
        let urls: Vec<String> = core
            .model()
            .spans
            .iter()
            .filter_map(|span| {
                let data = span.data.as_deref()?;
                match span.kind {
                    SpanKind::UploadImage => Some(data.to_owned()),
                    SpanKind::Emoji => core
                        .lookup
                        .emoji
                        .get(&data.to_lowercase())?
                        .image_url
                        .clone(),
                    _ => None,
                }
            })
            .filter(|url| {
                !core.lookup.images.contains_key(url) && !self.fetching.peek().contains(url)
            })
            .collect();
        drop(core);
        for url in urls {
            self.fetching.write_unchecked().insert(url.clone());
            let session = self.session();
            self.spawn(async move {
                if let Ok(path) = session.image_file(&url).await
                    && let Some(local) = crate::shell::url_for(&path)
                {
                    self.core.write_unchecked().lookup.images.insert(url, local);
                    self.push(false, false, false);
                }
            });
        }
    }

    /// User, channel or emoji data changed: whether a mention exists may differ.
    pub fn refresh_lookup(self) {
        let images = self.core.peek().lookup.images.clone();
        let lookup = build_lookup(&self.session(), images);
        self.core.write_unchecked().set_lookup(lookup);
        self.push(false, false, false);
    }

    // --------- Drafts, typing ---------

    fn schedule_draft(self) {
        let generation = *self.draft_gen.peek() + 1;
        *self.draft_gen.write_unchecked() = generation;
        self.spawn(async move {
            tokio::time::sleep(DRAFT_DELAY).await;
            if *self.draft_gen.peek() == generation && !self.is_editing() {
                let text = self.core.peek().text().to_owned();
                if let Err(error) = self.session().save_draft(&self.key(), &text).await {
                    log::warn!("保存草稿失败：{error}");
                }
            }
        });
    }

    // --------- Sending ---------

    pub fn can_send(self) -> bool {
        let view = self.view.peek();
        !*self.sending.peek()
            && self.uploads.peek().is_empty()
            && (self.is_editing() || (!view.blank && !view.quote_only))
    }

    fn max_length_ok(self, text: &str) -> bool {
        let max = self.session().realm().max_message_length;
        if max > 0 && text.chars().count() > max as usize {
            self.overlays.toast("消息过长");
            return false;
        }
        true
    }

    pub fn send(self) {
        if !self.can_send() {
            return;
        }
        if self.is_editing() {
            self.save_edit();
            return;
        }
        self.dispatch(move |session, key, text| async move {
            session.send_message(&key, &text).await.map(|_| ())
        });
    }

    /// Scheduled sending.
    pub fn send_later(self) {
        if !self.can_send() || self.is_editing() {
            return;
        }
        self.spawn(async move {
            let Some(at) = ask_future_time(self.overlays, "定时发送", None).await else {
                self.refocus();
                return;
            };
            self.dispatch(move |session, key, text| async move {
                session.schedule_message(&key, &text, at).await.map(|_| ())
            });
            self.overlays
                .toast(format!("将在 {} 发送", datetime_label(at)));
        });
    }

    /// Send and set a reminder.
    pub fn send_with_reminder(self) {
        if !self.can_send() || self.is_editing() {
            return;
        }
        self.spawn(async move {
            let Some(at) = ask_future_time(self.overlays, "发送并提醒", None).await else {
                self.refocus();
                return;
            };
            self.dispatch(move |session, key, text| async move {
                let id = session.send_message(&key, &text).await?;
                session.create_reminder(id, at).await.map(|_| ())
            });
        });
    }

    /// Messages that do not go through the editor, such as polls.
    pub fn send_text(self, text: String) {
        let session = self.session();
        let key = self.key();
        let overlays = self.overlays;
        self.spawn(async move {
            match plugins().filter_outgoing(&session, &key, &text).await {
                Ok(text) => {
                    overlays
                        .attempt("发送失败", session.send_message(&key, &text))
                        .await;
                }
                Err(rejected) => overlays.toast(rejected.to_string()),
            }
        });
    }

    fn dispatch<F, E>(
        self,
        action: impl FnOnce(Arc<Session>, ConversationKey, String) -> F + 'static,
    ) where
        F: Future<Output = Result<(), E>> + 'static,
        E: std::fmt::Display,
    {
        let text = self.core.peek().text().trim_end().to_owned();
        if !self.max_length_ok(&text) {
            return;
        }
        let mut sending = self.sending;
        sending.set(true);
        self.close_popup();
        self.spawn(async move {
            let session = self.session();
            let key = self.key();
            let filtered = plugins().filter_outgoing(&session, &key, &text).await;
            sending.set(false);
            let text = match filtered {
                Ok(text) => text,
                Err(rejected) => {
                    self.overlays.toast(rejected.to_string());
                    return;
                }
            };
            self.when_idle(|ctl| {
                ctl.core.write_unchecked().reset(String::new());
                ctl.push(true, true, false);
                ctl.update_view();
                *ctl.draft_gen.write_unchecked() += 1;
            });
            session.stop_typing();
            if let Err(error) = session.save_draft(&key, "").await {
                log::warn!("删除草稿失败：{error}");
            }
            self.overlays
                .attempt("发送失败", action(session, key, text))
                .await;
        });
    }

    // --------- Editing a message ---------

    /// Called when `ComposerHandle.editing` changes.
    pub fn on_editing(self, id: Option<i64>) {
        let current = self.edit.peek().as_ref().map(|e| e.id);
        if current == id {
            return;
        }
        let mut edit = self.edit;
        match id {
            Some(id) => self.spawn(async move {
                let session = self.session();
                let Some(raw) = self
                    .overlays
                    .attempt("编辑失败", session.raw_content(id))
                    .await
                else {
                    let mut editing = self.handle.editing;
                    editing.set(None);
                    return;
                };
                let preview = session
                    .message(id)
                    .map(|m| to_preview(&m.content_html))
                    .unwrap_or_default();
                let previous = edit.peek().as_ref().map(|e| e.stash.clone());
                let stash = previous.unwrap_or_else(|| self.core.peek().text().to_owned());
                let original = format::with_trailing_line(&raw);
                session.stop_typing();
                edit.set(Some(EditState {
                    id,
                    preview,
                    original: original.clone(),
                    stash,
                }));
                self.core.write_unchecked().reset(original);
                self.send_options();
                self.push(true, true, false);
                self.after_change(false);
            }),
            None => {
                let state = edit.peek().clone();
                if let Some(state) = state {
                    edit.set(None);
                    self.core.write_unchecked().reset(state.stash);
                    self.send_options();
                    self.push(true, true, false);
                    self.after_change(false);
                }
            }
        }
    }

    fn save_edit(self) {
        let Some(state) = self.edit.peek().clone() else {
            return;
        };
        let text = self.core.peek().text().trim_end().to_owned();
        if text.trim().is_empty() {
            self.overlays.toast("消息内容不能为空");
            return;
        }
        if !self.max_length_ok(&text) {
            return;
        }
        let mut editing = self.handle.editing;
        if text == state.original.trim_end() {
            editing.set(None);
            return;
        }
        let mut sending = self.sending;
        sending.set(true);
        self.spawn(async move {
            let session = self.session();
            let saved = self
                .overlays
                .attempt("编辑失败", session.edit_message(state.id, &text))
                .await;
            sending.set(false);
            if saved.is_some() {
                editing.set(None);
            }
        });
    }

    // --------- Other actions ---------

    pub fn account(self) -> i64 {
        self.session().account_id()
    }

    pub fn set_source(self, source: bool) {
        write_account_setting(self.account(), |s| s.set_composer_source_mode(source));
        let mut rich = self.source;
        rich.set(source);
        self.core.write_unchecked().set_source(source);
        self.send_options();
        self.push(true, true, true);
        self.after_change(false);
    }

    pub fn set_enter_sends(self, value: bool) {
        let session = self.session();
        let overlays = self.overlays;
        self.spawn(async move {
            overlays
                .attempt(
                    "设置失败",
                    session.set_zulip_setting("enter_sends", json!(value)),
                )
                .await;
            self.send_options();
            self.refocus();
        });
    }

    pub fn create_snippet(self) {
        let initial = self.core.peek().selected_text();
        let overlays = self.overlays;
        self.spawn(async move {
            let content = if initial.is_empty() {
                overlays
                    .prompt_text(TextPrompt::new("新建常用语", "内容", "").max_len(10000))
                    .await
            } else {
                Some(initial)
            };
            let Some(content) = content else {
                self.refocus();
                return;
            };
            let Some(title) = overlays
                .prompt_text(TextPrompt::new("常用语名称", "名称", "").max_len(60))
                .await
            else {
                self.refocus();
                return;
            };
            let session = self.session();
            if overlays
                .attempt("新建常用语失败", session.create_snippet(&title, &content))
                .await
                .is_some()
            {
                overlays.toast("已保存");
            }
            self.refocus();
        });
    }

    pub fn delete_snippet(self, id: i64, title: String) {
        let overlays = self.overlays;
        let session = self.session();
        self.spawn(async move {
            overlays
                .delete(&format!("常用语「{title}」"), None, || {
                    session.delete(DeleteTarget::Snippet(id))
                })
                .await;
            self.refocus();
        });
    }

    pub fn insert_time(self) {
        self.spawn(async move {
            let Some(at) = ask_future_time(self.overlays, "全局时间", None).await else {
                self.refocus();
                return;
            };
            if let Some(time) = Local.timestamp_opt(at, 0).single() {
                self.edit(|core| {
                    core.edit_active(|text, _, s, e| {
                        format::insert_time(text, s, e, time.fixed_offset())
                    });
                });
            }
        });
    }

    pub fn insert_table(self) {
        self.spawn(async move {
            match dialogs::ask_table(self.overlays).await {
                Some((columns, rows)) => self.edit(|core| {
                    core.edit_active(|text, _, s, e| {
                        format::insert_table(text, s, e, columns, rows)
                    });
                }),
                None => self.refocus(),
            }
        });
    }

    pub fn create_poll(self) {
        self.spawn(async move {
            if let Some(text) = dialogs::ask_poll(self.overlays).await {
                self.send_text(text);
            }
            self.refocus();
        });
    }
}

/// The whole text is a URL.
fn is_url(text: &str) -> bool {
    (text.starts_with("http://") || text.starts_with("https://"))
        && !text.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_detected() {
        assert!(is_url("https://a.b/c"));
        assert!(!is_url("https://a.b c"));
        assert!(!is_url("a.b"));
    }
}
