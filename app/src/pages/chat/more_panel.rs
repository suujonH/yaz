//! More panel: topic info, members, announcement, notes, chat history, switches, plugins, topic
//! actions.

use super::actions;
use super::channel_dialogs::{can_manage_channel, edit_notice, open_channel_settings};
use super::message::ChannelNotice;
use super::sidebar::{
    MemberItem, build_members, filter_members, notice_of, topic_stream, use_channel_members,
};
use super::store::Chat;
use crate::state::use_session_event;
use crate::ui::{
    Avatar, Button, ButtonKind, ContextMenuArea, DialogFrame, Icon, IconButton, IconName, ListRow,
    MenuEntry, Picker, PickerItem, Rect, RowDensity, Select, SelectOption, SettingCard, SettingRow,
    TextInput, TextPrompt, Toggle, Tooltip,
};
use crate::widgets::{
    FieldLayout, PluginPanelGroups, RemoteAvatar, SessionRef, cached_avatar, open_user_card,
    plugin_context_menus, topic_delete_item, topic_move_item, topic_rename_item,
    topic_resolve_item, topic_target,
};
use crate::windows;
use dioxus::prelude::*;
use std::sync::Arc;
use zulip_core::html::to_preview;
use zulip_core::model::{Channel, PinItem, PinScope, RemarkTarget};
use zulip_core::push::{DndLevel, TopicVisibility};
use zulip_core::text::time;
use zulip_core::{ConversationKey, links};
use zulip_plugin_host::ContextMenuTarget;
use zulip_session::{DeleteTarget, RenameTarget, Session, SessionEvent};

const MEMBER_PREVIEW: usize = 10;
const REMARK_MAX: usize = 50;

fn level_value(level: DndLevel) -> &'static str {
    match level {
        DndLevel::Notify => "notify",
        DndLevel::Silent => "silent",
        DndLevel::Assistant => "assistant",
        DndLevel::Blocked => "blocked",
    }
}

fn level_of(value: &str) -> Option<DndLevel> {
    match value {
        "notify" => Some(DndLevel::Notify),
        "silent" => Some(DndLevel::Silent),
        "assistant" => Some(DndLevel::Assistant),
        "blocked" => Some(DndLevel::Blocked),
        _ => None,
    }
}

fn remark_target(key: &ConversationKey) -> RemarkTarget {
    match key {
        ConversationKey::Dm(d) if d.other_user_ids().len() == 1 => {
            RemarkTarget::User(d.other_user_ids()[0])
        }
        key => RemarkTarget::Conversation(key.clone()),
    }
}

fn edit_remark(chat: Chat, target: RemarkTarget, original: String) {
    chat.spawn(async move {
        let session = chat.session();
        let current = session.remark(&target);
        let prompt = TextPrompt::remark("备注", &original, current.as_deref()).max_len(REMARK_MAX);
        if let Some(text) = chat.overlays.prompt_text(prompt).await {
            chat.overlays
                .attempt(
                    "设置备注失败",
                    session.rename(RenameTarget::Remark(target), &text),
                )
                .await;
        }
    });
}

fn change_members(chat: Chat, channel: Channel, ids: Vec<i64>, add: bool) {
    chat.spawn(async move {
        let session = chat.session();
        let candidates: Vec<PickerItem> = if add {
            session
                .users()
                .into_iter()
                .filter(|u| u.is_active && !ids.contains(&u.id))
                .map(|u| picker_item(&session, u.id, u.avatar_url.as_deref()))
                .collect()
        } else {
            ids.iter()
                .filter(|id| **id != session.self_id())
                .map(|id| {
                    let avatar = session.user(*id).and_then(|u| u.avatar_url);
                    picker_item(&session, *id, avatar.as_deref())
                })
                .collect()
        };
        let (title, confirm) = if add {
            ("邀请成员", "邀请")
        } else {
            ("移出成员", "移出")
        };
        let picker = Picker::multi(title, candidates, Vec::new()).confirm_text(confirm);
        let Some(picked) = chat.overlays.pick(picker).await else {
            return;
        };
        let users: Vec<i64> = picked.iter().map(|id| *id as i64).collect();
        if users.is_empty() {
            return;
        }
        if add {
            chat.overlays
                .attempt("邀请失败", session.add_subscribers(&channel.name, &users))
                .await;
        } else {
            chat.overlays
                .attempt(
                    "移出失败",
                    session.remove_subscribers(&channel.name, &users),
                )
                .await;
        }
    });
}

fn picker_item(session: &Arc<Session>, id: i64, avatar: Option<&str>) -> PickerItem {
    let item = PickerItem::new(id as u64, session.display_name(id));
    match avatar.and_then(|url| cached_avatar(session, url)) {
        Some(local) => item.avatar(local),
        None => item,
    }
}

fn show_all_members(chat: Chat, channel: Option<Channel>, ids: Vec<i64>) {
    let session = chat.session();
    let overlays = chat.overlays;
    overlays.open_dialog(true, move |handle| {
        rsx! {
            AllMembersDialog {
                session: SessionRef(session.clone()),
                channel: channel.clone(),
                ids: ids.clone(),
                on_close: move |()| handle.close(),
            }
        }
    });
}

#[component]
fn AllMembersDialog(
    session: SessionRef,
    channel: Option<Channel>,
    ids: Vec<i64>,
    on_close: EventHandler<()>,
) -> Element {
    let mut query = use_signal(String::new);
    let overlays = crate::ui::use_overlays();
    let all = build_members(&session.0, channel.as_ref(), &ids);
    let shown = filter_members(&all, &query());
    rsx! {
        DialogFrame { title: "频道成员 ({all.len()})", width: 360.0, on_close,
            div { class: "members-dialog",
                TextInput {
                    value: query(),
                    placeholder: "搜索成员",
                    autofocus: true,
                    clearable: true,
                    oninput: move |v| query.set(v),
                }
                div { class: "history-list scroll",
                    for member in shown {
                        {
                            let id = member.id;
                            rsx! {
                                ListRow {
                                    key: "{id}",
                                    density: RowDensity::Compact,
                                    leading: rsx! { crate::widgets::RemoteAvatar { text: member.name.clone(), url: member.avatar.clone(), size: 24.0 } },
                                    title: member.name.clone(),
                                    subtitle: member.badge.map(str::to_owned),
                                    onclick: move |e: MouseEvent| {
                                        let at = e.client_coordinates();
                                        crate::widgets::open_user_card(
                                            overlays,
                                            crate::ui::Rect { x: at.x, y: at.y, w: 4.0, h: 4.0 },
                                            id,
                                        );
                                    },
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A fully clickable row: text on the left, hint in the middle, arrow on the right.
#[component]
fn PanelLink(
    #[props(into)] label: String,
    #[props(into)] value: Option<String>,
    /// The hint is placeholder text (the "not set" label).
    #[props(default)]
    placeholder: bool,
    #[props(default = true)] chevron: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    rsx! {
        div {
            class: "panel-link",
            onclick: move |e| onclick.call(e),
            span { class: "panel-link-label", "{label}" }
            match value {
                Some(value) => rsx! {
                    span { class: if placeholder { "panel-link-value empty" } else { "panel-link-value" }, "{value}" }
                },
                None => rsx! { span { class: "panel-link-spacer" } },
            }
            if chevron {
                span { class: "panel-chevron",
                    Icon { name: IconName::ChevronRight, size: 14.0 }
                }
            }
        }
    }
}

/// One member in the member card: avatar and name.
#[component]
fn PanelMember(member: MemberItem) -> Element {
    let chat = use_context::<Chat>();
    let id = member.id;
    rsx! {
        ContextMenuArea {
            items: move |()| plugin_context_menus(&chat.session(), &ContextMenuTarget::User(id)),
            div {
                class: "panel-member",
                title: "{member.name}",
                onclick: move |e: MouseEvent| {
                    let at = e.client_coordinates();
                    open_user_card(chat.overlays, Rect { x: at.x, y: at.y, w: 4.0, h: 4.0 }, id);
                },
                RemoteAvatar { text: member.name.clone(), url: member.avatar.clone(), size: 36.0, online: member.online }
                span { class: "panel-member-name", "{member.name}" }
            }
        }
    }
}

/// Sub-pages of the panel.
#[derive(Clone, Copy, PartialEq)]
enum PanelPage {
    Main,
    Notice,
    Scheduled,
}

/// Title row of a sub-page: back button, title, buttons on the right.
#[component]
fn PanelPageHead(
    #[props(into)] title: String,
    on_back: EventHandler<()>,
    #[props(default)] children: Element,
) -> Element {
    rsx! {
        div { class: "panel-page-head",
            IconButton {
                icon: IconName::ChevronLeft,
                size: 28.0,
                icon_size: 16.0,
                onclick: move |_| on_back.call(()),
            }
            span { class: "panel-page-title", "{title}" }
            {children}
        }
    }
}

/// Topic actions: menu items shown as one row.
fn action_row(entry: Option<MenuEntry>) -> Element {
    let Some(entry) = entry else {
        return rsx! {};
    };
    let label = entry.label().to_owned();
    rsx! {
        PanelLink { label, chevron: false, onclick: move |_| entry.select() }
    }
}

#[component]
pub fn ChatMorePanel() -> Element {
    let chat = use_context::<Chat>();
    let mut tick = use_signal(|| 0u32);
    let mut page = use_signal(|| PanelPage::Main);
    let _ = tick();
    use_session_event(move |event| {
        if matches!(
            event,
            SessionEvent::ChannelChanged(_)
                | SessionEvent::ChannelsChanged
                | SessionEvent::PermissionsChanged
                | SessionEvent::Resynced
        ) {
            tick += 1;
        }
    });
    let key = chat.key.read().clone();
    let session = chat.session();
    let account = chat.account;
    let overlays = chat.overlays;
    let is_topic = matches!(key, ConversationKey::Topic(_));
    let (is_single, is_group_dm) = match &key {
        ConversationKey::Dm(d) => (d.other_user_ids().len() == 1, d.is_group_dm()),
        ConversationKey::Topic(_) => (false, false),
    };
    let is_group = is_topic || is_group_dm;
    let peer = match &key {
        ConversationKey::Dm(d) if is_single => Some(d.other_user_ids()[0]),
        _ => None,
    };
    let ids = use_channel_members(chat);
    let channel = topic_stream(&key).and_then(|id| session.channel(id));
    let dm_members: Vec<i64> = match &key {
        ConversationKey::Dm(d) => {
            let mut list = d.other_user_ids().to_vec();
            list.push(session.self_id());
            list
        }
        ConversationKey::Topic(_) => Vec::new(),
    };
    let member_ids = if is_topic {
        ids.read().clone()
    } else {
        dm_members
    };
    let members = build_members(&session, channel.as_ref(), &member_ids);
    let member_summary = format!(
        "查看{}名{}",
        members.len(),
        if is_topic { "频道成员" } else { "成员" }
    );

    let pin_item = PinItem::Conversation(key.clone());
    let pinned = session.is_pinned(PinScope::Messages, &pin_item);
    let level = session.conversation_level(&key);
    let remark_for = remark_target(&key);
    let remark = session.remark(&remark_for);
    let remark_label = if is_topic { "话题备注" } else { "备注" };
    let original_title = session.title(&key);
    let scheduled = session.scheduled_for(&key);
    let use_24h = session.use_24h();
    let now = chrono::Local::now().naive_local();
    let permissions = session.permissions();
    let can_add = channel
        .as_ref()
        .is_some_and(|c| !c.is_archived && permissions.can_add_subscribers(&c.permissions));
    let can_remove = channel
        .as_ref()
        .is_some_and(|c| !c.is_archived && permissions.can_remove_subscribers(&c.permissions));
    let manage_channel = channel.clone().filter(|c| can_manage_channel(&session, c));
    let notice_channel = manage_channel.clone();
    let notice = channel.as_ref().and_then(notice_of);
    let notice_text = notice
        .as_ref()
        .map(|(_, html)| to_preview(html))
        .filter(|text| !text.is_empty());
    let followed = if let ConversationKey::Topic(t) = &key {
        session.topic_visibility(t.stream_id, &session.topic_name(&key))
            == TopicVisibility::Followed
    } else {
        false
    };

    let close = move || {
        let mut open = chat.more_open;
        open.set(false);
    };
    let back = move |()| page.set(PanelPage::Main);

    if page() == PanelPage::Notice {
        return rsx! {
            div { class: "more-backdrop", onclick: move |_| close() }
            div { class: "more-panel scroll",
                PanelPageHead { title: "频道公告", on_back: back,
                    if let Some(channel) = notice_channel {
                        Tooltip { text: "编辑",
                            IconButton {
                                icon: IconName::Edit,
                                size: 28.0,
                                icon_size: 14.0,
                                onclick: move |_| edit_notice(chat, channel.clone()),
                            }
                        }
                    }
                }
                SettingCard {
                    div { class: "panel-page-body",
                        match notice {
                            Some((stream_id, html)) => rsx! { ChannelNotice { stream_id, html } },
                            None => rsx! { div { class: "text-sub", "暂无公告" } },
                        }
                    }
                }
            }
        };
    }
    if page() == PanelPage::Scheduled {
        let title = format!("定时消息 ({})", scheduled.len());
        return rsx! {
            div { class: "more-backdrop", onclick: move |_| close() }
            div { class: "more-panel scroll",
                PanelPageHead { title, on_back: back }
                SettingCard {
                    for item in scheduled {
                        {
                            let id = item.id;
                            let when = time::from_unix(item.deliver_at)
                                .map_or_else(String::new, |t| time::precise_time(t, now, use_24h));
                            rsx! {
                                ListRow {
                                    key: "{id}",
                                    density: RowDensity::Compact,
                                    title: item.content.clone(),
                                    subtitle: when,
                                    trailing: rsx! {
                                        IconButton {
                                            icon: IconName::Delete,
                                            size: 22.0,
                                            icon_size: 13.0,
                                            onclick: move |_| {
                                                chat.spawn(async move {
                                                    let session = chat.session();
                                                    overlays
                                                        .delete("这条定时消息", None, || {
                                                            session.delete(DeleteTarget::ScheduledMessage(id))
                                                        })
                                                        .await;
                                                    tick += 1;
                                                });
                                            },
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }
        };
    }

    let pin_session = session.clone();
    let pin_value = pin_item.clone();
    let level_session = session.clone();
    let level_key = key.clone();
    let quiet_session = session.clone();
    let quiet_key = key.clone();
    let follow_key = key.clone();
    let history_key = key.clone();
    let share_key = key.clone();
    let share_session = session.clone();
    let all_channel = channel.clone();
    let all_ids = member_ids.clone();
    let invite_channel = channel.clone();
    let invite_ids = member_ids.clone();
    let kick_channel = channel.clone();
    let kick_ids = member_ids.clone();
    let panel_target = topic_target(&session, &key);
    let (resolve, move_to, rename, delete) = if is_topic {
        (
            topic_resolve_item(&session, overlays, &key),
            topic_move_item(&session, overlays, &key),
            topic_rename_item(&session, overlays, &key),
            topic_delete_item(&session, overlays, &key),
        )
    } else {
        (None, None, None, None)
    };
    let has_topic_actions = resolve.is_some() || move_to.is_some() || rename.is_some();
    let scheduled_count = scheduled.len();

    rsx! {
        div { class: "more-backdrop", onclick: move |_| close() }
        div { class: "more-panel scroll",
            if is_topic {
                SettingCard {
                    div { class: "panel-info",
                        Avatar {
                            text: original_title.clone(),
                            fill: channel.as_ref().map(|c| c.color.clone()).filter(|c| !c.is_empty()),
                            size: 44.0,
                        }
                        div { class: "panel-info-text",
                            div { class: "panel-info-title", "{original_title}" }
                            if let Some(channel) = &channel {
                                div { class: "panel-info-channel",
                                    if channel.invite_only {
                                        Icon { name: IconName::Lock, size: 12.0 }
                                    }
                                    span { "#{channel.name}" }
                                }
                            }
                        }
                        Tooltip { text: "复制话题链接",
                            button {
                                class: "panel-share",
                                r#type: "button",
                                onclick: move |_| {
                                    if let ConversationKey::Topic(t) = &share_key {
                                        let channel_name = share_session.channel(t.stream_id).map_or_else(String::new, |c| c.name);
                                        let link = links::topic_link(
                                            share_session.realm_url(),
                                            t.stream_id,
                                            &channel_name,
                                            &share_session.topic_name(&share_key),
                                        );
                                        overlays.copy_text(&link);
                                    }
                                },
                                Icon { name: IconName::Share, size: 18.0 }
                                span { "分享" }
                            }
                        }
                    }
                }
            }
            if is_group {
                SettingCard {
                    PanelLink {
                        label: if is_topic { "频道成员" } else { "聊天成员" },
                        value: member_summary,
                        onclick: move |_| show_all_members(chat, all_channel.clone(), all_ids.clone()),
                    }
                    div { class: "panel-members",
                        for member in members.iter().take(MEMBER_PREVIEW).cloned() {
                            PanelMember { key: "{member.id}", member }
                        }
                    }
                    if can_add || can_remove {
                        div { class: "panel-member-actions",
                            if can_add {
                                Button {
                                    kind: ButtonKind::Secondary,
                                    onclick: move |_| {
                                        if let Some(channel) = invite_channel.clone() {
                                            change_members(chat, channel, invite_ids.clone(), true);
                                        }
                                    },
                                    "邀请"
                                }
                            }
                            if can_remove {
                                Button {
                                    kind: ButtonKind::Secondary,
                                    onclick: move |_| {
                                        if let Some(channel) = kick_channel.clone() {
                                            change_members(chat, channel, kick_ids.clone(), false);
                                        }
                                    },
                                    "移出"
                                }
                            }
                        }
                    }
                }
            }
            if let Some(channel) = manage_channel {
                SettingCard {
                    PanelLink {
                        label: "频道设置",
                        onclick: move |_| open_channel_settings(chat, channel.clone()),
                    }
                }
            }
            SettingCard {
                if is_topic {
                    div {
                        class: "panel-notice",
                        onclick: move |_| page.set(PanelPage::Notice),
                        div { class: "panel-notice-title",
                            span { "频道公告" }
                            span { class: "panel-chevron",
                                Icon { name: IconName::ChevronRight, size: 14.0 }
                            }
                        }
                        match notice_text {
                            Some(text) => rsx! { div { class: "panel-notice-text", "{text}" } },
                            None => rsx! { div { class: "panel-notice-text empty", "暂无公告" } },
                        }
                    }
                }
                PanelLink {
                    label: remark_label,
                    placeholder: remark.is_none(),
                    value: remark.unwrap_or_else(|| "未设置".to_owned()),
                    chevron: false,
                    onclick: move |_| {
                        edit_remark(chat, remark_for.clone(), original_title.clone());
                        tick += 1;
                    },
                }
            }
            SettingCard {
                PanelLink {
                    label: "查找聊天记录",
                    onclick: move |_| windows::open_history(account, history_key.clone(), None),
                }
                if !is_topic {
                    PanelLink {
                        label: "文件传输列表",
                        onclick: move |_| windows::open_files(account),
                    }
                }
                if scheduled_count > 0 {
                    PanelLink {
                        label: "定时消息 ({scheduled_count})",
                        onclick: move |_| page.set(PanelPage::Scheduled),
                    }
                }
            }
            SettingCard {
                SettingRow { label: "设为置顶",
                    Toggle {
                        checked: pinned,
                        onchange: move |on: bool| {
                            pin_session.set_pinned(PinScope::Messages, &pin_value, on);
                            tick += 1;
                        },
                    }
                }
                if !is_group {
                    SettingRow { label: "消息免打扰",
                        Toggle {
                            checked: level != DndLevel::Notify,
                            onchange: move |on: bool| {
                                quiet_session.set_conversation_level(
                                    &quiet_key,
                                    if on { DndLevel::Silent } else { DndLevel::Notify },
                                );
                                tick += 1;
                            },
                        }
                    }
                }
                if let Some(user) = peer {
                    SettingRow { label: "屏蔽此人",
                        Toggle {
                            checked: session.is_user_muted(user),
                            onchange: move |on: bool| {
                                actions::toggle_muted(chat, user, on);
                                tick += 1;
                            },
                        }
                    }
                }
                if is_group {
                    SettingRow { label: "消息设置",
                        Select {
                            options: vec![
                                SelectOption::new("notify", "允许消息提醒"),
                                SelectOption::new("silent", "接收消息但不提醒"),
                                SelectOption::new("assistant", "收进话题助手且不提醒"),
                                SelectOption::new("blocked", "屏蔽消息"),
                            ],
                            value: level_value(level).to_owned(),
                            onchange: move |value: String| {
                                if let Some(level) = level_of(&value) {
                                    level_session.set_conversation_level(&level_key, level);
                                    tick += 1;
                                }
                            },
                        }
                    }
                }
                if is_topic {
                    SettingRow { label: "关注",
                        Toggle {
                            checked: followed,
                            onchange: move |on: bool| {
                                let key = follow_key.clone();
                                chat.spawn(async move {
                                    let visibility = if on { TopicVisibility::Followed } else { TopicVisibility::Inherit };
                                    overlays
                                        .attempt("操作失败", chat.session().set_topic_visibility(&key, visibility))
                                        .await;
                                    tick += 1;
                                });
                            },
                        }
                    }
                }
            }
            if let Some(target) = panel_target {
                PluginPanelGroups { target, layout: FieldLayout::Setting }
            }
            if has_topic_actions {
                SettingCard {
                    {action_row(resolve)}
                    if let Some(entry) = move_to {
                        PanelLink {
                            label: "移动话题到其他频道",
                            onclick: move |e: MouseEvent| {
                                let at = e.client_coordinates();
                                overlays.show_menu(at.x, at.y, entry.items());
                            },
                        }
                    }
                    {action_row(rename)}
                }
            }
            if let Some(entry) = delete {
                button {
                    class: "panel-delete",
                    r#type: "button",
                    onclick: move |_| entry.select(),
                    "删除话题"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_values_round_trip() {
        for level in [
            DndLevel::Notify,
            DndLevel::Silent,
            DndLevel::Assistant,
            DndLevel::Blocked,
        ] {
            assert_eq!(level_of(level_value(level)), Some(level));
        }
        assert_eq!(level_of("x"), None);
    }
}
