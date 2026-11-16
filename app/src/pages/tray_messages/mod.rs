//! Tray message list: shown when hovering the icon while the tray flashes; lists conversations with
//! unread messages and message reminders under the name of the account that received them. Click
//! to open; "忽略全部" stops the flashing (unread stays unread), "查看全部" opens every item in
//! standalone windows. Closes shortly after the mouse leaves both the icon and the list.

use crate::platform::{self, IconBox};
use crate::services::services;
use crate::shell::{WindowShell, close_window, use_window_args};
use crate::state::{AppCommand, send_command, use_app_command, use_session_event};
use crate::ui::*;
use crate::widgets::*;
use crate::windows;
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
/// Height of an account's name above its items, and of the buttons row at the bottom.
const HEAD_HEIGHT: f64 = 36.0;
const FOOT_HEIGHT: f64 = 40.0;
const MAX_HEIGHT: f64 = 420.0;
const POLL: Duration = Duration::from_millis(50);
/// Closes after the mouse has been off the list for this long (it closes at once when the mouse
/// leaves the icon without going onto the list).
const LEAVE_DELAY: Duration = Duration::from_millis(500);
/// Periodic refresh of countdowns and unread state.
const REFRESH: Duration = Duration::from_secs(3);
/// Reminders about to expire: within 24 hours.
const UPCOMING_SECONDS: i64 = 24 * 3600;

/// List item: a conversation with unread messages, or a message reminder.
#[derive(Clone, Debug, PartialEq)]
struct Item {
    account: i64,
    /// The account that received it (the list shows it above the account's items).
    account_name: String,
    /// A reminder has no conversation.
    key: Option<ConversationKey>,
    title: String,
    notice: Option<&'static str>,
    preview: String,
    avatar: ConversationAvatar,
    unread: usize,
    muted: bool,
    countdown: Option<String>,
    /// By account (in the order of the online accounts), then reminders first (by reminder time),
    /// then conversations (newest first).
    order: (usize, u8, i64),
}

fn reminder_item(
    session: &Session,
    (account_index, account_name): (usize, String),
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
        order: (account_index, 0, at),
    }
}

/// Unread messages in conversations that allow notifications, unread @-mentions or alert words in
/// muted conversations, unread messages that were already reminded, and reminders due within 24
/// hours.
fn tray_items() -> Vec<Item> {
    let sessions = services().hub.online_sessions();
    let (now, now_unix) = (
        chrono::Local::now().naive_local(),
        chrono::Utc::now().timestamp(),
    );
    let mut items = Vec::new();
    for (index, session) in sessions.into_iter().enumerate() {
        let name = session.display_name(session.self_id());
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
                order: (index, 1, -row.time),
                key: Some(key),
            });
        }
        for notice in session.delivered_reminders().iter().filter(|n| !n.is_read) {
            let preview = to_preview(&notice.html);
            items.push(reminder_item(
                &session,
                (index, name.clone()),
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
                (index, name.clone()),
                preview,
                reminder.deliver_at,
                now,
            ));
        }
    }
    items.sort_by_key(|item| item.order);
    items
}

/// Window position in the work area (`screen`: the screen without the taskbar): centered on the
/// icon; right on the taskbar, above it when the icon is in the lower half of the screen,
/// otherwise below it; never beyond the work area.
fn place(anchor: (f64, f64), size: (f64, f64), screen: (f64, f64, f64, f64)) -> (f64, f64) {
    let (sx, sy, sw, sh) = screen;
    let x = (anchor.0 - size.0 / 2.0).clamp(sx, (sx + sw - size.0).max(sx));
    let y = if anchor.1 > sy + sh / 2.0 {
        (sy + sh - size.1).max(sy)
    } else {
        sy
    };
    (x, y)
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
    let anchor = use_window_args::<IconBox>();
    let window = use_window();
    use_app_command(move |command| {
        if command == AppCommand::TrayFlashingStopped {
            close_list();
        }
    });
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
                close_list();
                return;
            }
            let accounts = account_count(&items.read());
            let height =
                (count as f64 * ROW_HEIGHT + accounts as f64 * HEAD_HEIGHT + FOOT_HEIGHT + PADDING)
                    .min(MAX_HEIGHT);
            let scale = window.scale_factor();
            window.set_min_inner_size(Some(LogicalSize::new(WIDTH, height)));
            window.set_inner_size(LogicalSize::new(WIDTH, height));
            let icon = anchor();
            let center = (icon.x + icon.width / 2.0, icon.y + icon.height / 2.0);
            let Some(area) = platform::work_area_at(center.0 as i32, center.1 as i32) else {
                return;
            };
            let (x, y) = place(
                center,
                (WIDTH * scale, height * scale),
                (
                    f64::from(area.x),
                    f64::from(area.y),
                    f64::from(area.width),
                    f64::from(area.height),
                ),
            );
            window.set_outer_position(PhysicalPosition::new(x as i32, y as i32));
            // Created hidden (a popup): shown once in place
            window.set_visible(true);
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
                // Whether the mouse has been on the list (not only on the icon)
                let mut on_list = false;
                loop {
                    tokio::time::sleep(POLL).await;
                    let (Ok(cursor), Ok(position)) =
                        (window.cursor_position(), window.outer_position())
                    else {
                        continue;
                    };
                    let size = window.outer_size();
                    let icon = *anchor.peek();
                    let list = (
                        f64::from(position.x),
                        f64::from(position.y),
                        f64::from(size.width),
                        f64::from(size.height),
                    );
                    let inside = cursor.x >= list.0
                        && cursor.x < list.0 + list.2
                        && cursor.y >= list.1
                        && cursor.y < list.1 + list.3;
                    if inside {
                        on_list = true;
                        outside = Duration::ZERO;
                    } else if on_icon((cursor.x, cursor.y), icon, list) {
                        outside = Duration::ZERO;
                    } else if !on_list {
                        close_list();
                        return;
                    } else {
                        outside += POLL;
                        if outside >= LEAVE_DELAY {
                            close_list();
                            return;
                        }
                    }
                }
            }
        });
    }

    let list = items();
    let ignore = move |_| {
        close_list();
        send_command(AppCommand::StopTrayFlashing);
    };
    let view_all = {
        let list = list.clone();
        move |_| {
            close_list();
            open_all(&list);
            send_command(AppCommand::StopTrayFlashing);
        }
    };
    rsx! {
        div { class: "tray-list",
            div { class: "tray-items scroll",
                for (index, item) in list.iter().cloned().enumerate() {
                    div { key: "{index}",
                        if index == 0 || list[index - 1].account != item.account {
                            div { class: "tray-head", "{item.account_name}" }
                        }
                        TrayRow { item }
                    }
                }
            }
            div { class: "tray-foot",
                Button { kind: ButtonKind::Flat, class: "tray-action", onclick: ignore, "忽略全部" }
                Button { kind: ButtonKind::Flat, class: "tray-action", onclick: view_all, "查看全部" }
            }
        }
    }
}

/// Number of accounts with items (each has its name above its items).
fn account_count(items: &[Item]) -> usize {
    items
        .iter()
        .enumerate()
        .filter(|(i, item)| *i == 0 || items[i - 1].account != item.account)
        .count()
}

/// "查看全部": every conversation in its own standalone window (whatever the window setting), and
/// the reminders of each account with reminders.
fn open_all(items: &[Item]) {
    let mut reminders_opened = Vec::new();
    for item in items {
        match &item.key {
            Some(key) => windows::open_chat_standalone(item.account, key.clone()),
            None if !reminders_opened.contains(&item.account) => {
                reminders_opened.push(item.account);
                windows::open_reminders(item.account);
            }
            None => {}
        }
    }
}

#[component]
fn TrayRow(item: Item) -> Element {
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
                close_list();
                open_notification_target(account, key.clone(), None);
            },
        }
    }
}

/// Whether the mouse is on the tray icon, or straight on the way from it to the list (`list`: x, y,
/// width, height; it may pass over the icons between), so moving onto the list does not count as
/// leaving the icon. Next to the icon (on another tray icon) it does not count.
fn on_icon(cursor: (f64, f64), icon: IconBox, list: (f64, f64, f64, f64)) -> bool {
    let (x, y) = cursor;
    if x < icon.x || x >= icon.x + icon.width {
        return false;
    }
    let (top, bottom) = (icon.y, icon.y + icon.height);
    let list_bottom = list.1 + list.3;
    if list_bottom <= top {
        // The list is above the icon (taskbar at the bottom)
        y >= list_bottom - 1.0 && y < bottom
    } else {
        y >= top && y <= list.1 + 1.0
    }
}

/// Closes the list, forgetting it at once so the next hover opens a new one (not the closing
/// one).
fn close_list() {
    close_window(windows::TRAY_MESSAGES);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_way_from_the_icon_to_the_list_counts_as_the_icon() {
        // List right on a bottom taskbar, a 36 pixel icon 12 pixels below its top
        let list = (1600.0, 800.0, 300.0, 232.0);
        let icon = IconBox {
            x: 1800.0,
            y: 1044.0,
            width: 36.0,
            height: 36.0,
        };
        assert!(on_icon((1810.0, 1050.0), icon, list));
        assert!(on_icon((1810.0, 1035.0), icon, list));
        // The next icon to the right / left, and below the icon
        assert!(!on_icon((1840.0, 1050.0), icon, list));
        assert!(!on_icon((1795.0, 1050.0), icon, list));
        assert!(!on_icon((1810.0, 1081.0), icon, list));
    }

    // Work area of a 1920x1080 screen with a 48 pixel taskbar at the bottom
    const SCREEN: (f64, f64, f64, f64) = (0.0, 0.0, 1920.0, 1032.0);

    #[test]
    fn icon_in_the_lower_half_puts_the_list_right_on_the_taskbar() {
        let (x, y) = place((1800.0, 1060.0), (300.0, 200.0), SCREEN);
        assert_eq!(x, 1620.0);
        assert_eq!(y, 832.0);
    }

    #[test]
    fn icon_in_the_upper_half_puts_the_list_right_below_the_taskbar() {
        let (x, y) = place((100.0, 10.0), (300.0, 200.0), (0.0, 48.0, 1920.0, 1032.0));
        assert_eq!(x, 0.0);
        assert_eq!(y, 48.0);
    }

    #[test]
    fn list_stays_on_the_screen() {
        let (_, y) = place((960.0, 1070.0), (300.0, 2000.0), SCREEN);
        assert_eq!(y, 0.0);
        let (x, _) = place((960.0, 500.0), (3000.0, 100.0), SCREEN);
        assert_eq!(x, 0.0);
    }
}
