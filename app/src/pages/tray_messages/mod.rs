//! Tray message list: shown when hovering the icon while the tray flashes; lists conversations with
//! unread messages and message reminders. Click to open; closes shortly after the mouse leaves both
//! the icon and the list.

use crate::services::services;
use crate::shell::{WindowShell, use_window_args};
use crate::state::{AppCommand, use_app_command, use_session_event};
use crate::ui::*;
use crate::widgets::*;
use chrono::NaiveDateTime;
use dioxus::desktop::tao::dpi::PhysicalPosition;
use dioxus::desktop::{LogicalSize, use_window};
use dioxus::prelude::*;
use std::rc::Rc;
use std::time::Duration;
use zulip_core::ConversationKey;
use zulip_core::html::to_preview;
use zulip_core::notice::localize_reminder;
use zulip_core::push::DndLevel;
use zulip_core::text::time::{countdown, from_unix};
use zulip_session::{ConversationAvatar, Session, SessionEvent};

const WIDTH: f64 = 300.0;
const ROW_HEIGHT: f64 = 64.0;
const PADDING: f64 = 8.0;
const MAX_HEIGHT: f64 = 420.0;
/// Gap from the tray icon.
const GAP: f64 = 8.0;
/// The mouse within this distance (physical pixels) of the icon center still counts as on the icon.
const ICON_RADIUS: f64 = 24.0;
const POLL: Duration = Duration::from_millis(150);
/// Closes after the mouse has been off the icon and list for this long.
const LEAVE_DELAY: Duration = Duration::from_millis(500);
/// Periodic refresh of countdowns and unread state.
const REFRESH: Duration = Duration::from_secs(3);
/// Reminders about to expire: within 24 hours.
const UPCOMING_SECONDS: i64 = 24 * 3600;

/// List item: a conversation with unread messages, or a message reminder.
#[derive(Clone, Debug, PartialEq)]
struct Item {
    account: i64,
    account_name: Option<String>,
    /// A reminder has no conversation.
    key: Option<ConversationKey>,
    title: String,
    notice: Option<&'static str>,
    preview: String,
    avatar: ConversationAvatar,
    unread: usize,
    muted: bool,
    countdown: Option<String>,
    /// Reminders first (by reminder time), then conversations (newest first).
    order: (u8, i64),
}

fn reminder_item(
    session: &Session,
    account_name: Option<String>,
    preview: String,
    at: i64,
    now: NaiveDateTime,
) -> Item {
    Item {
        account: session.account_id(),
        account_name,
        key: None,
        title: "消息提醒".to_owned(),
        notice: None,
        preview: if preview.is_empty() {
            "你设置的消息提醒".to_owned()
        } else {
            preview
        },
        avatar: ConversationAvatar::Text {
            text: "提醒".to_owned(),
            color: String::new(),
        },
        unread: 0,
        muted: false,
        countdown: from_unix(at).map(|t| countdown(t, now)),
        order: (0, at),
    }
}

/// Unread messages in conversations that allow notifications, unread @-mentions or alert words in
/// muted conversations, unread messages that were already reminded, and reminders due within 24
/// hours.
fn tray_items() -> Vec<Item> {
    let sessions = services().hub.online_sessions();
    let multiple = sessions.len() > 1;
    let (now, now_unix) = (
        chrono::Local::now().naive_local(),
        chrono::Utc::now().timestamp(),
    );
    let mut items = Vec::new();
    for session in sessions {
        let name = multiple.then(|| session.display_name(session.self_id()));
        let alerts = session.unread_muted_alert_keys();
        for (key, row) in conversation_rows(&session) {
            if row.unread_count == 0 || (row.level != DndLevel::Notify && !alerts.contains(&key)) {
                continue;
            }
            items.push(Item {
                account: session.account_id(),
                account_name: name.clone(),
                title: session.display_title(&key),
                notice: notice_label(row.notice),
                preview: row.preview.clone(),
                avatar: row.avatar.clone(),
                unread: row.unread_count,
                muted: row.level != DndLevel::Notify,
                countdown: None,
                order: (1, -row.time),
                key: Some(key),
            });
        }
        for notice in session.delivered_reminders().iter().filter(|n| !n.is_read) {
            let preview = to_preview(&notice.html);
            items.push(reminder_item(
                &session,
                name.clone(),
                preview,
                notice.timestamp,
                now,
            ));
        }
        for reminder in session
            .pending_reminders()
            .iter()
            .filter(|r| !r.failed && r.deliver_at - now_unix <= UPCOMING_SECONDS)
        {
            let preview = to_preview(&localize_reminder(reminder.html()));
            items.push(reminder_item(
                &session,
                name.clone(),
                preview,
                reminder.deliver_at,
                now,
            ));
        }
    }
    items.sort_by_key(|item| item.order);
    items
}

/// Window position: centered on the icon; above the icon when the icon is in the lower half of the
/// screen, otherwise below; never beyond the screen.
fn place(
    anchor: (f64, f64),
    size: (f64, f64),
    screen: (f64, f64, f64, f64),
    gap: f64,
) -> (f64, f64) {
    let (sx, sy, sw, sh) = screen;
    let x = (anchor.0 - size.0 / 2.0).clamp(sx, (sx + sw - size.0).max(sx));
    let y = if anchor.1 > sy + sh / 2.0 {
        anchor.1 - size.1 - gap
    } else {
        anchor.1 + gap
    };
    (x, y.clamp(sy, (sy + sh - size.1).max(sy)))
}

#[component]
pub fn TrayMessagesWindow() -> Element {
    rsx! {
        WindowShell { resizable: false, TrayBody {} }
    }
}

/// The window argument is the screen position of the tray icon.
#[component]
fn TrayBody() -> Element {
    let anchor = use_window_args::<(f64, f64)>();
    let window = use_window();
    {
        let window = window.clone();
        use_app_command(move |command| {
            if command == AppCommand::TrayFlashingStopped {
                window.close();
            }
        });
    }
    let mut tick = use_signal(|| 0u32);
    use_future(move || async move {
        loop {
            tokio::time::sleep(REFRESH).await;
            tick += 1;
        }
    });
    use_session_event(move |event| {
        if matches!(
            event,
            SessionEvent::UnreadChanged(_)
                | SessionEvent::ConversationChanged(_)
                | SessionEvent::ConversationsChanged
                | SessionEvent::RemindersChanged
                | SessionEvent::SystemNoticesChanged
                | SessionEvent::Resynced
        ) {
            tick += 1;
        }
    });
    let items = use_memo(move || {
        let _ = tick();
        Rc::new(tray_items())
    });

    {
        let window = window.clone();
        use_effect(move || {
            let count = items.read().len();
            if count == 0 {
                window.close();
                return;
            }
            let height = (count as f64 * ROW_HEIGHT + PADDING).min(MAX_HEIGHT);
            let scale = window.scale_factor();
            window.set_min_inner_size(Some(LogicalSize::new(WIDTH, height)));
            window.set_inner_size(LogicalSize::new(WIDTH, height));
            let point = anchor();
            let monitor = window
                .monitor_from_point(point.0, point.1)
                .or_else(|| window.current_monitor());
            let Some(monitor) = monitor else { return };
            let (origin, area) = (monitor.position(), monitor.size());
            let (x, y) = place(
                point,
                (WIDTH * scale, height * scale),
                (
                    f64::from(origin.x),
                    f64::from(origin.y),
                    f64::from(area.width),
                    f64::from(area.height),
                ),
                GAP * scale,
            );
            window.set_outer_position(PhysicalPosition::new(x as i32, y as i32));
        });
    }

    // The system does not notify when the mouse leaves the tray icon: periodically check whether
    // the mouse is still on the icon or the list
    {
        let window = window.clone();
        use_future(move || {
            let window = window.clone();
            async move {
                let mut outside = Duration::ZERO;
                loop {
                    tokio::time::sleep(POLL).await;
                    let (Ok(cursor), Ok(position)) =
                        (window.cursor_position(), window.outer_position())
                    else {
                        continue;
                    };
                    let size = window.outer_size();
                    let point = *anchor.peek();
                    let near_icon = (cursor.x - point.0).abs() < ICON_RADIUS
                        && (cursor.y - point.1).abs() < ICON_RADIUS;
                    let inside = cursor.x >= f64::from(position.x)
                        && cursor.x < f64::from(position.x) + f64::from(size.width)
                        && cursor.y >= f64::from(position.y)
                        && cursor.y < f64::from(position.y) + f64::from(size.height);
                    if near_icon || inside {
                        outside = Duration::ZERO;
                    } else {
                        outside += POLL;
                        if outside >= LEAVE_DELAY {
                            window.close();
                            return;
                        }
                    }
                }
            }
        });
    }

    let list = items();
    rsx! {
        div { class: "tray-list scroll",
            for (index, item) in list.iter().cloned().enumerate() {
                TrayRow { key: "{index}", item }
            }
        }
    }
}

#[component]
fn TrayRow(item: Item) -> Element {
    let window = use_window();
    let (text, url, fill) = match item.avatar.clone() {
        ConversationAvatar::Image(url) => (item.title.clone(), url, None),
        ConversationAvatar::Text { text, color } => {
            (text, None, Some(color).filter(|c| !c.is_empty()))
        }
    };
    let (account, key) = (item.account, item.key.clone());
    rsx! {
        ListRow {
            density: RowDensity::Large,
            leading: rsx! { AccountAvatar { account, text, url, fill, size: 36.0 } },
            title: item.title.clone(),
            meta: item.account_name.clone(),
            subtitle_prefix: rsx! {
                if let Some(notice) = item.notice {
                    span { class: "tray-notice", "{notice}" }
                }
            },
            subtitle: item.preview.clone(),
            trailing: rsx! {
                if item.unread > 0 {
                    Badge { count: item.unread as u32, muted: item.muted }
                }
                if let Some(text) = item.countdown.clone() {
                    span { class: "text-caption", "{text}" }
                }
            },
            onclick: move |_| {
                window.close();
                open_notification_target(account, key.clone(), None);
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (f64, f64, f64, f64) = (0.0, 0.0, 1920.0, 1080.0);

    #[test]
    fn icon_in_the_lower_half_puts_the_list_above() {
        let (x, y) = place((1800.0, 1060.0), (300.0, 200.0), SCREEN, 8.0);
        assert_eq!(x, 1620.0);
        assert_eq!(y, 852.0);
    }

    #[test]
    fn icon_in_the_upper_half_puts_the_list_below() {
        let (x, y) = place((100.0, 10.0), (300.0, 200.0), SCREEN, 8.0);
        assert_eq!(x, 0.0);
        assert_eq!(y, 18.0);
    }

    #[test]
    fn list_stays_on_the_screen() {
        let (_, y) = place((960.0, 1070.0), (300.0, 2000.0), SCREEN, 8.0);
        assert_eq!(y, 0.0);
        let (x, _) = place((960.0, 500.0), (3000.0, 100.0), SCREEN, 8.0);
        assert_eq!(x, 0.0);
    }
}
