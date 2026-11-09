//! Chat page state: timeline entries, banners, multi-select, locating.

use crate::pages::composer::ComposerHandle;
use crate::services::plugins;
use crate::ui::{Overlays, ScrollAlign, VirtualListHandle};
use dioxus::dioxus_core::{Runtime, current_scope_id};
use dioxus::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use zulip_core::ConversationKey;
use zulip_core::model::{ChatMessage, MentionKind, MessageState};
use zulip_core::notice::SystemNoticeKind;
use zulip_core::text::time;
use zulip_session::{ScrollPosition, Session};

const FLASH_MS: u64 = 1500;
const RELOAD_DELAY: Duration = Duration::from_millis(16);
/// Spacing left when aligning to the top.
const TOP_GAP: f64 = 8.0;
/// Approximate height of a time separator: converted to a message position when the anchor lands on
/// a separator.
const SEPARATOR_HEIGHT: f64 = 34.0;

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Loading,
    Time { id: i64, text: String },
    Divider,
    Notice(i64),
    Deleted(i64),
    Message(i64),
}

const KIND_MESSAGE: u64 = 0;
const KIND_TIME: u64 = 1;
const KIND_NOTICE: u64 = 2;
const KIND_DELETED: u64 = 3;
const KEY_DIVIDER: u64 = 4;
const KEY_LOADING: u64 = 5;

/// The id of a local echo is negative, so the key reserves a separate bit for the sign.
fn id_key(id: i64, kind: u64) -> u64 {
    (id.unsigned_abs() << 4) | (u64::from(id < 0) << 3) | kind
}

pub fn message_key(id: i64) -> u64 {
    id_key(id, KIND_MESSAGE)
}

pub fn item_key(item: &Item) -> u64 {
    match item {
        Item::Loading => KEY_LOADING,
        Item::Divider => KEY_DIVIDER,
        Item::Time { id, .. } => id_key(*id, KIND_TIME),
        Item::Notice(id) => id_key(*id, KIND_NOTICE),
        Item::Deleted(id) => id_key(*id, KIND_DELETED),
        Item::Message(id) => id_key(*id, KIND_MESSAGE),
    }
}

/// The message for a key; a divider belongs to no message.
pub fn key_message(key: u64) -> Option<i64> {
    let kind = key & 0b111;
    if key < 16 || kind > KIND_DELETED {
        return None;
    }
    let id = (key >> 4) as i64;
    Some(if key & 0b1000 != 0 { -id } else { id })
}

fn key_kind_is_time(key: u64) -> bool {
    key >= 16 && key & 0b111 == KIND_TIME
}

/// Timeline -> list rows: inserts time separators and the "new messages" divider as needed (always
/// followed by a time separator).
pub fn build_items(
    messages: &[Arc<ChatMessage>],
    first_unread: Option<i64>,
    loading_older: bool,
    now: chrono::NaiveDateTime,
    use_24h: bool,
) -> Vec<Item> {
    let mut items = Vec::with_capacity(messages.len() + messages.len() / 4 + 2);
    if loading_older {
        items.push(Item::Loading);
    }
    let divider_at = first_unread.and_then(|first| {
        messages
            .iter()
            .position(|m| m.id > 0 && m.id >= first)
            .map(|i| messages[i].id)
    });
    let mut previous = None;
    for m in messages {
        let at_divider = divider_at == Some(m.id);
        if at_divider {
            items.push(Item::Divider);
        }
        let separated = previous.is_none_or(|p| time::needs_separator(p, m.timestamp));
        if (separated || at_divider)
            && let Some(when) = time::from_unix(m.timestamp)
        {
            items.push(Item::Time {
                id: m.id,
                text: time::separator_time(when, now, use_24h),
            });
        }
        previous = Some(m.timestamp);
        items.push(if m.is_deleted() {
            Item::Deleted(m.id)
        } else if m.notice == SystemNoticeKind::Inline {
            Item::Notice(m.id)
        } else {
            Item::Message(m.id)
        });
    }
    items
}

/// The "N new messages" banner at the top: the number of unread messages at open time that have not
/// been scrolled to yet.
pub fn unseen_above(unread_at_open: &[i64], min_top_seen: i64) -> usize {
    unread_at_open
        .iter()
        .filter(|id| **id < min_top_seen)
        .count()
}

/// Where to scroll to on entering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Bottom,
    Divider,
    Message(i64),
    Restore { id: i64, offset: f64 },
}

/// Message locate -> unread -> last position -> bottom.
pub fn initial_target(
    focus: Option<i64>,
    first_unread: Option<i64>,
    saved: Option<ScrollPosition>,
) -> Target {
    if let Some(id) = focus {
        Target::Message(id)
    } else if first_unread.is_some() {
        Target::Divider
    } else if let Some(p) = saved {
        Target::Restore {
            id: p.message_id,
            offset: f64::from(-p.offset),
        }
    } else {
        Target::Bottom
    }
}

/// Page state of the current conversation.
#[derive(Clone, Copy)]
pub struct Chat {
    scope: ScopeId,
    pub session: CopyValue<Arc<Session>>,
    pub account: i64,
    pub overlays: Overlays,
    pub composer: ComposerHandle,
    pub key: Signal<ConversationKey>,
    pub list: CopyValue<Option<VirtualListHandle>>,

    pub messages: Signal<Rc<HashMap<i64, Arc<ChatMessage>>>>,
    pub order: CopyValue<Vec<i64>>,
    pub items: Signal<Rc<Vec<Item>>>,
    pub keys: Signal<Rc<Vec<u64>>>,
    pub loaded: Signal<bool>,
    pub load_failed: Signal<bool>,
    pub found_newest: CopyValue<bool>,
    pub found_oldest: CopyValue<bool>,
    pub loading_older: Signal<bool>,
    pub loading_newer: CopyValue<bool>,
    /// Incremented when user profiles or the mute list change, so row components re-read names.
    pub users_tick: Signal<u64>,
    /// Incremented when plugin annotations change.
    pub plugin_tick: Signal<u64>,

    pub first_unread: Signal<Option<i64>>,
    pub unread_at_open: Signal<Rc<Vec<i64>>>,
    pub mentions: Signal<Vec<(i64, MentionKind)>>,
    pub min_top_seen: Signal<i64>,
    pub new_first: Signal<Option<i64>>,
    pub new_count: Signal<usize>,
    pub at_bottom: Signal<bool>,
    pub auto_read_suspended: CopyValue<bool>,
    pub position: CopyValue<Option<ScrollPosition>>,
    pub shown: CopyValue<HashSet<i64>>,
    pub visible: CopyValue<Vec<i64>>,
    pub top_anchor: CopyValue<Option<(u64, f64)>>,
    pub reload_pending: CopyValue<bool>,

    pub peer_read: Signal<Option<bool>>,
    pub last_own: Signal<Option<i64>>,

    pub multi: Signal<bool>,
    pub selected: Signal<BTreeSet<i64>>,
    pub flash: Signal<Option<i64>>,
    pub source_mode: Signal<bool>,
    pub revealed: Signal<HashSet<i64>>,
    pub sources: Signal<HashMap<(i64, Option<i64>), String>>,
    pub suppress_menu: CopyValue<bool>,
    pub more_open: Signal<bool>,
}

impl Chat {
    pub fn new(
        session: Arc<Session>,
        key: ConversationKey,
        overlays: Overlays,
        composer: ComposerHandle,
    ) -> Self {
        Self {
            scope: current_scope_id(),
            account: session.account_id(),
            session: CopyValue::new(session),
            overlays,
            composer,
            key: Signal::new(key),
            list: CopyValue::new(None),
            messages: Signal::new(Rc::default()),
            order: CopyValue::new(Vec::new()),
            items: Signal::new(Rc::default()),
            keys: Signal::new(Rc::default()),
            loaded: Signal::new(false),
            load_failed: Signal::new(false),
            found_newest: CopyValue::new(false),
            found_oldest: CopyValue::new(false),
            loading_older: Signal::new(false),
            loading_newer: CopyValue::new(false),
            users_tick: Signal::new(0),
            plugin_tick: Signal::new(0),
            first_unread: Signal::new(None),
            unread_at_open: Signal::new(Rc::default()),
            mentions: Signal::new(Vec::new()),
            min_top_seen: Signal::new(i64::MAX),
            new_first: Signal::new(None),
            new_count: Signal::new(0),
            at_bottom: Signal::new(true),
            auto_read_suspended: CopyValue::new(false),
            position: CopyValue::new(None),
            shown: CopyValue::new(HashSet::new()),
            visible: CopyValue::new(Vec::new()),
            top_anchor: CopyValue::new(None),
            reload_pending: CopyValue::new(false),
            peer_read: Signal::new(None),
            last_own: Signal::new(None),
            multi: Signal::new(false),
            selected: Signal::new(BTreeSet::new()),
            flash: Signal::new(None),
            source_mode: Signal::new(false),
            revealed: Signal::new(HashSet::new()),
            sources: Signal::new(HashMap::new()),
            suppress_menu: CopyValue::new(false),
            more_open: Signal::new(false),
        }
    }

    /// Background tasks that follow the chat page lifecycle; callbacks of menus and dialogs use it
    /// too, so they never touch page state after the page is unmounted.
    pub fn spawn(self, task: impl Future<Output = ()> + 'static) {
        Runtime::current().in_scope(self.scope, || {
            spawn(task);
        });
    }

    pub fn session(self) -> Arc<Session> {
        self.session.peek().clone()
    }

    pub fn current_key(self) -> ConversationKey {
        self.key.peek().clone()
    }

    pub fn message(self, id: i64) -> Option<Arc<ChatMessage>> {
        self.messages.peek().get(&id).cloned()
    }

    /// Reloads after a batch of events are merged into one.
    pub fn schedule_reload(self) {
        let mut pending = self.reload_pending;
        if *pending.peek() {
            return;
        }
        *pending.write() = true;
        self.spawn(async move {
            tokio::time::sleep(RELOAD_DELAY).await;
            *pending.write() = false;
            self.reload();
        });
    }

    /// Re-read the timeline from the conversation layer.
    pub fn reload(mut self) {
        let session = self.session();
        let key = self.current_key();
        let snapshot = session.timeline(&key);
        *self.found_newest.write() = snapshot.found_newest;
        *self.found_oldest.write() = snapshot.found_oldest;
        let now = chrono::Local::now().naive_local();
        let use_24h = session.use_24h();
        let items = build_items(
            &snapshot.messages,
            *self.first_unread.peek(),
            *self.loading_older.peek(),
            now,
            use_24h,
        );
        let keys: Vec<u64> = items.iter().map(item_key).collect();
        if let Some(list) = self.list.peek().as_ref() {
            list.sync_keys(&keys);
        }
        let self_id = session.self_id();
        let last_own = snapshot
            .messages
            .iter()
            .rev()
            .find(|m| m.sender_id == self_id && m.id > 0 && !m.is_deleted())
            .map(|m| m.id);
        if *self.last_own.peek() != last_own {
            self.last_own.set(last_own);
        }
        *self.order.write() = snapshot.messages.iter().map(|m| m.id).collect();
        let map: HashMap<i64, Arc<ChatMessage>> = snapshot
            .messages
            .iter()
            .map(|m| (m.id, m.clone()))
            .collect();
        self.messages.set(Rc::new(map));
        if **self.items.peek() != items {
            self.items.set(Rc::new(items));
            self.keys.set(Rc::new(keys));
        }
    }

    /// Refresh "read/unread" (only one-to-one direct messages have a result).
    pub async fn refresh_peer_read(mut self) {
        let key = self.current_key();
        if let Ok(read) = self.session().peer_has_read(&key).await
            && *self.peer_read.peek() != read
        {
            self.peer_read.set(read);
        }
    }

    pub async fn load_older(mut self) {
        if *self.loading_older.peek() || *self.found_oldest.peek() {
            return;
        }
        self.loading_older.set(true);
        self.reload();
        let key = self.current_key();
        if let Err(error) = self.session().load_older(&key).await {
            log::warn!("加载更早的消息失败：{error}");
        }
        self.loading_older.set(false);
        self.reload();
    }

    pub async fn load_newer(mut self) {
        if *self.loading_newer.peek() || *self.found_newest.peek() {
            return;
        }
        *self.loading_newer.write() = true;
        let key = self.current_key();
        if let Err(error) = self.session().load_newer(&key).await {
            log::warn!("加载更新的消息失败：{error}");
        }
        *self.loading_newer.write() = false;
        let anchor = *self.top_anchor.peek();
        self.reload();
        // Sticking to the bottom only takes effect once the latest is loaded: paging down must stay
        // in place
        if let (true, Some((key, offset)), Some(list)) =
            (*self.at_bottom.peek(), anchor, self.list.peek().as_ref())
        {
            list.scroll_with_offset(key, ScrollAlign::Start, offset);
        }
    }

    pub fn flash_message(mut self, id: i64) {
        self.flash.set(Some(id));
        self.spawn(async move {
            tokio::time::sleep(Duration::from_millis(FLASH_MS)).await;
            if *self.flash.peek() == Some(id) {
                self.flash.set(None);
            }
        });
    }

    /// Locate a message: when it is not in the current window, load the messages around it first.
    pub async fn jump_to(self, id: i64, highlight: bool, align: ScrollAlign) {
        if self.messages.peek().get(&id).is_none() {
            let key = self.current_key();
            if let Err(error) = self.session().locate_message(Some(&key), id).await {
                self.overlays.report_error("定位消息失败", error);
                return;
            }
            self.reload();
        }
        let key = self.scroll_key_of(id);
        let offset = if align == ScrollAlign::Start {
            -TOP_GAP
        } else {
            0.0
        };
        if let Some(list) = self.list.peek().as_ref() {
            list.scroll_with_offset(key, align, offset);
        }
        if highlight {
            self.flash_message(id);
        }
    }

    fn scroll_key_of(self, id: i64) -> u64 {
        let items = self.items.peek();
        let item = items.iter().find(|item| {
            matches!(item, Item::Message(i) | Item::Notice(i) | Item::Deleted(i) if *i == id)
        });
        item.map_or_else(|| message_key(id), item_key)
    }

    /// Jump to the first unread message, with the "new messages" divider at the top of the
    /// viewport.
    pub async fn jump_to_first_unread(mut self) {
        let Some(first) = *self.first_unread.peek() else {
            return;
        };
        if self.messages.peek().get(&first).is_none() {
            let key = self.current_key();
            if let Err(error) = self.session().locate_message(Some(&key), first).await {
                self.overlays.report_error("定位消息失败", error);
                return;
            }
            self.reload();
        }
        if let Some(list) = self.list.peek().as_ref() {
            list.scroll_to_key(KEY_DIVIDER, ScrollAlign::Start);
        }
        let seen = (*self.min_top_seen.peek()).min(first);
        self.min_top_seen.set(seen);
    }

    /// Back to the latest: reload when the window does not contain the latest message.
    pub async fn show_latest(mut self) {
        self.new_first.set(None);
        self.new_count.set(0);
        if !*self.found_newest.peek() {
            let key = self.current_key();
            if let Err(error) = self.session().load_newest(&key).await {
                self.overlays.report_error("消息加载失败", error);
                return;
            }
            self.reload();
        }
        if let Some(list) = self.list.peek().as_ref() {
            list.scroll_to_bottom();
        }
    }

    /// Viewport change: update the banner and scroll position; messages that were seen are handed
    /// to plugins.
    pub fn on_viewport(
        mut self,
        first: Option<u64>,
        offset: f64,
        last: Option<u64>,
        at_bottom: bool,
    ) {
        if *self.at_bottom.peek() != at_bottom {
            self.at_bottom.set(at_bottom);
        }
        *self.top_anchor.write() = first.map(|key| (key, offset));
        let first_id = first.and_then(key_message);
        let position = match (first, first_id) {
            (_, _) if at_bottom => None,
            (Some(key), Some(id)) if id > 0 => Some(ScrollPosition {
                message_id: id,
                offset: if key_kind_is_time(key) {
                    (SEPARATOR_HEIGHT - offset) as i32
                } else {
                    -offset as i32
                },
            }),
            _ => None,
        };
        *self.position.write() = position;
        if at_bottom && *self.found_newest.peek() && *self.new_count.peek() > 0 {
            self.new_count.set(0);
            self.new_first.set(None);
        }
        let Some(top) = first_id.filter(|id| *id > 0) else {
            return;
        };
        if top < *self.min_top_seen.peek() {
            self.min_top_seen.set(top);
        }
        // The bottom is estimated from the height table, so right after loading it may lag behind
        // the top
        let bottom = last.and_then(key_message).unwrap_or(top).max(top);
        if self
            .mentions
            .peek()
            .iter()
            .any(|(id, _)| *id >= top && *id <= bottom)
        {
            self.mentions
                .write()
                .retain(|(id, _)| *id < top || *id > bottom);
        }
        let order = self.order.peek();
        let start = order.partition_point(|id| *id < top);
        let end = order.partition_point(|id| *id <= bottom);
        let visible: Vec<i64> = order[start..end]
            .iter()
            .copied()
            .filter(|id| *id > 0)
            .collect();
        drop(order);
        *self.visible.write() = visible.clone();
        let fresh: Vec<i64> = {
            let mut shown = self.shown.write();
            visible.into_iter().filter(|id| shown.insert(*id)).collect()
        };
        if !fresh.is_empty() {
            plugins().notify_messages_shown(&self.session(), &fresh);
        }
    }

    /// Unread messages within the viewport.
    pub fn visible_unread(self) -> Vec<i64> {
        let messages = self.messages.peek();
        self.visible
            .peek()
            .iter()
            .filter(|id| {
                messages
                    .get(id)
                    .is_some_and(|m| !m.flags.read && m.state == MessageState::Normal)
            })
            .copied()
            .collect()
    }

    pub fn toggle_selected(mut self, id: i64) {
        let mut selected = self.selected.write();
        if !selected.remove(&id) {
            selected.insert(id);
        }
    }

    pub fn enter_multi(mut self, id: i64) {
        self.selected.write().clear();
        self.selected.write().insert(id);
        self.multi.set(true);
    }

    pub fn exit_multi(mut self) {
        self.multi.set(false);
        self.selected.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn message(id: i64, timestamp: i64) -> Arc<ChatMessage> {
        Arc::new(ChatMessage::new(
            id,
            ConversationKey::topic(1, "t"),
            1,
            timestamp,
        ))
    }

    fn now() -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .expect("日期有效")
    }

    #[test]
    fn keys_round_trip_and_do_not_collide() {
        assert_eq!(key_message(message_key(12)), Some(12));
        assert_eq!(key_message(message_key(-12)), Some(-12));
        assert_eq!(key_message(id_key(12, KIND_TIME)), Some(12));
        assert_eq!(key_message(KEY_DIVIDER), None);
        assert_eq!(key_message(KEY_LOADING), None);
        assert_ne!(message_key(12), id_key(12, KIND_TIME));
        assert_ne!(message_key(1), KEY_DIVIDER);
    }

    #[test]
    fn items_insert_time_and_divider() {
        let messages = vec![message(1, 1000), message(2, 1010), message(3, 1020)];
        let items = build_items(&messages, Some(2), false, now(), true);
        assert!(matches!(items[0], Item::Time { id: 1, .. }));
        assert_eq!(items[1], Item::Message(1));
        assert_eq!(items[2], Item::Divider);
        assert!(matches!(items[3], Item::Time { id: 2, .. }));
        assert_eq!(items[4], Item::Message(2));
        assert_eq!(items[5], Item::Message(3));
    }

    #[test]
    fn divider_falls_on_next_message_when_first_unread_gone() {
        let messages = vec![message(1, 1000), message(5, 1010)];
        let items = build_items(&messages, Some(3), false, now(), true);
        assert_eq!(items[2], Item::Divider);
    }

    #[test]
    fn loading_row_comes_first() {
        let items = build_items(&[message(1, 1000)], None, true, now(), true);
        assert_eq!(items[0], Item::Loading);
    }

    #[test]
    fn unseen_counts_only_above_viewport() {
        assert_eq!(unseen_above(&[5, 6, 7, 8], 7), 2);
        assert_eq!(unseen_above(&[5, 6], i64::MAX), 2);
        assert_eq!(unseen_above(&[], 3), 0);
    }

    #[test]
    fn target_priority() {
        let saved = Some(ScrollPosition {
            message_id: 3,
            offset: -20,
        });
        assert_eq!(initial_target(Some(9), Some(4), saved), Target::Message(9));
        assert_eq!(initial_target(None, Some(4), saved), Target::Divider);
        assert_eq!(
            initial_target(None, None, saved),
            Target::Restore {
                id: 3,
                offset: 20.0
            }
        );
        assert_eq!(initial_target(None, None, None), Target::Bottom);
    }
}
