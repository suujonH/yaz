//! Me: avatar, name, status text, login info, invisible mode, draft sync, account email.

use super::Store;
use super::avatar::{AccountAvatar, crop_avatar};
use super::field::*;
use crate::platform;
use crate::ui::*;
use crate::widgets::SessionRef;
use crate::windows;
use dioxus::prelude::*;
use std::sync::Arc;
use zulip_core::avatar::large_url;
use zulip_core::permission::role_label;
use zulip_session::Session;

const SIGNATURE_MAX: usize = 60;

/// Choose image -> crop -> upload.
async fn change_avatar(session: Arc<Session>, overlays: Overlays, mut busy: Signal<bool>) {
    let Some(path) = platform::pick_file("选择头像", &[platform::IMAGE_FILTER]).await else {
        return;
    };
    busy.set(true);
    let result = async {
        let Some(png) = crop_avatar(overlays, path).await? else {
            return Ok(false);
        };
        session
            .upload_avatar(png)
            .await
            .map_err(|e| e.to_string())?;
        Ok::<bool, String>(true)
    }
    .await;
    busy.set(false);
    match result {
        Ok(true) => overlays.toast("头像已更换"),
        Ok(false) => {}
        Err(error) => overlays.report_error("更换头像失败", error),
    }
}

/// Text edited in place after a click; submitted on Enter or when focus is lost, cancelled with
/// Esc.
#[component]
fn EditableText(
    #[props(into)] value: String,
    #[props(into)] empty: String,
    on_commit: EventHandler<String>,
) -> Element {
    let mut editing = use_signal(|| false);
    if editing() {
        return rsx! {
            div { class: "settings-text",
                InlineEdit {
                    value: value.clone(),
                    oncommit: move |text: String| {
                        editing.set(false);
                        on_commit.call(text);
                    },
                    oncancel: move |_| editing.set(false),
                }
            }
        };
    }
    rsx! {
        Button {
            kind: ButtonKind::Flat,
            onclick: move |_| editing.set(true),
            if value.is_empty() {
                span { class: "text-sub", "{empty}" }
            } else {
                "{value}"
            }
        }
    }
}

#[component]
pub fn Me() -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let overlays = use_overlays();
    let session = store.session();
    let busy = use_signal(|| false);
    let self_id = session.self_id();
    let user = session.user(self_id);
    let name = user
        .as_ref()
        .map_or_else(String::new, |u| u.full_name.clone());
    let role = user.as_ref().map_or(0, |u| u.role);
    let avatar_url = user.and_then(|u| u.avatar_url);
    let signature = session.status_text(self_id).unwrap_or_default();
    let account = session.account_id();
    let invisible = field::<bool>(Source::Zulip(&["presence_enabled"])).inverted();
    let on_name = {
        let (session, current) = (session.clone(), name.clone());
        move |text: String| {
            let (session, text) = (session.clone(), text.trim().to_owned());
            if text.is_empty() || text == current {
                return;
            }
            spawn(async move {
                overlays
                    .attempt("修改失败", session.update_user(self_id, Some(&text), None))
                    .await;
            });
        }
    };
    let on_signature = {
        let (session, current) = (session.clone(), signature.clone());
        move |text: String| {
            let (session, text) = (session.clone(), text.trim().to_owned());
            if text == current || text.chars().count() > SIGNATURE_MAX {
                return;
            }
            spawn(async move {
                overlays
                    .attempt("修改失败", session.set_status_text(&text))
                    .await;
            });
        }
    };
    let preview_url = avatar_url.clone();
    let change = session.clone();
    rsx! {
        SettingsPage { title: "我",
            SettingCard {
                ListRow {
                    density: RowDensity::Large,
                    leading: rsx! {
                        AccountAvatar {
                            session: SessionRef(session.clone()),
                            text: name.clone(),
                            url: avatar_url.clone(),
                            size: 56.0,
                            onclick: move |_| {
                                if let Some(url) = &preview_url {
                                    windows::open_image_viewer(account, vec![large_url(url)], 0);
                                }
                            },
                        }
                    },
                    cells: rsx! {
                        div { class: "me-head-text",
                            div { class: "text-title", "{name}" }
                            div { class: "text-sub", "{session.email()}" }
                            div { class: "text-sub", "ID {self_id} · {role_label(role)}" }
                        }
                    },
                    trailing: rsx! {
                        Button {
                            disabled: busy(),
                            onclick: move |_| {
                                spawn(change_avatar(change.clone(), overlays, busy));
                            },
                            "更换头像"
                        }
                    },
                }
            }
            Group { title: "资料",
                SettingRow { label: "姓名",
                    EditableText { value: name.clone(), empty: "未设置", on_commit: on_name }
                }
                SettingRow { label: "个性签名",
                    EditableText { value: signature.clone(), empty: "未设置", on_commit: on_signature }
                }
            }
            Group { title: "状态与同步",
                ToggleRow { label: "隐身", bind: invisible }
                ToggleRow { label: "草稿同步", bind: field(Source::Zulip(&["enable_drafts_synchronization"])) }
            }
            Group { title: "账号邮件",
                ToggleRow { label: "新设备登录时发送提醒邮件", bind: field(Source::Zulip(&["enable_login_emails"])) }
                ToggleRow { label: "接收 Zulip 产品与功能邮件", bind: field(Source::Zulip(&["enable_marketing_emails"])) }
            }
        }
    }
}
