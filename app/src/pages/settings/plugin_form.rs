//! 插件的设置：按 plugin.json 的 settings 生成的表单（点“保存”才提交），以及 panels 的列表（修改立即由插件保存）。

use crate::services::plugins;
use crate::state::use_plugin_events;
use crate::ui::*;
use crate::widgets::{PluginPanelField, PluginRef};
use dioxus::prelude::*;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use zulip_plugin_host::{
    NativePlugin, PanelEntry, PanelField, PanelRequest, PluginEvent, PluginSettingField,
    PluginSettingType, PluginTarget, PluginTargetKind, ResourceSelection, SettingsButtonClick,
    parse_entries, parse_fields,
};

/// 列表行在值里的编号：本次运行内唯一，插件回复设置窗口时据此定位这一行；保存时被宿主丢弃。
const ROW_ID: &str = "_row";

/// 值树里的一层：设置项所在的对象（顶层，或列表的一行）。
#[derive(Clone, Debug, PartialEq)]
enum Seg {
    Key(String),
    Row(i64),
}

fn object_at<'a>(root: &'a Value, scope: &[Seg]) -> Option<&'a Value> {
    scope.iter().try_fold(root, |current, seg| match seg {
        Seg::Key(key) => current.get(key),
        Seg::Row(id) => current.as_array()?.iter().find(|row| row[ROW_ID] == *id),
    })
}

fn object_at_mut<'a>(root: &'a mut Value, scope: &[Seg]) -> Option<&'a mut Value> {
    scope.iter().try_fold(root, |current, seg| match seg {
        Seg::Key(key) => current.get_mut(key),
        Seg::Row(id) => current
            .as_array_mut()?
            .iter_mut()
            .find(|row| row[ROW_ID] == *id),
    })
}

/// 结果文字、候选等按“行号:键”记录。
fn result_key(scope: &[Seg], key: &str) -> String {
    let row = scope.iter().find_map(|seg| match seg {
        Seg::Row(id) => Some(id.to_string()),
        Seg::Key(_) => None,
    });
    format!("{}:{key}", row.unwrap_or_default())
}

#[derive(Clone, Debug, PartialEq)]
enum ItemState {
    Running,
    Done(bool, String),
}

/// 表单的状态。
#[derive(Clone)]
struct Form {
    plugin: Arc<NativePlugin>,
    values: Signal<Value>,
    /// 按钮、候选获取的结果文字。
    results: Signal<HashMap<String, String>>,
    /// tags 的候选：版本号变化时打开候选列表。
    candidates: Signal<HashMap<String, (u64, Vec<String>)>>,
    /// tags 某一项的按钮状态。
    items: Signal<HashMap<String, ItemState>>,
    next_row: Signal<i64>,
}

impl Form {
    fn text(&self, scope: &[Seg], key: &str) -> String {
        object_at(&self.values.read(), scope)
            .and_then(|o| o[key].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    fn value(&self, scope: &[Seg], key: &str) -> Value {
        object_at(&self.values.read(), scope).map_or(Value::Null, |o| o[key].clone())
    }

    fn set(&self, scope: &[Seg], key: &str, value: Value) {
        let mut values = self.values;
        if let Some(object) = object_at_mut(&mut values.write(), scope) {
            object[key] = value;
        }
    }

    fn object(&self, scope: &[Seg]) -> Map<String, Value> {
        object_at(&self.values.read(), scope)
            .and_then(|o| o.as_object().cloned())
            .unwrap_or_default()
    }

    fn tags(&self, scope: &[Seg], key: &str) -> Vec<String> {
        self.value(scope, key)
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn add_tag(&self, scope: &[Seg], key: &str, tag: &str) {
        let tag = tag.trim();
        let mut tags = self.tags(scope, key);
        if !tag.is_empty() && !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_owned());
            self.set(scope, key, json!(tags));
        }
    }

    /// 点击按钮（或 tags 某一项旁边的按钮）：以表单当前的值调用插件，回复经设置反馈显示。
    fn run_button(&self, scope: &[Seg], key: &str, item: Option<&str>) {
        let row = scope.iter().find_map(|seg| match seg {
            Seg::Row(id) => Some(*id),
            Seg::Key(_) => None,
        });
        let click = SettingsButtonClick {
            key: key.to_owned(),
            row,
            item: item.map(str::to_owned),
            values: self.object(scope),
            settings: self.object(&[]),
        };
        let invoked = plugins().invoke_settings_button(&self.plugin, &click);
        let rk = result_key(scope, key);
        match item {
            None => {
                let mut results = self.results;
                let text = if invoked {
                    "正在执行…"
                } else {
                    "请先开启插件"
                };
                results.write().insert(rk, text.to_owned());
            }
            Some(item) => {
                let mut items = self.items;
                let state = if invoked {
                    ItemState::Running
                } else {
                    ItemState::Done(false, "请先开启插件".to_owned())
                };
                items.write().insert(format!("{rk}:{item}"), state);
            }
        }
    }
}

/// 值树里的初始值：补全默认值，给列表行编号。
fn initial_values(plugin: &NativePlugin, next_row: &mut i64) -> Value {
    let mut values = Value::Object(plugins().load_settings(plugin));
    if let Some(manifest) = plugin.manifest() {
        number_rows(&manifest.settings, &mut values, next_row);
    }
    values
}

fn number_rows(fields: &[PluginSettingField], values: &mut Value, next_row: &mut i64) {
    for field in fields.iter().filter(|f| f.kind == PluginSettingType::List) {
        if let Some(rows) = values.get_mut(&field.key).and_then(Value::as_array_mut) {
            for row in rows {
                *next_row += 1;
                row[ROW_ID] = json!(*next_row);
            }
        }
    }
}

#[component]
fn FieldView(field: PluginSettingField, scope: Vec<Seg>) -> Element {
    let form = use_context::<Form>();
    if !field.is_visible(&Value::Object(form.object(&scope))) {
        return rsx! {};
    }
    let rk = result_key(&scope, &field.key);
    let result = form.results.read().get(&rk).cloned().unwrap_or_default();
    let shows_label = !matches!(
        field.kind,
        PluginSettingType::Bool | PluginSettingType::Button
    );
    rsx! {
        div { class: "settings-field",
            if shows_label {
                div { class: "text-sub", "{field.label}" }
            }
            FieldControl { field: field.clone(), scope: scope.clone() }
            if !result.is_empty() {
                div { class: "text-sub selectable", "{result}" }
            }
            if !field.hint.is_empty() {
                div { class: "text-sub", "{field.hint}" }
            }
        }
    }
}

#[component]
fn FieldControl(field: PluginSettingField, scope: Vec<Seg>) -> Element {
    let form = use_context::<Form>();
    let key = field.key.clone();
    match field.kind {
        PluginSettingType::Text | PluginSettingType::Password | PluginSettingType::Multiline => {
            let multiline = field.kind == PluginSettingType::Multiline;
            rsx! {
                TextInput {
                    value: form.text(&scope, &key),
                    placeholder: field.placeholder.clone(),
                    password: field.kind == PluginSettingType::Password,
                    multiline,
                    rows: 4u32,
                    oninput: move |v: String| form.set(&scope, &key, json!(v)),
                }
            }
        }
        PluginSettingType::Select => rsx! {
            Select {
                options: field
                    .options
                    .iter()
                    .map(|o| SelectOption::new(o.value.clone(), o.label.clone()))
                    .collect::<Vec<_>>(),
                value: form.text(&scope, &key),
                onchange: move |v: String| form.set(&scope, &key, json!(v)),
            }
        },
        PluginSettingType::Bool => {
            let checked = form.value(&scope, &key).as_bool().unwrap_or(false);
            rsx! {
                Checkbox {
                    checked,
                    onchange: move |v| form.set(&scope, &key, json!(v)),
                    "{field.label}"
                }
            }
        }
        PluginSettingType::Button => rsx! {
            Button {
                onclick: move |_| form.run_button(&scope, &key, None),
                "{field.label}"
            }
        },
        PluginSettingType::Resource => rsx! { ResourceField { field: field.clone(), scope } },
        PluginSettingType::Tags => rsx! { TagsField { field: field.clone(), scope } },
        PluginSettingType::List => rsx! { ListField { field: field.clone(), scope } },
    }
}

/// 选择其他插件发布的资源，第一项是“不使用”。
#[component]
fn ResourceField(field: PluginSettingField, scope: Vec<Seg>) -> Element {
    let form = use_context::<Form>();
    let _ = plugins_changed();
    let saved = ResourceSelection::from_json(&form.value(&scope, &field.key));
    let options = plugins().resource_options(&field, saved.as_ref());
    let current = options
        .iter()
        .position(|o| o.selection == saved)
        .unwrap_or(0);
    let none_available = options
        .iter()
        .all(|o| o.selection.is_none() || !o.available);
    let key = field.key.clone();
    let choices: Vec<SelectOption> = options
        .iter()
        .enumerate()
        .map(|(i, o)| SelectOption::new(i.to_string(), o.label.clone()))
        .collect();
    rsx! {
        Select {
            options: choices,
            value: current.to_string(),
            onchange: move |v: String| {
                let selection = v
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| options.get(i))
                    .and_then(|o| o.selection.as_ref().map(ResourceSelection::to_json));
                form.set(&scope, &key, selection.unwrap_or_else(|| json!({})));
            },
        }
        if none_available {
            div { class: "text-sub", "{plugins().no_resources_text(&field)}" }
        }
    }
}

/// 资源变化时重新读取选项。
fn plugins_changed() -> Signal<u64> {
    let mut rev = use_signal(|| 0u64);
    use_plugin_events(move |event| {
        if matches!(
            event,
            PluginEvent::ResourcesChanged | PluginEvent::PluginsChanged
        ) {
            rev += 1;
        }
    });
    rev
}

/// 字符串列表：每一项可带按钮和删除，下面是输入框；插件提供候选时可以从候选中选择。
#[component]
fn TagsField(field: PluginSettingField, scope: Vec<Seg>) -> Element {
    let form = use_context::<Form>();
    let overlays = use_overlays();
    let mut input = use_signal(String::new);
    let rk = result_key(&scope, &field.key);
    let tags = form.tags(&scope, &field.key);
    // 获取到候选后直接打开候选列表
    let mut seen = use_signal(|| 0u64);
    let candidates = form.candidates;
    use_effect({
        let (form, scope, field, rk) = (form.clone(), scope.clone(), field.clone(), rk.clone());
        move || {
            let Some((version, list)) = candidates.read().get(&rk).cloned() else {
                return;
            };
            if version == *seen.peek() || list.is_empty() {
                return;
            }
            seen.set(version);
            let (form, scope, field) = (form.clone(), scope.clone(), field.clone());
            spawn(async move { pick_candidates(overlays, form, scope, field, list).await });
        }
    });
    let mut add = {
        let (form, scope, key) = (form.clone(), scope.clone(), field.key.clone());
        move || {
            form.add_tag(&scope, &key, &input.peek());
            input.set(String::new());
        }
    };
    let mut on_submit = add.clone();
    rsx! {
        div { class: "settings-tags",
            for tag in tags {
                {
                    let state = form.items.read().get(&format!("{rk}:{tag}")).cloned();
                    let (remove_form, remove_scope, remove_key, remove_tag) =
                        (form.clone(), scope.clone(), field.key.clone(), tag.clone());
                    let (run_form, run_scope, run_key, run_tag) =
                        (form.clone(), scope.clone(), field.key.clone(), tag.clone());
                    rsx! {
                        div { key: "{tag}", class: "settings-tag",
                            span { "{tag}" }
                            if !field.item_button.is_empty() {
                                match state {
                                    Some(ItemState::Running) => rsx! { Icon { name: IconName::Loading, size: 12.0, class: "spin" } },
                                    Some(ItemState::Done(ok, text)) => rsx! {
                                        Tooltip { text: if ok { String::new() } else { text.clone() },
                                            Button {
                                                kind: ButtonKind::Flat,
                                                onclick: move |_| run_form.run_button(&run_scope, &run_key, Some(&run_tag)),
                                                if ok { "成功" } else { "失败" }
                                            }
                                        }
                                    },
                                    None => rsx! {
                                        Button {
                                            kind: ButtonKind::Flat,
                                            onclick: move |_| run_form.run_button(&run_scope, &run_key, Some(&run_tag)),
                                            "{field.item_button}"
                                        }
                                    },
                                }
                            }
                            IconButton {
                                icon: IconName::Close,
                                size: 22.0,
                                icon_size: 10.0,
                                onclick: move |_| {
                                    let tags: Vec<String> = remove_form
                                        .tags(&remove_scope, &remove_key)
                                        .into_iter()
                                        .filter(|t| *t != remove_tag)
                                        .collect();
                                    remove_form.set(&remove_scope, &remove_key, json!(tags));
                                },
                            }
                        }
                    }
                }
            }
        }
        div { class: "settings-tag-input",
            TextInput {
                value: input(),
                placeholder: field.placeholder.clone(),
                oninput: move |v| input.set(v),
                onsubmit: move |_| on_submit(),
            }
            Button { onclick: move |_| add(), "添加" }
        }
    }
}

/// 从候选中选择：选中的加入，取消勾选的候选移除，不在候选中的项保持不变。
async fn pick_candidates(
    overlays: Overlays,
    form: Form,
    scope: Vec<Seg>,
    field: PluginSettingField,
    candidates: Vec<String>,
) {
    let current = form.tags(&scope, &field.key);
    let items: Vec<PickerItem> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| PickerItem::new(i as u64, c.clone()))
        .collect();
    let initial: Vec<u64> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| current.contains(c))
        .map(|(i, _)| i as u64)
        .collect();
    let picker = Picker::multi(format!("选择{}", field.label), items, initial);
    let Some(chosen) = overlays.pick(picker).await else {
        return;
    };
    let mut tags: Vec<String> = current
        .into_iter()
        .filter(|t| !candidates.contains(t))
        .collect();
    tags.extend(
        chosen
            .iter()
            .filter_map(|i| candidates.get(*i as usize).cloned()),
    );
    form.set(&scope, &field.key, json!(tags));
}

fn row_title(form: &Form, field: &PluginSettingField, scope: &[Seg]) -> String {
    field
        .fields
        .iter()
        .find(|f| f.kind == PluginSettingType::Text)
        .map(|f| form.text(scope, &f.key).trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "未命名".to_owned())
}

/// 可增删的列表：左侧是各行（显示第一个文本项），右侧是选中行的设置项。
#[component]
fn ListField(field: PluginSettingField, scope: Vec<Seg>) -> Element {
    let form = use_context::<Form>();
    let overlays = use_overlays();
    let mut selected = use_signal(|| None::<i64>);
    let rows: Vec<i64> = form
        .value(&scope, &field.key)
        .as_array()
        .map(|a| a.iter().filter_map(|r| r[ROW_ID].as_i64()).collect())
        .unwrap_or_default();
    let current = selected()
        .filter(|id| rows.contains(id))
        .or_else(|| rows.first().copied());
    let list_scope = |id: i64| {
        let mut path = scope.clone();
        path.push(Seg::Key(field.key.clone()));
        path.push(Seg::Row(id));
        path
    };
    let add = {
        let (form, scope, field) = (form.clone(), scope.clone(), field.clone());
        move |_| {
            let mut next = form.next_row;
            let id = *next.peek() + 1;
            next.set(id);
            let mut row = Map::new();
            for sub in &field.fields {
                row.insert(sub.key.clone(), sub.default_value());
            }
            row.insert(ROW_ID.to_owned(), json!(id));
            let mut rows = form
                .value(&scope, &field.key)
                .as_array()
                .cloned()
                .unwrap_or_default();
            rows.push(Value::Object(row));
            form.set(&scope, &field.key, Value::Array(rows));
            selected.set(Some(id));
        }
    };
    rsx! {
        div { class: "settings-list",
            div { class: "settings-list-rows scroll",
                for id in rows.iter().copied() {
                    ListRow {
                        key: "{id}",
                        density: RowDensity::Compact,
                        selected: Some(id) == current,
                        title: row_title(&form, &field, &list_scope(id)),
                        onclick: move |_| selected.set(Some(id)),
                    }
                }
                Button { onclick: add, "添加" }
            }
            div { class: "settings-list-detail scroll",
                if let Some(id) = current {
                    for sub in field.fields.iter().cloned() {
                        FieldView { key: "{id}:{sub.key}", field: sub, scope: list_scope(id) }
                    }
                    Button {
                        onclick: {
                            let (form, scope, key) = (form.clone(), scope.clone(), field.key.clone());
                            move |_| {
                                let rows: Vec<Value> = form
                                    .value(&scope, &key)
                                    .as_array()
                                    .map(|a| a.iter().filter(|r| r[ROW_ID].as_i64() != Some(id)).cloned().collect())
                                    .unwrap_or_default();
                                form.set(&scope, &key, Value::Array(rows));
                                let _ = overlays;
                            }
                        },
                        "删除"
                    }
                } else {
                    div { class: "text-sub", "点击左侧的“添加”新建" }
                }
            }
        }
    }
}

/// 面板的对象选择：所有已登录账号的用户或话题。
async fn pick_target(overlays: Overlays, kind: PluginTargetKind) -> Option<PluginTarget> {
    let candidates = plugins().target_candidates(kind).await;
    let items: Vec<PickerItem> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| PickerItem::new(i as u64, c.title.clone()).subtitle(c.subtitle.clone()))
        .collect();
    let title = match kind {
        PluginTargetKind::User => "选择用户",
        PluginTargetKind::Topic => "选择话题",
    };
    let index = overlays.pick(Picker::single(title, items)).await?;
    candidates
        .get(*index.first()? as usize)
        .map(|c| c.target.clone())
}

fn entry_title(entry: &PanelEntry) -> String {
    if entry.text.is_empty() {
        entry.target.name().to_owned()
    } else {
        format!("{}：{}", entry.target.name(), entry.text)
    }
}

/// 一个面板：插件保存了设置的用户 / 话题，右侧是选中项的设置项。
#[component]
fn PanelSection(plugin: PluginRef, kind: PluginTargetKind, label: String) -> Element {
    let overlays = use_overlays();
    let plugin = plugin.0;
    let available = plugin.active_panels().iter().any(|p| p.kind == kind);
    let mut entries = use_signal(Vec::<PanelEntry>::new);
    let mut selected = use_signal(|| None::<PluginTarget>);
    let mut fields = use_signal(Vec::<PanelField>::new);
    let reload = {
        let plugin = plugin.clone();
        move || {
            let plugin = plugin.clone();
            spawn(async move {
                let reply = plugins().panel(&plugin, PanelRequest::List(kind)).await;
                let list = reply
                    .ok()
                    .flatten()
                    .map(|r| parse_entries(&r, kind))
                    .unwrap_or_default();
                // 刚添加、插件还没有保存的项保留
                let pending = selected
                    .peek()
                    .clone()
                    .filter(|t| !list.iter().any(|e| e.target.same_as(t)));
                let mut list = list;
                if let Some(target) = pending {
                    list.push(PanelEntry {
                        target,
                        text: String::new(),
                    });
                }
                entries.set(list);
            });
        }
    };
    use_hook({
        let reload = reload.clone();
        move || {
            if available {
                reload();
            }
        }
    });
    let select = {
        let plugin = plugin.clone();
        move |target: PluginTarget| {
            let mut selected = selected;
            let mut fields = fields;
            selected.set(Some(target.clone()));
            let plugin = plugin.clone();
            spawn(async move {
                let reply = plugins().panel(&plugin, PanelRequest::Get(target)).await;
                fields.set(
                    reply
                        .ok()
                        .flatten()
                        .map(|r| parse_fields(&r))
                        .unwrap_or_default(),
                );
            });
        }
    };
    let add = {
        let (select, reload) = (select.clone(), reload.clone());
        move |_| {
            let (select, reload) = (select.clone(), reload.clone());
            spawn(async move {
                if let Some(target) = pick_target(overlays, kind).await {
                    let mut list = entries.peek().clone();
                    if !list.iter().any(|e| e.target.same_as(&target)) {
                        list.push(PanelEntry {
                            target: target.clone(),
                            text: String::new(),
                        });
                        entries.set(list);
                    }
                    select(target);
                    reload();
                }
            });
        }
    };
    let current = selected();
    rsx! {
        div { class: "settings-field",
            div { class: "text-sub", "{label}" }
            if !available {
                div { class: "text-sub", "请先开启插件" }
            } else {
                div { class: "settings-list",
                    div { class: "settings-list-rows scroll",
                        for entry in entries.read().iter().cloned() {
                            ListRow {
                                key: "{entry_title(&entry)}",
                                density: RowDensity::Compact,
                                selected: current.as_ref().is_some_and(|t| t.same_as(&entry.target)),
                                title: entry_title(&entry),
                                onclick: {
                                    let (select, target) = (select.clone(), entry.target.clone());
                                    move |_| select(target.clone())
                                },
                            }
                        }
                        Button { onclick: add, "添加" }
                    }
                    div { class: "settings-list-detail scroll",
                        if let Some(target) = current {
                            for field in fields.read().iter().cloned() {
                                PluginPanelField {
                                    key: "{field.key}",
                                    plugin: PluginRef(plugin.clone()),
                                    target: target.clone(),
                                    field,
                                    on_changed: {
                                        let reload = reload.clone();
                                        move |next: Vec<PanelField>| {
                                            fields.set(next);
                                            reload();
                                        }
                                    },
                                }
                            }
                            Button {
                                onclick: {
                                    let (plugin, target, reload) = (plugin.clone(), target.clone(), reload.clone());
                                    move |_| {
                                        let (plugin, target, reload) = (plugin.clone(), target.clone(), reload.clone());
                                        spawn(async move {
                                            let name = format!("“{}”的设置", target.name());
                                            let removed = overlays
                                                .delete(&name, None, || async {
                                                    plugins().panel(&plugin, PanelRequest::Remove(target.clone())).await
                                                })
                                                .await;
                                            if removed.is_some() {
                                                selected.set(None);
                                                entries.write().retain(|e| !e.target.same_as(&target));
                                                reload();
                                            }
                                        });
                                    }
                                },
                                "删除"
                            }
                        } else {
                            div { class: "text-sub", "点击左侧的“添加”新建" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn FormBody(plugin: PluginRef) -> Element {
    let plugin = plugin.0;
    let form = use_context::<Form>();
    let fields = plugin
        .manifest()
        .map(|m| m.settings.clone())
        .unwrap_or_default();
    let panels = plugin
        .manifest()
        .map(|m| m.panels.clone())
        .unwrap_or_default();
    // 插件通过 set_settings_feedback 回复设置窗口
    let (mut results, mut candidates, mut items) = (form.results, form.candidates, form.items);
    let id = plugin.id();
    use_plugin_events(move |event| {
        let PluginEvent::SettingsFeedback {
            plugin_id,
            feedback,
        } = event
        else {
            return;
        };
        if plugin_id != id {
            return;
        }
        let rk = format!(
            "{}:{}",
            feedback.row.map(|r| r.to_string()).unwrap_or_default(),
            feedback.key
        );
        if let (Some(item), Some(ok)) = (&feedback.item, feedback.ok) {
            let text = feedback
                .text
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "失败".to_owned());
            items
                .write()
                .insert(format!("{rk}:{item}"), ItemState::Done(ok, text));
            return;
        }
        if let Some(text) = feedback.text {
            results.write().insert(rk.clone(), text);
        }
        if let Some(options) = feedback.options {
            let version = candidates.peek().get(&rk).map_or(0, |(v, _)| *v) + 1;
            candidates.write().insert(rk, (version, options));
        }
    });
    rsx! {
        for field in fields {
            FieldView { key: "{field.key}", field, scope: Vec::<Seg>::new() }
        }
        for panel in panels {
            PanelSection {
                key: "{panel.label}",
                plugin: PluginRef(plugin.clone()),
                kind: panel.kind,
                label: panel.label.clone(),
            }
        }
    }
}

/// 插件修改了自己的设置时对打开中的表单做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsRefresh {
    /// 没有未保存的修改：重新读取。
    Reload,
    /// 有未保存的修改：问用户，不直接覆盖。
    Ask,
    /// 已经在问了，答复后读取到的是最新的值。
    Skip,
}

fn settings_refresh(dirty: bool, asking: bool) -> SettingsRefresh {
    match (dirty, asking) {
        (false, _) => SettingsRefresh::Reload,
        (true, false) => SettingsRefresh::Ask,
        (true, true) => SettingsRefresh::Skip,
    }
}

/// 用保存的设置替换表单的内容。
fn reload_form(
    plugin: &NativePlugin,
    mut saved: Signal<Value>,
    mut values: Signal<Value>,
    mut next_row: Signal<i64>,
) {
    let mut row = *next_row.peek();
    let fresh = initial_values(plugin, &mut row);
    next_row.set(row);
    saved.set(fresh.clone());
    values.set(fresh);
}

#[component]
fn PluginSettingsDialog(plugin: PluginRef, answer: Answer<bool>) -> Element {
    let overlays = use_overlays();
    let (initial, first_row) = use_hook(|| {
        let mut next_row = 0;
        (initial_values(&plugin.0, &mut next_row), next_row)
    });
    let saved = use_signal(|| initial.clone());
    let values = use_signal(|| initial.clone());
    let next_row = use_signal(|| first_row);
    use_context_provider(|| Form {
        plugin: plugin.0.clone(),
        values,
        results: Signal::new(HashMap::new()),
        candidates: Signal::new(HashMap::new()),
        items: Signal::new(HashMap::new()),
        next_row,
    });
    let mut asking = use_signal(|| false);
    use_plugin_events({
        let plugin = plugin.0.clone();
        move |event| {
            let PluginEvent::SettingsChanged(id) = event else {
                return;
            };
            if id != plugin.id() {
                return;
            }
            let dirty = *values.peek() != *saved.peek();
            let refresh = settings_refresh(dirty, *asking.peek());
            match refresh {
                SettingsRefresh::Reload => reload_form(&plugin, saved, values, next_row),
                SettingsRefresh::Skip => {}
                SettingsRefresh::Ask => {
                    asking.set(true);
                    let plugin = plugin.clone();
                    spawn(async move {
                        if overlays.confirm_discard().await {
                            reload_form(&plugin, saved, values, next_row);
                        }
                        asking.set(false);
                    });
                }
            }
        }
    });
    let dirty = *values.read() != *saved.read();
    let wide = plugin.0.manifest().is_some_and(|m| {
        !m.panels.is_empty() || m.settings.iter().any(|f| f.kind == PluginSettingType::List)
    });
    let (ok, cancel, close) = (answer.clone(), answer.clone(), answer);
    let save_plugin = plugin.0.clone();
    // 有未保存的修改时离开前确认
    let leave = move |answer: Answer<bool>| {
        spawn(async move {
            if !dirty || overlays.confirm_discard().await {
                answer.cancel();
            }
        });
    };
    rsx! {
        DialogFrame {
            title: format!("{} 设置", plugin.0.name()),
            width: if wide { 760.0 } else { 480.0 },
            on_close: move |_| leave(close.clone()),
            footer: rsx! {
                DialogButtons {
                    confirm_text: "保存",
                    on_confirm: move |_| {
                        let map = values.peek().as_object().cloned().unwrap_or_default();
                        match plugins().save_settings(&save_plugin, &map) {
                            Ok(()) => {
                                overlays.toast("已保存");
                                ok.send(true);
                            }
                            Err(error) => overlays.report_error("保存失败", error),
                        }
                    },
                    on_cancel: move |_| leave(cancel.clone()),
                }
            },
            FormBody { plugin: plugin.clone() }
        }
    }
}

/// 打开插件设置；保存后返回 true。
pub async fn open_plugin_settings(overlays: Overlays, plugin: Arc<NativePlugin>) -> bool {
    overlays
        .ask_dialog(move |answer| {
            rsx! { PluginSettingsDialog { plugin: PluginRef(plugin.clone()), answer } }
        })
        .await
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_found_by_id_and_keys_by_path() {
        let mut root = json!({"models": [{"_row": 1, "name": "a"}, {"_row": 2, "name": "b"}]});
        let scope = [Seg::Key("models".into()), Seg::Row(2)];
        assert_eq!(object_at(&root, &scope).unwrap()["name"], "b");
        object_at_mut(&mut root, &scope).unwrap()["name"] = json!("c");
        assert_eq!(root["models"][1]["name"], "c");
        assert!(object_at(&root, &[Seg::Key("models".into()), Seg::Row(9)]).is_none());
    }

    #[test]
    fn plugin_changes_never_overwrite_unsaved_edits() {
        assert_eq!(settings_refresh(false, false), SettingsRefresh::Reload);
        assert_eq!(settings_refresh(false, true), SettingsRefresh::Reload);
        assert_eq!(settings_refresh(true, false), SettingsRefresh::Ask);
        assert_eq!(settings_refresh(true, true), SettingsRefresh::Skip);
    }

    #[test]
    fn result_keys_include_the_row() {
        assert_eq!(result_key(&[], "token"), ":token");
        assert_eq!(
            result_key(&[Seg::Key("l".into()), Seg::Row(3)], "token"),
            "3:token"
        );
    }
}
