//! Drafts, system notifications, message reminders, scheduled messages, canned replies and "typing"
//! indicators.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;
use zulip_api::{DraftDto, ReminderDto, ScheduledMessageDto};
use zulip_core::ConversationKey;
use zulip_core::model::{ChatMessage, Reminder, SavedSnippet, ScheduledMessage};
use zulip_core::notice::{self, SystemNoticeCategory, SystemNoticeKind};
use zulip_store::{AccountSettings, LocalDraftRepo};

static USER_ID_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"data-user-id="(\d+)""#).expect("正则表达式有效"));
static NEAR_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/near/(\d+)").expect("正则表达式有效"));

/// A draft.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub content: String,
    /// Id on the server; `None` when stored only on this machine.
    pub server_id: Option<i64>,
    pub updated_at: i64,
}

/// Drafts: in sync mode the server is authoritative, otherwise they live only on this machine. This
/// only maintains the displayed value; sending to the server is serialized by the session.
pub struct Drafts {
    entries: HashMap<ConversationKey, Draft>,
    pub repo: LocalDraftRepo,
    /// Conversations currently being saved to the server: server event content is ignored
    /// meanwhile, so stale content does not overwrite what is being typed.
    pub saving: HashSet<ConversationKey>,
    /// If there were changes during the save, this value is sent once the save finishes.
    pub queued: HashMap<ConversationKey, String>,
}

pub fn draft_key(dto: &DraftDto, self_id: i64) -> Option<ConversationKey> {
    match dto.kind.as_str() {
        "stream" => Some(ConversationKey::topic(*dto.to.first()?, dto.topic.clone())),
        "private" if dto.to.is_empty() => Some(ConversationKey::dm([self_id], self_id)),
        "private" => Some(ConversationKey::dm(dto.to.iter().copied(), self_id)),
        _ => None,
    }
}

pub fn draft_to_dto(
    key: &ConversationKey,
    topic: &str,
    content: &str,
    self_id: i64,
    id: Option<i64>,
    at: i64,
) -> DraftDto {
    let (kind, to, topic) = match key {
        ConversationKey::Topic(t) => ("stream", vec![t.stream_id], topic.to_owned()),
        ConversationKey::Dm(d) if d.is_self_dm() => ("private", vec![self_id], String::new()),
        ConversationKey::Dm(d) => ("private", d.other_user_ids().to_vec(), String::new()),
    };
    DraftDto {
        id,
        kind: kind.to_owned(),
        to,
        topic,
        content: content.to_owned(),
        timestamp: Some(at),
    }
}

impl Drafts {
    /// `local` holds the drafts stored on this machine, used only when sync is turned off.
    pub fn new(
        sync: bool,
        server: &[DraftDto],
        local: HashMap<String, (String, i64)>,
        repo: LocalDraftRepo,
        self_id: i64,
    ) -> Self {
        let mut entries: HashMap<ConversationKey, Draft> = HashMap::new();
        if sync {
            for dto in server {
                if let Some(key) = draft_key(dto, self_id) {
                    upsert_latest(&mut entries, key, dto);
                }
            }
        } else {
            for (key, (content, updated_at)) in local {
                if let Ok(key) = key.parse() {
                    entries.insert(
                        key,
                        Draft {
                            content,
                            server_id: None,
                            updated_at,
                        },
                    );
                }
            }
        }
        Self {
            entries,
            repo,
            saving: HashSet::new(),
            queued: HashMap::new(),
        }
    }

    pub fn get(&self, key: &ConversationKey) -> Option<&Draft> {
        self.entries.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ConversationKey, &Draft)> {
        self.entries.iter()
    }

    /// Updates the displayed value; empty content deletes it.
    pub fn set_visible(&mut self, key: &ConversationKey, content: &str, at: i64) {
        if content.is_empty() {
            self.entries.remove(key);
        } else {
            let entry = self.entries.entry(key.clone()).or_default();
            content.clone_into(&mut entry.content);
            entry.updated_at = at;
        }
    }

    pub fn set_server_id(&mut self, key: &ConversationKey, id: Option<i64>) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.server_id = id;
        }
    }

    pub fn replace_all(&mut self, entries: HashMap<ConversationKey, Draft>) {
        self.entries = entries;
    }

    /// `drafts` event; returns the conversations that changed.
    pub fn apply_event(&mut self, event: &Value, self_id: i64) -> Vec<ConversationKey> {
        let mut changed = Vec::new();
        match event.get("op").and_then(Value::as_str).unwrap_or_default() {
            "add" => {
                for dto in event
                    .get("drafts")
                    .and_then(|v| serde_json::from_value::<Vec<DraftDto>>(v.clone()).ok())
                    .unwrap_or_default()
                {
                    changed.extend(self.apply_server_draft(&dto, self_id));
                }
            }
            "update" => {
                if let Some(dto) = event
                    .get("draft")
                    .and_then(|v| serde_json::from_value::<DraftDto>(v.clone()).ok())
                {
                    changed.extend(self.apply_server_draft(&dto, self_id));
                }
            }
            "remove" => {
                let id = event.get("draft_id").and_then(Value::as_i64);
                let key = self
                    .entries
                    .iter()
                    .find(|(_, d)| d.server_id == id)
                    .map(|(k, _)| k.clone());
                if let Some(key) = key.filter(|k| !self.saving.contains(k)) {
                    self.entries.remove(&key);
                    changed.push(key);
                }
            }
            _ => {}
        }
        changed
    }

    fn apply_server_draft(&mut self, dto: &DraftDto, self_id: i64) -> Option<ConversationKey> {
        let key = draft_key(dto, self_id)?;
        if self.saving.contains(&key) {
            self.set_server_id(&key, dto.id);
            return None;
        }
        upsert_latest(&mut self.entries, key.clone(), dto).then_some(key)
    }
}

fn upsert_latest(
    entries: &mut HashMap<ConversationKey, Draft>,
    key: ConversationKey,
    dto: &DraftDto,
) -> bool {
    let at = dto.timestamp.unwrap_or_default();
    if entries.get(&key).is_some_and(|d| d.updated_at > at) {
        return false;
    }
    entries.insert(
        key,
        Draft {
            content: dto.content.clone(),
            server_id: dto.id,
            updated_at: at,
        },
    );
    true
}

/// A system notification or a delivered message reminder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemNotice {
    pub id: i64,
    pub kind: SystemNoticeKind,
    pub category: SystemNoticeCategory,
    /// Translated HTML.
    pub html: String,
    pub timestamp: i64,
    pub stream_id: Option<i64>,
    pub topic: String,
    pub mentioned_user_ids: Vec<i64>,
    pub is_read: bool,
    /// The target message of the reminder.
    pub target_message_id: Option<i64>,
}

pub struct SystemNotices {
    entries: Vec<SystemNotice>,
    settings: AccountSettings,
}

impl SystemNotices {
    pub fn new(settings: AccountSettings) -> Self {
        Self {
            entries: Vec::new(),
            settings,
        }
    }

    pub fn add(&mut self, m: &ChatMessage) -> bool {
        if self.entries.iter().any(|e| e.id == m.id) {
            return false;
        }
        let is_reminder = m.notice == SystemNoticeKind::Reminder;
        let html = if is_reminder {
            notice::localize_reminder(&m.content_html)
        } else {
            notice::localize(&m.content_html)
        };
        let target_message_id = NEAR_ID
            .captures(&m.content_html)
            .filter(|_| is_reminder)
            .and_then(|c| c[1].parse().ok());
        self.entries.push(SystemNotice {
            id: m.id,
            kind: m.notice,
            category: notice::category_of(m.stream_id, &m.topic),
            html,
            timestamp: m.timestamp,
            stream_id: m.stream_id,
            topic: m.topic.clone(),
            mentioned_user_ids: USER_ID_ATTR
                .captures_iter(&m.content_html)
                .filter_map(|c| c[1].parse().ok())
                .collect(),
            is_read: m.is_read(),
            target_message_id,
        });
        self.entries.sort_by_key(|e| std::cmp::Reverse(e.id));
        true
    }

    pub fn contains(&self, id: i64) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    pub fn remove(&mut self, ids: &[i64]) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| !ids.contains(&e.id));
        before != self.entries.len()
    }

    /// Returns the ids that actually changed.
    pub fn set_read(&mut self, ids: &[i64], read: bool) -> Vec<i64> {
        let mut changed = Vec::new();
        for entry in self.entries.iter_mut().filter(|e| ids.contains(&e.id)) {
            if entry.is_read != read {
                entry.is_read = read;
                changed.push(entry.id);
            }
        }
        changed
    }

    pub fn mark_all_read(&mut self) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            changed |= !entry.is_read;
            entry.is_read = true;
        }
        changed
    }

    /// Newest first; cleared ones are not shown.
    pub fn notices(&self) -> Vec<SystemNotice> {
        let cleared_before = self.settings.system_notices_cleared_before();
        self.entries
            .iter()
            .filter(|e| e.kind == SystemNoticeKind::Notice && e.id > cleared_before)
            .cloned()
            .collect()
    }

    pub fn delivered_reminders(&self) -> Vec<SystemNotice> {
        let cleared = self.settings.cleared_reminder_ids();
        self.entries
            .iter()
            .filter(|e| e.kind == SystemNoticeKind::Reminder && !cleared.contains(&e.id))
            .cloned()
            .collect()
    }

    pub fn unread_notice_count(&self) -> usize {
        self.notices().iter().filter(|e| !e.is_read).count()
    }

    pub fn unread_reminder_count(&self) -> usize {
        self.delivered_reminders()
            .iter()
            .filter(|e| !e.is_read)
            .count()
    }

    pub fn unread_ids(&self) -> Vec<i64> {
        self.entries
            .iter()
            .filter(|e| !e.is_read)
            .map(|e| e.id)
            .collect()
    }

    pub fn clear_notices(&mut self) {
        let cleared_before = self.settings.system_notices_cleared_before();
        let max = self
            .entries
            .iter()
            .filter(|e| e.kind == SystemNoticeKind::Notice)
            .map(|e| e.id)
            .max()
            .unwrap_or(cleared_before);
        self.settings
            .set_system_notices_cleared_before(cleared_before.max(max));
    }

    /// Clears one delivered reminder (on this machine only). Cleared ids keep only reminders that
    /// are still loaded, so the set does not grow without bound.
    pub fn clear_reminder(&mut self, id: i64) {
        let mut cleared = self.settings.cleared_reminder_ids();
        cleared.insert(id);
        let loaded: HashSet<i64> = self.entries.iter().map(|e| e.id).collect();
        cleared.retain(|i| loaded.contains(i));
        self.settings.set_cleared_reminder_ids(&cleared);
    }

    pub fn clear_all_reminders(&mut self) {
        self.settings.set_cleared_reminder_ids(
            &self
                .entries
                .iter()
                .filter(|e| e.kind == SystemNoticeKind::Reminder)
                .map(|e| e.id)
                .collect(),
        );
    }
}

fn scheduled_key(m: &ScheduledMessageDto, self_id: i64) -> Option<ConversationKey> {
    match m.kind.as_str() {
        "stream" => Some(ConversationKey::topic(m.to.as_i64()?, m.topic.clone())),
        _ => {
            let ids: Vec<i64> = m.to.as_array()?.iter().filter_map(Value::as_i64).collect();
            Some(ConversationKey::dm(ids, self_id))
        }
    }
}

/// Returns `None` for a scheduled message whose recipient cannot be recognized.
pub fn scheduled_from_dto(m: ScheduledMessageDto, self_id: i64) -> Option<ScheduledMessage> {
    Some(ScheduledMessage {
        id: m.scheduled_message_id,
        key: scheduled_key(&m, self_id)?,
        topic: m.topic,
        content: m.content,
        rendered_content: m.rendered_content,
        deliver_at: m.scheduled_delivery_timestamp,
        failed: m.failed,
    })
}

pub fn reminder_from_dto(m: ReminderDto) -> Reminder {
    Reminder {
        id: m.reminder_id,
        target_message_id: m.reminder_target_message_id,
        content: m.content,
        rendered_content: m.rendered_content,
        deliver_at: m.scheduled_delivery_timestamp,
        failed: m.failed,
    }
}

/// Scheduled messages, message reminders and canned replies.
#[derive(Default)]
pub struct Queued {
    pub scheduled: Vec<ScheduledMessage>,
    pub reminders: Vec<Reminder>,
    pub snippets: Vec<SavedSnippet>,
}

impl Queued {
    pub fn sort(&mut self) {
        self.scheduled.sort_by_key(|m| m.deliver_at);
        self.reminders.sort_by_key(|m| m.deliver_at);
        self.snippets.sort_by_key(|s| (s.date_created, s.id));
    }

    fn put_scheduled(&mut self, dto: ScheduledMessageDto, self_id: i64) {
        self.scheduled.retain(|m| m.id != dto.scheduled_message_id);
        self.scheduled.extend(scheduled_from_dto(dto, self_id));
    }

    pub fn apply_scheduled_event(&mut self, event: &Value, self_id: i64) {
        match event.get("op").and_then(Value::as_str).unwrap_or_default() {
            "add" => {
                let added: Vec<ScheduledMessageDto> = event
                    .get("scheduled_messages")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                for item in added {
                    self.put_scheduled(item, self_id);
                }
            }
            "update" => {
                if let Some(item) = event
                    .get("scheduled_message")
                    .and_then(|v| serde_json::from_value::<ScheduledMessageDto>(v.clone()).ok())
                {
                    self.put_scheduled(item, self_id);
                }
            }
            "remove" => {
                let id = event.get("scheduled_message_id").and_then(Value::as_i64);
                self.scheduled.retain(|m| Some(m.id) != id);
            }
            _ => {}
        }
        self.sort();
    }

    pub fn apply_reminders_event(&mut self, event: &Value) {
        match event.get("op").and_then(Value::as_str).unwrap_or_default() {
            "add" => {
                let added: Vec<ReminderDto> = event
                    .get("reminders")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                for item in added {
                    self.reminders.retain(|m| m.id != item.reminder_id);
                    self.reminders.push(reminder_from_dto(item));
                }
            }
            "remove" => {
                let id = event.get("reminder_id").and_then(Value::as_i64);
                self.reminders.retain(|m| Some(m.id) != id);
            }
            _ => {}
        }
        self.sort();
    }

    pub fn apply_snippets_event(&mut self, event: &Value) {
        match event.get("op").and_then(Value::as_str).unwrap_or_default() {
            "add" | "update" => {
                if let Some(item) = event
                    .get("saved_snippet")
                    .and_then(|v| serde_json::from_value::<SavedSnippet>(v.clone()).ok())
                {
                    self.snippets.retain(|s| s.id != item.id);
                    self.snippets.push(item);
                }
            }
            "remove" => {
                let id = event.get("saved_snippet_id").and_then(Value::as_i64);
                self.snippets.retain(|s| Some(s.id) != id);
            }
            _ => {}
        }
        self.sort();
    }
}

/// Users who are typing and their expiry times.
#[derive(Default)]
pub struct Typing {
    by_key: HashMap<ConversationKey, HashMap<i64, Instant>>,
}

impl Typing {
    pub fn start(&mut self, key: &ConversationKey, user: i64, expiry: Duration) {
        self.by_key
            .entry(key.clone())
            .or_default()
            .insert(user, Instant::now() + expiry);
    }

    pub fn stop(&mut self, key: &ConversationKey, user: i64) -> bool {
        self.by_key
            .get_mut(key)
            .is_some_and(|m| m.remove(&user).is_some())
    }

    /// Clears expired ones; returns whether anything changed.
    pub fn expire(&mut self, key: &ConversationKey) -> bool {
        let now = Instant::now();
        self.by_key.get_mut(key).is_some_and(|m| {
            let before = m.len();
            m.retain(|_, at| *at > now);
            before != m.len()
        })
    }

    pub fn users(&self, key: &ConversationKey) -> Vec<i64> {
        let now = Instant::now();
        let mut users: Vec<i64> = self
            .by_key
            .get(key)
            .map(|m| {
                m.iter()
                    .filter(|(_, at)| **at > now)
                    .map(|(id, _)| *id)
                    .collect()
            })
            .unwrap_or_default();
        users.sort_unstable();
        users
    }
}

/// Hint text for "typing".
pub fn typing_text(key: &ConversationKey, names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    if matches!(key, ConversationKey::Dm(d) if !d.is_group_dm()) {
        return Some("对方正在输入…".to_owned());
    }
    if names.len() > 3 {
        return Some("多人正在输入…".to_owned());
    }
    Some(format!("{} 正在输入…", names.join("、")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::test_db;
    use serde_json::json;

    fn drafts(sync: bool) -> Drafts {
        let db = test_db();
        Drafts::new(sync, &[], HashMap::new(), LocalDraftRepo::new(&db, 1), 7)
    }

    #[test]
    fn draft_key_and_dto_round_trip() {
        let topic = ConversationKey::topic(3, "主题");
        let dto = draft_to_dto(&topic, "主题", "内容", 7, Some(1), 5);
        assert_eq!((dto.kind.as_str(), dto.to.as_slice()), ("stream", &[3][..]));
        assert_eq!(draft_key(&dto, 7), Some(topic));
        let own = ConversationKey::dm([7], 7);
        let dto = draft_to_dto(&own, "", "x", 7, None, 5);
        assert_eq!(dto.to, [7]);
        assert_eq!(draft_key(&dto, 7), Some(own));
        let dm = ConversationKey::dm([2, 3], 7);
        let dto = draft_to_dto(&dm, "", "x", 7, None, 5);
        assert_eq!(dto.to, [2, 3]);
        assert_eq!(draft_key(&dto, 7), Some(dm));
    }

    #[test]
    fn latest_timestamp_wins_and_saving_masks_events() {
        let mut d = drafts(true);
        let event = |content: &str, at: i64| json!({"op": "update", "draft": {"id": 1, "type": "stream", "to": [3], "topic": "t", "content": content, "timestamp": at}});
        let key = ConversationKey::topic(3, "t");
        assert_eq!(
            d.apply_event(&event("新", 10), 7),
            std::slice::from_ref(&key)
        );
        assert!(d.apply_event(&event("旧", 5), 7).is_empty());
        assert_eq!(d.get(&key).unwrap().content, "新");

        d.saving.insert(key.clone());
        assert!(d.apply_event(&event("回显", 20), 7).is_empty());
        assert_eq!(d.get(&key).unwrap().content, "新");
        assert!(
            d.apply_event(&json!({"op": "remove", "draft_id": 1}), 7)
                .is_empty()
        );
        d.saving.clear();
        assert_eq!(
            d.apply_event(&json!({"op": "remove", "draft_id": 1}), 7),
            std::slice::from_ref(&key)
        );
        assert!(d.get(&key).is_none());
    }

    fn notice_store() -> SystemNotices {
        let db = test_db();
        SystemNotices::new(AccountSettings::new(&db, 1))
    }

    fn notice_msg(id: i64, kind: SystemNoticeKind, html: &str) -> ChatMessage {
        let mut m = ChatMessage::new(id, ConversationKey::dm([9], 1), 9, id);
        m.notice = kind;
        m.content_html = html.to_owned();
        m
    }

    #[test]
    fn notices_sorted_cleared_and_counted() {
        let mut n = notice_store();
        n.add(&notice_msg(1, SystemNoticeKind::Notice, "<p>a</p>"));
        n.add(&notice_msg(
            2,
            SystemNoticeKind::Notice,
            "<p>b <span data-user-id=\"5\"></span></p>",
        ));
        assert!(!n.add(&notice_msg(2, SystemNoticeKind::Notice, "<p>b</p>")));
        assert_eq!(n.notices()[0].id, 2);
        assert_eq!(n.notices()[0].mentioned_user_ids, [5]);
        assert_eq!(n.unread_notice_count(), 2);
        assert_eq!(n.set_read(&[1], true), [1]);
        assert_eq!(n.unread_notice_count(), 1);
        n.clear_notices();
        assert!(n.notices().is_empty());
        n.add(&notice_msg(3, SystemNoticeKind::Notice, "<p>c</p>"));
        assert_eq!(n.notices().len(), 1);
    }

    #[test]
    fn reminders_can_be_cleared_individually() {
        let mut n = notice_store();
        let html = format!(
            "<p>{} <a href=\"#narrow/dm/1/near/77\">x</a></p>",
            notice::REMINDER_MARKER
        );
        n.add(&notice_msg(1, SystemNoticeKind::Reminder, &html));
        n.add(&notice_msg(2, SystemNoticeKind::Reminder, &html));
        assert_eq!(n.delivered_reminders()[0].target_message_id, Some(77));
        n.clear_reminder(2);
        assert_eq!(n.delivered_reminders().len(), 1);
        assert!(n.notices().is_empty());
        n.clear_all_reminders();
        assert!(n.delivered_reminders().is_empty());
    }

    #[test]
    fn queued_events() {
        let mut q = Queued::default();
        q.apply_scheduled_event(
            &json!({"op": "add", "scheduled_messages": [
                {"scheduled_message_id": 1, "type": "stream", "to": 3, "topic": "t", "scheduled_delivery_timestamp": 20},
                {"scheduled_message_id": 2, "type": "private", "to": [5], "scheduled_delivery_timestamp": 10},
            ]}),
            7,
        );
        assert_eq!(q.scheduled[0].id, 2);
        assert_eq!(q.scheduled[1].key, ConversationKey::topic(3, "t"));
        assert_eq!(q.scheduled[0].key, ConversationKey::dm([5], 7));
        q.apply_scheduled_event(&json!({"op": "remove", "scheduled_message_id": 2}), 7);
        assert_eq!(q.scheduled.len(), 1);

        q.apply_reminders_event(&json!({"op": "add", "reminders": [{"reminder_id": 1, "scheduled_delivery_timestamp": 5}]}));
        assert_eq!(q.reminders.len(), 1);
        q.apply_reminders_event(&json!({"op": "remove", "reminder_id": 1}));
        assert!(q.reminders.is_empty());

        q.apply_snippets_event(
            &json!({"op": "add", "saved_snippet": {"id": 2, "title": "b", "date_created": 2}}),
        );
        q.apply_snippets_event(
            &json!({"op": "add", "saved_snippet": {"id": 1, "title": "a", "date_created": 1}}),
        );
        assert_eq!(q.snippets[0].id, 1);
        q.apply_snippets_event(
            &json!({"op": "update", "saved_snippet": {"id": 1, "title": "改", "date_created": 1}}),
        );
        assert_eq!(q.snippets[0].title, "改");
        q.apply_snippets_event(&json!({"op": "remove", "saved_snippet_id": 1}));
        assert_eq!(q.snippets.len(), 1);
    }

    #[test]
    fn typing_tracks_expiry_and_text() {
        let mut t = Typing::default();
        let key = ConversationKey::topic(1, "t");
        t.start(&key, 2, Duration::from_secs(60));
        t.start(&key, 3, Duration::ZERO);
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(t.users(&key), [2]);
        assert!(t.expire(&key));
        assert!(!t.expire(&key));
        assert!(t.stop(&key, 2));
        assert!(t.users(&key).is_empty());

        let names = |n: usize| vec!["甲".to_owned(); n];
        assert_eq!(typing_text(&key, &[]), None);
        assert_eq!(
            typing_text(&ConversationKey::dm([2], 1), &names(1)).unwrap(),
            "对方正在输入…"
        );
        assert_eq!(typing_text(&key, &names(2)).unwrap(), "甲、甲 正在输入…");
        assert_eq!(typing_text(&key, &names(4)).unwrap(), "多人正在输入…");
    }
}
