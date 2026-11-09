//! Login: the startup login window and the "add account" window share [`LoginForm`].

use crate::platform::{self, FileFilter};
use crate::services::services;
use crate::shell::{WindowShell, use_window_args};
use crate::state::{AppCommand, send_command};
use crate::ui::*;
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use std::sync::Arc;
use zulip_session::{PasswordLogin, Session, describe_login_error};
use zulip_store::{AccountRepo, AccountRow};

/// Parameters of the "add account" window: the prefilled account and the error to show (when
/// clicking an account that failed to log in).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddAccountArgs {
    pub account: Option<i64>,
    pub error: Option<String>,
}

fn repo() -> AccountRepo {
    AccountRepo::new(&services().db)
}

/// Account records in the email dropdown. Accounts already logged in are not listed when adding an
/// account.
fn history(exclude_online: bool) -> Vec<AccountRow> {
    let hub = &services().hub;
    repo()
        .get_all()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !exclude_online || hub.session(a.id).is_none())
        .collect()
}

fn same_realm(a: &str, b: &str) -> bool {
    a.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

/// Startup login window ([`crate::windows::open_login`]). When done, the root window enters the
/// main UI and closes this window; meanwhile the form stays locked with its progress showing.
#[component]
pub fn LoginWindow() -> Element {
    use_hook(|| spawn(crate::services::load_plugins()));
    let prefill = use_hook(|| {
        let repo = repo();
        repo.get_listed()
            .ok()
            .and_then(|list| list.into_iter().next())
            .or_else(|| repo.get_all().ok().and_then(|list| list.into_iter().next()))
    });
    rsx! {
        WindowShell {
            resizable: false,
            on_close: move |_| send_command(AppCommand::Exit),
            LoginForm {
                prefill,
                later: move |_| send_command(AppCommand::EnterMain),
                on_success: move |_| send_command(AppCommand::EnterMain),
            }
        }
    }
}

#[component]
pub fn AddAccountWindow() -> Element {
    let args = use_window_args::<AddAccountArgs>();
    let window = use_window();
    let AddAccountArgs { account, error } = args();
    let prefill = account.and_then(|id| repo().get(id).ok().flatten());
    let form_key = account.unwrap_or(0);
    rsx! {
        WindowShell { title: "添加账号", resizable: false,
            LoginForm {
                key: "{form_key}",
                prefill,
                error,
                add_account: true,
                on_success: move |_| window.close(),
            }
        }
    }
}

#[component]
fn LoginForm(
    prefill: Option<AccountRow>,
    error: Option<String>,
    #[props(default)] add_account: bool,
    /// The "log in later" option, present only at startup.
    later: Option<EventHandler<()>>,
    on_success: EventHandler<Arc<Session>>,
) -> Element {
    let overlays = use_overlays();
    let mut realm = use_signal(|| {
        prefill
            .as_ref()
            .map_or_else(String::new, |a| a.realm_url.clone())
    });
    let mut email = use_signal(|| {
        prefill
            .as_ref()
            .map_or_else(String::new, |a| a.email.clone())
    });
    let mut password = use_signal(String::new);
    let mut allow_invalid = use_signal(|| prefill.as_ref().is_some_and(|a| a.allow_invalid_cert));
    let mut advanced = use_signal(|| prefill.as_ref().is_some_and(|a| a.allow_invalid_cert));
    let mut message = use_signal(|| error.clone());
    let mut busy = use_signal(|| false);
    let mut accounts = use_signal(move || history(add_account));

    let saved = move || {
        accounts
            .read()
            .iter()
            .find(|a| {
                a.has_key()
                    && same_realm(&a.realm_url, &realm.read())
                    && a.email.eq_ignore_ascii_case(email.read().trim())
            })
            .cloned()
    };
    let password_hint = if saved().is_some() {
        "已记住密码"
    } else {
        "密码"
    };

    let mut submit = move || {
        if *busy.peek() {
            return;
        }
        let hub = services().hub.clone();
        let existing = saved();
        if add_account
            && existing
                .as_ref()
                .is_some_and(|a| hub.session(a.id).is_some())
        {
            message.set(Some("该账号已登录".to_owned()));
            return;
        }
        let typed = password.peek().clone();
        let request = PasswordLogin {
            realm: realm.peek().trim().to_owned(),
            email: email.peek().trim().to_owned(),
            password: typed.clone(),
            allow_invalid_cert: *allow_invalid.peek(),
        };
        busy.set(true);
        message.set(None);
        spawn(async move {
            let result = match existing.filter(|_| typed.is_empty()) {
                Some(account) => {
                    repo().set_allow_invalid_cert(account.id, request.allow_invalid_cert);
                    hub.login_account(account.id).await
                }
                None => hub.login_with_password(request).await,
            };
            // On success the form stays locked until its window closes
            match result {
                Ok(session) => on_success.call(session),
                Err(error) => {
                    busy.set(false);
                    log::warn!("登录失败：{error}");
                    message.set(Some(describe_login_error(&error)));
                }
            }
        });
    };

    let login_zuliprc = move |_| {
        spawn(async move {
            let filters = [FileFilter {
                name: "zuliprc",
                extensions: &["*"],
            }];
            let Some(path) = platform::pick_file("选择 zuliprc 文件", &filters).await else {
                return;
            };
            let text = match tokio::fs::read_to_string(&path).await {
                Ok(text) => text,
                Err(error) => {
                    message.set(Some(format!("读取文件失败：{error}")));
                    return;
                }
            };
            busy.set(true);
            message.set(None);
            let hub = services().hub.clone();
            let result = hub.login_with_zuliprc(&text, *allow_invalid.peek()).await;
            match result {
                Ok(session) => on_success.call(session),
                Err(error) => {
                    busy.set(false);
                    message.set(Some(describe_login_error(&error)));
                }
            }
        });
    };

    let is_busy = busy();
    let ready = !realm.read().trim().is_empty()
        && !email.read().trim().is_empty()
        && (!password.read().is_empty() || saved().is_some());
    rsx! {
        div { class: if is_busy { "login busy" } else { "login" },
            div { class: "login-logo",
                Icon { name: IconName::Logo, size: 64.0 }
            }
            TextInput {
                value: realm(),
                placeholder: "服务器地址",
                disabled: is_busy,
                oninput: move |v| realm.set(v),
            }
            div { class: "login-email",
                TextInput {
                    value: email(),
                    placeholder: "邮箱",
                    disabled: is_busy,
                    oninput: move |v| email.set(v),
                }
                if !accounts.read().is_empty() && !is_busy {
                    div { class: "login-history",
                        Popover {
                            options: PopoverOptions { placement: Placement::BottomStart, gap: 4.0, min_width: 260.0 },
                            content: move |handle: PopoverHandle| rsx! {
                                div { class: "login-history-list",
                                    for account in accounts.read().iter().cloned() {
                                        ListRow {
                                            key: "{account.id}",
                                            density: RowDensity::Compact,
                                            leading: rsx! {
                                                Avatar { text: account.full_name.clone(), size: 28.0 }
                                            },
                                            title: account.email.clone(),
                                            subtitle: account.realm_url.clone(),
                                            trailing: {
                                                let account = account.clone();
                                                rsx! {
                                                    Tooltip { text: "删除此账号记录",
                                                        IconButton {
                                                            icon: IconName::Close,
                                                            size: 22.0,
                                                            icon_size: 12.0,
                                                            onclick: move |e: MouseEvent| {
                                                                e.stop_propagation();
                                                                let id = account.id;
                                                                let target = format!("账号记录“{}”", account.email);
                                                                spawn(async move {
                                                                    let hub = services().hub.clone();
                                                                    let deleted = overlays
                                                                        .delete(&target, None, || hub.delete_account(id, false))
                                                                        .await;
                                                                    if deleted.is_some() {
                                                                        accounts.set(history(add_account));
                                                                    }
                                                                });
                                                            },
                                                        }
                                                    }
                                                }
                                            },
                                            onclick: {
                                                let account = account.clone();
                                                move |_| {
                                                    realm.set(account.realm_url.clone());
                                                    email.set(account.email.clone());
                                                    password.set(String::new());
                                                    allow_invalid.set(account.allow_invalid_cert);
                                                    handle.close();
                                                }
                                            },
                                        }
                                    }
                                }
                            },
                            IconButton { icon: IconName::ChevronDown, size: 24.0, icon_size: 14.0 }
                        }
                    }
                }
            }
            TextInput {
                value: password(),
                placeholder: password_hint,
                password: true,
                disabled: is_busy,
                oninput: move |v| password.set(v),
                onsubmit: move |_| submit(),
            }
            div { class: "login-links",
                Checkbox {
                    checked: advanced(),
                    disabled: is_busy,
                    onchange: move |v| advanced.set(v),
                    "高级"
                }
                if let Some(later) = later {
                    Button { kind: ButtonKind::Flat, disabled: is_busy, onclick: move |_| later.call(()), "稍后登录" }
                }
            }
            ExpandPanel { open: advanced(),
                div { class: "login-advanced",
                    Checkbox {
                        checked: allow_invalid(),
                        disabled: is_busy,
                        onchange: move |v| allow_invalid.set(v),
                        "允许不受信任的证书（仅测试服务器）"
                    }
                    Button { kind: ButtonKind::Flat, disabled: is_busy, onclick: login_zuliprc, "使用 zuliprc 登录…" }
                }
            }
            if let Some(text) = message() {
                div { class: "login-error", "{text}" }
            }
            div { class: "login-spacer" }
            Button {
                kind: ButtonKind::Primary,
                class: "login-submit",
                disabled: is_busy || !ready,
                onclick: move |_| submit(),
                if is_busy {
                    Icon { name: IconName::Loading, size: 14.0, class: "spin" }
                    "正在登录…"
                } else {
                    "登录"
                }
            }
        }
    }
}
