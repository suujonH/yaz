//! "Zulip accounts": all accounts on the left, settings navigation of the selected account in the
//! middle, the current page on the right.

use super::account_pages::AccountView;
use super::avatar::AccountAvatar;
use super::field::{Group, Scope, SettingsPage, Source, field};
use super::section_nav::SectionNav;
use super::{AccountPage, Nav, Place, Scrolled};
use crate::pages::login::AddAccountArgs;
use crate::services::services;
use crate::state::use_app_state;
use crate::ui::*;
use crate::widgets::SessionRef;
use crate::widgets::logout_confirm;
use crate::windows;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::sync::Arc;
use zulip_session::{AccountEntry, AccountStatus};

pub const ACCOUNT_PAGES: [(AccountPage, &str); 10] = [
    (AccountPage::Server, "服务器"),
    (AccountPage::Me, "我"),
    (AccountPage::Preferences, "偏好设置"),
    (AccountPage::Notifications, "消息通知"),
    (AccountPage::EnhancedPush, "强化推送"),
    (AccountPage::GroupMessages, "话题消息设置"),
    (AccountPage::Privacy, "隐私"),
    (AccountPage::AlertWords, "提醒词"),
    (AccountPage::Resources, "资源"),
    (AccountPage::Organization, "组织管理"),
];

/// Display name of an account: the user's own nickname when online, otherwise the name recorded at
/// the last login.
fn name_of(entry: &AccountEntry) -> String {
    entry
        .session
        .as_ref()
        .map(|s| s.display_name(s.self_id()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| entry.account.full_name.clone())
}

/// Log out of an account: uses the same confirmation as the main window menu.
async fn logout(entry: AccountEntry, nav: Nav, overlays: Overlays) {
    if !nav.leave(overlays).await {
        return;
    }
    if overlays
        .confirm(logout_confirm(&name_of(&entry), true))
        .await
    {
        services().hub.logout(entry.account.id, false).await;
    }
}

/// Delete an account: confirm first, and local data can be deleted at the same time.
pub async fn delete(entry: AccountEntry, nav: Nav, overlays: Overlays) {
    if !nav.leave(overlays).await {
        return;
    }
    let confirm = Confirm::delete(
        &format!("账号“{}”", name_of(&entry)),
        Some("将删除该账号的登录状态，再次登录需要重新输入密码。"),
    )
    .check("同时删除此账号的所有本地数据（聊天记录、设置、草稿等）");
    if let Some(delete_data) = overlays.ask(confirm).await {
        overlays
            .attempt(
                "删除失败",
                services().hub.delete_account(entry.account.id, delete_data),
            )
            .await;
    }
}

fn status_hint(status: &AccountStatus) -> Option<String> {
    match status {
        AccountStatus::Online | AccountStatus::Disabled => None,
        AccountStatus::Connecting => Some("登录中".to_owned()),
        AccountStatus::Failed(_) => Some("登录失败".to_owned()),
    }
}

#[component]
pub fn AccountsPanel(place: Place) -> Element {
    let state = use_app_state();
    let nav = use_context::<Nav>();
    let overlays = use_overlays();
    let accounts = state.accounts.read().clone();
    let selected = place
        .account
        .filter(|id| accounts.iter().any(|e| e.account.id == *id))
        .or_else(|| {
            state
                .account
                .read()
                .filter(|id| accounts.iter().any(|e| e.account.id == *id))
        })
        .or_else(|| accounts.first().map(|e| e.account.id));
    let entry = accounts
        .iter()
        .find(|e| Some(e.account.id) == selected)
        .cloned();
    let (enabled, disabled): (Vec<_>, Vec<_>) = accounts
        .iter()
        .cloned()
        .partition(|e| e.account.login_on_startup);
    let row = |entry: AccountEntry| {
        let id = entry.account.id;
        let online = entry.session.is_some();
        let menu_entry = entry.clone();
        rsx! {
            ListRow {
                key: "{id}",
                density: RowDensity::Normal,
                selected: Some(id) == selected,
                leading: rsx! {
                    AccountAvatar {
                        session: entry.session.clone().map(SessionRef),
                        text: name_of(&entry),
                        url: entry.session.as_ref().and_then(|s| s.user(s.self_id())).and_then(|u| u.avatar_url).or_else(|| entry.account.avatar_url.clone()),
                        size: 32.0,
                        dimmed: !online,
                    }
                },
                title: name_of(&entry),
                subtitle: entry.account.email.clone(),
                meta: status_hint(&entry.status),
                onclick: move |_| {
                    spawn(nav.go(overlays, Place { account: Some(id), ..place }));
                },
                menu: move |_| {
                    let mut items = Vec::new();
                    if online {
                        let entry = menu_entry.clone();
                        items.push(MenuEntry::item("退出账号", move || {
                            spawn_forever(logout(entry.clone(), nav, overlays));
                        }));
                    }
                    let entry = menu_entry.clone();
                    items.push(MenuEntry::item("删除账号", move || {
                        spawn_forever(delete(entry.clone(), nav, overlays));
                    }));
                    items
                },
            }
        }
    };
    rsx! {
        div { class: "settings-accounts",
            div { class: "settings-account-list",
                div { class: "settings-account-scroll scroll",
                    for entry in enabled {
                        {row(entry)}
                    }
                    if !disabled.is_empty() {
                        div { class: "settings-account-header text-sub", "未启用" }
                        for entry in disabled {
                            {row(entry)}
                        }
                    }
                }
                div { class: "settings-account-add",
                    Button {
                        kind: ButtonKind::Flat,
                        icon: IconName::Add,
                        onclick: move |_| windows::open_add_account(AddAccountArgs::default()),
                        "添加账号"
                    }
                }
            }
            if let Some(entry) = entry {
                if entry.session.is_some() {
                    div { class: "settings-account-nav scroll",
                        for (page, title) in ACCOUNT_PAGES {
                            div { key: "{title}",
                                ListRow {
                                    density: RowDensity::Compact,
                                    selected: page == place.account_page,
                                    title,
                                    onclick: move |_| {
                                        spawn(nav.go(overlays, Place { account_page: page, account: selected, ..place }));
                                    },
                                }
                                if page == place.account_page {
                                    SectionNav { key: "{title}" }
                                }
                            }
                        }
                    }
                }
                {account_body(&entry, place.account_page)}
            }
        }
    }
}

fn account_body(entry: &AccountEntry, page: AccountPage) -> Element {
    match &entry.session {
        Some(session) => {
            let id = Arc::as_ptr(session) as usize;
            rsx! {
                AccountView { key: "{entry.account.id}:{id}", session: SessionRef(session.clone()), page }
            }
        }
        None => rsx! {
            OfflineAccount { key: "{entry.account.id}", entry: EntryRef(entry.clone()) }
        },
    }
}

/// The account entry in component props.
#[derive(Clone)]
pub struct EntryRef(pub AccountEntry);

impl PartialEq for EntryRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.account == other.0.account && self.0.status == other.0.status
    }
}

/// "Start": when turned off the account logs out and stays in the "disabled" list, and is not
/// logged in on program startup either.
#[component]
pub fn StartupRow() -> Element {
    let bind = field(Source::Custom(
        Scope::Account,
        |env| env.row().is_some_and(|row| row.login_on_startup),
        |env, enabled| {
            let id = env.account.ok_or("账号不存在")?;
            let overlays = env.overlays;
            spawn_forever(async move {
                overlays
                    .attempt("设置失败", services().hub.set_login_on_startup(id, enabled))
                    .await;
            });
            Ok(())
        },
    ));
    rsx! {
        SettingRow { label: "启动",
            Toggle { checked: bind.value, onchange: move |v| bind.set(v) }
        }
    }
}

/// Accounts that are not online (logging in, login failed, disabled): basic info plus login and
/// delete.
#[component]
fn OfflineAccount(entry: EntryRef) -> Element {
    let EntryRef(entry) = entry;
    let id = entry.account.id;
    use_store_for(id);
    let nav = use_context::<Nav>();
    let overlays = use_overlays();
    let status = entry.status.clone();
    let delete_entry = entry.clone();
    rsx! {
        div { class: "settings-main",
            Scrolled {
                SettingsPage { title: name_of(&entry),
                    Group { title: None::<String>,
                        SettingRow { label: "邮箱", span { class: "text-sub selectable", "{entry.account.email}" } }
                        SettingRow { label: "服务器地址", span { class: "text-sub selectable", "{entry.account.realm_url}" } }
                        SettingRow { label: "状态", span { class: "text-sub", "{status.text()}" } }
                        StartupRow {}
                    }
                    div { class: "settings-actions",
                        match status {
                            AccountStatus::Failed(error) => rsx! {
                                Button {
                                    kind: ButtonKind::Primary,
                                    onclick: move |_| windows::open_add_account(AddAccountArgs { account: Some(id), error: Some(error.clone()) }),
                                    "重新登录"
                                }
                            },
                            AccountStatus::Disabled => rsx! {
                                Button {
                                    kind: ButtonKind::Primary,
                                    onclick: move |_| {
                                        spawn_forever(async move {
                                            overlays.attempt("登录失败", services().hub.set_login_on_startup(id, true)).await;
                                        });
                                    },
                                    "登录"
                                }
                            },
                            _ => rsx! {},
                        }
                        Button {
                            onclick: move |_| {
                                spawn(delete(delete_entry.clone(), nav, overlays));
                            },
                            "删除账号"
                        }
                    }
                }
            }
        }
    }
}

fn use_store_for(account: i64) {
    super::use_store(None, Some(account));
}
