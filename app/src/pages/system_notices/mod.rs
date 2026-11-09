//! System notices: the standalone window and the right side of the main window share
//! [`SystemNoticesPane`].

use crate::shell::{WindowShell, use_presented};
use crate::state::{use_app_state, use_session_data};
use crate::ui::*;
use crate::widgets::*;
use dioxus::prelude::*;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::html::to_plain_text;
use zulip_core::notice::SystemNoticeCategory;
use zulip_core::text::time::{from_unix, list_time};
use zulip_session::{Session, SessionError, SessionEvent, SystemNotice};

/// Content longer than this is collapsed by default.
const LONG_TEXT_CHARS: usize = 160;
const LONG_TEXT_LINES: usize = 4;
const ALL: &str = "all";

/// Order in which they appear in the filter.
const CATEGORIES: [SystemNoticeCategory; 6] = [
    SystemNoticeCategory::NewChannels,
    SystemNoticeCategory::Signups,
    SystemNoticeCategory::ZulipUpdates,
    SystemNoticeCategory::Moderation,
    SystemNoticeCategory::Direct,
    SystemNoticeCategory::Other,
];

#[derive(Clone, Debug, Default, PartialEq)]
struct Data {
    notices: Vec<SystemNotice>,
    /// Avatar of the notification bot.
    bot_avatar: Option<String>,
    use_24h: bool,
}

fn read_data(session: &Arc<Session>) -> Data {
    Data {
        notices: session.system_notices(),
        bot_avatar: session
            .notification_bot_id()
            .and_then(|id| session.user(id))
            .and_then(|u| u.avatar_url),
        use_24h: session.use_24h(),
    }
}

fn data_changed(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::SystemNoticesChanged
            | SessionEvent::UserChanged(_)
            | SessionEvent::UsersChanged
    )
}

fn category_id(category: SystemNoticeCategory) -> String {
    format!("{category:?}")
}

fn is_long(text: &str) -> bool {
    text.chars().count() > LONG_TEXT_CHARS || text.lines().count() >= LONG_TEXT_LINES
}

#[component]
pub fn SystemNoticesPane() -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let data = use_session_data(data_changed, read_data);
    let mut filter = use_signal(|| None::<SystemNoticeCategory>);
    // Unread notices that appeared since opening: the red dot stays until closed
    let mut seen = use_signal(HashSet::<i64>::new);
    // Not while rendered in advance in the hidden spare window
    let presented = use_presented();
    use_effect(move || {
        if !presented() {
            return;
        }
        let unread: Vec<i64> = data
            .read()
            .notices
            .iter()
            .filter(|n| !n.is_read)
            .map(|n| n.id)
            .collect();
        if unread.is_empty() {
            return;
        }
        seen.write().extend(unread.iter().copied());
        if let Some(session) = state.session.peek().clone() {
            spawn(async move {
                if let Err(error) = session.mark_read(&unread).await {
                    log::warn!("标记系统通知已读失败：{error}");
                }
            });
        }
    });

    let snapshot = data();
    let present: Vec<SystemNoticeCategory> = CATEGORIES
        .into_iter()
        .filter(|c| snapshot.notices.iter().any(|n| n.category == *c))
        .collect();
    let selected = filter().filter(|c| present.contains(c));
    let shown: Rc<Vec<SystemNotice>> = Rc::new(
        snapshot
            .notices
            .iter()
            .filter(|n| selected.is_none_or(|c| n.category == c))
            .cloned()
            .collect(),
    );
    let keys: Rc<Vec<u64>> = Rc::new(shown.iter().map(|n| n.id as u64).collect());
    let options: Vec<SelectOption> = std::iter::once(SelectOption::new(ALL, "全部"))
        .chain(
            present
                .iter()
                .map(|c| SelectOption::new(category_id(*c), c.text())),
        )
        .collect();
    let value = selected.map_or_else(|| ALL.to_owned(), category_id);
    let has_any = !snapshot.notices.is_empty();
    let status = shown.is_empty().then(|| "暂无系统通知".to_owned());
    let (bot_avatar, use_24h) = (snapshot.bot_avatar.clone(), snapshot.use_24h);
    let seen_now = seen.read().clone();
    rsx! {
        div { class: "notices",
            PaneHeader { title: "系统通知",
                div { class: "pane-select",
                    Select {
                        options,
                        value,
                        onchange: move |id: String| {
                            filter.set(CATEGORIES.into_iter().find(|c| category_id(*c) == id));
                        },
                    }
                }
                Tooltip { text: "清空",
                    IconButton {
                        icon: IconName::Delete,
                        disabled: !has_any,
                        onclick: move |_| {
                            let Some(session) = state.session.peek().clone() else { return };
                            spawn(async move {
                                overlays
                                    .delete("全部系统通知", Some("只在本机不再显示，不会删除服务器上的消息。"), || async {
                                        session.clear_notices();
                                        Ok::<(), SessionError>(())
                                    })
                                    .await;
                            });
                        },
                    }
                }
            }
            ResultList {
                keys,
                status,
                estimate: 96.0,
                on_more: move |()| {},
                render: move |i: usize| rsx! {
                    NoticeRow {
                        notice: shown[i].clone(),
                        unread: seen_now.contains(&shown[i].id),
                        bot_avatar: bot_avatar.clone(),
                        use_24h,
                    }
                },
            }
        }
    }
}

/// One notice: the avatar is the first user mentioned in the notice, or the notification bot when
/// there is none.
#[component]
fn NoticeRow(
    notice: SystemNotice,
    unread: bool,
    bot_avatar: Option<String>,
    use_24h: bool,
) -> Element {
    let state = use_app_state();
    let mut expanded = use_signal(|| false);
    let session = state.session.read().clone();
    let mentioned = session.as_ref().and_then(|s| {
        notice
            .mentioned_user_ids
            .iter()
            .find_map(|id| s.user(*id).map(|u| (s.display_name(*id), u.avatar_url)))
    });
    let (name, avatar) = mentioned.unwrap_or_else(|| ("系统通知".to_owned(), bot_avatar));
    let text = to_plain_text(&notice.html).trim().to_owned();
    let long = is_long(&text);
    let time = from_unix(notice.timestamp)
        .map(|t| list_time(t, chrono::Local::now().naive_local(), use_24h));
    rsx! {
        ListRow {
            density: RowDensity::Large,
            leading: rsx! { RemoteAvatar { text: name.clone(), url: avatar, size: 40.0 } },
            title: name,
            meta: time,
            subtitle_prefix: rsx! {
                if unread {
                    Badge { dot: true }
                }
                div { class: if long && !expanded() { "result-text notice-text clamp" } else { "result-text notice-text" },
                    "{text}"
                }
            },
            trailing: rsx! {
                if long {
                    Button {
                        kind: ButtonKind::Flat,
                        onclick: move |_| expanded.toggle(),
                        if expanded() { "收起" } else { "展开" }
                    }
                }
                span { class: "text-caption", "{notice.category.text()}" }
            },
        }
    }
}

#[component]
pub fn SystemNoticesWindow() -> Element {
    rsx! {
        WindowShell { title: "系统通知", NoticesFrame {} }
    }
}

#[component]
fn NoticesFrame() -> Element {
    use_window_keys(|_| {});
    rsx! {
        SystemNoticesPane {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_notices_are_collapsed() {
        assert!(!is_long("新成员加入"));
        assert!(is_long(&"字".repeat(LONG_TEXT_CHARS + 1)));
        assert!(is_long("一\n二\n三\n四"));
        assert!(!is_long("一\n二\n三"));
    }
}
