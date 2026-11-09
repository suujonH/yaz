//! 用户名片和话题名片：所有点到人、话题的地方共用。

use super::menus::plugin_context_menus;
use super::plugin_ui::{FieldLayout, PluginPanelGroups, topic_target, user_target};
use super::remote::RemoteAvatar;
use crate::pages::settings::SettingsTarget;
use crate::state::{use_app_state, use_session_data};
use crate::ui::*;
use crate::windows;
use chrono::Utc;
use chrono_tz::Tz;
use dioxus::prelude::*;
use serde_json::Value;
use zulip_core::ConversationKey;
use zulip_core::avatar::large_url;
use zulip_core::model::{CustomProfileField, PresenceState, RemarkTarget, ZulipUser};
use zulip_core::permission::{ROLE_OWNER, special_role_label};
use zulip_core::text::time::clock_time;
use zulip_plugin_host::ContextMenuTarget;
use zulip_session::{Session, SessionEvent};

const REMARK_MAX: usize = 32;
const NO_GROUP: &str = "default";

/// 在 `anchor` 旁打开用户名片。
pub fn open_user_card(overlays: Overlays, anchor: Rect, user_id: i64) {
    overlays.open_popover(anchor, PopoverOptions::default(), move |handle| {
        rsx! { UserCard { user_id, handle } }
    });
}

/// 在 `anchor` 旁打开话题名片。
pub fn open_topic_card(overlays: Overlays, anchor: Rect, key: ConversationKey) {
    overlays.open_popover(anchor, PopoverOptions::default(), move |handle| {
        rsx! { TopicCard { key: "{key}", conversation: key.clone(), handle } }
    });
}

#[derive(Clone, Debug, Default, PartialEq)]
struct UserCardData {
    name: String,
    avatar: Option<String>,
    online: bool,
    presence: &'static str,
    role: Option<&'static str>,
    owner: bool,
    email: String,
    status: Option<String>,
    local_time: Option<String>,
    fields: Vec<(String, String)>,
    remark: String,
    group: Option<i64>,
    groups: Vec<(i64, String)>,
    is_self: bool,
    editable: bool,
}

fn local_time(timezone: &str, use_24h: bool) -> Option<String> {
    let zone: Tz = timezone.parse().ok()?;
    let time = Utc::now().with_timezone(&zone).naive_local();
    Some(format!(
        "{}（{timezone}）",
        clock_time(time, use_24h, false)
    ))
}

fn option_text(field_data: &Value, option: &str) -> Option<String> {
    if let Value::String(text) = field_data {
        return option_text(&serde_json::from_str(text).ok()?, option);
    }
    Some(field_data.get(option)?.get("text")?.as_str()?.to_owned())
}

fn user_names(session: &Session, ids: impl Iterator<Item = i64>) -> String {
    ids.map(|id| session.display_name(id))
        .collect::<Vec<_>>()
        .join("、")
}

/// 选项字段显示选项文字，用户字段显示用户名，其余显示原值。
fn field_text(session: &Session, field: &CustomProfileField, value: &Value) -> Option<String> {
    const CHOICE: i32 = 3;
    const USER: i32 = 6;
    match (field.field_type, value) {
        (CHOICE, Value::String(option)) => {
            option_text(&field.field_data, option).or_else(|| Some(option.clone()))
        }
        (USER, Value::Array(ids)) => {
            Some(user_names(session, ids.iter().filter_map(Value::as_i64)))
        }
        (USER, Value::String(text)) => match serde_json::from_str::<Vec<i64>>(text) {
            Ok(ids) => Some(user_names(session, ids.into_iter())),
            Err(_) => Some(text.clone()),
        },
        (_, Value::String(text)) => Some(text.clone()),
        (_, Value::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

fn profile_fields(session: &Session, user: &ZulipUser) -> Vec<(String, String)> {
    let realm = session.realm();
    let mut fields: Vec<&CustomProfileField> = realm
        .profile_fields
        .iter()
        .filter(|f| f.display_in_profile_summary)
        .collect();
    fields.sort_by_key(|f| f.order);
    fields
        .into_iter()
        .filter_map(|field| {
            let value = user.profile_data.get(&field.id.to_string())?;
            let text = field_text(session, field, &value.value)?;
            (!text.is_empty()).then(|| (field.name.clone(), text))
        })
        .collect()
}

fn role_text(user: &ZulipUser) -> Option<&'static str> {
    if user.is_bot {
        return Some("机器人");
    }
    special_role_label(user.role)
}

fn read_user(session: &Session, user_id: i64) -> UserCardData {
    let user = session.user(user_id);
    let remark = session.remark(&RemarkTarget::User(user_id));
    let presence = session.presence(user_id);
    let is_self = user_id == session.self_id();
    UserCardData {
        // 名片标题始终是对方的昵称，备注只显示在“备注”一栏
        name: user
            .as_ref()
            .map_or_else(|| session.display_name(user_id), |u| u.full_name.clone()),
        avatar: user.as_ref().and_then(|u| u.avatar_url.clone()),
        online: presence == PresenceState::Online,
        presence: match presence {
            PresenceState::Online => "在线",
            PresenceState::Idle => "离开",
            PresenceState::Offline => "离线",
        },
        role: user.as_ref().and_then(role_text),
        owner: user.as_ref().is_some_and(|u| u.role == ROLE_OWNER),
        email: user.as_ref().map_or_else(String::new, |u| {
            u.delivery_email.clone().unwrap_or_else(|| u.email.clone())
        }),
        status: session.status_text(user_id),
        local_time: user
            .as_ref()
            .and_then(|u| local_time(&u.timezone, session.use_24h())),
        fields: user
            .as_ref()
            .map_or_else(Vec::new, |u| profile_fields(session, u)),
        remark: remark.unwrap_or_default(),
        group: session.friend_group_of(user_id),
        groups: session
            .friend_groups()
            .into_iter()
            .map(|g| (g.id, g.name))
            .collect(),
        is_self,
        editable: !is_self && user.as_ref().is_some_and(|u| !u.is_bot),
    }
}

fn user_changed(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::UserChanged(_)
            | SessionEvent::UsersChanged
            | SessionEvent::PresenceChanged(_)
            | SessionEvent::ContactsChanged
            | SessionEvent::RealmChanged
    )
}

/// 名片弹层关闭后再执行操作。
fn close_card(handle: Option<PopoverHandle>) {
    if let Some(handle) = handle {
        handle.close();
    }
}

#[component]
fn UserCard(user_id: i64, handle: Option<PopoverHandle>) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    let data = use_session_data(user_changed, move |s| read_user(s, user_id));
    let mut editing = use_signal(|| false);
    let mut copied = use_signal(|| false);
    let d = data.read().clone();
    let session = state.session.read().clone();
    let account = session.as_ref().map_or(0, |s| s.account_id());
    let self_id = session.as_ref().map_or(0, |s| s.self_id());
    let email = d.email.clone();
    let avatar_url = d.avatar.clone();
    let group_options: Vec<SelectOption> = std::iter::once(SelectOption::new(NO_GROUP, "我的好友"))
        .chain(
            d.groups
                .iter()
                .map(|(id, name)| SelectOption::new(id.to_string(), name.clone())),
        )
        .collect();
    let group_value = d
        .group
        .map_or_else(|| NO_GROUP.to_owned(), |g| g.to_string());
    let session_for_group = session.clone();
    let session_for_remark = session.clone();
    let panel_target = session.as_ref().map(|s| user_target(s, user_id));
    let session_for_menu = session.clone();
    let plugin_target = ContextMenuTarget::User(user_id);
    let has_plugin_menus = session
        .as_ref()
        .is_some_and(|s| !plugin_context_menus(s, &plugin_target).is_empty());
    rsx! {
        ProfileCard {
            name: d.name.clone(),
            avatar: rsx! {
                RemoteAvatar {
                    text: d.name.clone(),
                    url: d.avatar.clone(),
                    size: 64.0,
                    online: d.online,
                    onclick: move |_| {
                        if let Some(url) = &avatar_url {
                            close_card(handle);
                            windows::open_image_viewer(account, vec![large_url(url)], 0);
                        }
                    },
                }
            },
            badge: d.role.map(|role| rsx! { RoleBadge { owner: d.owner, "{role}" } }),
            subtitle: rsx! {
                div { class: "user-card-email text-sub",
                    span { class: "user-card-email-text", "{d.email}" }
                    IconButton {
                        icon: if copied() { IconName::Check } else { IconName::Copy },
                        class: if copied() { "user-card-copied" } else { "" },
                        size: 20.0,
                        icon_size: 12.0,
                        onclick: move |_| {
                            overlays.copy_text(&email);
                            copied.set(true);
                        },
                    }
                }
                div { class: "text-sub", "{d.presence}" }
            },
            actions: rsx! {
                if d.is_self {
                    Button {
                        kind: ButtonKind::Secondary,
                        onclick: move |_| {
                            close_card(handle);
                            windows::open_settings(SettingsTarget::AccountProfile(account));
                        },
                        "编辑资料"
                    }
                }
                Button {
                    kind: ButtonKind::Primary,
                    onclick: move |_| {
                        close_card(handle);
                        windows::open_chat(account, ConversationKey::dm([user_id], self_id), None);
                    },
                    "发消息"
                }
                if has_plugin_menus {
                    IconButton {
                        icon: IconName::More,
                        onclick: move |e: MouseEvent| {
                            let Some(session) = &session_for_menu else { return };
                            let at = e.client_coordinates();
                            overlays.show_menu(at.x, at.y, plugin_context_menus(session, &plugin_target));
                        },
                    }
                }
            },
            if let Some(status) = &d.status {
                ProfileField { label: "签名", "{status}" }
            }
            if let Some(time) = &d.local_time {
                ProfileField { label: "当地时间", "{time}" }
            }
            for (name, value) in d.fields.iter() {
                ProfileField { key: "{name}", label: name.clone(), "{value}" }
            }
            if d.editable {
                ProfileField { label: "备注",
                    if editing() {
                        InlineEdit {
                            value: d.remark.clone(),
                            oncommit: move |text: String| {
                                editing.set(false);
                                let text: String = text.trim().chars().take(REMARK_MAX).collect();
                                let Some(session) = session_for_remark.clone() else { return };
                                spawn(async move {
                                    let target = zulip_session::RenameTarget::Remark(RemarkTarget::User(user_id));
                                    overlays.attempt("设置备注失败", session.rename(target, &text)).await;
                                });
                            },
                            oncancel: move |_| editing.set(false),
                        }
                    } else {
                        span {
                            class: if d.remark.is_empty() { "user-card-edit empty" } else { "user-card-edit" },
                            onclick: move |_| editing.set(true),
                            if d.remark.is_empty() { "设置备注" } else { "{d.remark}" }
                        }
                    }
                }
                ProfileField { label: "分组",
                    Select {
                        options: group_options,
                        value: group_value,
                        onchange: move |value: String| {
                            let Some(session) = &session_for_group else { return };
                            if let Err(error) = session.move_friends(&[user_id], value.parse().ok()) {
                                overlays.report_error("设置分组失败", error);
                            }
                        },
                    }
                }
            }
            if let Some(target) = panel_target {
                PluginPanelGroups { target, layout: FieldLayout::Profile }
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct TopicCardData {
    title: String,
    channel: String,
    locked: bool,
    color: String,
    remark: String,
    key: Option<ConversationKey>,
}

fn read_topic(session: &Session, key: &ConversationKey) -> TopicCardData {
    let channel = match key {
        ConversationKey::Topic(topic) => session.channel(topic.stream_id),
        _ => None,
    };
    let topic_name = session.topic_name(key);
    let title = session.title(key);
    TopicCardData {
        remark: if title == topic_name {
            String::new()
        } else {
            title.clone()
        },
        title: if topic_name.is_empty() {
            title
        } else {
            topic_name
        },
        channel: channel
            .as_ref()
            .map_or_else(String::new, |c| c.name.clone()),
        locked: channel.as_ref().is_some_and(|c| c.invite_only),
        color: channel.map_or_else(String::new, |c| c.color),
        key: Some(key.clone()),
    }
}

#[component]
fn TopicCard(conversation: ConversationKey, handle: Option<PopoverHandle>) -> Element {
    let state = use_app_state();
    let overlays = use_overlays();
    // 话题改名、移动后跟着新键走
    let mut current = use_signal(|| conversation.clone());
    let data = use_session_data(
        |e| {
            matches!(
                e,
                SessionEvent::ConversationChanged(_)
                    | SessionEvent::ConversationsChanged
                    | SessionEvent::ContactsChanged
                    | SessionEvent::ChannelChanged(_)
                    | SessionEvent::ChannelsChanged
            )
        },
        move |s| read_topic(s, &current.peek()),
    );
    crate::state::use_session_event(move |event| {
        if let SessionEvent::ConversationRekeyed { old, new } = event
            && *current.peek() == old
        {
            current.set(new);
        }
    });
    let mut editing = use_signal(|| false);
    let d = data.read().clone();
    let session = state.session.read().clone();
    let account = session.as_ref().map_or(0, |s| s.account_id());
    let panel_target = session
        .as_ref()
        .and_then(|s| topic_target(s, &current.read()));
    rsx! {
        ProfileCard {
            name: d.title.clone(),
            avatar: rsx! {
                Avatar { text: d.title.clone(), fill: Some(d.color.clone()).filter(|c| !c.is_empty()), size: 64.0 }
            },
            subtitle: rsx! {
                div { class: "topic-card-channel text-sub",
                    if d.locked {
                        Icon { name: IconName::Lock, size: 12.0 }
                    }
                    "#{d.channel}"
                }
            },
            actions: rsx! {
                Button {
                    kind: ButtonKind::Primary,
                    onclick: move |_| {
                        close_card(handle);
                        windows::open_chat(account, current.peek().clone(), None);
                    },
                    "发消息"
                }
            },
            ProfileField { label: "话题备注",
                if editing() {
                    InlineEdit {
                        value: d.remark.clone(),
                        oncommit: move |text: String| {
                            editing.set(false);
                            let text: String = text.trim().chars().take(REMARK_MAX).collect();
                            let Some(session) = session.clone() else { return };
                            let key = current.peek().clone();
                            spawn(async move {
                                let target = zulip_session::RenameTarget::Remark(RemarkTarget::Conversation(key));
                                overlays.attempt("设置备注失败", session.rename(target, &text)).await;
                            });
                        },
                        oncancel: move |_| editing.set(false),
                    }
                } else {
                    span {
                        class: if d.remark.is_empty() { "user-card-edit empty" } else { "user-card-edit" },
                        onclick: move |_| editing.set(true),
                        if d.remark.is_empty() { "设置备注" } else { "{d.remark}" }
                    }
                }
            }
            if let Some(target) = panel_target {
                PluginPanelGroups { target, layout: FieldLayout::Profile }
            }
        }
    }
}

/// 主窗口右侧的资料面板：内容与名片相同。
#[component]
pub fn ProfilePane(user: Option<i64>, topic: Option<ConversationKey>) -> Element {
    rsx! {
        div { class: "profile-pane",
            div { class: "profile-pane-card",
                if let Some(user_id) = user {
                    UserCard { key: "{user_id}", user_id }
                }
                if let Some(conversation) = topic {
                    TopicCard { key: "{conversation}", conversation }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn option_text_reads_object_and_encoded_json() {
        let data = json!({"a": {"text": "选项一", "order": "1"}});
        assert_eq!(option_text(&data, "a").as_deref(), Some("选项一"));
        assert_eq!(option_text(&data, "b"), None);
        let encoded = Value::String(data.to_string());
        assert_eq!(option_text(&encoded, "a").as_deref(), Some("选项一"));
        assert_eq!(option_text(&Value::String("bad".into()), "a"), None);
    }

    #[test]
    fn local_time_skips_unknown_zone() {
        assert_eq!(local_time("", true), None);
        assert_eq!(local_time("Not/AZone", true), None);
        assert!(
            local_time("Asia/Shanghai", true).is_some_and(|t| t.ends_with("（Asia/Shanghai）"))
        );
    }
}
