//! 托盘消息列表：托盘闪烁时悬停图标显示，列出有未读的会话和消息提醒。
//! 点击打开；鼠标离开图标和列表一会儿后关闭。

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
/// 与托盘图标的间距。
const GAP: f64 = 8.0;
/// 鼠标离图标中心这么近（物理像素）算还在图标上。
const ICON_RADIUS: f64 = 24.0;
const POLL: Duration = Duration::from_millis(150);
/// 鼠标离开图标和列表超过这么久就关闭。
const LEAVE_DELAY: Duration = Duration::from_millis(500);
/// 倒计时和未读的定期刷新。
const REFRESH: Duration = Duration::from_secs(3);
/// 即将到时的提醒：24 小时内。
const UPCOMING_SECONDS: i64 = 24 * 3600;

/// 列表项：有未读的会话，或消息提醒。
#[derive(Clone, Debug, PartialEq)]
struct Item {
    account: i64,
    account_name: Option<String>,
    /// 提醒没有会话。
    key: Option<ConversationKey>,
    title: String,
    notice: Option<&'static str>,
    preview: String,
    avatar: ConversationAvatar,
    unread: usize,
    muted: bool,
    countdown: Option<String>,
    /// 提醒在前（按提醒时间），会话在后（最新的在前）。
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

/// 允许提醒的会话里的未读，免打扰会话里 @ 我或提醒词的未读，未读的已提醒，以及 24 小时内将到时的提醒。
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

/// 窗口的位置：以图标为中心；图标在屏幕下半部分时在图标上方，否则在下方；不超出屏幕。
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

/// 窗口参数是托盘图标的屏幕位置。
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

    // 系统不通知鼠标离开托盘图标：定时检查鼠标是否还在图标或列表上
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
