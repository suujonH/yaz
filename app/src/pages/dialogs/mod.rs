//! “+”菜单：发起群聊、新建话题、加入频道、创建频道、邀请成员。

use crate::services::show_unavailable;
use crate::ui::*;
use crate::widgets::{SessionRef, cached_avatar};
use crate::windows;
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::sync::Arc;
use zulip_core::model::Channel;
use zulip_core::{ConversationKey, contains_ignore_case};
use zulip_session::Session;

pub const CHANNEL_NAME_MAX: usize = 60;
pub const CHANNEL_DESCRIPTION_MAX: usize = 1024;

fn shows(allowed: bool) -> bool {
    allowed || show_unavailable()
}

/// 可以新建话题的频道：已订阅、未归档、可以发言、可以建话题、允许命名话题。
fn topic_channels(session: &Session) -> Vec<Channel> {
    let permissions = session.permissions();
    let realm = session.realm();
    session
        .subscribed_channels()
        .into_iter()
        .filter(|c| {
            !c.is_archived
                && permissions.can_send_to_channel(&c.permissions)
                && permissions.can_create_topic(&c.permissions)
                && realm.allows_named_topics(c)
        })
        .collect()
}

/// “+”按钮的菜单。
pub fn plus_menu_items(session: Arc<Session>, overlays: Overlays) -> Vec<MenuEntry> {
    let permissions = session.permissions();
    let run = |action: fn(Arc<Session>, Overlays)| {
        let session = session.clone();
        move || action(session.clone(), overlays)
    };
    let mut items = vec![
        MenuEntry::item("发起群聊", run(start_group_chat)).icon(IconName::Usergroup),
        MenuEntry::item("新建话题", run(new_topic)).icon(IconName::Edit),
        MenuEntry::item("加入频道", run(browse_channels)).icon(IconName::Search),
    ];
    if shows(permissions.can_create_public_channel() || permissions.can_create_private_channel()) {
        items.push(MenuEntry::item("创建频道", run(create_channel)).icon(IconName::Add));
    }
    if shows(permissions.can_invite_users()) {
        items.push(MenuEntry::item("邀请成员", run(invite)).icon(IconName::Mail));
    }
    items
}

fn user_items(session: &Arc<Session>) -> Vec<PickerItem> {
    let self_id = session.self_id();
    session
        .users()
        .iter()
        .filter(|u| u.is_active && !u.is_bot && u.id != self_id)
        .map(|u| {
            let mut item =
                PickerItem::new(u.id as u64, session.display_name(u.id)).subtitle(u.email.clone());
            if let Some(src) = u
                .avatar_url
                .as_deref()
                .and_then(|url| cached_avatar(session, url))
            {
                item = item.avatar(src);
            }
            item
        })
        .collect()
}

fn start_group_chat(session: Arc<Session>, overlays: Overlays) {
    spawn_forever(async move {
        let picker =
            Picker::multi("发起群聊", user_items(&session), Vec::new()).confirm_text("确定");
        let Some(ids) = overlays.pick(picker).await else {
            return;
        };
        if ids.len() < 2 {
            overlays.toast("请至少选择 2 人");
            return;
        }
        let ids: Vec<i64> = ids.into_iter().map(|id| id as i64).collect();
        windows::open_chat(
            session.account_id(),
            ConversationKey::dm(ids, session.self_id()),
            None,
        );
    });
}

fn new_topic(session: Arc<Session>, overlays: Overlays) {
    spawn_forever(async move {
        let answer = overlays
            .ask_dialog({
                let session = session.clone();
                move |answer| rsx! { NewTopicDialog { session: SessionRef(session.clone()), answer } }
            })
            .await;
        if let Some(key) = answer {
            windows::open_chat(session.account_id(), key, None);
        }
    });
}

#[component]
fn NewTopicDialog(session: SessionRef, answer: Answer<ConversationKey>) -> Element {
    let channels = use_hook(|| topic_channels(&session.0));
    let mut channel = use_signal(|| channels.first().map_or(0, |c| c.stream_id));
    let mut topic = use_signal(String::new);
    let max_len = session.0.realm().max_topic_length as usize;
    let options: Vec<SelectOption> = channels
        .iter()
        .map(|c| SelectOption::new(c.stream_id.to_string(), format!("#{}", c.name)))
        .collect();
    let name = topic.read().trim().to_owned();
    let valid = channel() != 0 && !name.is_empty() && name.chars().count() <= max_len;
    let cancel = answer.clone();
    let close = answer.clone();
    rsx! {
        DialogFrame {
            title: "新建话题",
            on_close: move |_| close.cancel(),
            footer: rsx! {
                DialogButtons {
                    disabled: !valid,
                    on_confirm: move |_| {
                        answer.send(ConversationKey::topic(channel(), topic.read().trim()));
                    },
                    on_cancel: move |_| cancel.cancel(),
                }
            },
            div { class: "dialog-form",
                label { class: "dialog-label", "频道" }
                Select {
                    options,
                    value: channel().to_string(),
                    onchange: move |v: String| channel.set(v.parse().unwrap_or(0)),
                }
                label { class: "dialog-label", "话题" }
                TextInput {
                    value: topic(),
                    placeholder: "填写话题名称",
                    autofocus: true,
                    oninput: move |v| topic.set(v),
                }
            }
        }
    }
}

fn browse_channels(session: Arc<Session>, overlays: Overlays) {
    overlays.open_dialog(true, move |handle| {
        rsx! { BrowseChannelsDialog { session: SessionRef(session.clone()), handle } }
    });
}

#[component]
fn BrowseChannelsDialog(session: SessionRef, handle: DialogHandle) -> Element {
    let overlays = use_overlays();
    let mut query = use_signal(String::new);
    let channels = crate::state::use_session_data(
        |e| {
            matches!(
                e,
                zulip_session::SessionEvent::ChannelsChanged
                    | zulip_session::SessionEvent::ChannelChanged(_)
            )
        },
        |s| s.channels(),
    );
    let query_text = query.read();
    let text = query_text.trim();
    let list: Vec<Channel> = channels
        .read()
        .iter()
        .filter(|c| !c.is_archived)
        .filter(|c| {
            c.is_subscribed()
                || session
                    .0
                    .permissions()
                    .can_subscribe(&c.permissions, c.invite_only)
        })
        .filter(|c| {
            contains_ignore_case(&c.name, text) || contains_ignore_case(&c.description, text)
        })
        .cloned()
        .collect();
    let empty = list.is_empty().then(|| "没有找到频道".to_owned());
    rsx! {
        PickerShell {
            title: "加入频道",
            query: query(),
            on_query: move |v| query.set(v),
            on_close: move |_| handle.close(),
            empty_text: empty,
            for channel in list {
                ListRow {
                    key: "{channel.stream_id}",
                    density: RowDensity::Normal,
                    leading: rsx! { Avatar { text: channel.name.clone(), fill: channel.color.clone(), size: 32.0 } },
                    title: format!("#{}", channel.name),
                    subtitle: format!("{} 人 {}", channel.subscriber_count, channel.description),
                    trailing: {
                        let session = session.0.clone();
                        let names = vec![channel.name.clone()];
                        let subscribed = channel.is_subscribed();
                        rsx! {
                            Button {
                                kind: if subscribed { ButtonKind::Secondary } else { ButtonKind::Primary },
                                onclick: move |_| {
                                    let session = session.clone();
                                    let names = names.clone();
                                    spawn(async move {
                                        if subscribed {
                                            overlays.attempt("退订失败", session.unsubscribe_channel(&names)).await;
                                        } else {
                                            overlays.attempt("加入失败", session.subscribe_channel(&names)).await;
                                        }
                                    });
                                },
                                if subscribed { "退出" } else { "加入" }
                            }
                        }
                    },
                }
            }
        }
    }
}

fn create_channel(session: Arc<Session>, overlays: Overlays) {
    overlays.open_dialog(true, move |handle| {
        rsx! { CreateChannelDialog { session: SessionRef(session.clone()), handle } }
    });
}

#[component]
fn CreateChannelDialog(session: SessionRef, handle: DialogHandle) -> Element {
    let overlays = use_overlays();
    let permissions = session.0.permissions();
    let can_public = permissions.can_create_public_channel();
    let can_private = permissions.can_create_private_channel();
    let mut name = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut private = use_signal(move || !can_public && can_private);
    let mut members = use_signal(Vec::<u64>::new);
    let mut busy = use_signal(|| false);
    let for_pick = session.0.clone();
    let for_create = session.0.clone();
    let count = members.read().len();
    let trimmed = name.read().trim().to_owned();
    rsx! {
        DialogFrame {
            title: "创建频道",
            width: 400.0,
            on_close: move |_| handle.close(),
            footer: rsx! {
                DialogButtons {
                    disabled: trimmed.is_empty() || busy(),
                    on_confirm: move |_| {
                        let session = for_create.clone();
                        busy.set(true);
                        spawn(async move {
                            let mut subscribers: Vec<i64> = members.peek().iter().map(|&id| id as i64).collect();
                            subscribers.push(session.self_id());
                            let result = overlays
                                .attempt(
                                    "创建失败",
                                    session.create_channel(
                                        name.peek().trim(),
                                        description.peek().trim(),
                                        *private.peek(),
                                        &subscribers,
                                    ),
                                )
                                .await;
                            busy.set(false);
                            if result.is_some() {
                                overlays.toast("频道已创建");
                                handle.close();
                            }
                        });
                    },
                    on_cancel: move |_| handle.close(),
                }
            },
            div { class: "dialog-form",
                label { class: "dialog-label", "名称" }
                TextInput {
                    value: name(),
                    autofocus: true,
                    invalid: name.read().chars().count() > CHANNEL_NAME_MAX,
                    oninput: move |v: String| name.set(v.chars().take(CHANNEL_NAME_MAX).collect()),
                }
                label { class: "dialog-label", "描述" }
                TextInput {
                    value: description(),
                    multiline: true,
                    oninput: move |v: String| description.set(v.chars().take(CHANNEL_DESCRIPTION_MAX).collect()),
                }
                if can_public && can_private {
                    Toggle {
                        checked: private(),
                        onchange: move |v| private.set(v),
                        "私有频道"
                    }
                }
                div { class: "dialog-row",
                    Button {
                        kind: ButtonKind::Secondary,
                        onclick: move |_| {
                            let session = for_pick.clone();
                            spawn(async move {
                                let picker = Picker::multi("选择成员", user_items(&session), members.peek().clone());
                                if let Some(ids) = overlays.pick(picker).await {
                                    members.set(ids);
                                }
                            });
                        },
                        "选择成员"
                    }
                    if count > 0 {
                        span { class: "text-sub", "已选择 {count} 人" }
                    }
                }
            }
        }
    }
}

fn invite(session: Arc<Session>, overlays: Overlays) {
    overlays.open_dialog(true, move |handle| {
        rsx! { InviteDialog { session: SessionRef(session.clone()), handle } }
    });
}

#[component]
fn InviteDialog(session: SessionRef, handle: DialogHandle) -> Element {
    let overlays = use_overlays();
    let channels = use_hook(|| session.0.subscribed_channels());
    let mut emails = use_signal(String::new);
    let mut selected = use_signal(Vec::<i64>::new);
    let mut invalid = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let empty = emails.read().trim().is_empty();
    rsx! {
        DialogFrame {
            title: "邀请成员",
            width: 400.0,
            on_close: move |_| handle.close(),
            footer: rsx! {
                DialogButtons {
                    disabled: empty || busy(),
                    on_confirm: move |_| {
                        let text = emails.peek().clone();
                        let bad = text
                            .split(|c: char| c == ',' || c == '，' || c.is_whitespace())
                            .find(|e| !e.is_empty() && !e.contains('@'))
                            .map(str::to_owned);
                        if let Some(bad) = bad {
                            invalid.set(Some(format!("邮箱格式不正确：{bad}")));
                            return;
                        }
                        invalid.set(None);
                        busy.set(true);
                        let session = session.0.clone();
                        spawn(async move {
                            let streams = selected.peek().clone();
                            let result = overlays.attempt("邀请失败", session.send_invites(&text, &streams)).await;
                            busy.set(false);
                            if result.is_some() {
                                overlays.toast("邀请已发送");
                                handle.close();
                            }
                        });
                    },
                    on_cancel: move |_| handle.close(),
                }
            },
            div { class: "dialog-form",
                label { class: "dialog-label", "邮箱" }
                TextInput {
                    value: emails(),
                    multiline: true,
                    autofocus: true,
                    placeholder: "多个邮箱用逗号或换行分隔",
                    invalid: invalid.read().is_some(),
                    oninput: move |v| emails.set(v),
                }
                if let Some(text) = invalid() {
                    div { class: "dialog-error", "{text}" }
                }
                label { class: "dialog-label", "加入频道" }
                div { class: "dialog-checks scroll",
                    for channel in channels.iter() {
                        Checkbox {
                            key: "{channel.stream_id}",
                            checked: selected.read().contains(&channel.stream_id),
                            onchange: {
                                let id = channel.stream_id;
                                move |on: bool| {
                                    let mut list = selected.write();
                                    list.retain(|&s| s != id);
                                    if on {
                                        list.push(id);
                                    }
                                }
                            },
                            "#{channel.name}"
                        }
                    }
                }
            }
        }
    }
}
