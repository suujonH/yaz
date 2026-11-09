//! All in-memory state of a single account. No IO here (apart from writing the local database); see
//! [`crate::router`] for how events become state.

pub mod channels;
pub mod contacts;
pub mod conversations;
pub mod marks;
pub mod messages;
pub mod misc;
pub mod unread;
pub mod users;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use zulip_api::{MessageDto, RegisterResult};
use zulip_core::emoji::{EmojiCatalog, EmojiEntry, EmojiKind};
use zulip_core::markdown::MarkdownResolver;
use zulip_core::model::{
    ChatMessage, FriendGroup, PinItem, PinScope, RealmInfo, RemarkTarget, UserSettings,
};
use zulip_core::notice;
use zulip_core::permission::{PermissionEvaluator, PermissionInputs, PermissionSettingInfo};
use zulip_core::push::DndLevel;
use zulip_core::text::{remark, topic_title};
use zulip_core::{ConversationFolder, ConversationKey};
use zulip_store::{
    AccountSettings, ContactRepo, ConversationCache, ConversationCacheRepo, ConversationPref,
    ConversationPrefRepo, Db, DeletedMessage, DeletedMessageRepo, LocalDraftRepo, LocalSettings,
    MessageVersionRepo, PinRepo, RemarkRepo, Result as StoreResult,
};

use crate::convert::{
    channel_permission_infos, message_from_dto, permission_inputs, realm_from_register,
};
use crate::topic::{RESOLVED_LABEL, is_resolved_name, strip_resolved};

use channels::Channels;
use contacts::Contacts;
use conversations::Conversations;
use marks::{Pins, Remarks};
use messages::MessageCache;
use misc::{Drafts, Queued, SystemNotices, Typing, reminder_from_dto, scheduled_from_dto};
use unread::Unread;
use users::Users;

pub const SELF_DM_TITLE: &str = "我";

pub fn folder_title(folder: ConversationFolder) -> &'static str {
    match folder {
        ConversationFolder::Resolved => "已解决话题",
        ConversationFolder::Helper => "话题助手",
    }
}

/// Scroll position of a chat window. In memory only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollPosition {
    pub message_id: i64,
    /// Offset of the top of the message relative to the viewport (pixels).
    pub offset: i32,
}

/// State reported by the UI: which conversations are in the foreground viewing the newest messages,
/// and scroll positions.
#[derive(Default)]
pub struct ViewState {
    active: HashSet<ConversationKey>,
    scroll: HashMap<ConversationKey, ScrollPosition>,
}

impl ViewState {
    pub fn set_actively_viewing(&mut self, key: &ConversationKey, active: bool) {
        if active {
            self.active.insert(key.clone());
        } else {
            self.active.remove(key);
        }
    }

    pub fn is_actively_viewing(&self, key: &ConversationKey) -> bool {
        self.active.contains(key)
    }

    /// `None` clears it.
    pub fn save_scroll(&mut self, key: &ConversationKey, position: Option<ScrollPosition>) {
        match position {
            Some(position) => self.scroll.insert(key.clone(), position),
            None => self.scroll.remove(key),
        };
    }

    pub fn scroll(&self, key: &ConversationKey) -> Option<ScrollPosition> {
        self.scroll.get(key).copied()
    }

    pub fn rekey(&mut self, old: &ConversationKey, new: &ConversationKey) {
        if self.active.remove(old) {
            self.active.insert(new.clone());
        }
        if let Some(position) = self.scroll.remove(old) {
            self.scroll.insert(new.clone(), position);
        }
    }
}

pub struct State {
    pub users: Users,
    pub channels: Channels,
    pub realm: RealmInfo,
    pub inputs: PermissionInputs,
    pub permissions: Arc<PermissionEvaluator>,
    pub channel_permission_infos: HashMap<String, PermissionSettingInfo>,
    pub settings: UserSettings,
    pub alert_words: Vec<String>,
    pub messages: MessageCache,
    pub unread: Unread,
    pub conversations: Conversations,
    pub contacts: Contacts,
    pub remarks: Remarks,
    pub pins: Pins,
    pub drafts: Drafts,
    pub notices: SystemNotices,
    pub queued: Queued,
    pub typing: Typing,
    pub view: ViewState,
    pub realm_emoji: Vec<EmojiEntry>,
    pub unicode_emoji: Vec<EmojiEntry>,
    pub emoji: Arc<EmojiCatalog>,
    pub versions: MessageVersionRepo,
    pub local: LocalSettings,
    pub account: AccountSettings,
}

/// Data in the local database belonging to one account. Read before taking the state lock, then
/// handed to [`State::new`].
pub struct Stored {
    prefs: Vec<ConversationPref>,
    cache: Vec<ConversationCache>,
    remarks: HashMap<RemarkTarget, String>,
    pins: HashMap<(PinScope, PinItem), i64>,
    groups: Vec<FriendGroup>,
    membership: HashMap<i64, i64>,
    drafts: HashMap<String, (String, i64)>,
    deleted: Vec<DeletedMessage>,
}

impl Stored {
    pub fn load(db: &Db, account_id: i64) -> StoreResult<Self> {
        let contacts = ContactRepo::new(db, account_id);
        Ok(Self {
            prefs: ConversationPrefRepo::new(db, account_id).load_all()?,
            cache: ConversationCacheRepo::new(db, account_id).load_all()?,
            remarks: RemarkRepo::new(db, account_id).load_all()?,
            pins: PinRepo::new(db, account_id).load_all()?,
            groups: contacts.load_groups()?,
            membership: contacts.load_membership()?,
            drafts: LocalDraftRepo::new(db, account_id).load_all()?,
            deleted: DeletedMessageRepo::new(db, account_id).load_all()?,
        })
    }
}

/// Custom emoji that are not deactivated, sorted by name.
pub fn realm_emoji_from<'a>(
    emoji: impl Iterator<Item = &'a zulip_api::RealmEmojiDto>,
) -> Vec<EmojiEntry> {
    let mut list: Vec<EmojiEntry> = emoji
        .filter(|e| !e.deactivated)
        .map(|e| EmojiEntry {
            name: e.name.clone(),
            code: e.id.clone(),
            kind: EmojiKind::Realm,
            aliases: Vec::new(),
            text: None,
            image_url: Some(e.still_url.clone().unwrap_or_else(|| e.source_url.clone())),
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

impl State {
    /// Builds the state from the register result and local data.
    pub fn new(
        reg: &RegisterResult,
        realm_url: &str,
        stored: Stored,
        db: &Db,
        account_id: i64,
    ) -> Self {
        let local = LocalSettings::new(db);
        let account = AccountSettings::new(db, account_id);
        let users = Users::load(reg);
        let inputs = permission_inputs(reg, users.groups.clone());
        let mut queued = Queued {
            scheduled: reg
                .scheduled_messages
                .iter()
                .filter_map(|m| scheduled_from_dto(m.clone(), reg.user_id))
                .collect(),
            reminders: reg
                .reminders
                .iter()
                .cloned()
                .map(reminder_from_dto)
                .collect(),
            snippets: reg.saved_snippets.clone(),
        };
        queued.sort();
        let realm_emoji = realm_emoji_from(reg.realm_emoji.values());
        let drafts = Drafts::new(
            reg.user_settings.enable_drafts_synchronization,
            &reg.drafts,
            stored.drafts,
            LocalDraftRepo::new(db, account_id),
            reg.user_id,
        );
        Self {
            channels: Channels::load(reg),
            realm: realm_from_register(reg, realm_url),
            permissions: Arc::new(PermissionEvaluator::new(inputs.clone())),
            inputs,
            channel_permission_infos: channel_permission_infos(reg),
            settings: reg.user_settings.clone(),
            alert_words: reg.alert_words.clone(),
            messages: MessageCache::new(
                DeletedMessageRepo::new(db, account_id),
                stored.deleted,
                account_id,
            ),
            unread: Unread::load(&reg.unread_msgs, reg.user_id),
            conversations: Conversations::new(
                ConversationPrefRepo::new(db, account_id),
                ConversationCacheRepo::new(db, account_id),
                local.clone(),
                stored.prefs,
                stored.cache,
            ),
            contacts: Contacts::new(
                ContactRepo::new(db, account_id),
                stored.groups,
                stored.membership,
            ),
            remarks: Remarks::new(RemarkRepo::new(db, account_id), stored.remarks),
            pins: Pins::new(PinRepo::new(db, account_id), stored.pins),
            drafts,
            notices: SystemNotices::new(account.clone()),
            queued,
            typing: Typing::default(),
            view: ViewState::default(),
            emoji: Arc::new(EmojiCatalog::new(realm_emoji.clone(), Vec::new())),
            realm_emoji,
            unicode_emoji: Vec::new(),
            versions: MessageVersionRepo::new(db, account_id),
            local,
            account,
            users,
        }
    }

    /// After re-registering, takes over the parts of the old state belonging to this run: UI state,
    /// downloaded emoji data, and the open windows, intercepted messages and local echo numbering
    /// in the message cache.
    pub fn inherit(&mut self, previous: State) {
        self.view = previous.view;
        self.unicode_emoji = previous.unicode_emoji;
        self.messages.inherit(previous.messages);
        self.rebuild_emoji();
    }

    /// Opens or closes a chat window. Returns whether the conversation went from closed to open, or
    /// from open to all closed.
    pub fn set_open(&mut self, key: &ConversationKey, open: bool) -> bool {
        let changed = self.messages.set_open(key, open);
        if !open && !self.messages.is_open(key) {
            self.view.set_actively_viewing(key, false);
        }
        changed
    }

    /// System groups selectable for channel permission settings: group id and Chinese name; system
    /// groups absent from the organization are not listed. No restriction when the server gives no
    /// information for this setting.
    pub fn channel_permission_choices(&self, setting: &str) -> Vec<(i64, &'static str)> {
        let info = self
            .channel_permission_infos
            .get(setting)
            .cloned()
            .unwrap_or_else(PermissionSettingInfo::unrestricted);
        info.system_group_choices()
            .into_iter()
            .filter_map(|choice| {
                let group = self
                    .users
                    .groups
                    .values()
                    .find(|g| g.is_system_group && g.name == choice.name)?;
                Some((group.id, choice.text))
            })
            .collect()
    }

    pub fn rebuild_permissions(&mut self) {
        self.inputs.groups = self.users.groups.clone();
        self.inputs.allow_message_editing = self.realm.allow_message_editing;
        self.inputs.edit_limit_seconds = self.realm.edit_limit_seconds;
        self.inputs.delete_limit_seconds = self.realm.delete_limit_seconds;
        self.permissions = Arc::new(PermissionEvaluator::new(self.inputs.clone()));
    }

    pub fn rebuild_emoji(&mut self) {
        self.emoji = Arc::new(EmojiCatalog::new(
            self.realm_emoji.clone(),
            self.unicode_emoji.clone(),
        ));
    }

    /// API message -> domain model: classifies notices. The edit history carried by the message is
    /// stored in the local version history.
    pub fn prepare_message(&self, dto: &MessageDto) -> ChatMessage {
        for edit in dto.edit_history.iter().flatten() {
            if let Some(content) = &edit.prev_rendered_content {
                self.versions.add(dto.id, edit.timestamp, content);
            }
        }
        let mut m = message_from_dto(dto, self.users.self_id);
        self.classify(&mut m);
        m
    }

    pub fn classify(&self, m: &mut ChatMessage) {
        m.notice = notice::classify(
            self.users.notification_bot_id,
            &self.realm.announcement_channel_ids(),
            m.sender_id,
            m.stream_id,
            &m.topic,
            &m.content_html,
        );
    }

    /// Topic renamed or moved as a whole: list state, note, pin and UI state move to the new key.
    /// Returns whether the pin changed.
    pub fn rekey_conversation(
        &mut self,
        old: &ConversationKey,
        new: &ConversationKey,
        title: &str,
    ) -> bool {
        self.conversations.rename(old, new, title);
        self.remarks.rekey(old, new);
        self.messages.rekey_open(old, new);
        self.view.rekey(old, new);
        self.pins.rekey(old, new)
    }

    pub fn is_bot_dm(&self, key: &ConversationKey) -> bool {
        matches!((key, self.users.notification_bot_id),
            (ConversationKey::Dm(d), Some(bot)) if d.other_user_ids() == [bot])
    }

    /// Hints from Notification Bot in a topic; not used as a preview.
    pub fn is_bot_notice(&self, sender_id: i64, key: &ConversationKey) -> bool {
        self.users.notification_bot_id == Some(sender_id) && !self.is_bot_dm(key)
    }

    /// Note -> nickname -> "unknown user".
    pub fn display_name(&self, user_id: i64) -> String {
        self.remarks
            .of_user(user_id)
            .unwrap_or_else(|| self.users.name_of(user_id))
            .to_owned()
    }

    /// "note(nickname)".
    pub fn name_with_original(&self, user_id: i64) -> String {
        remark::with_original(self.remarks.of_user(user_id), self.users.name_of(user_id))
    }

    pub fn channel_display_name(&self, stream_id: i64) -> String {
        self.remarks
            .of_channel(stream_id)
            .map(str::to_owned)
            .or_else(|| self.channels.get(stream_id).map(|c| c.name.clone()))
            .unwrap_or_default()
    }

    /// The original-case name of a topic: prefers the newest message, then the channel's topic
    /// list, then the list cache, and finally the (lowercase) key.
    pub fn topic_name(&self, key: &ConversationKey) -> String {
        let ConversationKey::Topic(t) = key else {
            return String::new();
        };
        if let Some(latest) = self.messages.latest_visible(key) {
            return latest.topic.clone();
        }
        let lower = t.topic.to_lowercase();
        if let Some(topic) = self
            .channels
            .topics(t.stream_id)
            .and_then(|list| list.iter().find(|x| x.name.to_lowercase() == lower))
        {
            return topic.name.clone();
        }
        if let Some(entry) = self.conversations.get(key)
            && !entry.title.is_empty()
        {
            return entry.title.clone();
        }
        t.topic.clone()
    }

    fn conversation_remark(&self, key: &ConversationKey) -> Option<&str> {
        self.remarks.get(&RemarkTarget::Conversation(key.clone()))
    }

    /// The title of a conversation (also used as the text avatar). Resolved topics lose the leading
    /// checkmark and space.
    pub fn title(&self, key: &ConversationKey) -> String {
        match key {
            ConversationKey::Topic(_) => {
                if let Some(remark) = self.conversation_remark(key) {
                    return remark.to_owned();
                }
                let name = self.topic_name(key);
                let stripped = strip_resolved(&name);
                if stripped.is_empty() {
                    self.realm.empty_topic_display_name.clone()
                } else {
                    stripped.to_owned()
                }
            }
            ConversationKey::Dm(d) if d.is_self_dm() => SELF_DM_TITLE.to_owned(),
            ConversationKey::Dm(d) if !d.is_group_dm() => d
                .other_user_ids()
                .first()
                .map(|id| self.display_name(*id))
                .unwrap_or_default(),
            ConversationKey::Dm(d) => {
                if let Some(remark) = self.conversation_remark(key) {
                    return remark.to_owned();
                }
                let ids = d.other_user_ids();
                let names: Vec<String> = ids
                    .iter()
                    .take(3)
                    .map(|id| self.display_name(*id))
                    .collect();
                if ids.len() > 3 {
                    format!("{}等{}人", names.join("、"), ids.len())
                } else {
                    names.join("、")
                }
            }
        }
    }

    /// Used for list and chat titles: a topic is joined with the channel name per the template, and
    /// a resolved one gets the "(resolved)" prefix (in Chinese) at the front.
    pub fn display_title(&self, key: &ConversationKey, strip_resolved_label: bool) -> String {
        let ConversationKey::Topic(t) = key else {
            return self.title(key);
        };
        let title = topic_title::format(
            Some(&self.local.topic_title_format()),
            &self.channel_display_name(t.stream_id),
            &self.title(key),
        );
        if !strip_resolved_label && is_resolved_name(&self.topic_name(key)) {
            format!("{RESOLVED_LABEL}{title}")
        } else {
            title
        }
    }

    /// "note(original topic name)".
    pub fn topic_title_with_original(&self, key: &ConversationKey) -> String {
        let name = self.topic_name(key);
        let stripped = strip_resolved(&name);
        let original = if stripped.is_empty() {
            self.realm.empty_topic_display_name.as_str()
        } else {
            stripped
        };
        remark::with_original(self.conversation_remark(key), original)
    }

    /// Taskbar badge: counts only conversations that are allowed to alert.
    pub fn total_unread_for_badge(&self) -> usize {
        self.unread
            .keys()
            .iter()
            .filter(|k| self.conversations.effective_level(k) == DndLevel::Notify)
            .map(|k| self.unread.count(k))
            .sum()
    }

    /// Conversations in do-not-disturb that still have unread @-mentions or reminder words.
    pub fn unread_muted_alert_keys(&self) -> Vec<ConversationKey> {
        self.unread
            .keys()
            .into_iter()
            .filter(|k| {
                let level = self.conversations.effective_level(k);
                level != DndLevel::Notify
                    && level != DndLevel::Blocked
                    && (self.unread.latest_mention(k).is_some()
                        || self.unread.latest_alert_word(k).is_some())
            })
            .collect()
    }
}

impl MarkdownResolver for State {
    fn has_user(&self, name: &str, id: Option<i64>) -> bool {
        match id {
            Some(id) => self
                .users
                .get(id)
                .is_some_and(|u| u.full_name.eq_ignore_ascii_case(name)),
            None => self
                .users
                .all()
                .any(|u| u.full_name.eq_ignore_ascii_case(name)),
        }
    }

    fn has_group(&self, name: &str) -> bool {
        self.users
            .groups
            .values()
            .any(|g| g.name.eq_ignore_ascii_case(name))
    }

    fn has_channel(&self, name: &str) -> bool {
        self.channels
            .all()
            .any(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn has_emoji(&self, name: &str) -> bool {
        self.emoji.get(&name.to_lowercase()).is_some()
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use serde_json::{Value, json};

    /// Self has id 1; users 2, 3, 4; notification bot 9; channels 10 ("Dev" in Chinese) and 11
    /// ("Games" in Chinese).
    pub fn register_json() -> Value {
        json!({
            "queue_id": "q", "last_event_id": -1, "zulip_feature_level": 445, "user_id": 1,
            "email": "me@x", "full_name": "我",
            "realm_users": [
                {"user_id": 1, "full_name": "我", "email": "me@x", "role": 100},
                {"user_id": 2, "full_name": "阿杰", "email": "a@x", "avatar_url": "/a2.png"},
                {"user_id": 3, "full_name": "小雨", "email": "b@x"},
                {"user_id": 4, "full_name": "老王", "email": "c@x"},
            ],
            "cross_realm_bots": [{"user_id": 9, "email": "notification-bot@x", "full_name": "Bot", "is_bot": true}],
            "subscriptions": [
                {"stream_id": 10, "name": "开发", "color": "#123456", "topics_policy": "inherit"},
                {"stream_id": 11, "name": "游戏", "color": "#654321", "topics_policy": "inherit"},
            ],
            "user_settings": {
                "enable_desktop_notifications": true, "enable_sounds": true,
                "enable_stream_desktop_notifications": false, "enable_stream_audible_notifications": false,
                "notification_sound": "zulip", "pm_content_in_desktop_notifications": true,
                "enable_drafts_synchronization": true,
            },
            "realm_empty_topic_display_name": "一般聊天",
            "realm_allow_message_editing": true,
            "server_presence_offline_threshold_seconds": 140,
            "server_typing_started_expiry_period_milliseconds": 15000,
        })
    }

    /// The data must belong to an existing account. This account's id is 1.
    pub fn test_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        let id = zulip_store::AccountRepo::new(&db)
            .upsert(&zulip_store::AccountRow {
                id: 0,
                realm_url: "https://z.test".into(),
                email: "me@x".into(),
                user_id: 1,
                full_name: "我".into(),
                avatar_url: None,
                api_key: None,
                allow_invalid_cert: false,
                login_on_startup: true,
                last_login_at: 0,
            })
            .unwrap();
        assert_eq!(id, 1);
        db
    }

    pub fn state() -> State {
        state_with(register_json(), &test_db())
    }

    pub fn state_with(reg: Value, db: &Db) -> State {
        let reg: RegisterResult = serde_json::from_value(reg).unwrap();
        let stored = Stored::load(db, 1).unwrap();
        State::new(&reg, "https://z.test", stored, db, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use zulip_core::permission::UserGroup;

    #[test]
    fn channel_permission_choices_follow_server_limits() {
        let mut s = state();
        let system = |id: i64, name: &str| UserGroup {
            id,
            name: name.to_owned(),
            is_system_group: true,
            ..UserGroup::default()
        };
        s.users.groups = [
            system(1, "role:everyone"),
            system(2, "role:members"),
            system(3, "role:nobody"),
        ]
        .into_iter()
        .map(|g| (g.id, g))
        .collect();
        s.users.groups.insert(
            4,
            UserGroup {
                id: 4,
                name: "role:owners".to_owned(),
                ..UserGroup::default()
            },
        );
        assert_eq!(
            s.channel_permission_choices("can_send_message_group"),
            [(1, "所有人"), (2, "成员"), (3, "无人")]
        );
        s.channel_permission_infos.insert(
            "can_send_message_group".to_owned(),
            PermissionSettingInfo {
                allow_nobody: false,
                allow_everyone: false,
                allowed_system_groups: Vec::new(),
            },
        );
        assert_eq!(
            s.channel_permission_choices("can_send_message_group"),
            [(2, "成员")]
        );
    }

    #[test]
    fn titles() {
        let mut s = state();
        let topic = ConversationKey::topic(10, "接口");
        assert_eq!(s.title(&topic), "接口");
        s.conversations.set_title(&topic, "✔ 接口");
        assert_eq!(s.title(&topic), "接口");
        assert_eq!(
            s.display_title(&topic, false),
            format!("(已解决){}", s.display_title(&topic, true))
        );
        s.remarks
            .set(&RemarkTarget::Conversation(topic.clone()), Some("备注"));
        assert_eq!(s.title(&topic), "备注");
        assert_eq!(s.topic_title_with_original(&topic), "备注(接口)");

        assert_eq!(s.title(&ConversationKey::topic(10, "")), "一般聊天");
        assert_eq!(s.title(&ConversationKey::dm([1], 1)), "我");
        assert_eq!(s.title(&ConversationKey::dm([2], 1)), "阿杰");
        s.remarks.set(&RemarkTarget::User(2), Some("杰哥"));
        assert_eq!(s.title(&ConversationKey::dm([2], 1)), "杰哥");
        assert_eq!(s.title(&ConversationKey::dm([2, 3], 1)), "杰哥、小雨");
        assert_eq!(
            s.title(&ConversationKey::dm([2, 3, 4, 9], 1)),
            "杰哥、小雨、老王等4人"
        );
        assert_eq!(folder_title(ConversationFolder::Resolved), "已解决话题");
        assert_eq!(folder_title(ConversationFolder::Helper), "话题助手");
    }

    #[test]
    fn scroll_positions() {
        let mut v = ViewState::default();
        let key = ConversationKey::topic(1, "a");
        assert_eq!(v.scroll(&key), None);
        let p = ScrollPosition {
            message_id: 5,
            offset: 12,
        };
        v.save_scroll(&key, Some(p));
        assert_eq!(v.scroll(&key), Some(p));
        v.save_scroll(&key, None);
        assert_eq!(v.scroll(&key), None);
        v.save_scroll(&key, Some(p));
        let other = ConversationKey::topic(1, "b");
        v.rekey(&key, &other);
        assert_eq!(v.scroll(&other), Some(p));
    }

    #[test]
    fn badge_counts_only_notify_level() {
        let mut s = state();
        let topic = ConversationKey::topic(10, "a");
        let mut m = ChatMessage::new(1, topic.clone(), 2, 1);
        s.unread.add_message(&m);
        m.id = 2;
        m.flags.mentioned = true;
        s.unread.add_message(&m);
        assert_eq!(s.total_unread_for_badge(), 2);
        s.conversations.set_level(&topic, DndLevel::Silent);
        assert_eq!(s.total_unread_for_badge(), 0);
        assert_eq!(s.unread_muted_alert_keys(), [topic]);
    }
}
