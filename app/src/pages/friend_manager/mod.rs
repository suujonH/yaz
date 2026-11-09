//! Friend manager window: filter by group, a table showing nickname / note / group, edit notes,
//! move groups, and set groups in bulk after multi-select.

use crate::shell::WindowShell;
use crate::state::{use_app_state, use_session_data};
use crate::ui::*;
use crate::widgets::*;
use dioxus::prelude::*;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::contains_ignore_case;
use zulip_core::model::{FriendGroup, RemarkTarget};
use zulip_plugin_host::ContextMenuTarget;
use zulip_session::{FriendSectionKind, RenameTarget, Session, SessionEvent};

const REMARK_MAX: usize = 32;
const NO_GROUP: &str = "none";
/// ID of the "new group" item when choosing a group.
const NEW_GROUP_ID: u64 = u64::MAX;

#[derive(Clone, Debug, Default, PartialEq)]
struct Friend {
    id: i64,
    nickname: String,
    remark: String,
    avatar: Option<String>,
    group: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Data {
    friends: Vec<Friend>,
    groups: Vec<FriendGroup>,
}

fn contacts_changed(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ContactsChanged | SessionEvent::UserChanged(_) | SessionEvent::UsersChanged
    )
}

fn read_data(session: &Arc<Session>) -> Data {
    let sections = session.friend_sections();
    let mut group_of: HashMap<i64, i64> = HashMap::new();
    for section in &sections {
        if let FriendSectionKind::Group(id) = section.kind {
            group_of.extend(section.items.iter().map(|item| (item.user_id, id)));
        }
    }
    let mut friends: Vec<Friend> = sections
        .iter()
        .filter(|section| section.kind == FriendSectionKind::All)
        .flat_map(|section| section.items.iter())
        .map(|item| {
            let user = session.user(item.user_id);
            Friend {
                id: item.user_id,
                nickname: user
                    .as_ref()
                    .map_or_else(|| item.display_name.clone(), |u| u.full_name.clone()),
                remark: session
                    .remark(&RemarkTarget::User(item.user_id))
                    .unwrap_or_default(),
                avatar: item.avatar_url.clone(),
                group: group_of.get(&item.user_id).copied(),
            }
        })
        .collect();
    friends.sort_by_key(|f| (f.nickname.to_lowercase(), f.id));
    Data {
        friends,
        groups: session.friend_groups(),
    }
}

#[component]
pub fn FriendManagerWindow() -> Element {
    rsx! {
        WindowShell { title: "好友管理器", FriendManagerBody {} }
    }
}

/// Choose a group: my friends, an existing group, or create a new one. `None` on cancel;
/// `Some(None)` is "my friends".
async fn pick_group(
    overlays: Overlays,
    session: &Arc<Session>,
    groups: &[FriendGroup],
) -> Option<Option<i64>> {
    let mut items = vec![PickerItem::new(0, "我的好友")];
    items.extend(
        groups
            .iter()
            .map(|g| PickerItem::new(g.id as u64, g.name.clone())),
    );
    items.push(PickerItem::new(NEW_GROUP_ID, "新建分组…"));
    let picked = overlays.pick(Picker::single("设置分组", items)).await?;
    match picked.first().copied()? {
        0 => Some(None),
        NEW_GROUP_ID => add_friend_group(overlays, session)
            .await
            .map(|group| Some(group.id)),
        id => Some(Some(id as i64)),
    }
}

#[component]
fn FriendManagerBody() -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    use_window_keys(|_| {});
    let data = use_session_data(contacts_changed, read_data);
    let mut filter = use_signal(|| None::<i64>);
    let mut search = use_signal(String::new);
    let selection = use_selection();
    use_effect(move || selection.set_active(true));

    // When a group is deleted, go back to all friends
    use_effect(move || {
        if let Some(id) = filter()
            && !data.read().groups.iter().any(|g| g.id == id)
        {
            filter.set(None);
        }
    });
    let visible = use_memo(move || {
        let search_text = search();
        let needle = search_text.trim();
        let wanted = filter();
        let all = data.read();
        Rc::new(
            all.friends
                .iter()
                .filter(|f| wanted.is_none_or(|group| f.group == Some(group)))
                .filter(|f| {
                    contains_ignore_case(&f.nickname, needle)
                        || contains_ignore_case(&f.remark, needle)
                })
                .cloned()
                .collect::<Vec<Friend>>(),
        )
    });
    // Rows hidden by search or by moving no longer count as selected
    use_effect(move || {
        let shown: HashSet<i64> = visible.read().iter().map(|f| f.id).collect();
        selection.retain(|id| shown.contains(&id));
    });
    let keys = use_memo(move || {
        Rc::new(
            visible
                .read()
                .iter()
                .map(|f| f.id as u64)
                .collect::<Vec<u64>>(),
        )
    });

    let snapshot = data();
    let groups = Rc::new(snapshot.groups.clone());
    let list = visible();
    let all_selected = !list.is_empty() && selection.count() == list.len();
    let session = state.session.read().clone();
    let current = filter();
    let title = current
        .and_then(|id| snapshot.groups.iter().find(|g| g.id == id))
        .map_or("全部好友", |g| g.name.as_str())
        .to_owned();
    let status = list.is_empty().then(|| "没有找到相关的好友".to_owned());
    let render_groups = groups.clone();
    let rows = list.clone();

    rsx! {
        SplitView {
            nav: rsx! {
                NavRow {
                    label: "全部好友",
                    count: snapshot.friends.len().to_string(),
                    selected: current.is_none(),
                    onclick: move |_| {
                        filter.set(None);
                        selection.clear();
                    },
                }
                div { class: "text-sub friend-nav-title", "分组" }
                for group in snapshot.groups.iter().cloned() {
                    NavRow {
                        key: "{group.id}",
                        label: group.name.clone(),
                        count: snapshot.friends.iter().filter(|f| f.group == Some(group.id)).count().to_string(),
                        selected: current == Some(group.id),
                        onclick: {
                            let id = group.id;
                            move |_| {
                                filter.set(Some(id));
                                selection.clear();
                            }
                        },
                        menu: {
                            let (session, group) = (session.clone(), group.clone());
                            move |()| {
                                session.as_ref().map_or_else(Vec::new, |s| friend_group_menu(overlays, s, &group))
                            }
                        },
                    }
                }
                div { class: "friend-nav-add",
                    Button {
                        icon: IconName::Add,
                        onclick: move |_| {
                            let Some(session) = state.session.peek().clone() else { return };
                            spawn(async move {
                                add_friend_group(overlays, &session).await;
                            });
                        },
                        "添加分组"
                    }
                }
            },
            PaneHeader { title,
                div { class: "friend-search",
                    TextInput {
                        value: search(),
                        placeholder: "搜索",
                        style_kind: InputStyle::Search,
                        icon: IconName::Search,
                        clearable: true,
                        oninput: move |v| search.set(v),
                    }
                }
            }
            div { class: "friend-head",
                Checkbox {
                    checked: all_selected,
                    onchange: {
                        let list = list.clone();
                        move |checked: bool| {
                            selection.replace(if checked { list.iter().map(|f| f.id).collect() } else { Vec::new() });
                        }
                    },
                    "昵称"
                }
                span { class: "friend-cell", "备注" }
                span { class: "friend-cell", "分组" }
            }
            ResultList {
                keys: keys(),
                status,
                estimate: 44.0,
                on_more: move |()| {},
                render: move |i: usize| rsx! {
                    FriendRow { friend: rows[i].clone(), groups: render_groups.clone(), selection }
                },
            }
            SelectionBar {
                count: selection.count(),
                on_close: move |()| selection.clear(),
                Button {
                    icon: IconName::UserSetting,
                    onclick: move |_| {
                        let Some(session) = state.session.peek().clone() else { return };
                        let ids = selection.ids();
                        let groups = groups.clone();
                        spawn(async move {
                            let Some(target) = pick_group(overlays, &session, &groups).await else {
                                return;
                            };
                            match session.move_friends(&ids, target) {
                                Ok(()) => selection.clear(),
                                Err(error) => overlays.report_error("设置分组失败", error),
                            }
                        });
                    },
                    "设置分组"
                }
            }
        }
    }
}

#[component]
fn FriendRow(friend: Friend, groups: Rc<Vec<FriendGroup>>, selection: Selection) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let id = friend.id;
    let group_value = friend
        .group
        .map_or_else(|| NO_GROUP.to_owned(), |g| g.to_string());
    let options: Vec<SelectOption> = std::iter::once(SelectOption::new(NO_GROUP, "我的好友"))
        .chain(
            groups
                .iter()
                .map(|g| SelectOption::new(g.id.to_string(), g.name.clone())),
        )
        .collect();
    let move_to = move |group: Option<i64>| {
        if let Some(session) = state.session.peek().as_ref()
            && let Err(error) = session.move_friends(&[id], group)
        {
            overlays.report_error("设置分组失败", error);
        }
    };
    // Editing the note in place: clicking the note cell turns it into an edit box
    let mut editing = use_signal(|| false);
    let remark = friend.remark.clone();
    let commit_remark = move |text: String| {
        editing.set(false);
        let text: String = text.trim().chars().take(REMARK_MAX).collect();
        if text == remark {
            return;
        }
        let Some(session) = state.session.peek().clone() else {
            return;
        };
        spawn(async move {
            let target = RenameTarget::Remark(RemarkTarget::User(id));
            overlays
                .attempt("设置备注失败", session.rename(target, &text))
                .await;
        });
    };
    let checked = selection.checked(id);
    let menu_friend = friend.clone();
    let menu_groups = groups.clone();
    rsx! {
        div { class: "friend-row",
        ListRow {
            density: RowDensity::Compact,
            checked,
            selected: checked == Some(true),
            leading: rsx! { RemoteAvatar { text: friend.nickname.clone(), url: friend.avatar.clone(), size: 28.0 } },
            title: friend.nickname.clone(),
            trailing: rsx! {
                span {
                    class: if friend.remark.is_empty() && !editing() { "friend-cell friend-remark empty" } else { "friend-cell friend-remark" },
                    // Clicks on the note (and inside its edit box) do not toggle the row
                    onclick: move |e| {
                        e.stop_propagation();
                        if !editing() {
                            editing.set(true);
                        }
                    },
                    if editing() {
                        InlineEdit {
                            value: friend.remark.clone(),
                            oncommit: commit_remark,
                            oncancel: move |_| editing.set(false),
                        }
                    } else if friend.remark.is_empty() {
                        "设置备注"
                    } else {
                        "{friend.remark}"
                    }
                }
                div { class: "friend-cell",
                    Select {
                        options,
                        value: group_value,
                        onchange: move |value: String| move_to(value.parse().ok()),
                    }
                }
            },
            onclick: move |_| selection.toggle(id),
            menu: move |()| {
                let mut moves = vec![
                    MenuEntry::item("我的好友", move || move_to(None)).checked(menu_friend.group.is_none()),
                ];
                moves.extend(menu_groups.iter().map(|group| {
                    let group_id = group.id;
                    MenuEntry::item(group.name.clone(), move || move_to(Some(group_id)))
                        .checked(menu_friend.group == Some(group_id))
                }));
                let mut items = vec![MenuEntry::submenu("移动联系人至", moves).icon(IconName::ArrowRightUp)];
                if let Some(session) = state.session.peek().as_ref() {
                    items.extend(plugin_context_menus(session, &ContextMenuTarget::User(id)));
                }
                items
            },
        }
        }
    }
}
