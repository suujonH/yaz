//! Shared plugin UI: panel fields, the plugin section in profile cards and topic profiles, the
//! show_dialog dialog.

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

/// Plugin in component props, compared by pointer.
#[derive(Clone)]
pub struct PluginRef(pub Arc<NativePlugin>);

impl PartialEq for PluginRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Text that tells the user which plugin it is from: "{plugin name}: {text}"; just the plugin name
/// when the text is empty.
pub fn plugin_text(plugin: &str, text: &str) -> String {
    if text.is_empty() {
        plugin.to_owned()
    } else {
        format!("{plugin}：{text}")
    }
}

/// Where a panel field is placed; decides which kind of row it uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldLayout {
    /// Form: settings window, dialog.
    #[default]
    Form,
    /// User and topic cards.
    Profile,
    /// Chat settings panel.
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

/// Text in a panel: with `live`, submit on every input; otherwise submit on Enter or blur.
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

/// One item in a panel (select, bool, text, button, info). The value is stored by the caller:
/// changes go through `on_change`, clicks through `on_click`.
#[component]
pub fn PanelFieldView(
    field: PanelField,
    #[props(default)] layout: FieldLayout,
    /// Submit the text on every input (dialog); otherwise submit on Enter or blur.
    #[props(default)]
    live: bool,
    #[props(default)] autofocus: bool,
    /// Enter pressed inside the text.
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

/// One item in a panel: changes and clicks are handed to the plugin immediately, which replies with
/// new setting items.
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

/// Target of the plugin section in a user card.
pub fn user_target(session: &Session, user_id: i64) -> PluginTarget {
    PluginTarget::User {
        account_id: session.account_id(),
        user_id,
        name: session.display_name(user_id),
    }
}

/// Target of the plugin section in topic profiles; DMs have none.
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

/// One plugin's section: setting items are requested from the plugin only after it is opened; not
/// shown if no reply arrives or there are no setting items.
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

/// The sections of enabled plugins on this target. Loaded asynchronously, without delaying the card
/// or panel from opening.
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

/// Initial value of a field: button and info have none.
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

/// Show the field with the dialog's current value.
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

/// Show a dialog requested by a plugin. If the user cancels or closes it, the result's `ok` is
/// false; when `closed` completes (the host stops waiting), the dialog closes with it.
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
