//! Desktop notification popup: bottom right of the screen, click to open the conversation, closes
//! automatically after a few seconds (it has no close button); it does not close while the mouse
//! rests on it.

use crate::platform;
use crate::services::services;
use crate::shell::{WindowShell, use_window_args};
use crate::state::{AppCommand, send_command};
use crate::ui::*;
use crate::widgets::*;
use dioxus::desktop::tao::dpi::PhysicalPosition;
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use std::time::{Duration, Instant};
use zulip_session::{NotificationAvatar, NotificationRequest};

const VISIBLE_FOR: Duration = Duration::from_secs(6);
/// After the mouse rested on it and left without clicking, it closes this soon.
const CLOSE_AFTER_LEAVE: Duration = Duration::from_millis(500);
/// A notification stays at least this long before the next one replaces it.
const MIN_VISIBLE: Duration = Duration::from_secs(2);
/// The replaced notification slides out to the left in this time (the `notification-leave`
/// animation in page.css), then the next one shows.
const LEAVE_ANIMATION: Duration = Duration::from_millis(200);
const SCREEN_MARGIN: i32 = 16;

/// What to do when the notification is clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationClick {
    /// Open the conversation or reminder the notification refers to.
    OpenTarget,
    /// Only show the main window (a plugin's notification belongs to no conversation).
    ShowMain,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotificationArgs {
    pub account: i64,
    pub request: NotificationRequest,
    pub click: NotificationClick,
}

#[component]
pub fn NotificationWindow() -> Element {
    rsx! {
        WindowShell { resizable: false, NotificationBody {} }
    }
}

#[component]
fn NotificationBody() -> Element {
    let args = use_window_args::<NotificationArgs>();
    let window = use_window();
    use_hook({
        let window = window.clone();
        move || {
            // Bottom right of the work area (the screen without the taskbar): right on the
            // taskbar, whatever its height and side
            // On the primary screen
            let primary = window.primary_monitor().map(|m| {
                let (p, s) = (m.position(), m.size());
                (p.x + s.width as i32 / 2, p.y + s.height as i32 / 2)
            });
            if let Some(area) = primary.and_then(|(x, y)| platform::work_area_at(x, y)) {
                let size = window.outer_size();
                window.set_outer_position(PhysicalPosition::new(
                    area.x + area.width - size.width as i32 - SCREEN_MARGIN,
                    area.y + area.height - size.height as i32,
                ));
            }
            // Created hidden (a popup): shown once in place
            window.set_visible(true);
        }
    });
    // `None`: the mouse has not been on this notification; `Some(true)`: it is on it;
    // `Some(false)`: it left without clicking
    let mut hovered = use_signal(|| None::<bool>);
    // The notification shown, and since when: a newer one (`args`) waits until this one has been
    // shown for MIN_VISIBLE, then this one slides out (`leaving`) and the newer one shows. Of
    // several waiting, only the newest shows.
    let mut shown = use_signal(|| args.peek().clone());
    let mut shown_at = use_hook(|| CopyValue::new(Instant::now()));
    let mut leaving = use_signal(|| false);
    use_resource(move || {
        let next = args();
        async move {
            if next == *shown.peek() {
                return;
            }
            tokio::time::sleep(MIN_VISIBLE.saturating_sub(shown_at.peek().elapsed())).await;
            leaving.set(true);
            tokio::time::sleep(LEAVE_ANIMATION).await;
            shown.set(next);
            shown_at.set(Instant::now());
            hovered.set(None);
            leaving.set(false);
        }
    });
    {
        let window = window.clone();
        use_resource(move || {
            // Not while a newer notification waits to show (it closes after that one)
            let waiting = args() != shown();
            let wait = match hovered() {
                _ if waiting => None,
                None => Some(VISIBLE_FOR),
                Some(true) => None,
                Some(false) => Some(CLOSE_AFTER_LEAVE),
            };
            let window = window.clone();
            async move {
                if let Some(wait) = wait {
                    tokio::time::sleep(wait).await;
                    window.close();
                }
            }
        });
    }
    let NotificationArgs {
        account,
        request,
        click,
    } = shown();
    // When several accounts are online, mark which account received it
    let online = services().hub.online_sessions();
    let account_name = (click == NotificationClick::OpenTarget && online.len() > 1)
        .then(|| online.iter().find(|s| s.account_id() == account))
        .flatten()
        .map(|s| s.display_name(s.self_id()));
    let (text, url, fill) = match request.avatar.clone() {
        NotificationAvatar::Image(url) => (request.title.clone(), Some(url), None),
        NotificationAvatar::Text { text, color } => {
            (text, None, Some(color).filter(|c| !c.is_empty()))
        }
    };
    let opening = window.clone();
    let (key, message_id, is_reminder) =
        (request.key.clone(), request.message_id, request.is_reminder);
    // Red hint before the message ([有人@我], [有新文件] ...), as in the conversation list
    let notice = (!is_reminder && click == NotificationClick::OpenTarget)
        .then(|| services().hub.session(account))
        .flatten()
        .and_then(|session| notice_label(session.unread_notice(&key)));
    rsx! {
        div {
            class: if leaving() { "notification leaving" } else { "notification" },
            onmouseenter: move |_| hovered.set(Some(true)),
            onmouseleave: move |_| hovered.set(Some(false)),
            ListRow {
                density: RowDensity::Large,
                leading: rsx! { AccountAvatar { account, text, url, fill, size: 44.0 } },
                title: request.title.clone(),
                meta: account_name,
                subtitle_prefix: rsx! {
                    if let Some(notice) = notice {
                        span { class: "notification-notice", "{notice}" }
                    }
                },
                subtitle: request.body.clone(),
                onclick: move |_| {
                    match click {
                        NotificationClick::OpenTarget => {
                            let target = (!is_reminder).then(|| key.clone());
                            open_notification_target(account, target, message_id);
                        }
                        NotificationClick::ShowMain => send_command(AppCommand::ShowMain),
                    }
                    opening.close();
                },
            }
        }
    }
}
