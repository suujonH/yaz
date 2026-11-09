//! 组织管理：组织名称、消息编辑、已读回执。所有人可以查看，组织管理员可以修改；点“保存”才提交。

use super::channels::shows;
use super::field::*;
use super::{Store, use_dirty};
use crate::ui::*;
use crate::widgets::mock_notice;
use dioxus::prelude::*;
use serde_json::{Value, json};

const NAME_MAX: usize = 40;
const STANDARD_LIMITS: [(Option<i64>, &str); 4] = [
    (None, "不限"),
    (Some(600), "10 分钟"),
    (Some(3600), "1 小时"),
    (Some(86400), "1 天"),
];

/// 编辑时限的选项；服务器上的值不在固定选项中时末尾加上它。
fn limit_options(current: Option<i64>) -> Vec<(Option<i64>, String)> {
    let mut list: Vec<(Option<i64>, String)> = STANDARD_LIMITS
        .iter()
        .map(|(v, t)| (*v, (*t).to_owned()))
        .collect();
    if !STANDARD_LIMITS.iter().any(|(v, _)| *v == current) {
        list.push((
            current,
            format!("{} 分钟", current.unwrap_or_default() / 60),
        ));
    }
    list
}

/// 编辑时限提交的值：不限是 JSON 字符串 "unlimited"。
fn limit_value(limit: Option<i64>) -> Value {
    match limit {
        Some(seconds) => json!(seconds),
        None => json!("\"unlimited\""),
    }
}

#[component]
pub fn Organization() -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let overlays = use_overlays();
    let session = store.session();
    let realm = session.realm();
    let can_edit = shows(session.permissions().is_org_admin());
    // 修改中的值；为 None 时是服务器上的值
    let name = use_signal(|| None::<String>);
    let allow = use_signal(|| None::<bool>);
    let limit = use_signal(|| None::<Option<i64>>);
    let receipts = use_signal(|| None::<bool>);
    let shown_name = name().unwrap_or_else(|| realm.name.clone());
    let shown_allow = allow().unwrap_or(realm.allow_message_editing);
    let shown_limit = limit().unwrap_or(realm.edit_limit_seconds);
    let shown_receipts = receipts().unwrap_or(realm.enable_read_receipts);
    let dirty = shown_name.trim() != realm.name
        || shown_allow != realm.allow_message_editing
        || shown_limit != realm.edit_limit_seconds
        || shown_receipts != realm.enable_read_receipts;
    use_dirty(dirty);
    let revert = move || {
        let (mut name, mut allow, mut limit, mut receipts) = (name, allow, limit, receipts);
        name.set(None);
        allow.set(None);
        limit.set(None);
        receipts.set(None);
    };
    let changes = {
        let realm = realm.clone();
        let shown_name = shown_name.trim().to_owned();
        move || {
            let mut args: Vec<(&'static str, Value)> = Vec::new();
            if shown_name != realm.name {
                args.push(("name", json!(shown_name)));
            }
            if shown_allow != realm.allow_message_editing {
                args.push(("allow_message_editing", json!(shown_allow)));
            }
            if shown_limit != realm.edit_limit_seconds {
                args.push((
                    "message_content_edit_limit_seconds",
                    limit_value(shown_limit),
                ));
            }
            if shown_receipts != realm.enable_read_receipts {
                args.push(("enable_read_receipts", json!(shown_receipts)));
            }
            args
        }
    };
    let save = {
        let session = session.clone();
        move || {
            let args = changes();
            if args.is_empty() {
                return;
            }
            if !session.permissions().is_org_admin() {
                mock_notice(overlays);
                return;
            }
            let session = session.clone();
            spawn(async move {
                if overlays
                    .attempt("保存失败", session.update_realm(&args))
                    .await
                    .is_some()
                {
                    overlays.toast("设置已保存");
                    revert();
                }
            });
        }
    };
    rsx! {
        SettingsPage { title: "组织管理",
            if !can_edit {
                div { class: "text-sub", "只有组织管理员可以修改，你可以查看当前的设置。" }
            }
            Group { title: "基本信息",
                TextRow {
                    label: "组织名称",
                    bind: Bind::new(shown_name.clone(), move |v: String| {
                        if v.chars().count() <= NAME_MAX && !v.trim().is_empty() {
                            let mut draft = name;
                            draft.set(Some(v));
                        }
                    }),
                    placeholder: "组织名称",
                }
            }
            Group { title: "消息",
                ToggleRow {
                    label: "允许编辑消息",
                    bind: Bind::new(shown_allow, move |v| {
                        let mut draft = allow;
                        draft.set(Some(v));
                    }),
                    disabled: !can_edit,
                }
                ChoiceRow {
                    label: "编辑时限",
                    bind: Bind::new(shown_limit, move |v| {
                        let mut draft = limit;
                        draft.set(Some(v));
                    }),
                    options: limit_options(realm.edit_limit_seconds),
                    disabled: !can_edit || !shown_allow,
                }
                ToggleRow {
                    label: "已读回执",
                    bind: Bind::new(shown_receipts, move |v| {
                        let mut draft = receipts;
                        draft.set(Some(v));
                    }),
                    disabled: !can_edit,
                }
            }
        }
        UnsavedBar {
            visible: dirty && can_edit,
            on_save: move |_| save(),
            on_revert: move |_| revert(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_is_sent_as_json_string() {
        assert_eq!(limit_value(None), json!("\"unlimited\""));
        assert_eq!(limit_value(Some(600)), json!(600));
    }

    #[test]
    fn unknown_limit_is_appended() {
        assert_eq!(limit_options(Some(600)).len(), 4);
        let list = limit_options(Some(1800));
        assert_eq!(list.last(), Some(&(Some(1800), "30 分钟".to_owned())));
    }
}
