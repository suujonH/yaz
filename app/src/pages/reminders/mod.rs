//! Message reminders window: pending, reminded, scheduled messages. Viewing, changing the time and
//! deleting all go through a unified flow.

use crate::shell::{WindowShell, use_presented};
use crate::state::{use_app_state, use_session_data};
use crate::ui::*;
use crate::widgets::*;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::html::to_plain_text;
use zulip_core::model::{Reminder, ScheduledMessage};
use zulip_core::notice::localize_reminder;
use zulip_core::text::time::{countdown, from_unix, precise_time};
use zulip_session::{DeleteTarget, Session, SessionError, SessionEvent, SystemNotice};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Pending,
    Delivered,
    Scheduled,
}

impl Tab {
    const ALL: [Self; 3] = [Self::Pending, Self::Delivered, Self::Scheduled];

    const fn label(self) -> &'static str {
        match self {
            Self::Pending => "待提醒",
            Self::Delivered => "已提醒",
            Self::Scheduled => "定时消息",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Entry {
    Pending(Reminder),
    Delivered(SystemNotice),
    Scheduled(ScheduledMessage),
}

impl Entry {
    fn key(&self) -> u64 {
        text_id(&match self {
            Self::Pending(r) => format!("p{}", r.id),
            Self::Delivered(n) => format!("d{}", n.id),
            Self::Scheduled(m) => format!("s{}", m.id),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Data {
    pending: Vec<Reminder>,
    delivered: Vec<SystemNotice>,
    scheduled: Vec<ScheduledMessage>,
    use_24h: bool,
}

fn read_data(session: &Arc<Session>) -> Data {
    Data {
        pending: session.pending_reminders(),
        delivered: session.delivered_reminders(),
        scheduled: session.scheduled_messages(),
        use_24h: session.use_24h(),
    }
}

fn data_changed(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::RemindersChanged
            | SessionEvent::ScheduledMessagesChanged
            | SessionEvent::SystemNoticesChanged
            | SessionEvent::SettingsChanged(_)
    )
}

/// Text of the reminder content: translated into Chinese, with markup removed.
fn reminder_text(html: &str) -> String {
    to_plain_text(&localize_reminder(html)).trim().to_owned()
}

fn timestamp_text(timestamp: i64, use_24h: bool) -> String {
    from_unix(timestamp).map_or_else(String::new, |t| {
        precise_time(t, chrono::Local::now().naive_local(), use_24h)
    })
}

#[component]
pub fn RemindersWindow() -> Element {
    rsx! {
        WindowShell { title: "消息提醒", RemindersFrame {} }
    }
}

#[component]
fn RemindersFrame() -> Element {
    use_window_keys(|_| {});
    rsx! {
        RemindersPane {}
    }
}

/// Message reminders: on the right side of the main window, or in its own window in classic mode.
#[component]
pub fn RemindersPane() -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let data = use_session_data(data_changed, read_data);
    let mut tab = use_signal(|| {
        if data.peek().delivered.iter().any(|n| !n.is_read) {
            Tab::Delivered
        } else {
            Tab::Pending
        }
    });
    // When viewing "reminded", mark unread reminders as read
    // Not while rendered in advance in the hidden spare window
    let presented = use_presented();
    use_effect(move || {
        if !presented() || tab() != Tab::Delivered {
            return;
        }
        let unread: Vec<i64> = data
            .read()
            .delivered
            .iter()
            .filter(|n| !n.is_read)
            .map(|n| n.id)
            .collect();
        let Some(session) = state.session.peek().clone() else {
            return;
        };
        if !unread.is_empty() {
            spawn(async move {
                if let Err(error) = session.mark_read(&unread).await {
                    log::warn!("标记提醒已读失败：{error}");
                }
            });
        }
    });

    let snapshot = data();
    let current = tab();
    let entries: Rc<Vec<Entry>> = Rc::new(match current {
        Tab::Pending => snapshot
            .pending
            .iter()
            .cloned()
            .map(Entry::Pending)
            .collect(),
        Tab::Delivered => snapshot
            .delivered
            .iter()
            .cloned()
            .map(Entry::Delivered)
            .collect(),
        Tab::Scheduled => snapshot
            .scheduled
            .iter()
            .cloned()
            .map(Entry::Scheduled)
            .collect(),
    });
    let keys: Rc<Vec<u64>> = Rc::new(entries.iter().map(Entry::key).collect());
    let unread_delivered = snapshot.delivered.iter().any(|n| !n.is_read);
    let status = entries.is_empty().then(|| {
        match current {
            Tab::Pending => "没有待提醒的消息",
            Tab::Delivered => "还没有到时间的提醒",
            Tab::Scheduled => "没有定时消息",
        }
        .to_owned()
    });
    let use_24h = snapshot.use_24h;
    rsx! {
        SplitView {
            nav: rsx! {
                for item in Tab::ALL {
                    NavRow {
                        key: "{item.label()}",
                        label: item.label(),
                        dot: item == Tab::Delivered && unread_delivered,
                        selected: item == current,
                        onclick: move |_| tab.set(item),
                    }
                }
            },
            PaneHeader { title: current.label(),
                if current == Tab::Delivered && !entries.is_empty() {
                    Button {
                        kind: ButtonKind::Flat,
                        onclick: move |_| {
                            let Some(session) = state.session.peek().clone() else { return };
                            spawn(async move {
                                overlays
                                    .delete("全部已提醒的消息", Some("只在本机不再显示。"), || async {
                                        session.clear_delivered_reminders();
                                        Ok::<(), SessionError>(())
                                    })
                                    .await;
                            });
                        },
                        "清除全部"
                    }
                }
            }
            ResultList {
                keys,
                status,
                estimate: 72.0,
                on_more: move |()| {},
                render: move |i: usize| rsx! {
                    EntryRow { entry: entries[i].clone(), use_24h }
                },
            }
        }
    }
}

/// Change the reminder time: recreate it with the new time, then delete the old one.
async fn change_reminder(overlays: Overlays, session: Arc<Session>, reminder: Reminder) {
    let Some(at) = ask_future_time(overlays, "修改提醒时间", Some(reminder.deliver_at)).await
    else {
        return;
    };
    let Some(_) = overlays
        .attempt(
            "修改失败",
            session.create_reminder(reminder.target_message_id, at),
        )
        .await
    else {
        return;
    };
    overlays
        .attempt(
            "修改失败",
            session.delete(DeleteTarget::Reminder(reminder.id)),
        )
        .await;
}

/// Change the send time of a scheduled message: recreate it with the new time, then delete the old
/// one.
async fn change_scheduled(overlays: Overlays, session: Arc<Session>, message: ScheduledMessage) {
    let Some(at) = ask_future_time(overlays, "修改发送时间", Some(message.deliver_at)).await
    else {
        return;
    };
    let Some(_) = overlays
        .attempt(
            "修改失败",
            session.schedule_message(&message.key, &message.content, at),
        )
        .await
    else {
        return;
    };
    overlays
        .attempt(
            "修改失败",
            session.delete(DeleteTarget::ScheduledMessage(message.id)),
        )
        .await;
}

/// Actions for one reminder or scheduled message.
#[derive(Clone, Debug, PartialEq)]
struct Actions {
    view: Option<i64>,
    change: Option<Entry>,
    delete: Entry,
}

fn actions_of(entry: &Entry) -> Actions {
    Actions {
        view: match entry {
            Entry::Pending(r) => Some(r.target_message_id),
            Entry::Delivered(n) => n.target_message_id,
            Entry::Scheduled(_) => None,
        },
        change: matches!(entry, Entry::Pending(_) | Entry::Scheduled(_)).then(|| entry.clone()),
        delete: entry.clone(),
    }
}

async fn view_message(overlays: Overlays, session: Arc<Session>, id: i64) {
    locate_in_chat(overlays, &session, None, id).await;
}

async fn change(overlays: Overlays, session: Arc<Session>, entry: Entry) {
    match entry {
        Entry::Pending(reminder) => change_reminder(overlays, session, reminder).await,
        Entry::Scheduled(message) => change_scheduled(overlays, session, message).await,
        Entry::Delivered(_) => {}
    }
}

async fn remove(overlays: Overlays, session: Arc<Session>, entry: Entry) {
    match entry {
        Entry::Pending(r) => {
            overlays
                .delete("这条提醒", None, || {
                    session.delete(DeleteTarget::Reminder(r.id))
                })
                .await;
        }
        Entry::Delivered(n) => {
            overlays
                .delete(
                    "这条已提醒的消息",
                    Some("只在本机不再显示。"),
                    || session.delete(DeleteTarget::DeliveredReminder(n.id)),
                )
                .await;
        }
        Entry::Scheduled(m) => {
            overlays
                .delete("这条定时消息", None, || {
                    session.delete(DeleteTarget::ScheduledMessage(m.id))
                })
                .await;
        }
    }
}

#[component]
fn EntryRow(entry: Entry, use_24h: bool) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let Some(session) = state.session.read().clone() else {
        return rsx! {};
    };
    let now = chrono::Local::now().naive_local();
    let (title, meta, text, failed) = match &entry {
        Entry::Pending(r) => (
            timestamp_text(r.deliver_at, use_24h),
            from_unix(r.deliver_at).map(|t| countdown(t, now)),
            reminder_text(r.html()),
            r.failed,
        ),
        Entry::Delivered(n) => (
            timestamp_text(n.timestamp, use_24h),
            None,
            to_plain_text(&n.html).trim().to_owned(),
            false,
        ),
        Entry::Scheduled(m) => (
            format!(
                "{} · {}",
                session.display_title(&m.key),
                timestamp_text(m.deliver_at, use_24h)
            ),
            from_unix(m.deliver_at).map(|t| countdown(t, now)),
            to_plain_text(m.html()).trim().to_owned(),
            m.failed,
        ),
    };
    let actions = actions_of(&entry);
    let delete_label = if matches!(entry, Entry::Delivered(_)) {
        "清除"
    } else {
        "删除"
    };
    let menu_actions = actions.clone();
    let (view_session, change_session, delete_session) =
        (session.clone(), session.clone(), session.clone());
    rsx! {
        ListRow {
            density: RowDensity::Large,
            leading: rsx! { Icon { name: IconName::Time, size: 18.0 } },
            title,
            meta,
            subtitle_prefix: rsx! {
                if failed {
                    span { class: "reminder-failed", if matches!(entry, Entry::Scheduled(_)) { "发送失败" } else { "提醒失败" } }
                }
                div { class: "result-text", "{text}" }
            },
            trailing: rsx! {
                if let Some(id) = actions.view {
                    Button {
                        kind: ButtonKind::Flat,
                        onclick: move |_| {
                            let session = view_session.clone();
                            spawn(async move { view_message(overlays, session, id).await });
                        },
                        "查看消息"
                    }
                }
                if let Some(target) = actions.change.clone() {
                    Button {
                        kind: ButtonKind::Flat,
                        onclick: move |_| {
                            let (session, target) = (change_session.clone(), target.clone());
                            spawn(async move { change(overlays, session, target).await });
                        },
                        "修改时间"
                    }
                }
                Button {
                    kind: ButtonKind::Flat,
                    onclick: {
                        let target = actions.delete.clone();
                        move |_| {
                            let (session, target) = (delete_session.clone(), target.clone());
                            spawn(async move { remove(overlays, session, target).await });
                        }
                    },
                    "{delete_label}"
                }
            },
            menu: move |()| {
                let mut items = Vec::new();
                if let Some(id) = menu_actions.view {
                    let session = session.clone();
                    items.push(MenuEntry::item("查看消息", move || {
                        spawn_forever(view_message(overlays, session.clone(), id));
                    }));
                }
                if let Some(target) = menu_actions.change.clone() {
                    let session = session.clone();
                    items.push(MenuEntry::item("修改时间", move || {
                        spawn_forever(change(overlays, session.clone(), target.clone()));
                    }));
                }
                let (session, target) = (session.clone(), menu_actions.delete.clone());
                items.push(
                    MenuEntry::item(delete_label, move || {
                        spawn_forever(remove(overlays, session.clone(), target.clone()));
                    })
                    .danger(),
                );
                items
            },
        }
    }
}
