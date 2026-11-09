//! 服务器：账号的启动、连接配置、服务器信息、删除账号。

use super::accounts::{StartupRow, delete};
use super::field::*;
use super::{Nav, Store};
use crate::services::services;
use crate::state::use_app_state;
use crate::ui::*;
use dioxus::prelude::*;
use zulip_session::ConnectionState;
use zulip_store::AccountRepo;

fn connection_text(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Connected => "已连接",
        ConnectionState::Reconnecting => "正在重新连接…",
        ConnectionState::AuthFailed => "登录已失效",
    }
}

#[component]
pub fn Server() -> Element {
    let store = consume_context::<Store>();
    let _ = store.rev.read();
    let nav = use_context::<Nav>();
    let overlays = use_overlays();
    let state = use_app_state();
    let session = store.session();
    let realm = session.realm();
    let account = session.account_id();
    // 重新登录该账号后生效
    let invalid_cert = field(Source::Custom(
        Scope::Account,
        |env| env.row().is_some_and(|row| row.allow_invalid_cert),
        |env, allow| {
            let id = env.account.ok_or("账号不存在")?;
            AccountRepo::new(&services().db).set_allow_invalid_cert(id, allow);
            env.overlays.toast("重新登录该账号后生效");
            Ok(())
        },
    ));
    let entry = state
        .accounts
        .read()
        .iter()
        .find(|e| e.account.id == account)
        .cloned();
    rsx! {
        SettingsPage { title: "服务器",
            Group { title: "账号", StartupRow {} }
            Group { title: "连接配置",
                ToggleRow { label: "允许不受信任的证书", bind: invalid_cert }
            }
            Group { title: "服务器信息",
                SettingRow { label: "服务器地址", span { class: "text-sub selectable", "{session.realm_url()}" } }
                SettingRow { label: "组织名称", span { class: "text-sub selectable", "{realm.name}" } }
                SettingRow { label: "Zulip 版本", span { class: "text-sub selectable", "{realm.zulip_version}" } }
                SettingRow { label: "Feature level", span { class: "text-sub selectable", "{realm.feature_level}" } }
                SettingRow { label: "连接状态", span { class: "text-sub", "{connection_text(session.connection())}" } }
            }
            div { class: "settings-actions center",
                Button {
                    kind: ButtonKind::Danger,
                    onclick: move |_| {
                        if let Some(entry) = entry.clone() {
                            spawn(delete(entry, nav, overlays));
                        }
                    },
                    "删除账号"
                }
            }
        }
    }
}
