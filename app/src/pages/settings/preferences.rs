//! 保存在 Zulip 上的个人设置页：偏好设置、消息通知、隐私、提醒词。

use super::Store;
use super::avatar::AccountAvatar;
use super::field::*;
use crate::ui::*;
use crate::widgets::SessionRef;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;

fn options<T: Clone>(items: &[(T, &str)]) -> Vec<(T, String)> {
    items
        .iter()
        .map(|(value, text)| (value.clone(), (*text).to_owned()))
        .collect()
}

#[component]
pub fn Preferences() -> Element {
    let scheme = field(Source::Zulip(&["color_scheme"]));
    let enter_sends = field(Source::Zulip(&["enter_sends"]));
    let twenty_four = field(Source::Zulip(&["twenty_four_hour_time"]));
    rsx! {
        SettingsPage { title: "偏好设置",
            Group { title: "外观",
                ChoiceRow {
                    label: "颜色模式",
                    bind: scheme.map(|v: &i32| if matches!(*v, 2 | 3) { *v } else { 1 }, Some),
                    options: options(&[(1, "跟随系统"), (3, "白天"), (2, "夜间")]),
                }
            }
            Group { title: "聊天",
                ChoiceRow {
                    label: "发送消息",
                    bind: enter_sends,
                    options: options(&[(true, "Enter"), (false, "Ctrl+Enter")]),
                }
                ToggleRow { label: "24 小时制显示时间", bind: twenty_four }
            }
        }
    }
}

#[component]
pub fn Notifications() -> Element {
    let wildcard = field(Source::AccountLocal(
        |s| s.wildcard_mentions_notify(),
        |s, v| s.set_wildcard_mentions_notify(v),
    ));
    rsx! {
        SettingsPage { title: "消息通知",
            Group { title: "通知",
                ToggleRow { label: "新消息通知", bind: field(Source::Zulip(&["enable_desktop_notifications"])) }
                ToggleRow { label: "频道消息通知", bind: field(Source::Zulip(&["enable_stream_desktop_notifications"])) }
                ToggleRow { label: "@全体成员时通知", bind: wildcard }
                ToggleRow { label: "通知显示消息摘要", bind: field(Source::Zulip(&["pm_content_in_desktop_notifications"])) }
            }
            Group { title: "提示音",
                ToggleRow { label: "提示音", bind: field(Source::Zulip(&["enable_sounds"])) }
                ToggleRow { label: "频道消息提示音", bind: field(Source::Zulip(&["enable_stream_audible_notifications"])) }
            }
        }
    }
}

#[component]
pub fn Privacy() -> Element {
    let store = consume_context::<Store>();
    let overlays = use_overlays();
    let session = store.session();
    let mut muted: Vec<i64> = session.muted_users();
    muted.sort_by_key(|id| session.display_name(*id));
    rsx! {
        SettingsPage { title: "隐私",
            Group { title: None::<String>,
                ToggleRow { label: "发送已读回执", bind: field(Source::Zulip(&["send_read_receipts"])) }
                ToggleRow {
                    label: "发送“正在输入”状态",
                    bind: field(Source::Zulip(&["send_private_typing_notifications", "send_stream_typing_notifications"])),
                }
                ChoiceRow {
                    label: "谁可以看到我的邮箱",
                    bind: field(Source::Zulip(&["email_address_visibility"])),
                    options: options(&[(1, "所有人"), (2, "成员"), (5, "版主"), (3, "管理员"), (4, "无人")]),
                }
            }
            Group { title: "已屏蔽的用户",
                if muted.is_empty() {
                    SettingRow { label: "没有屏蔽任何人" }
                }
                for id in muted {
                    {
                        let session = session.clone();
                        let name = session.display_name(id);
                        let url = session.user(id).and_then(|u| u.avatar_url);
                        rsx! {
                            ListRow {
                                key: "{id}",
                                density: RowDensity::Compact,
                                leading: rsx! { AccountAvatar { session: SessionRef(session.clone()), text: name.clone(), url, size: 28.0 } },
                                title: name,
                                trailing: rsx! {
                                    Button {
                                        kind: ButtonKind::Flat,
                                        onclick: move |_| {
                                            let session = session.clone();
                                            spawn_forever(async move {
                                                overlays.attempt("取消屏蔽失败", session.set_user_muted(id, false)).await;
                                            });
                                        },
                                        "取消屏蔽"
                                    }
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 提醒词：消息中出现这些词时按“@我”的规则提醒。
#[component]
pub fn AlertWords() -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let overlays = use_overlays();
    let session = store.session();
    let mut input = use_signal(String::new);
    let words = session.alert_words();
    let mut add = {
        let session = session.clone();
        let words = words.clone();
        move || {
            let word = input.peek().trim().to_owned();
            if word.is_empty() {
                return;
            }
            if words.iter().any(|w| w.eq_ignore_ascii_case(&word)) {
                input.set(String::new());
                return;
            }
            let session = session.clone();
            spawn_forever(async move {
                if overlays
                    .attempt("添加失败", session.add_alert_words(&[word]))
                    .await
                    .is_some()
                {
                    input.set(String::new());
                }
            });
        }
    };
    let mut on_submit = add.clone();
    rsx! {
        SettingsPage { title: "提醒词",
            Group { title: None::<String>,
                SettingRow { label: "提醒词",
                    div { class: "settings-text",
                        TextInput {
                            value: input(),
                            placeholder: "输入提醒词",
                            oninput: move |v| input.set(v),
                            onsubmit: move |_| on_submit(),
                        }
                    }
                    Button { kind: ButtonKind::Primary, disabled: input.read().trim().is_empty(), onclick: move |_| add(), "添加" }
                }
            }
            Group { title: None::<String>,
                if words.is_empty() {
                    SettingRow { label: "还没有提醒词" }
                }
                for word in words {
                    SettingRow {
                        key: "{word}",
                        label: word.clone(),
                        IconButton {
                            icon: IconName::Close,
                            onclick: {
                                let session = session.clone();
                                move |_| {
                                    let session = session.clone();
                                    let word = word.clone();
                                    spawn_forever(async move {
                                        let target = format!("提醒词“{word}”");
                                        overlays
                                            .delete(&target, None, || session.remove_alert_words(std::slice::from_ref(&word)))
                                            .await;
                                    });
                                }
                            },
                        }
                    }
                }
            }
        }
    }
}
