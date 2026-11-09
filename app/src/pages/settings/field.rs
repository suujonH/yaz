//! Field bindings shared by the settings pages. Every setting uses [`field`] to get a [`Bind`] from
//! a [`Source`], which is then handed to a row component for display; reading, writing and failure
//! notices are all here; debug builds also have the developer-mode source label.

use super::Store;
use crate::services::{services, write_account_setting, write_setting};
use crate::shell::{ThemeConfig, ThemeContext};
use crate::state::AppState;
use crate::ui::*;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::rc::Rc;
use std::sync::Arc;
use zulip_session::Session;
use zulip_store::{AccountRepo, AccountRow, AccountSettings, LocalSettings};

/// Environment available when writing.
pub struct Env {
    pub overlays: Overlays,
    pub session: Option<Arc<Session>>,
    pub theme: ThemeContext,
    pub account: Option<i64>,
    rev: Signal<u64>,
}

impl Env {
    /// The account's login info.
    pub fn row(&self) -> Option<AccountRow> {
        AccountRepo::new(&services().db)
            .get(self.account?)
            .ok()
            .flatten()
    }

    /// The page re-reads the displayed value.
    pub fn refresh(&self) {
        let mut rev = self.rev;
        rev += 1;
    }
}

/// Scope of a local setting, shown as the source label in developer mode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Shared by all accounts.
    App,
    /// Belongs to this account only.
    Account,
}

#[cfg(debug_assertions)]
impl Scope {
    const fn text(self) -> &'static str {
        match self {
            Self::App => "YaZ 设置 · 全局",
            Self::Account => "YaZ 设置 · 账号",
        }
    }
}

/// Where a setting comes from; decides how it is read and written.
pub enum Source<T> {
    /// Zulip user settings (stored on the server); several setting names toggle together, and
    /// reading is true if any one is true.
    Zulip(&'static [&'static str]),
    /// Local setting shared by all accounts.
    App(fn(&LocalSettings) -> T, fn(&LocalSettings, T)),
    /// Local setting belonging to this account only.
    AccountLocal(fn(&AccountSettings) -> T, fn(&AccountSettings, T)),
    /// Theme (palette, font size).
    Theme(fn(&ThemeConfig) -> T, fn(&ThemeContext, T)),
    /// Local state stored elsewhere (autostart entry, account login properties); returns the reason
    /// when the write fails.
    Custom(
        // Compromise: the scope is only used for the debug-build source label; to avoid conditional
        // compilation at every use site, the field is kept in Release.
        #[cfg_attr(not(debug_assertions), allow(dead_code))] Scope,
        fn(&Env) -> T,
        fn(&Env, T) -> Result<(), String>,
    ),
}

/// The current value and write method bound to a source.
#[derive(Clone)]
pub struct Bind<T> {
    pub value: T,
    /// The source shown in developer mode.
    pub hint: Option<String>,
    write: Rc<dyn Fn(T)>,
}

impl<T: PartialEq> PartialEq for Bind<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.hint == other.hint
    }
}

impl<T: 'static> Bind<T> {
    /// A per-item binding within a list (topic mute level, etc.); use [`field`] for named settings.
    pub fn new(value: T, write: impl Fn(T) + 'static) -> Self {
        Self {
            value,
            hint: None,
            write: Rc::new(write),
        }
    }

    pub fn set(&self, value: T) {
        (self.write)(value);
    }

    /// Displays and writes a different representation of the value; nothing is written when `from`
    /// returns `None`.
    pub fn map<U>(
        self,
        to: impl FnOnce(&T) -> U,
        from: impl Fn(U) -> Option<T> + 'static,
    ) -> Bind<U> {
        let write = self.write;
        Bind {
            value: to(&self.value),
            hint: self.hint,
            write: Rc::new(move |value| {
                if let Some(value) = from(value) {
                    write(value);
                }
            }),
        }
    }
}

impl Bind<bool> {
    /// The switch means the opposite ("invisible mode" corresponds to `presence_enabled`).
    pub fn inverted(self) -> Self {
        self.map(|v| !v, |v| Some(!v))
    }
}

fn read_zulip<T: DeserializeOwned + Default>(session: &Session, names: &[&str]) -> T {
    let Ok(all) = serde_json::to_value(session.settings()) else {
        return T::default();
    };
    let values: Vec<&Value> = names.iter().filter_map(|name| all.get(name)).collect();
    let merged = if values.len() > 1 && values.iter().all(|v| v.is_boolean()) {
        Some(Value::Bool(
            values.iter().any(|v| v.as_bool() == Some(true)),
        ))
    } else {
        values.first().map(|v| (*v).clone())
    };
    merged
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

#[cfg(debug_assertions)]
fn tag_of<T>(source: &Source<T>) -> String {
    match source {
        Source::Zulip(names) => names.join(" "),
        Source::App(..) | Source::Theme(..) => Scope::App.text().to_owned(),
        Source::AccountLocal(..) => Scope::Account.text().to_owned(),
        Source::Custom(scope, ..) => scope.text().to_owned(),
    }
}

/// Binds one setting. Must be called while rendering a component of the settings window; the
/// component re-renders when the value changes.
pub fn field<T>(source: Source<T>) -> Bind<T>
where
    T: Clone + Default + PartialEq + Serialize + DeserializeOwned + 'static,
{
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let _ = consume_context::<AppState>().accounts.read();
    let env = Env {
        overlays: consume_context::<Overlays>(),
        session: store.session.clone(),
        theme: consume_context::<ThemeContext>(),
        account: store.account,
        rev: store.rev,
    };
    let local = &services().settings;
    let value = match &source {
        Source::Zulip(names) => env
            .session
            .as_ref()
            .map(|s| read_zulip(s, names))
            .unwrap_or_default(),
        Source::App(read, _) => read(local),
        Source::AccountLocal(read, _) => env
            .account
            .map(|id| read(&AccountSettings::new(&services().db, id)))
            .unwrap_or_default(),
        Source::Theme(read, _) => read(&env.theme.config()),
        Source::Custom(_, read, _) => read(&env),
    };
    #[cfg(debug_assertions)]
    let hint = local.show_setting_source().then(|| tag_of(&source));
    #[cfg(not(debug_assertions))]
    let hint = None;
    let write: Rc<dyn Fn(T)> = Rc::new(move |value: T| match &source {
        Source::Zulip(names) => {
            let names = *names;
            let Some(session) = env.session.clone() else {
                return;
            };
            let Ok(json) = serde_json::to_value(&value) else {
                return;
            };
            let overlays = env.overlays;
            spawn_forever(async move {
                for name in names {
                    overlays
                        .attempt("设置失败", session.set_zulip_setting(name, json.clone()))
                        .await;
                }
            });
        }
        Source::App(_, write) => write_setting(|s| write(s, value)),
        Source::AccountLocal(_, write) => {
            if let Some(id) = env.account {
                write_account_setting(id, |s| write(s, value));
            }
        }
        Source::Theme(_, write) => write(&env.theme, value),
        Source::Custom(_, _, write) => match write(&env, value) {
            Ok(()) => env.refresh(),
            Err(error) => env.overlays.report_error("设置失败", error),
        },
    });
    Bind { value, hint, write }
}

/// Settings page: title and content.
#[component]
pub fn SettingsPage(
    #[props(into)] title: String,
    /// Actions on the right of the title.
    actions: Option<Element>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "settings-page",
            div { class: "settings-title-row",
                div { class: "setting-title", "{title}" }
                {actions}
            }
            {children}
        }
    }
}

/// A group of settings: an optional small heading and a white card.
#[component]
pub fn Group(#[props(into)] title: Option<String>, children: Element) -> Element {
    rsx! {
        if let Some(title) = title {
            div { class: "setting-section", "{title}" }
        }
        SettingCard { {children} }
    }
}

#[component]
pub fn ToggleRow(
    #[props(into)] label: String,
    bind: Bind<bool>,
    #[props(default)] disabled: bool,
) -> Element {
    rsx! {
        SettingRow { label, description: bind.hint.clone(),
            Toggle { checked: bind.value, disabled, onchange: move |v| bind.set(v) }
        }
    }
}

/// Radio buttons when there are at most 3 options, otherwise a dropdown.
#[component]
pub fn ChoiceRow<T: Clone + PartialEq + 'static>(
    #[props(into)] label: String,
    bind: Bind<T>,
    options: Vec<(T, String)>,
    #[props(default)] disabled: bool,
    /// Description text; defaults to the source label.
    #[props(into)]
    description: Option<String>,
) -> Element {
    let description = description.or_else(|| bind.hint.clone());
    rsx! {
        SettingRow { label, description,
            if options.len() <= 3 {
                div { class: "settings-radios",
                    for (value, text) in options.iter().cloned() {
                        Checkbox {
                            key: "{text}",
                            radio: true,
                            disabled,
                            checked: bind.value == value,
                            onchange: {
                                let bind = bind.clone();
                                move |_| bind.set(value.clone())
                            },
                            "{text}"
                        }
                    }
                }
            } else {
                div { class: "settings-select",
                    Select {
                        disabled,
                        options: options
                            .iter()
                            .enumerate()
                            .map(|(i, (_, text))| SelectOption::new(i.to_string(), text.clone()))
                            .collect::<Vec<_>>(),
                        value: options
                            .iter()
                            .position(|(v, _)| *v == bind.value)
                            .map(|i| i.to_string())
                            .unwrap_or_default(),
                        onchange: {
                            let bind = bind.clone();
                            let options = options.clone();
                            move |v: String| {
                                if let Some((value, _)) = v.parse::<usize>().ok().and_then(|i| options.get(i)) {
                                    bind.set(value.clone());
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}

/// Draft of the text: typing only changes the draft; it is written on Enter or when focus is lost.
#[derive(Clone, Copy)]
pub struct Draft(Signal<Option<String>>);

pub fn use_draft() -> Draft {
    Draft(use_signal(|| None))
}

impl Draft {
    pub fn text(&self, bind: &Bind<String>) -> String {
        self.0.read().clone().unwrap_or_else(|| bind.value.clone())
    }

    pub fn input(&self, text: String) {
        let mut draft = self.0;
        draft.set(Some(text));
    }

    /// Written when the draft differs from the current value.
    pub fn commit(&self, bind: &Bind<String>) {
        let mut draft = self.0;
        if let Some(text) = draft.take()
            && text != bind.value
        {
            bind.set(text);
        }
    }
}

/// A piece of a text that contains variables: plain text, or a variable (index into the `tokens`
/// of [`TextRow`], text as written).
#[derive(Clone, Debug, PartialEq)]
enum Piece {
    Text(String),
    Token(usize, String),
}

/// Splits `value` into pieces; variables are matched ignoring ASCII case. The result always starts
/// and ends with text (possibly empty), and text and variables alternate.
fn split_tokens(value: &str, tokens: &[(String, String)]) -> Vec<Piece> {
    let mut pieces = vec![Piece::Text(String::new())];
    let mut rest = value;
    while let Some(c) = rest.chars().next() {
        let hit = tokens.iter().enumerate().find(|(_, (_, token))| {
            rest.len() >= token.len()
                && rest.as_bytes()[..token.len()].eq_ignore_ascii_case(token.as_bytes())
        });
        if let Some((index, (_, token))) = hit {
            pieces.push(Piece::Token(index, rest[..token.len()].to_owned()));
            pieces.push(Piece::Text(String::new()));
            rest = &rest[token.len()..];
        } else {
            if let Some(Piece::Text(text)) = pieces.last_mut() {
                text.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    pieces
}

fn join_pieces(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Text(text) | Piece::Token(_, text) => text.as_str(),
        })
        .collect()
}

/// Text box in which variables are shown as tags with a delete button in the top-right corner;
/// the text around them is edited in auto-sized inputs.
#[component]
fn TokenInput(
    value: String,
    placeholder: String,
    tokens: Vec<(String, String)>,
    oninput: EventHandler<String>,
    oncommit: EventHandler<()>,
) -> Element {
    let pieces = split_tokens(&value, &tokens);
    let single = pieces.len() == 1;
    rsx! {
        div { class: "input token-input",
            for (i, piece) in pieces.iter().cloned().enumerate() {
                match piece {
                    Piece::Text(text) => {
                        let (edited, removed) = (pieces.clone(), pieces.clone());
                        let empty = text.is_empty();
                        rsx! {
                            input {
                                key: "{i}",
                                class: "token-text",
                                r#type: "text",
                                value: "{text}",
                                placeholder: if single { placeholder.clone() } else { String::new() },
                                spellcheck: "false",
                                oninput: move |e| {
                                    let mut pieces = edited.clone();
                                    pieces[i] = Piece::Text(e.value());
                                    oninput.call(join_pieces(&pieces));
                                },
                                onkeydown: move |e| {
                                    if e.is_composing() {
                                        return;
                                    }
                                    if e.key() == Key::Enter {
                                        oncommit.call(());
                                    } else if e.key() == Key::Backspace && empty && i > 0 {
                                        // Backspace in an empty piece deletes the variable before it
                                        let mut pieces = removed.clone();
                                        pieces.remove(i - 1);
                                        oninput.call(join_pieces(&pieces));
                                        oncommit.call(());
                                    }
                                },
                                onblur: move |_| oncommit.call(()),
                            }
                        }
                    }
                    Piece::Token(index, _) => {
                        let label = tokens.get(index).map(|(label, _)| label.clone()).unwrap_or_default();
                        let rest = pieces.clone();
                        rsx! {
                            span { key: "{i}", class: "token-tag token-{index}",
                                "{label}"
                                button {
                                    class: "token-tag-remove",
                                    r#type: "button",
                                    title: "删除",
                                    onclick: move |_| {
                                        let mut pieces = rest.clone();
                                        pieces.remove(i);
                                        oninput.call(join_pieces(&pieces));
                                        oncommit.call(());
                                    },
                                    Icon { name: IconName::Close, size: 8.0 }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Single-line text; `tokens` are variable buttons that append to the end on click (button text,
/// inserted text). With tokens, the variables in the text are shown as deletable tags.
#[component]
pub fn TextRow(
    #[props(into)] label: String,
    bind: Bind<String>,
    #[props(into)] placeholder: String,
    #[props(default)] password: bool,
    #[props(default)] tokens: Vec<(String, String)>,
    /// Description text; defaults to the source label.
    #[props(into)]
    description: Option<String>,
) -> Element {
    let draft = use_draft();
    let description = description.or_else(|| bind.hint.clone());
    let shown = draft.text(&bind);
    let (for_blur, for_submit) = (bind.clone(), bind.clone());
    rsx! {
        SettingRow { label, description,
            div { class: "settings-text",
                if tokens.is_empty() {
                    TextInput {
                        value: shown.clone(),
                        placeholder,
                        password,
                        oninput: move |v| draft.input(v),
                        onblur: move |_| draft.commit(&for_blur),
                        onsubmit: move |_| draft.commit(&for_submit),
                    }
                } else {
                    TokenInput {
                        value: shown.clone(),
                        placeholder,
                        tokens: tokens.clone(),
                        oninput: move |v| draft.input(v),
                        oncommit: move |_| draft.commit(&for_blur),
                    }
                    div { class: "settings-tokens",
                        for (index, (text, insert)) in tokens.iter().cloned().enumerate() {
                            Button {
                                key: "{text}",
                                kind: ButtonKind::Flat,
                                class: "token-{index}",
                                onclick: {
                                    let bind = bind.clone();
                                    let shown = shown.clone();
                                    move |_| {
                                        draft.input(format!("{shown}{insert}"));
                                        draft.commit(&bind);
                                    }
                                },
                                "{text}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Integer slider, with the current value shown on the right.
#[component]
pub fn SliderRow(
    #[props(into)] label: String,
    bind: Bind<i32>,
    min: i32,
    max: i32,
    #[props(into)] unit: String,
) -> Element {
    let value = bind.value;
    rsx! {
        SettingRow { label, description: bind.hint.clone(),
            div { class: "settings-slider",
                Slider {
                    value: f64::from(value),
                    min: f64::from(min),
                    max: f64::from(max),
                    oninput: move |v: f64| bind.set(v.round() as i32),
                }
                span { class: "text-sub", "{value}{unit}" }
            }
        }
    }
}

/// A value that is copied when clicked.
#[component]
pub fn CopyText(text: String) -> Element {
    let overlays = use_overlays();
    rsx! {
        span {
            class: "text-sub settings-copy",
            title: "点击复制",
            onclick: move |_| overlays.copy_text(&text),
            "{text}"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> Vec<(String, String)> {
        vec![
            ("频道名".to_owned(), "{Channel}".to_owned()),
            ("话题名".to_owned(), "{Topic}".to_owned()),
        ]
    }

    #[test]
    fn splits_text_and_variables() {
        let text = |s: &str| Piece::Text(s.to_owned());
        assert_eq!(split_tokens("", &tokens()), vec![text("")]);
        assert_eq!(split_tokens("标题", &tokens()), vec![text("标题")]);
        assert_eq!(
            split_tokens("{channel} / {TOPIC}", &tokens()),
            vec![
                text(""),
                Piece::Token(0, "{channel}".to_owned()),
                text(" / "),
                Piece::Token(1, "{TOPIC}".to_owned()),
                text(""),
            ]
        );
        assert_eq!(
            split_tokens("{Topic}{Topic}", &tokens()),
            vec![
                text(""),
                Piece::Token(1, "{Topic}".to_owned()),
                text(""),
                Piece::Token(1, "{Topic}".to_owned()),
                text(""),
            ]
        );
    }

    #[test]
    fn join_restores_the_original_text() {
        for value in [
            "",
            "#{Channel}",
            "【{topic}】 {Unknown}",
            "{Channel",
            "话题{Topic}",
        ] {
            assert_eq!(join_pieces(&split_tokens(value, &tokens())), value);
        }
    }
}
