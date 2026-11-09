//! 设置页共用的字段绑定。所有设置项都用 [`field`] 从一个 [`Source`] 得到 [`Bind`]，再交给行组件显示；
//! 读取、写入、失败提示都在这里；调试版还有开发者模式的来源标签。

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

/// 写入时可用的环境。
pub struct Env {
    pub overlays: Overlays,
    pub session: Option<Arc<Session>>,
    pub theme: ThemeContext,
    pub account: Option<i64>,
    rev: Signal<u64>,
}

impl Env {
    /// 账号的登录信息。
    pub fn row(&self) -> Option<AccountRow> {
        AccountRepo::new(&services().db)
            .get(self.account?)
            .ok()
            .flatten()
    }

    /// 页面重新读取显示的值。
    pub fn refresh(&self) {
        let mut rev = self.rev;
        rev += 1;
    }
}

/// 本机设置的范围，开发者模式下作为来源标签显示。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 所有账号共用。
    App,
    /// 只属于这个账号。
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

/// 一个设置项的来源，决定怎么读、怎么写。
pub enum Source<T> {
    /// Zulip 用户设置（保存在服务器）；多个设置名一起开关，读取时任一为真即为真。
    Zulip(&'static [&'static str]),
    /// 本机设置，所有账号共用。
    App(fn(&LocalSettings) -> T, fn(&LocalSettings, T)),
    /// 本机设置，只属于这个账号。
    AccountLocal(fn(&AccountSettings) -> T, fn(&AccountSettings, T)),
    /// 主题（调色盘、字号）。
    Theme(fn(&ThemeConfig) -> T, fn(&ThemeContext, T)),
    /// 保存在其它地方的本机状态（开机启动项、账号的登录属性）；写入失败时返回原因。
    Custom(
        // 妥协：范围只用于调试版的来源标签；为了不在每个使用处写条件编译，Release 中保留这个字段。
        #[cfg_attr(not(debug_assertions), allow(dead_code))] Scope,
        fn(&Env) -> T,
        fn(&Env, T) -> Result<(), String>,
    ),
}

/// 绑定到来源的当前值与写入方式。
#[derive(Clone)]
pub struct Bind<T> {
    pub value: T,
    /// 开发者模式下显示的来源。
    pub hint: Option<String>,
    write: Rc<dyn Fn(T)>,
}

impl<T: PartialEq> PartialEq for Bind<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.hint == other.hint
    }
}

impl<T: 'static> Bind<T> {
    /// 列表中每一项各自的绑定（话题的免打扰级别等）；有名字的设置项用 [`field`]。
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

    /// 换一种值来显示和写入；`from` 返回 `None` 时不写入。
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
    /// 开关的含义相反（“隐身”对应 `presence_enabled`）。
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

/// 绑定一个设置项。必须在设置窗口的组件渲染中调用；值变化时组件会重新渲染。
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

/// 设置页：标题和内容。
#[component]
pub fn SettingsPage(
    #[props(into)] title: String,
    /// 标题右侧的操作。
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

/// 一组设置项：可选的小标题和白色卡片。
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

/// 选项不超过 3 个时是单选框，否则是下拉框。
#[component]
pub fn ChoiceRow<T: Clone + PartialEq + 'static>(
    #[props(into)] label: String,
    bind: Bind<T>,
    options: Vec<(T, String)>,
    #[props(default)] disabled: bool,
    /// 说明文字，缺省时显示来源标签。
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

/// 文字的草稿：输入时只改草稿，回车或失去焦点时才写入。
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

    /// 草稿与当前值不同时写入。
    pub fn commit(&self, bind: &Bind<String>) {
        let mut draft = self.0;
        if let Some(text) = draft.take()
            && text != bind.value
        {
            bind.set(text);
        }
    }
}

/// 单行文字；`tokens` 是点击后追加到末尾的变量按钮（按钮文字、插入的文字）。
#[component]
pub fn TextRow(
    #[props(into)] label: String,
    bind: Bind<String>,
    #[props(into)] placeholder: String,
    #[props(default)] password: bool,
    #[props(default)] tokens: Vec<(String, String)>,
    /// 说明文字，缺省时显示来源标签。
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
                TextInput {
                    value: shown.clone(),
                    placeholder,
                    password,
                    oninput: move |v| draft.input(v),
                    onblur: move |_| draft.commit(&for_blur),
                    onsubmit: move |_| draft.commit(&for_submit),
                }
                if !tokens.is_empty() {
                    div { class: "settings-tokens",
                        for (text, insert) in tokens.iter().cloned() {
                            Button {
                                key: "{text}",
                                kind: ButtonKind::Flat,
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

/// 整数滑块，右侧显示当前值。
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
