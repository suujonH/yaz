//! Desktop notification popup: bottom right of the screen, click to open the conversation, closes
//! automatically after a few seconds; it does not close while the mouse rests on it.

use crate::services::services;
use crate::shell::{WindowShell, use_window_args};
use crate::state::{AppCommand, send_command};
use crate::ui::*;
use crate::widgets::*;
use dioxus::desktop::tao::dpi::PhysicalPosition;
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use std::time::Duration;
use zulip_session::{NotificationAvatar, NotificationRequest};

const VISIBLE_FOR: Duration = Duration::from_secs(6);
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
            if let Some(monitor) = window.current_monitor() {
                let (area, origin, size) =
                    (monitor.size(), monitor.position(), window.outer_size());
                window.set_outer_position(PhysicalPosition::new(
                    origin.x + area.width as i32 - size.width as i32 - SCREEN_MARGIN,
                    origin.y + area.height as i32 - size.height as i32 - SCREEN_MARGIN * 4,
                ));
            }
        }
    });
    let mut hovered = use_signal(|| false);
    {
        let window = window.clone();
        use_resource(move || {
            let _ = args.read();
            let paused = hovered();
            let window = window.clone();
            async move {
                if !paused {
                    tokio::time::sleep(VISIBLE_FOR).await;
                    window.close();
                }
            }
        });
    }
    let NotificationArgs {
        account,
        request,
        click,
    } = args();
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
    let closing = window.clone();
    let opening = window.clone();
    let (key, message_id, is_reminder) =
        (request.key.clone(), request.message_id, request.is_reminder);
    rsx! {
        div {
            class: "notification",
            onmouseenter: move |_| hovered.set(true),
            onmouseleave: move |_| hovered.set(false),
            ListRow {
                density: RowDensity::Large,
                leading: rsx! { AccountAvatar { account, text, url, fill, size: 44.0 } },
                title: request.title.clone(),
                meta: account_name,
                subtitle: request.body.clone(),
                trailing: rsx! {
                    IconButton {
                        icon: IconName::Close,
                        size: 24.0,
                        icon_size: 12.0,
                        onclick: move |e: MouseEvent| {
                            e.stop_propagation();
                            closing.close();
                        },
                    }
                },
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
