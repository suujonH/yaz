//! Avatars that need login to download, account switcher.

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
    // Row components are created repeatedly while the list scrolls; an already downloaded avatar
    // need not wait for async again
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

/// Local address of an image already downloaded to this machine.
pub fn downloaded_image(session: &Session, url: &str) -> Option<String> {
    LOCAL_URLS.with_borrow(|c| c.get(&cache_key(session, url)).cloned())
}

/// Download an image and return the local address.
pub async fn local_image(session: Arc<Session>, url: String) -> Option<String> {
    local_url(session, url).await
}

/// Local address of a downloaded avatar. If not downloaded, downloads in the background so it is
/// there next time (picker list items only take the address).
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

/// User avatar: shows text first, then the image once downloaded.
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

/// Account picker shown when more than one account is online.
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
