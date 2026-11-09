//! 需要登录才能下载的头像、账号切换。

use crate::services::services;
use crate::shell::url_for;
use crate::state::use_app_state;
use crate::ui::{Avatar, Select, SelectOption};
use dioxus::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use zulip_session::Session;

thread_local! {
    // 列表滚动时行组件反复创建，已下载的头像不用再等一次异步
    static LOCAL_URLS: RefCell<HashMap<String, String>> = RefCell::default();
}

fn cache_key(session: &Session, url: &str) -> String {
    format!("{}:{url}", session.account_id())
}

async fn local_url(session: Arc<Session>, url: String) -> Option<String> {
    let key = cache_key(&session, &url);
    if let Some(cached) = LOCAL_URLS.with_borrow(|c| c.get(&key).cloned()) {
        return Some(cached);
    }
    let path = session
        .image_file(&url)
        .await
        .inspect_err(|e| log::debug!("下载头像失败：{e}"))
        .ok()?;
    let local = url_for(&path)?;
    LOCAL_URLS.with_borrow_mut(|c| c.insert(key, local.clone()));
    Some(local)
}

/// 已下载到本机的图片的本地地址。
pub fn downloaded_image(session: &Session, url: &str) -> Option<String> {
    LOCAL_URLS.with_borrow(|c| c.get(&cache_key(session, url)).cloned())
}

/// 下载图片，返回本地地址。
pub async fn local_image(session: Arc<Session>, url: String) -> Option<String> {
    local_url(session, url).await
}

/// 已下载的头像的本地地址。没有下载时在后台下载，下次就有了（选择器的列表项只收地址）。
pub fn cached_avatar(session: &Arc<Session>, url: &str) -> Option<String> {
    let cached = LOCAL_URLS.with_borrow(|c| c.get(&cache_key(session, url)).cloned());
    if cached.is_none() {
        let (session, url) = (session.clone(), url.to_owned());
        spawn(async move {
            local_url(session, url).await;
        });
    }
    cached
}

/// 用户头像：先显示文字，下载完显示图片。
#[component]
pub fn RemoteAvatar(
    #[props(into)] text: String,
    #[props(into)] url: Option<String>,
    #[props(default = 40.0)] size: f64,
    #[props(default)] online: bool,
    onclick: Option<EventHandler<MouseEvent>>,
) -> Element {
    let state = use_app_state();
    let cached = state
        .session
        .read()
        .as_ref()
        .zip(url.as_ref())
        .and_then(|(s, u)| LOCAL_URLS.with_borrow(|c| c.get(&cache_key(s, u)).cloned()));
    let loaded = use_resource(use_reactive!(|url| {
        let session = state.session.peek().clone();
        async move {
            match (session, url) {
                (Some(session), Some(url)) => local_url(session, url).await,
                _ => None,
            }
        }
    }));
    let src = cached.or_else(|| loaded.read().clone().flatten());
    rsx! {
        Avatar { text, src, size, online, onclick }
    }
}

/// 在线账号多于一个时显示的账号选择。
#[component]
pub fn AccountSwitcher(value: i64, onchange: EventHandler<i64>) -> Element {
    let state = use_app_state();
    let options: Vec<SelectOption> = state
        .accounts
        .read()
        .iter()
        .filter(|entry| entry.session.is_some())
        .map(|entry| {
            let name = services().hub.session(entry.account.id).map_or_else(
                || entry.account.full_name.clone(),
                |s| s.display_name(s.self_id()),
            );
            SelectOption::new(entry.account.id.to_string(), name)
        })
        .collect();
    if options.len() < 2 {
        return rsx! {};
    }
    rsx! {
        Select {
            options,
            value: value.to_string(),
            onchange: move |v: String| {
                if let Ok(id) = v.parse() {
                    onchange.call(id);
                }
            },
        }
    }
}
