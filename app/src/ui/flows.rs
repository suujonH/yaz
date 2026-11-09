//! Unified flows: execute after confirmation, text input, unsaved-change confirmation, error
//! notices, copy. The wording and steps of each kind of operation are defined only here.

use super::controls::{Button, ButtonKind, Checkbox};
use super::input::TextInput;
use super::overlay::{DialogFrame, DialogHandle, Overlays, ToastKind};
use crate::platform;
use dioxus::prelude::*;
use std::cell::Cell;
use std::fmt::Display;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use tokio::sync::oneshot;

/// Reply channel of a dialog. Esc, clicking the backdrop and closing the window all count as
/// cancel.
pub struct Answer<T: 'static> {
    tx: Rc<Cell<Option<oneshot::Sender<T>>>>,
    handle: DialogHandle,
}

impl<T> Clone for Answer<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            handle: self.handle,
        }
    }
}

impl<T> PartialEq for Answer<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.tx, &other.tx)
    }
}

impl<T> Answer<T> {
    pub fn send(&self, value: T) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(value);
        }
        self.handle.close();
    }

    pub fn cancel(&self) {
        self.handle.close();
    }
}

/// Confirmation box (no title, body plus OK and Cancel).
#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    message: String,
    confirm_text: String,
    cancel_text: String,
    danger: bool,
    check_text: Option<String>,
}

impl Confirm {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            confirm_text: "确定".into(),
            cancel_text: "取消".into(),
            danger: false,
            check_text: None,
        }
    }

    /// Wording shared by all deletions: "Delete {target}?", with `detail` explaining the
    /// consequences.
    pub fn delete(target: &str, detail: Option<&str>) -> Self {
        let message = match detail {
            Some(detail) => format!("删除{target}？{detail}"),
            None => format!("删除{target}？"),
        };
        Self::new(message).danger("删除")
    }

    pub fn confirm_text(mut self, text: impl Into<String>) -> Self {
        self.confirm_text = text.into();
        self
    }

    pub fn cancel_text(mut self, text: impl Into<String>) -> Self {
        self.cancel_text = text.into();
        self
    }

    /// Irreversible operation: the OK button is red.
    pub fn danger(mut self, confirm_text: impl Into<String>) -> Self {
        self.confirm_text = confirm_text.into();
        self.danger = true;
        self
    }

    /// Checkbox below the body ("also delete all data", etc.); get the result with
    /// [`Overlays::ask`].
    pub fn check(mut self, text: impl Into<String>) -> Self {
        self.check_text = Some(text.into());
        self
    }
}

/// Text input dialog: shared by rename, new folder and note.
#[derive(Clone, Debug, PartialEq)]
pub struct TextPrompt {
    title: String,
    placeholder: String,
    initial: String,
    max_len: usize,
    allow_empty: bool,
    require_change: bool,
    hint: Option<String>,
}

impl TextPrompt {
    /// OK is disabled when the text is empty after trimming whitespace or equals the original
    /// value.
    pub fn new(
        title: impl Into<String>,
        placeholder: impl Into<String>,
        initial: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            placeholder: placeholder.into(),
            initial: initial.into(),
            max_len: 60,
            allow_empty: false,
            require_change: true,
            hint: None,
        }
    }

    /// Note: may be left empty (clears the note), with the original name shown as a hint below.
    pub fn remark(title: impl Into<String>, original_name: &str, remark: Option<&str>) -> Self {
        Self {
            allow_empty: true,
            require_change: false,
            hint: Some(format!("原名称：{original_name}")),
            ..Self::new(title, "备注", remark.unwrap_or_default())
        }
    }

    pub fn max_len(mut self, max_len: usize) -> Self {
        self.max_len = max_len;
        self
    }

    /// Returns the text trimmed of leading and trailing whitespace when OK is possible.
    fn accept(&self, text: &str) -> Option<String> {
        let text = text.trim();
        let valid = (self.allow_empty || !text.is_empty())
            && text.chars().count() <= self.max_len
            && (!self.require_change || text != self.initial.trim());
        valid.then(|| text.to_string())
    }
}

/// Text of a failure notice: `{operation} failed: {reason}`; with no reason, only the first half.
fn error_text(fail_text: &str, error: &str) -> String {
    if error.is_empty() {
        fail_text.to_string()
    } else {
        format!("{fail_text}：{error}")
    }
}

impl Overlays {
    /// Opens a dialog and waits for the reply; `None` on cancel.
    pub async fn ask_dialog<T: 'static>(
        self,
        content: impl Fn(Answer<T>) -> Element + 'static,
    ) -> Option<T> {
        self.ask_dialog_until(std::future::pending(), content).await
    }

    /// Same as [`ask_dialog`](Self::ask_dialog); when `until` completes first the dialog is closed
    /// and the result is `None`.
    pub async fn ask_dialog_until<T: 'static>(
        self,
        until: impl Future<Output = ()>,
        content: impl Fn(Answer<T>) -> Element + 'static,
    ) -> Option<T> {
        let (tx, rx) = oneshot::channel::<T>();
        let tx = Rc::new(Cell::new(Some(tx)));
        let dialog = self.open_dialog(true, move |handle| {
            content(Answer {
                tx: tx.clone(),
                handle,
            })
        });
        tokio::select! {
            answer = rx => answer.ok(),
            () = until => {
                dialog.close();
                None
            }
        }
    }

    /// Confirmation box; on OK returns the checkbox state, `None` on cancel.
    pub async fn ask(self, confirm: Confirm) -> Option<bool> {
        self.ask_dialog(move |answer| {
            rsx! { ConfirmDialog { confirm: confirm.clone(), answer } }
        })
        .await
    }

    pub async fn confirm(self, confirm: Confirm) -> bool {
        self.ask(confirm).await.is_some()
    }

    /// The single entry point for error notices. `fail_text` looks like "Delete failed".
    pub fn report_error(self, fail_text: &str, error: impl Display) {
        log::warn!("{fail_text}：{error}");
        self.toast_kind(ToastKind::Error, error_text(fail_text, &error.to_string()));
    }

    /// Runs the operation and shows a notice on failure.
    pub async fn attempt<T, E: Display>(
        self,
        fail_text: &str,
        action: impl Future<Output = Result<T, E>>,
    ) -> Option<T> {
        match action.await {
            Ok(value) => Some(value),
            Err(error) => {
                self.report_error(fail_text, error);
                None
            }
        }
    }

    /// Confirm -> run -> failure notice. Returns the result on success for the caller to refresh.
    pub async fn confirm_then<T, E: Display, F: Future<Output = Result<T, E>>>(
        self,
        confirm: Confirm,
        fail_text: &str,
        action: impl FnOnce() -> F,
    ) -> Option<T> {
        if !self.confirm(confirm).await {
            return None;
        }
        self.attempt(fail_text, action()).await
    }

    /// Deleting any object goes through here. `target` looks like 'topic "xx"', and `detail`
    /// explains the consequences.
    pub async fn delete<T, E: Display, F: Future<Output = Result<T, E>>>(
        self,
        target: &str,
        detail: Option<&str>,
        action: impl FnOnce() -> F,
    ) -> Option<T> {
        self.confirm_then(Confirm::delete(target, detail), "删除失败", action)
            .await
    }

    /// Unsaved-change confirmation: true when it is OK to leave.
    pub async fn confirm_discard(self) -> bool {
        self.confirm(
            Confirm::new("有未保存的内容。")
                .danger("放弃变更")
                .cancel_text("留在本页"),
        )
        .await
    }

    /// Text input dialog; on OK returns the text trimmed of leading and trailing whitespace.
    pub async fn prompt_text(self, prompt: TextPrompt) -> Option<String> {
        self.ask_dialog(move |answer| {
            rsx! { TextPromptDialog { prompt: prompt.clone(), answer } }
        })
        .await
    }

    /// Opens a file, showing a notice on failure.
    pub fn open_file(self, path: &Path) {
        if let Err(error) = platform::open_path(path) {
            self.report_error("打开文件失败", error);
        }
    }

    /// Opens a directory, creating it first if it does not exist; shows a notice on failure.
    pub fn open_dir(self, dir: &Path) {
        match std::fs::create_dir_all(dir) {
            Ok(()) => self.open_file(dir),
            Err(error) => self.report_error("打开文件失败", error),
        }
    }

    /// Save as: choose a location -> write -> show the result. `None` when the selection is
    /// cancelled.
    pub async fn save_as<E: Display, F: Future<Output = Result<(), E>>>(
        self,
        default_name: &str,
        write: impl FnOnce(PathBuf) -> F,
    ) -> Option<PathBuf> {
        let path = platform::save_file("另存为", default_name, &[]).await?;
        self.attempt("保存失败", write(path.clone())).await?;
        self.toast("已保存");
        Some(path)
    }

    /// Copies text and shows a notice.
    pub fn copy_text(self, text: &str) {
        match platform::copy_text(text) {
            Ok(()) => self.toast("已复制"),
            Err(error) => self.report_error("复制失败", error),
        }
    }
}

/// Buttons at the bottom of a dialog: OK on the left, Cancel on the right.
#[component]
pub fn DialogButtons(
    #[props(default = "确定".to_string(), into)] confirm_text: String,
    #[props(default = "取消".to_string(), into)] cancel_text: String,
    #[props(default)] danger: bool,
    #[props(default)] disabled: bool,
    on_confirm: EventHandler<()>,
    on_cancel: EventHandler<()>,
) -> Element {
    rsx! {
        Button {
            kind: if danger { ButtonKind::Danger } else { ButtonKind::Primary },
            disabled,
            onclick: move |_| on_confirm.call(()),
            "{confirm_text}"
        }
        Button { onclick: move |_| on_cancel.call(()), "{cancel_text}" }
    }
}

#[component]
fn ConfirmDialog(confirm: Confirm, answer: Answer<bool>) -> Element {
    let mut checked = use_signal(|| false);
    let (ok, cancel) = (answer.clone(), answer);
    let close = cancel.clone();
    rsx! {
        DialogFrame {
            width: 380.0,
            on_close: move |_| close.cancel(),
            footer: rsx! {
                DialogButtons {
                    confirm_text: confirm.confirm_text.clone(),
                    cancel_text: confirm.cancel_text.clone(),
                    danger: confirm.danger,
                    on_confirm: move |_| ok.send(checked()),
                    on_cancel: move |_| cancel.cancel(),
                }
            },
            div { class: "confirm-message", "{confirm.message}" }
            if let Some(text) = confirm.check_text.clone() {
                div { style: "margin-top:10px",
                    Checkbox { checked: checked(), onchange: move |v| checked.set(v), "{text}" }
                }
            }
        }
    }
}

#[component]
fn TextPromptDialog(prompt: TextPrompt, answer: Answer<String>) -> Element {
    let mut text = use_signal(|| prompt.initial.clone());
    let accepted = prompt.accept(&text());
    let (ok, enter, cancel, close) = (answer.clone(), answer.clone(), answer.clone(), answer);
    let for_enter = prompt.clone();
    let for_button = accepted.clone();
    rsx! {
        DialogFrame {
            title: prompt.title.clone(),
            width: 320.0,
            on_close: move |_| close.cancel(),
            footer: rsx! {
                DialogButtons {
                    disabled: accepted.is_none(),
                    on_confirm: move |_| {
                        if let Some(value) = for_button.clone() {
                            ok.send(value);
                        }
                    },
                    on_cancel: move |_| cancel.cancel(),
                }
            },
            TextInput {
                value: text(),
                placeholder: prompt.placeholder.clone(),
                autofocus: true,
                clearable: true,
                oninput: move |v| text.set(v),
                onsubmit: move |v: String| {
                    if let Some(value) = for_enter.accept(&v) {
                        enter.send(value);
                    }
                },
            }
            if let Some(hint) = prompt.hint.clone() {
                div { class: "text-sub", style: "margin-top:6px", "{hint}" }
            }
        }
    }
}

/// Save / undo hint centered at the bottom when a manually-saved page has unsaved changes.
#[component]
pub fn UnsavedBar(
    visible: bool,
    on_save: EventHandler<()>,
    on_revert: EventHandler<()>,
) -> Element {
    rsx! {
        if visible {
            div { class: "unsaved-bar",
                span { "有未保存的修改" }
                Button { kind: ButtonKind::Primary, onclick: move |_| on_save.call(()), "保存" }
                Button { onclick: move |_| on_revert.call(()), "撤销" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_rejects_empty_and_unchanged() {
        let p = TextPrompt::new("重命名", "名称", "旧名");
        assert_eq!(p.accept("  "), None);
        assert_eq!(p.accept(" 旧名 "), None);
        assert_eq!(p.accept(" 新名 "), Some("新名".into()));
    }

    #[test]
    fn prompt_limits_length_in_characters() {
        let p = TextPrompt::new("t", "p", "").max_len(2);
        assert_eq!(p.accept("中文"), Some("中文".into()));
        assert_eq!(p.accept("中文字"), None);
    }

    #[test]
    fn remark_may_be_empty_or_unchanged() {
        let p = TextPrompt::remark("备注", "张三", Some("老张"));
        assert_eq!(p.accept("  "), Some(String::new()));
        assert_eq!(p.accept("老张"), Some("老张".into()));
        assert_eq!(p.hint.as_deref(), Some("原名称：张三"));
    }

    #[test]
    fn delete_uses_one_wording() {
        let c = Confirm::delete("话题「a」", Some("消息将被永久删除。"));
        assert_eq!(c.message, "删除话题「a」？消息将被永久删除。");
        assert_eq!(c.confirm_text, "删除");
        assert!(c.danger);
    }

    #[test]
    fn error_text_includes_reason_when_present() {
        assert_eq!(error_text("退订失败", ""), "退订失败");
        assert_eq!(error_text("退订失败", "无权限"), "退订失败：无权限");
    }
}
