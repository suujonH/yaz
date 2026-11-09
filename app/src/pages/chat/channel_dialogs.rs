//! 频道设置和频道公告的编辑。保存后由频道更新事件刷新界面。

use super::store::Chat;
use crate::pages::dialogs::{CHANNEL_DESCRIPTION_MAX, CHANNEL_NAME_MAX};
use crate::ui::{
    Checkbox, DialogButtons, DialogFrame, DialogHandle, Select, SelectOption, SettingCard,
    SettingRow, TextInput, use_overlays,
};
use crate::widgets::SessionRef;
use dioxus::prelude::*;
use serde_json::{Value, json};
use zulip_core::model::Channel;
use zulip_core::permission::{
    CHANNEL_PERMISSIONS, ChannelGroupSettings, ChannelPermission, GroupSettingValue,
};
use zulip_session::Session;

const CUSTOM: &str = "custom";
const CUSTOM_LABEL: &str = "自定义（请在网页端修改）";

/// 频道未归档，且自己是频道管理员。
pub fn can_manage_channel(session: &Session, channel: &Channel) -> bool {
    !channel.is_archived
        && session
            .permissions()
            .can_administer_channel(&channel.permissions)
}

/// 一项权限。当前值不是可选的系统组时只读，只能在网页端修改。
#[derive(Clone, Debug, PartialEq)]
struct PermissionRow {
    name: &'static str,
    label: &'static str,
    original: Option<GroupSettingValue>,
    options: Vec<SelectOption>,
    custom: bool,
}

impl PermissionRow {
    fn new(
        permission: &ChannelPermission,
        settings: &ChannelGroupSettings,
        choices: &[(i64, &'static str)],
    ) -> Self {
        let original = (permission.value)(settings).cloned();
        let custom = !matches!(
            &original,
            Some(GroupSettingValue::Group(id)) if choices.iter().any(|(choice, _)| choice == id)
        );
        let options = if custom {
            vec![SelectOption::new(CUSTOM, CUSTOM_LABEL)]
        } else {
            choices
                .iter()
                .map(|(id, text)| SelectOption::new(id.to_string(), *text))
                .collect()
        };
        Self {
            name: permission.name,
            label: permission.label,
            original,
            options,
            custom,
        }
    }

    fn initial(&self) -> String {
        match &self.original {
            Some(GroupSettingValue::Group(id)) if !self.custom => id.to_string(),
            _ => CUSTOM.to_owned(),
        }
    }

    /// 选择变化时的接口参数：带上原值，服务器发现已被他人修改时拒绝。
    fn change(&self, selected: &str) -> Option<(&'static str, Value)> {
        if self.custom || selected == self.initial() {
            return None;
        }
        let id: i64 = selected.parse().ok()?;
        Some((self.name, json!({"new": id, "old": self.original})))
    }
}

/// 对话框中的内容。
#[derive(Clone, Debug, PartialEq)]
struct SettingsDraft {
    name: String,
    description: String,
    private: bool,
    history_public: bool,
    selected: Vec<String>,
}

/// 只提交改动过的项。
fn settings_changes(
    channel: &Channel,
    rows: &[PermissionRow],
    draft: &SettingsDraft,
) -> Vec<(&'static str, Value)> {
    let mut args = Vec::new();
    let name = draft.name.trim();
    if name != channel.name {
        args.push(("new_name", Value::from(name)));
    }
    let description = draft.description.trim();
    if description != channel.description.trim() {
        args.push(("description", Value::from(description)));
    }
    if draft.private != channel.invite_only {
        args.push(("is_private", Value::from(draft.private)));
    }
    if draft.history_public != channel.history_public_to_subscribers {
        args.push((
            "history_public_to_subscribers",
            Value::from(draft.history_public),
        ));
    }
    args.extend(
        rows.iter()
            .zip(&draft.selected)
            .filter_map(|(row, selected)| row.change(selected)),
    );
    args
}

/// 打开频道设置对话框。
pub fn open_channel_settings(chat: Chat, channel: Channel) {
    let session = chat.session();
    chat.overlays.open_dialog(true, move |handle| {
        rsx! {
            ChannelSettingsDialog {
                session: SessionRef(session.clone()),
                channel: channel.clone(),
                handle,
            }
        }
    });
}

#[component]
fn ChannelSettingsDialog(session: SessionRef, channel: Channel, handle: DialogHandle) -> Element {
    let overlays = use_overlays();
    let rows = use_hook(|| {
        CHANNEL_PERMISSIONS
            .iter()
            .map(|p| {
                PermissionRow::new(
                    p,
                    &channel.permissions,
                    &session.0.channel_permission_choices(p.name),
                )
            })
            .collect::<Vec<_>>()
    });
    let mut name = use_signal(|| channel.name.clone());
    let mut description = use_signal(|| channel.description.clone());
    let mut private = use_signal(|| channel.invite_only);
    let mut history_public = use_signal(|| channel.history_public_to_subscribers);
    let mut selected = use_signal(|| rows.iter().map(PermissionRow::initial).collect::<Vec<_>>());
    let mut busy = use_signal(|| false);
    let title = format!("频道设置 - #{}", channel.name);
    let empty = name.read().trim().is_empty();
    let save_rows = rows.clone();
    rsx! {
        DialogFrame {
            title,
            width: 460.0,
            on_close: move |_| handle.close(),
            footer: rsx! {
                DialogButtons {
                    confirm_text: "保存",
                    disabled: empty || busy(),
                    on_confirm: move |_| {
                        let draft = SettingsDraft {
                            name: name.peek().clone(),
                            description: description.peek().clone(),
                            private: *private.peek(),
                            history_public: *history_public.peek(),
                            selected: selected.peek().clone(),
                        };
                        let args = settings_changes(&channel, &save_rows, &draft);
                        if args.is_empty() {
                            handle.close();
                            return;
                        }
                        let session = session.0.clone();
                        let stream_id = channel.stream_id;
                        busy.set(true);
                        spawn(async move {
                            let saved = overlays
                                .attempt("保存失败", session.update_channel(stream_id, &args))
                                .await;
                            busy.set(false);
                            if saved.is_some() {
                                overlays.toast("设置已保存");
                                handle.close();
                            }
                        });
                    },
                    on_cancel: move |_| handle.close(),
                }
            },
            div { class: "dialog-form",
                label { class: "dialog-label", "名称" }
                TextInput {
                    value: name(),
                    autofocus: true,
                    invalid: empty,
                    oninput: move |v: String| name.set(v.chars().take(CHANNEL_NAME_MAX).collect()),
                }
                label { class: "dialog-label", "描述（频道公告）" }
                TextInput {
                    value: description(),
                    multiline: true,
                    oninput: move |v: String| description.set(v.chars().take(CHANNEL_DESCRIPTION_MAX).collect()),
                }
                Checkbox {
                    checked: private(),
                    onchange: move |on: bool| {
                        private.set(on);
                        // 公开频道的历史总是对新成员可见，只有私有频道可以修改
                        if !on {
                            history_public.set(true);
                        }
                    },
                    "私有频道（仅受邀成员可见）"
                }
                Checkbox {
                    checked: history_public(),
                    disabled: !private(),
                    onchange: move |on| history_public.set(on),
                    "新成员可查看历史消息"
                }
                label { class: "dialog-label", "权限设置" }
                SettingCard {
                    for (index, row) in rows.iter().enumerate() {
                        SettingRow { key: "{row.name}", label: row.label,
                            div { class: "settings-select",
                                Select {
                                    options: row.options.clone(),
                                    value: selected.read()[index].clone(),
                                    disabled: row.custom,
                                    onchange: move |value: String| selected.write()[index] = value,
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 打开频道公告的编辑对话框。公告就是频道描述：编辑原始 Markdown，显示服务器渲染的结果。
pub fn edit_notice(chat: Chat, channel: Channel) {
    let session = chat.session();
    chat.overlays.open_dialog(true, move |handle| {
        rsx! {
            NoticeDialog {
                session: SessionRef(session.clone()),
                channel: channel.clone(),
                handle,
            }
        }
    });
}

#[component]
fn NoticeDialog(session: SessionRef, channel: Channel, handle: DialogHandle) -> Element {
    let overlays = use_overlays();
    let mut draft = use_signal(|| channel.description.clone());
    let mut busy = use_signal(|| false);
    let title = format!("频道公告 - #{}", channel.name);
    rsx! {
        DialogFrame {
            title,
            width: 440.0,
            on_close: move |_| handle.close(),
            footer: rsx! {
                DialogButtons {
                    confirm_text: "保存",
                    disabled: busy(),
                    on_confirm: move |_| {
                        let text = draft.peek().trim().to_owned();
                        if text == channel.description.trim() {
                            handle.close();
                            return;
                        }
                        let session = session.0.clone();
                        let stream_id = channel.stream_id;
                        busy.set(true);
                        spawn(async move {
                            let args = [("description", Value::from(text))];
                            let saved = overlays
                                .attempt("保存失败", session.update_channel(stream_id, &args))
                                .await;
                            busy.set(false);
                            if saved.is_some() {
                                handle.close();
                            }
                        });
                    },
                    on_cancel: move |_| handle.close(),
                }
            },
            TextInput {
                value: draft(),
                multiline: true,
                rows: 7,
                autofocus: true,
                placeholder: "支持 Markdown",
                oninput: move |v: String| draft.set(v.chars().take(CHANNEL_DESCRIPTION_MAX).collect()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHOICES: [(i64, &str); 2] = [(1, "所有人"), (3, "管理员")];

    fn settings(value: Option<GroupSettingValue>) -> ChannelGroupSettings {
        ChannelGroupSettings {
            can_send_message_group: value,
            ..ChannelGroupSettings::default()
        }
    }

    fn row(value: Option<GroupSettingValue>) -> PermissionRow {
        PermissionRow::new(&CHANNEL_PERMISSIONS[0], &settings(value), &CHOICES)
    }

    #[test]
    fn group_outside_choices_is_custom() {
        let plain = row(Some(GroupSettingValue::Group(3)));
        assert!(!plain.custom);
        assert_eq!(plain.initial(), "3");
        assert_eq!(plain.options.len(), 2);

        for value in [
            None,
            Some(GroupSettingValue::Group(99)),
            Some(GroupSettingValue::anonymous([7], [])),
        ] {
            let custom = row(value);
            assert!(custom.custom);
            assert_eq!(custom.initial(), CUSTOM);
            assert_eq!(custom.options.len(), 1);
            assert_eq!(custom.change("1"), None);
        }
    }

    #[test]
    fn permission_change_carries_the_old_value() {
        let row = row(Some(GroupSettingValue::Group(3)));
        assert_eq!(row.change("3"), None);
        assert_eq!(
            row.change("1"),
            Some(("can_send_message_group", json!({"new": 1, "old": 3})))
        );
    }

    fn draft_of(channel: &Channel, rows: &[PermissionRow]) -> SettingsDraft {
        SettingsDraft {
            name: channel.name.clone(),
            description: channel.description.clone(),
            private: channel.invite_only,
            history_public: channel.history_public_to_subscribers,
            selected: rows.iter().map(PermissionRow::initial).collect(),
        }
    }

    #[test]
    fn only_changed_settings_are_submitted() {
        let channel = Channel {
            name: "开发".to_owned(),
            description: "说明".to_owned(),
            ..Channel::default()
        };
        let rows = vec![row(Some(GroupSettingValue::Group(1)))];
        let mut draft = draft_of(&channel, &rows);
        assert!(settings_changes(&channel, &rows, &draft).is_empty());

        draft.name = " 开发 ".to_owned();
        draft.description = "说明 ".to_owned();
        assert!(settings_changes(&channel, &rows, &draft).is_empty());

        draft.name = " 新名 ".to_owned();
        draft.description = String::new();
        draft.private = true;
        draft.history_public = false;
        draft.selected = vec!["3".to_owned()];
        assert_eq!(
            settings_changes(&channel, &rows, &draft),
            vec![
                ("new_name", json!("新名")),
                ("description", json!("")),
                ("is_private", json!(true)),
                ("history_public_to_subscribers", json!(false)),
                ("can_send_message_group", json!({"new": 3, "old": 1})),
            ]
        );
    }
}
