//! Context menus for conversations, topics, and channels: shared by the message page and the
//! contacts page. Items without permission are not generated (in developer mode they are shown and
//! clicks are only simulated).

use crate::services::{plugins, show_unavailable};
use crate::ui::*;
use crate::windows;
use dioxus::dioxus_core::spawn_forever;
use serde_json::json;
use std::rc::Rc;
use std::sync::Arc;
use zulip_core::ConversationKey;
use zulip_core::links::{channel_link, topic_link};
use zulip_core::model::{Channel, PinItem, PinScope, RemarkTarget};
use zulip_core::push::DndLevel;
use zulip_plugin_host::{ContextMenuTarget, MenuContext, MenuConv, PluginMenu};
use zulip_session::topic::{RESOLVED_PREFIX, is_resolved_name, strip_resolved};
use zulip_session::{
    ConversationRow, DeleteTarget, RenameTarget, Session, SessionError, TopicDestination,
};

#[cfg(debug_assertions)]
const MOCK_TEXT: &str = "开发者模式：没有权限或服务器不支持，只模拟操作，未发送到服务器";
const CHANNEL_COLORS: [&str; 8] = [
    "#0052D9", "#2BA471", "#E37318", "#D54941", "#8E56DD", "#029CD4", "#E851B3", "#7A7A7A",
];

/// Conversation kind used in menu visibility conditions.
pub fn menu_conv(key: &ConversationKey) -> MenuConv {
    match key {
        ConversationKey::Topic(_) => MenuConv::Channel,
        ConversationKey::Dm(_) => MenuConv::Direct,
    }
}

/// Items a plugin menu shows in this context; `invoke` receives the id of the clicked menu item.
pub fn plugin_menu_entries(
    menus: &[PluginMenu],
    ctx: &MenuContext,
    invoke: &Rc<dyn Fn(i32)>,
) -> Vec<MenuEntry> {
    menus
        .iter()
        .filter(|menu| menu.visible_for(ctx))
        .map(|menu| {
            if menu.is_group() {
                MenuEntry::submenu(
                    menu.name.clone(),
                    plugin_menu_entries(&menu.children, ctx, invoke),
                )
            } else {
                let (invoke, id) = (invoke.clone(), menu.id);
                MenuEntry::item(menu.name.clone(), move || invoke(id))
            }
        })
        .collect()
}

/// Plugin items in the context menu: one submenu per plugin, titled with the plugin name.
pub fn plugin_context_menus(session: &Arc<Session>, target: &ContextMenuTarget) -> Vec<MenuEntry> {
    let ctx = MenuContext {
        conv: match target {
            ContextMenuTarget::Conversation(key) => menu_conv(key),
            ContextMenuTarget::User(_) | ContextMenuTarget::Channel(_) => MenuConv::default(),
        },
        ..MenuContext::default()
    };
    plugins()
        .plugins()
        .into_iter()
        .filter_map(|plugin| {
            let invoke: Rc<dyn Fn(i32)> = {
                let (plugin, session, target) = (plugin.clone(), session.clone(), target.clone());
                Rc::new(move |id| {
                    plugins().invoke_context_menu(&plugin, id, &session, &target);
                })
            };
            let items =
                plugin_menu_entries(plugin.active_context_menus(target.kind()), &ctx, &invoke);
            (!items.is_empty()).then(|| MenuEntry::submenu(plugin.name(), items))
        })
        .collect()
}

fn append_plugin_menus(
    items: &mut Vec<MenuEntry>,
    session: &Arc<Session>,
    target: &ContextMenuTarget,
) {
    let plugin_items = plugin_context_menus(session, target);
    if !plugin_items.is_empty() {
        items.push(MenuEntry::Separator);
        items.extend(plugin_items);
    }
}

fn logout_text(name: &str, multiple: bool) -> String {
    if multiple {
        format!("确定要退出账号“{name}”吗？")
    } else {
        "确定要退出当前账号吗？".to_owned()
    }
}

/// Confirmation for signing out of an account. With several accounts, it says which one.
pub fn logout_confirm(name: &str, multiple: bool) -> Confirm {
    Confirm::new(logout_text(name, multiple)).danger("退出")
}

fn shows(allowed: bool) -> bool {
    allowed || show_unavailable()
}

/// Hint shown when an action is only simulated.
#[cfg(debug_assertions)]
pub fn mock_notice(overlays: Overlays) {
    overlays.toast(MOCK_TEXT);
}

/// Release builds have no developer mode: items without permission are not generated, so this is
/// never reached and there is no hint.
#[cfg(not(debug_assertions))]
pub const fn mock_notice(_overlays: Overlays) {}

/// When there is no permission (only reachable in developer mode), only show a hint and return
/// true.
fn mocks(overlays: Overlays, allowed: bool) -> bool {
    if !allowed {
        mock_notice(overlays);
    }
    !allowed
}

fn channel_of(session: &Session, key: &ConversationKey) -> Option<Channel> {
    match key {
        ConversationKey::Topic(topic) => session.channel(topic.stream_id),
        _ => None,
    }
}

fn can_rename(session: &Session, key: &ConversationKey) -> bool {
    channel_of(session, key).is_some_and(|c| {
        session
            .permissions()
            .can_move_topic_within_channel(&c.permissions)
    })
}

fn can_resolve(session: &Session, key: &ConversationKey) -> bool {
    channel_of(session, key)
        .is_some_and(|c| session.permissions().can_resolve_topic(&c.permissions))
}

fn can_delete(session: &Session) -> bool {
    session.permissions().can_delete_topic()
}

fn can_move(session: &Session, key: &ConversationKey) -> bool {
    channel_of(session, key).is_some_and(|c| {
        session
            .permissions()
            .can_move_topic_between_channels(&c.permissions)
    })
}

/// Move or rename; when some messages exceed the time limit, confirm and then handle only the
/// allowed ones.
async fn change_topic(
    session: Arc<Session>,
    overlays: Overlays,
    key: ConversationKey,
    destination: TopicDestination,
    action: &str,
) {
    let fail_text = format!("{action}失败");
    match session.move_topic(&key, &destination).await {
        Ok(()) => {}
        Err(SessionError::TopicMoveLimited {
            allowed,
            total,
            first_allowed_id,
        }) => {
            if allowed == 0 {
                overlays.report_error(&fail_text, "所有消息都已超出时间限制");
                return;
            }
            let message = format!(
                "只有 {allowed}/{total} 条消息可以{action}（更早的消息已超出时间限制），是否{action}这 {allowed} 条？"
            );
            overlays
                .confirm_then(Confirm::new(message), &fail_text, || {
                    session.move_topic_from(&key, first_allowed_id, &destination)
                })
                .await;
        }
        Err(error) => overlays.report_error(&fail_text, error),
    }
}

fn topic_stream(key: &ConversationKey) -> i64 {
    match key {
        ConversationKey::Topic(topic) => topic.stream_id,
        _ => 0,
    }
}

fn set_level(session: &Session, key: &ConversationKey, level: DndLevel) {
    session.set_conversation_level(key, level);
}

/// The four radio options of "Message settings".
pub fn level_menu_items(session: &Arc<Session>, key: &ConversationKey) -> MenuEntry {
    const LEVELS: [(DndLevel, &str); 4] = [
        (DndLevel::Notify, "允许消息提醒"),
        (DndLevel::Silent, "接收消息但不提醒"),
        (DndLevel::Assistant, "收进话题助手且不提醒"),
        (DndLevel::Blocked, "屏蔽消息"),
    ];
    let current = session.conversation_level(key);
    let items = LEVELS
        .iter()
        .map(|&(level, text)| {
            let session = session.clone();
            let key = key.clone();
            MenuEntry::item(text, move || set_level(&session, &key, level))
                .checked(current == level)
        })
        .collect();
    MenuEntry::submenu("消息设置", items)
}

/// "More settings": mark as unread (when `mark_unread` is true), copy link, move, resolve, delete,
/// rename topic.
pub fn topic_more_submenu(
    session: &Arc<Session>,
    overlays: Overlays,
    key: &ConversationKey,
    mark_unread: bool,
) -> MenuEntry {
    MenuEntry::submenu(
        "更多设置",
        topic_action_items(session, overlays, key, mark_unread),
    )
}

/// The items of "More settings", also used by the chat's more panel.
fn topic_action_items(
    session: &Arc<Session>,
    overlays: Overlays,
    key: &ConversationKey,
    mark_unread: bool,
) -> Vec<MenuEntry> {
    let mut items = Vec::new();
    if mark_unread {
        items.push(mark_unread_item(session, overlays, key));
    }
    {
        let session = session.clone();
        let key = key.clone();
        items.push(MenuEntry::item("复制链接", move || {
            let channel = channel_of(&session, &key).map_or_else(String::new, |c| c.name);
            let link = topic_link(
                session.realm_url(),
                topic_stream(&key),
                &channel,
                &session.topic_name(&key),
            );
            overlays.copy_text(&link);
        }));
    }
    items.extend(topic_move_item(session, overlays, key));
    items.extend(topic_resolve_item(session, overlays, key));
    items.extend(topic_delete_item(session, overlays, key));
    items.extend(topic_rename_item(session, overlays, key));
    items
}

/// Submenu "Move to another group": present only when moving is possible.
pub fn topic_move_item(
    session: &Arc<Session>,
    overlays: Overlays,
    key: &ConversationKey,
) -> Option<MenuEntry> {
    let stream = topic_stream(key);
    let targets = session.topic_move_targets(stream);
    if shows(can_move(session, key)) && !targets.is_empty() {
        let options = session
            .subscribed_channels()
            .into_iter()
            .filter(|c| c.stream_id == stream || targets.iter().any(|t| t.stream_id == c.stream_id))
            .map(|channel| {
                let is_current = channel.stream_id == stream;
                let session = session.clone();
                let key = key.clone();
                MenuEntry::item(format!("#{}", channel.name), move || {
                    if !is_current {
                        move_topic(session.clone(), overlays, key.clone(), channel.clone());
                    }
                })
                .checked(is_current)
            })
            .collect();
        return Some(MenuEntry::submenu("移动到其他分组", options));
    }
    None
}

/// "Mark as resolved" or "Mark as unresolved": present only when the action is possible.
pub fn topic_resolve_item(
    session: &Arc<Session>,
    overlays: Overlays,
    key: &ConversationKey,
) -> Option<MenuEntry> {
    let topic = session.topic_name(key);
    if shows(can_resolve(session, key)) && !topic.is_empty() {
        let text = if is_resolved_name(&topic) {
            "取消已解决"
        } else {
            "标记为已解决"
        };
        let session = session.clone();
        let key = key.clone();
        return Some(MenuEntry::item(text, move || {
            if mocks(overlays, can_resolve(&session, &key)) {
                return;
            }
            let session = session.clone();
            let key = key.clone();
            spawn_forever(async move {
                overlays
                    .attempt("操作失败", session.toggle_topic_resolved(&key))
                    .await;
            });
        }));
    }
    None
}

/// "Delete topic": present only when deletion is possible.
pub fn topic_delete_item(
    session: &Arc<Session>,
    overlays: Overlays,
    key: &ConversationKey,
) -> Option<MenuEntry> {
    if shows(can_delete(session)) {
        let session = session.clone();
        let key = key.clone();
        return Some(
            MenuEntry::item("删除话题", move || {
                if mocks(overlays, can_delete(&session)) {
                    return;
                }
                let session = session.clone();
                let key = key.clone();
                spawn_forever(async move {
                    let target = format!("话题「{}」", session.topic_name(&key));
                    overlays
                        .delete(
                            &target,
                            Some("话题中的所有消息将被永久删除"),
                            || session.delete(DeleteTarget::Topic(key.clone())),
                        )
                        .await;
                });
            })
            .danger(),
        );
    }
    None
}

/// "Rename topic": present only when renaming is possible.
pub fn topic_rename_item(
    session: &Arc<Session>,
    overlays: Overlays,
    key: &ConversationKey,
) -> Option<MenuEntry> {
    if shows(can_rename(session, key)) {
        let session = session.clone();
        let key = key.clone();
        return Some(MenuEntry::item("重命名话题", move || {
            let session = session.clone();
            let key = key.clone();
            spawn_forever(async move {
                let current = session.topic_name(&key);
                let prompt = TextPrompt::new("重命名话题", "填写话题名称", current.clone())
                    .max_len(session.realm().max_topic_length as usize);
                let Some(name) = overlays.prompt_text(prompt).await else {
                    return;
                };
                if mocks(overlays, can_rename(&session, &key)) {
                    return;
                }
                let name = if is_resolved_name(&current) && !is_resolved_name(&name) {
                    format!("{RESOLVED_PREFIX}{}", strip_resolved(&name))
                } else {
                    name
                };
                change_topic(
                    session.clone(),
                    overlays,
                    key.clone(),
                    TopicDestination {
                        stream_id: topic_stream(&key),
                        topic: name,
                    },
                    "重命名",
                )
                .await;
            });
        }));
    }
    None
}

fn move_topic(session: Arc<Session>, overlays: Overlays, key: ConversationKey, target: Channel) {
    spawn_forever(async move {
        let message = format!(
            "将话题「{}」移动到 #{}？",
            session.topic_name(&key),
            target.name
        );
        if !overlays.confirm(Confirm::new(message)).await {
            return;
        }
        if mocks(overlays, can_move(&session, &key)) {
            return;
        }
        let topic = session.topic_name(&key);
        change_topic(
            session,
            overlays,
            key,
            TopicDestination {
                stream_id: target.stream_id,
                topic,
            },
            "移动",
        )
        .await;
    });
}

fn mark_unread_item(
    session: &Arc<Session>,
    overlays: Overlays,
    key: &ConversationKey,
) -> MenuEntry {
    let session = session.clone();
    let key = key.clone();
    MenuEntry::item("标为未读", move || {
        let session = session.clone();
        let key = key.clone();
        spawn_forever(async move {
            let action = async {
                let last = |s: &Session| s.timeline(&key).messages.last().map(|m| m.id);
                let id = match last(&session) {
                    Some(id) => Some(id),
                    None => {
                        session.load_newest(&key).await?;
                        last(&session)
                    }
                };
                match id {
                    Some(id) => session.mark_unread_from(&key, id).await,
                    None => Ok(()),
                }
            };
            overlays.attempt("标为未读失败", action).await;
        });
    })
}

fn has_group_levels(key: &ConversationKey) -> bool {
    match key {
        ConversationKey::Topic(_) => true,
        ConversationKey::Dm(dm) => dm.is_group_dm(),
    }
}

/// Context menu of a message-page row. `on_removed` is called after "Remove from message list"
///
/// (closes the chat being shown).
pub fn conversation_menu(
    session: &Arc<Session>,
    overlays: Overlays,
    row: &ConversationRow,
    on_removed: impl Fn(&ConversationKey) + 'static,
) -> Vec<MenuEntry> {
    let blocked = row.level == DndLevel::Blocked;
    let mut items = Vec::new();

    {
        let session = session.clone();
        let item = PinItem::from(&row.entry);
        let pinned = row.pinned;
        items.push(MenuEntry::item(
            if pinned { "取消置顶" } else { "置顶" },
            move || {
                session.set_pinned(PinScope::Messages, &item, !pinned);
            },
        ));
    }
    let Some(key) = row.entry.conversation() else {
        return items;
    };
    let is_topic = matches!(key, ConversationKey::Topic(_));
    if session.unread_count(key) > 0 {
        let session = session.clone();
        let key = key.clone();
        items.push(MenuEntry::item("标为已读", move || {
            let session = session.clone();
            let key = key.clone();
            spawn_forever(async move {
                overlays
                    .attempt("标为已读失败", session.mark_conversation_read(&key))
                    .await;
            });
        }));
    }
    if !is_topic && session.unread_count(key) == 0 && !blocked && row.time > 0 {
        items.push(mark_unread_item(session, overlays, key));
    }
    items.push(MenuEntry::Separator);
    if has_group_levels(key) {
        items.push(level_menu_items(session, key));
    } else {
        let muted = row.level != DndLevel::Notify;
        let session_for_level = session.clone();
        let key_for_level = key.clone();
        items.push(MenuEntry::item(
            if muted {
                "取消免打扰"
            } else {
                "设置免打扰"
            },
            move || {
                let level = if muted {
                    DndLevel::Notify
                } else {
                    DndLevel::Silent
                };
                set_level(&session_for_level, &key_for_level, level);
            },
        ));
    }
    items.push(MenuEntry::Separator);
    if !windows::uses_chat_windows() {
        let account = session.account_id();
        let key = key.clone();
        items.push(MenuEntry::item("打开独立聊天窗口", move || {
            windows::open_chat_window(account, key.clone(), None);
        }));
    }
    {
        let session = session.clone();
        let key = key.clone();
        items.push(MenuEntry::item("从消息列表中移除", move || {
            let session = session.clone();
            let target = DeleteTarget::ConversationEntry(key.clone());
            spawn_forever(async move {
                overlays.attempt("移除失败", session.delete(target)).await;
            });
            on_removed(&key);
        }));
    }
    if is_topic {
        items.push(MenuEntry::Separator);
        items.push(topic_more_submenu(
            session,
            overlays,
            key,
            session.unread_count(key) == 0 && !blocked && row.time > 0,
        ));
    }
    append_plugin_menus(
        &mut items,
        session,
        &ContextMenuTarget::Conversation(key.clone()),
    );
    items
}

/// Context menu of a channel group header on the contacts page.
pub fn channel_section_menu(
    session: &Arc<Session>,
    overlays: Overlays,
    stream_id: i64,
) -> Vec<MenuEntry> {
    let Some(channel) = session.channel(stream_id) else {
        return Vec::new();
    };
    let colors = CHANNEL_COLORS
        .iter()
        .map(|&color| {
            let session = session.clone();
            MenuEntry::item(color, move || {
                let session = session.clone();
                spawn_forever(async move {
                    overlays
                        .attempt(
                            "修改失败",
                            session.set_subscription_property(stream_id, "color", json!(color)),
                        )
                        .await;
                });
            })
            .checked(channel.color.eq_ignore_ascii_case(color))
        })
        .collect();
    let remark = {
        let session = session.clone();
        let name = channel.name.clone();
        MenuEntry::item("备注", move || {
            let session = session.clone();
            let name = name.clone();
            spawn_forever(async move {
                let current = session.channel_display_name(stream_id);
                let remark = (current != name).then_some(current);
                let prompt = TextPrompt::remark("设置备注", &name, remark.as_deref());
                if let Some(text) = overlays.prompt_text(prompt).await {
                    overlays
                        .attempt(
                            "设置备注失败",
                            session.rename(
                                RenameTarget::Remark(RemarkTarget::Channel(stream_id)),
                                &text,
                            ),
                        )
                        .await;
                }
            });
        })
    };
    let copy = {
        let session = session.clone();
        let name = channel.name.clone();
        MenuEntry::item("复制频道链接", move || {
            let link = channel_link(session.realm_url(), stream_id, &name);
            overlays.copy_text(&link);
        })
    };
    let leave = {
        let session = session.clone();
        let name = channel.name.clone();
        MenuEntry::item("退订频道", move || {
            let session = session.clone();
            let name = name.clone();
            spawn_forever(async move {
                let confirm = Confirm::new(format!("确定要退订频道 #{name} 吗？")).danger("退订");
                let names = [name.clone()];
                overlays
                    .confirm_then(confirm, "退订失败", || {
                        session.unsubscribe_channel(&names)
                    })
                    .await;
            });
        })
        .danger()
    };
    let mut items = vec![
        MenuEntry::submenu("频道颜色", colors),
        remark,
        MenuEntry::Separator,
        copy,
        leave,
    ];
    append_plugin_menus(&mut items, session, &ContextMenuTarget::Channel(stream_id));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use zulip_plugin_host::MenuWhen;

    fn menu(id: i32, conv: Option<MenuConv>, children: Vec<PluginMenu>) -> PluginMenu {
        PluginMenu {
            id,
            name: format!("菜单 {id}"),
            children,
            shortcut: None,
            when: conv.map(|conv| MenuWhen {
                conv: Some(conv),
                ..MenuWhen::default()
            }),
        }
    }

    #[test]
    fn plugin_menus_follow_the_context() {
        let menus = [
            menu(1, None, Vec::new()),
            menu(2, Some(MenuConv::Direct), Vec::new()),
            menu(
                0,
                None,
                vec![
                    menu(3, Some(MenuConv::Direct), Vec::new()),
                    menu(4, Some(MenuConv::Channel), Vec::new()),
                ],
            ),
            menu(0, None, vec![menu(5, Some(MenuConv::Direct), Vec::new())]),
        ];
        let invoke: Rc<dyn Fn(i32)> = Rc::new(|_| {});
        let count = |conv| {
            let ctx = MenuContext {
                conv,
                ..MenuContext::default()
            };
            plugin_menu_entries(&menus, &ctx, &invoke).len()
        };
        assert_eq!(count(MenuConv::Direct), 4);
        assert_eq!(count(MenuConv::Channel), 2);
    }

    #[test]
    fn conversation_kind_of_key() {
        assert_eq!(
            menu_conv(&ConversationKey::topic(1, "a")),
            MenuConv::Channel
        );
        assert_eq!(menu_conv(&ConversationKey::dm([2], 1)), MenuConv::Direct);
    }
}
