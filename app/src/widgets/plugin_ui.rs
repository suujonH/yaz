//! 插件的共用界面：面板字段、资料卡和话题资料中的插件分组、show_dialog 的对话框。

use crate::pages::settings::field::{Bind, use_draft};
use crate::services::plugins;
use crate::state::use_plugin_events;
use crate::ui::*;
use dioxus::prelude::*;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::sync::Arc;
use zulip_core::ConversationKey;
use zulip_plugin_host::{
    DialogKind, DialogResult, DialogSpec, NativePlugin, PanelField, PanelFieldValue, PanelRequest,
    PluginEvent, PluginTarget, parse_fields,
};
use zulip_session::Session;

/// 组件属性里的插件，按指针比较。
#[derive(Clone)]
pub struct PluginRef(pub Arc<NativePlugin>);

impl PartialEq for PluginRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// 让用户知道是哪个插件的文字：“{插件名}：{文字}”，文字为空时只有插件名。
pub fn plugin_text(plugin: &str, text: &str) -> String {
    if text.is_empty() {
        plugin.to_owned()
    } else {
        format!("{plugin}：{text}")
    }
}

/// 面板字段放在哪里，决定它用哪种行。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldLayout {
    /// 表单：设置窗口、对话框。
    #[default]
    Form,
    /// 用户、话题名片。
    Profile,
    /// 聊天设置面板。
    Setting,
}

fn field_row(layout: FieldLayout, label: String, control: Element) -> Element {
    match layout {
        FieldLayout::Form => rsx! {
            div { class: "settings-field",
                div { class: "text-sub", "{label}" }
                {control}
            }
        },
        FieldLayout::Profile => rsx! { ProfileField { label, {control} } },
        FieldLayout::Setting => rsx! { SettingRow { label, {control} } },
    }
}

/// 面板里的文字：`live` 时每次输入都提交，否则回车或失去焦点时提交。
#[component]
fn PanelText(
    #[props(into)] value: String,
    #[props(into)] placeholder: String,
    live: bool,
    autofocus: bool,
    on_commit: EventHandler<String>,
    on_submit: Option<EventHandler<()>>,
) -> Element {
    let draft = use_draft();
    let bind = Bind::new(value, move |v| on_commit.call(v));
    let (for_blur, for_submit) = (bind.clone(), bind.clone());
    rsx! {
        TextInput {
            value: draft.text(&bind),
            placeholder,
            autofocus,
            oninput: move |v: String| {
                if live {
                    on_commit.call(v);
                } else {
                    draft.input(v);
                }
            },
            onblur: move |_| draft.commit(&for_blur),
            onsubmit: move |_| {
                draft.commit(&for_submit);
                if let Some(submit) = on_submit {
                    submit.call(());
                }
            },
        }
    }
}

/// 面板中的一项（select、bool、text、button、info）。值由调用方保存：修改走 `on_change`，点击走 `on_click`。
#[component]
pub fn PanelFieldView(
    field: PanelField,
    #[props(default)] layout: FieldLayout,
    /// 文字每次输入都提交（对话框），否则回车或失去焦点时提交。
    #[props(default)]
    live: bool,
    #[props(default)] autofocus: bool,
    /// 在文字中按下回车。
    on_submit: Option<EventHandler<()>>,
    on_change: EventHandler<Value>,
    on_click: EventHandler<()>,
) -> Element {
    let label = field.label.clone();
    match field.value {
        PanelFieldValue::Bool(on) => match layout {
            FieldLayout::Form => rsx! {
                Checkbox { checked: on, onchange: move |v: bool| on_change.call(json!(v)), "{label}" }
            },
            _ => field_row(
                layout,
                label,
                rsx! { Toggle { checked: on, onchange: move |v: bool| on_change.call(json!(v)) } },
            ),
        },
        PanelFieldValue::Select { options, selected } => field_row(
            layout,
            label,
            rsx! {
                Select {
                    options: options
                        .iter()
                        .map(|o| SelectOption::new(o.value.clone(), o.label.clone()))
                        .collect::<Vec<_>>(),
                    value: selected,
                    onchange: move |v: String| on_change.call(json!(v)),
                }
            },
        ),
        PanelFieldValue::Text { value, placeholder } => field_row(
            layout,
            label,
            rsx! {
                PanelText {
                    value,
                    placeholder,
                    live,
                    autofocus,
                    on_commit: move |v: String| on_change.call(json!(v)),
                    on_submit,
                }
            },
        ),
        PanelFieldValue::Button => {
            let button = rsx! {
                Button { onclick: move |_| on_click.call(()), "{label}" }
            };
            match layout {
                FieldLayout::Form => button,
                _ => field_row(layout, String::new(), button),
            }
        }
        PanelFieldValue::Info(text) => field_row(
            layout,
            label,
            rsx! { div { class: "selectable", "{text}" } },
        ),
    }
}

/// 面板中的一项：修改、点击后立即交给插件，回复新的设置项。
#[component]
pub fn PluginPanelField(
    plugin: PluginRef,
    target: PluginTarget,
    field: PanelField,
    #[props(default)] layout: FieldLayout,
    on_changed: EventHandler<Vec<PanelField>>,
) -> Element {
    let overlays = use_overlays();
    let send = {
        let (plugin, target, key) = (plugin.0, target, field.key.clone());
        move |value: Option<Value>| {
            let (plugin, target, key) = (plugin.clone(), target.clone(), key.clone());
            spawn(async move {
                let request = match value {
                    Some(value) => PanelRequest::Set { target, key, value },
                    None => PanelRequest::Click { target, key },
                };
                match plugins().panel(&plugin, request).await {
                    Ok(Some(reply)) => on_changed.call(parse_fields(&reply)),
                    Ok(None) => {}
                    Err(error) => overlays.report_error("操作失败", error),
                }
            });
        }
    };
    let on_click = send.clone();
    rsx! {
        PanelFieldView {
            field,
            layout,
            on_change: move |value: Value| send(Some(value)),
            on_click: move |_| on_click(None),
        }
    }
}

/// 用户名片中插件分组的对象。
pub fn user_target(session: &Session, user_id: i64) -> PluginTarget {
    PluginTarget::User {
        account_id: session.account_id(),
        user_id,
        name: session.display_name(user_id),
    }
}

/// 话题资料中插件分组的对象；私聊没有。
pub fn topic_target(session: &Session, key: &ConversationKey) -> Option<PluginTarget> {
    let ConversationKey::Topic(topic) = key else {
        return None;
    };
    let channel = session
        .channel(topic.stream_id)
        .map_or_else(String::new, |c| c.name);
    let name = session.topic_name(key);
    (!name.is_empty()).then(|| PluginTarget::Topic {
        account_id: session.account_id(),
        stream_id: topic.stream_id,
        name: format!("{channel} / {name}"),
        topic: name,
    })
}

/// 一个插件的分组：打开后才向插件要设置项，没有等到回复或没有设置项时不显示。
#[component]
fn PluginPanelGroup(
    plugin: PluginRef,
    target: PluginTarget,
    label: String,
    layout: FieldLayout,
) -> Element {
    let mut fields = use_signal(Vec::<PanelField>::new);
    use_hook({
        let (plugin, target) = (plugin.0.clone(), target.clone());
        move || {
            spawn(async move {
                let request = PanelRequest::Get(target);
                if let Ok(Some(reply)) = plugins().panel(&plugin, request).await {
                    fields.set(parse_fields(&reply));
                }
            })
        }
    });
    if fields.read().is_empty() {
        return rsx! {};
    }
    let body = rsx! {
        for field in fields.read().iter().cloned() {
            PluginPanelField {
                key: "{field.key}",
                plugin: plugin.clone(),
                target: target.clone(),
                field,
                layout,
                on_changed: move |next: Vec<PanelField>| fields.set(next),
            }
        }
    };
    match layout {
        FieldLayout::Setting => rsx! {
            SettingCard {
                div { class: "panel-notice",
                    div { class: "panel-notice-title", "{label}" }
                }
                {body}
            }
        },
        _ => rsx! {
            div { class: "text-sub", "{label}" }
            {body}
        },
    }
}

/// 已开启的插件在这个对象上的分组。异步加载，不影响所在的名片或面板打开。
#[component]
pub fn PluginPanelGroups(target: PluginTarget, #[props(default)] layout: FieldLayout) -> Element {
    let mut rev = use_signal(|| 0u64);
    use_plugin_events(move |event| {
        if matches!(event, PluginEvent::PluginsChanged) {
            rev += 1;
        }
    });
    let _ = rev();
    let kind = target.kind();
    let target_key = target.to_json().to_string();
    let panels: Vec<(PluginRef, String)> = plugins()
        .plugins()
        .into_iter()
        .filter_map(|plugin| {
            let label = plugin
                .active_panels()
                .iter()
                .find(|panel| panel.kind == kind)?
                .label
                .clone();
            Some((PluginRef(plugin), label))
        })
        .collect();
    rsx! {
        for (plugin, label) in panels {
            PluginPanelGroup {
                key: "{plugin.0.id()}:{target_key}",
                plugin,
                target: target.clone(),
                label,
                layout,
            }
        }
    }
}

/// 字段的初始值：button、info 没有值。
fn initial_values(fields: &[PanelField]) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|field| {
            let value = match &field.value {
                PanelFieldValue::Bool(on) => json!(on),
                PanelFieldValue::Select { selected, .. } => json!(selected),
                PanelFieldValue::Text { value, .. } => json!(value),
                PanelFieldValue::Button | PanelFieldValue::Info(_) => return None,
            };
            Some((field.key.clone(), value))
        })
        .collect()
}

/// 用对话框当前的值显示字段。
fn field_with_value(field: &PanelField, values: &Map<String, Value>) -> PanelField {
    let mut shown = field.clone();
    let current = values.get(&field.key);
    match &mut shown.value {
        PanelFieldValue::Bool(on) => {
            if let Some(value) = current.and_then(Value::as_bool) {
                *on = value;
            }
        }
        PanelFieldValue::Select { selected, .. } => {
            if let Some(value) = current.and_then(Value::as_str) {
                value.clone_into(selected);
            }
        }
        PanelFieldValue::Text { value, .. } => {
            if let Some(text) = current.and_then(Value::as_str) {
                text.clone_into(value);
            }
        }
        PanelFieldValue::Button | PanelFieldValue::Info(_) => {}
    }
    shown
}

#[component]
fn PluginDialog(title: String, spec: DialogSpec, answer: Answer<DialogResult>) -> Element {
    let mut values = use_signal(|| initial_values(&spec.fields));
    let input = spec.kind == DialogKind::Input;
    let (ok, enter, cancel, close) = (answer.clone(), answer.clone(), answer.clone(), answer);
    let confirm = move |answer: &Answer<DialogResult>| {
        answer.send(DialogResult {
            ok: true,
            values: values.peek().clone(),
        });
    };
    rsx! {
        DialogFrame {
            title,
            width: 420.0,
            on_close: move |_| close.cancel(),
            footer: rsx! {
                DialogButtons {
                    on_confirm: move |_| confirm(&ok),
                    on_cancel: move |_| cancel.cancel(),
                }
            },
            if !spec.text.is_empty() {
                div { class: "confirm-message", "{spec.text}" }
            }
            for field in spec.fields.iter() {
                {
                    let (changed_key, clicked_key) = (field.key.clone(), field.key.clone());
                    let enter = enter.clone();
                    rsx! {
                        PanelFieldView {
                            key: "{field.key}",
                            field: field_with_value(field, &values.read()),
                            live: true,
                            autofocus: input,
                            on_submit: move |_| {
                                if input {
                                    confirm(&enter);
                                }
                            },
                            on_change: move |value: Value| {
                                values.write().insert(changed_key.clone(), value);
                            },
                            on_click: move |_| {
                                values.write().insert(clicked_key.clone(), json!(true));
                            },
                        }
                    }
                }
            }
        }
    }
}

/// 显示插件请求的对话框。用户取消或关闭时结果的 `ok` 为 false；`closed` 完成（宿主不再等待）时
/// 对话框随之关闭。
pub async fn ask_plugin_dialog(
    overlays: Overlays,
    plugin_name: &str,
    spec: DialogSpec,
    closed: impl Future<Output = ()>,
) -> DialogResult {
    let title = plugin_text(plugin_name, &spec.title);
    overlays
        .ask_dialog_until(closed, move |answer| {
            rsx! { PluginDialog { title: title.clone(), spec: spec.clone(), answer } }
        })
        .await
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zulip_plugin_host::PanelOption;

    fn field(key: &str, value: PanelFieldValue) -> PanelField {
        PanelField {
            key: key.to_owned(),
            label: key.to_owned(),
            value,
        }
    }

    fn fields() -> Vec<PanelField> {
        vec![
            field("on", PanelFieldValue::Bool(true)),
            field(
                "pick",
                PanelFieldValue::Select {
                    options: vec![PanelOption {
                        value: "a".to_owned(),
                        label: "A".to_owned(),
                    }],
                    selected: "a".to_owned(),
                },
            ),
            field(
                "text",
                PanelFieldValue::Text {
                    value: "原文".to_owned(),
                    placeholder: String::new(),
                },
            ),
            field("run", PanelFieldValue::Button),
            field("note", PanelFieldValue::Info("说明".to_owned())),
        ]
    }

    #[test]
    fn plugin_text_names_the_plugin() {
        assert_eq!(plugin_text("回声", "你好"), "回声：你好");
        assert_eq!(plugin_text("回声", ""), "回声");
    }

    #[test]
    fn only_editable_fields_have_initial_values() {
        let values = initial_values(&fields());
        assert_eq!(
            Value::Object(values),
            json!({"on": true, "pick": "a", "text": "原文"})
        );
    }

    #[test]
    fn fields_show_the_current_values() {
        let fields = fields();
        let mut values = initial_values(&fields);
        values.insert("on".to_owned(), json!(false));
        values.insert("pick".to_owned(), json!("b"));
        values.insert("text".to_owned(), json!("新"));
        assert_eq!(
            field_with_value(&fields[0], &values).value,
            PanelFieldValue::Bool(false)
        );
        let PanelFieldValue::Select { selected, .. } = field_with_value(&fields[1], &values).value
        else {
            panic!("应为下拉框");
        };
        assert_eq!(selected, "b");
        let PanelFieldValue::Text { value, .. } = field_with_value(&fields[2], &values).value
        else {
            panic!("应为文字");
        };
        assert_eq!(value, "新");
        assert_eq!(
            field_with_value(&fields[3], &values).value,
            PanelFieldValue::Button
        );
    }
}
