//! Session of a single account: post-login state, event loop and background tasks. Business
//! operations are under `ops`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zulip_api::{ApiError, MessageQuery, NarrowTerm, RegisterResult, ZulipClient};
use zulip_core::ConversationKey;
use zulip_core::model::ChatMessage;
use zulip_core::notice::SystemNoticeKind;
use zulip_store::{AccountRepo, AppPaths, Db};

use crate::convert::message_from_dto;
use crate::error::{Result, SessionError};
use crate::event::{ConnectionState, SessionEvent};
use crate::hooks::{HookEvent, SessionHooks};
use crate::image_cache::{ClientFetcher, ImageCache};
use crate::router::{self, Applied, Effect, latest_of};
use crate::state::messages::PageKind;
use crate::state::{State, Stored};

/// Feature level of the minimum supported Zulip version (11.0).
pub const MIN_FEATURE_LEVEL: i32 = 421;

const EVENT_CHANNEL_CAPACITY: usize = 1024;
const LONGPOLL_MARGIN: Duration = Duration::from_secs(30);
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const DELETE_QUEUE_LIMIT: Duration = Duration::from_secs(3);
const RECENT_MESSAGES: u32 = 1000;
const UNREAD_DETAILS: u32 = 2000;
const NOTICE_MESSAGES: u32 = 300;
const BACKFILL_INTERVAL: Duration = Duration::from_millis(300);
const BACKFILL_MESSAGES: u32 = 5;
const IDLE_AFTER: Duration = Duration::from_secs(300);
const DEFAULT_PRESENCE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub(crate) struct QueuePosition {
    pub queue_id: String,
    pub last_event_id: i64,
    pub longpoll_timeout: Duration,
}

#[derive(Default)]
pub(crate) struct TypingSender {
    pub current: Option<(ConversationKey, Instant)>,
    pub generation: u64,
}

pub struct Session {
    pub(crate) account_id: i64,
    pub(crate) client: ZulipClient,
    pub(crate) db: Db,
    pub(crate) paths: AppPaths,
    pub(crate) state: RwLock<State>,
    pub(crate) tx: broadcast::Sender<SessionEvent>,
    pub(crate) cancel: CancellationToken,
    pub(crate) queue: Mutex<QueuePosition>,
    pub(crate) connection: Mutex<ConnectionState>,
    pub(crate) images: ImageCache,
    /// Serializes async operations on the same resource by key (paging, downloads).
    pub(crate) gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) typing_sender: Mutex<TypingSender>,
    pub(crate) last_input: Mutex<Instant>,
    pub(crate) new_input: Mutex<bool>,
    backfilling: Mutex<bool>,
    pub(crate) hooks: Option<Arc<dyn SessionHooks>>,
    weak: Weak<Session>,
    /// The conversation the user switched to last.
    pub(crate) current: Mutex<Option<ConversationKey>>,
}

fn check_server(reg: &RegisterResult) -> Result<()> {
    if reg.zulip_feature_level < MIN_FEATURE_LEVEL {
        return Err(SessionError::ServerTooOld);
    }
    Ok(())
}

impl Session {
    /// Registers with the server and loads all state, starting the event loop and presence reporting.
    pub async fn connect(
        client: ZulipClient,
        account_id: i64,
        db: Db,
        paths: AppPaths,
        hooks: Option<Arc<dyn SessionHooks>>,
    ) -> Result<Arc<Self>> {
        let cancel = CancellationToken::new();
        let client = client.with_cancel(cancel.clone());
        let reg = client.register().await?;
        check_server(&reg)?;
        let realm = client.realm_url().clone();
        let stored = db.call(move |db| Stored::load(db, account_id)).await?;
        let state = State::new(&reg, realm.as_str(), stored, &db, account_id);
        AccountRepo::new(&db).update_profile(
            account_id,
            reg.user_id,
            &reg.full_name,
            reg.avatar_url.as_deref(),
            chrono::Utc::now().timestamp(),
        );
        let images = ImageCache::new(
            paths.image_cache_dir(),
            realm,
            Box::new(ClientFetcher {
                client: client.clone(),
                http: reqwest::Client::new(),
            }),
        );
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let session = Arc::new_cyclic(|weak| Self {
            account_id,
            client,
            db,
            paths,
            state: RwLock::new(state),
            tx,
            cancel,
            queue: Mutex::new(position_of(&reg)),
            connection: Mutex::new(ConnectionState::Connected),
            images,
            gates: Mutex::new(HashMap::new()),
            typing_sender: Mutex::new(TypingSender::default()),
            last_input: Mutex::new(Instant::now()),
            new_input: Mutex::new(true),
            backfilling: Mutex::new(false),
            hooks,
            weak: weak.clone(),
            current: Mutex::new(None),
        });
        session.bootstrap(&reg).await?;
        tokio::spawn(session.clone().run_events());
        tokio::spawn(session.clone().run_presence());
        let this = session.clone();
        tokio::spawn(async move {
            if let Err(error) = this.ensure_unicode_emoji().await {
                log::warn!("加载表情数据失败：{error}");
            }
        });
        Ok(session)
    }

    /// Subscribes to session events. On `Lagged`, re-read everything.
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.tx.subscribe()
    }

    pub fn account_id(&self) -> i64 {
        self.account_id
    }

    /// Address of the server the account is on.
    pub fn realm_url(&self) -> &str {
        self.client.realm_url().as_str()
    }

    /// Email used to log in.
    pub fn email(&self) -> &str {
        self.client.email()
    }

    /// The plugin API passes calls straight through to the server; used only by the plugin host.
    pub fn client(&self) -> &ZulipClient {
        &self.client
    }

    pub fn connection(&self) -> ConnectionState {
        *self.connection.lock()
    }

    /// Stops background tasks and deregisters the event queue (waits at most 3 seconds).
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let queue_id = self.queue.lock().queue_id.clone();
        // The cancellation token only affects this session's client; deregistering the queue uses a
        // clone without cancellation
        let client = self.client.with_cancel(CancellationToken::new());
        let _ = tokio::time::timeout(DELETE_QUEUE_LIMIT, client.delete_queue(&queue_id)).await;
    }

    pub(crate) fn emit(&self, event: SessionEvent) {
        let _ = self.tx.send(event);
    }

    pub(crate) fn notify_hooks(&self, event: HookEvent) {
        if let (Some(hooks), Some(session)) = (&self.hooks, self.weak.upgrade()) {
            hooks.on_event(&session, event);
        }
    }

    pub(crate) fn gate(&self, name: String) -> Arc<tokio::sync::Mutex<()>> {
        self.gates.lock().entry(name).or_default().clone()
    }

    pub(crate) fn dispatch(self: &Arc<Self>, applied: Applied) {
        for event in applied.events {
            self.emit(event);
        }
        for notice in applied.notices {
            self.notify_hooks(notice);
        }
        for effect in applied.effects {
            self.run_effect(effect);
        }
    }

    fn run_effect(self: &Arc<Self>, effect: Effect) {
        let this = self.clone();
        match effect {
            Effect::Backfill => self.start_backfill(),
            Effect::MarkRead(ids) => {
                tokio::spawn(async move {
                    if let Err(error) = this.mark_read(&ids).await {
                        log::warn!("自动标记已读失败：{error}");
                    }
                });
            }
            Effect::FetchUnread(id) => {
                tokio::spawn(async move {
                    if let Err(error) = this.fetch_unread(id).await {
                        log::warn!("获取未读消息 {id} 失败：{error}");
                    }
                });
            }
            Effect::SwitchDraftSync(enabled) => {
                tokio::spawn(async move {
                    if let Err(error) = this.switch_draft_sync(enabled).await {
                        log::warn!("切换草稿同步失败：{error}");
                    }
                });
            }
            Effect::FavoriteCopies { ids, starred } => {
                tokio::spawn(async move { this.sync_favorite_copies(&ids, starred).await });
            }
            Effect::ExpireTyping(key, after) => {
                tokio::spawn(async move {
                    tokio::time::sleep(after + Duration::from_millis(50)).await;
                    if this.state.write().typing.expire(&key) {
                        this.emit(SessionEvent::TypingChanged(key));
                    }
                });
            }
        }
    }

    async fn fetch_unread(self: &Arc<Self>, id: i64) -> Result<()> {
        let dto = self.client.get_message(id, true).await?;
        let mut applied = Applied::default();
        {
            let mut state = self.state.write();
            let mut message = state.prepare_message(&dto);
            message.flags.read = false;
            if !state.messages.is_hidden(&message) && state.unread.add_message(&message) {
                applied
                    .events
                    .push(SessionEvent::UnreadChanged(vec![message.key]));
            }
        }
        self.dispatch(applied);
        Ok(())
    }

    fn start_backfill(self: &Arc<Self>) {
        {
            let mut running = self.backfilling.lock();
            if *running {
                return;
            }
            *running = true;
        }
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                let next = this.state.write().conversations.next_backfill();
                let Some(key) = next else { break };
                if let Err(error) = this.backfill_one(&key).await {
                    if this.cancel.is_cancelled() {
                        break;
                    }
                    log::warn!("补取会话 {key} 的预览失败：{error}");
                }
                tokio::select! {
                    () = this.cancel.cancelled() => break,
                    () = tokio::time::sleep(BACKFILL_INTERVAL) => {}
                }
            }
            *this.backfilling.lock() = false;
        });
    }

    async fn backfill_one(self: &Arc<Self>, key: &ConversationKey) -> Result<()> {
        let narrow = self.narrow_of(key);
        let asked_after = {
            let state = self.state.read();
            state
                .conversations
                .get(key)
                .map_or(0, |e| e.last_message_id)
        };
        let result = self
            .client
            .get_messages(&MessageQuery::new(narrow, "newest", BACKFILL_MESSAGES, 0))
            .await?;
        let changed = {
            let mut state = self.state.write();
            let latest = result
                .messages
                .iter()
                .rev()
                .map(|dto| state.prepare_message(dto))
                .find(|m| !state.messages.is_hidden(m) && !state.is_bot_notice(m.sender_id, &m.key))
                .map(|m| latest_of(&m));
            state.conversations.apply_backfill(key, asked_after, latest)
        };
        if changed {
            self.emit(SessionEvent::ConversationChanged(key.clone()));
            self.emit(SessionEvent::ConversationsChanged);
        }
        Ok(())
    }

    pub(crate) fn narrow_of(&self, key: &ConversationKey) -> Vec<NarrowTerm> {
        let state = self.state.read();
        narrow_for(&state, key)
    }

    /// First load after registration: seed messages, conversation list, unread details and system
    /// notices.
    async fn bootstrap(self: &Arc<Self>, reg: &RegisterResult) -> Result<()> {
        let self_id = reg.user_id;
        let recent = self
            .client
            .get_messages(&MessageQuery::new(Vec::new(), "newest", RECENT_MESSAGES, 0))
            .await?;
        let unread_details = self
            .client
            .get_messages(&MessageQuery::new(
                vec![NarrowTerm::is("unread")],
                "newest",
                UNREAD_DETAILS,
                0,
            ))
            .await?;
        let bot_id = reg
            .cross_realm_bots
            .iter()
            .find(|u| u.email.starts_with("notification-bot@"))
            .map(|u| u.user_id);
        let notices = match bot_id {
            Some(bot) => {
                self.client
                    .get_messages(&MessageQuery::new(
                        vec![NarrowTerm::sender(bot)],
                        "newest",
                        NOTICE_MESSAGES,
                        0,
                    ))
                    .await?
                    .messages
            }
            None => Vec::new(),
        };

        let mut state = self.state.write();
        let mut by_key: HashMap<ConversationKey, Vec<_>> = HashMap::new();
        for dto in &recent.messages {
            let message = state.prepare_message(dto);
            by_key.entry(message.key.clone()).or_default().push(message);
        }
        for (key, mut messages) in by_key {
            messages.sort_by_key(|m| m.id);
            if let Some(latest) = messages
                .iter()
                .rev()
                .find(|m| !state.messages.is_hidden(m) && !state.is_bot_notice(m.sender_id, &m.key))
            {
                let latest = latest_of(latest);
                state.conversations.apply_latest(&key, &latest);
            }
            state
                .messages
                .merge_page(&key, messages, PageKind::Newest, recent.found_oldest, true);
        }

        let details: Vec<_> = unread_details
            .messages
            .iter()
            .map(|dto| state.prepare_message(dto))
            .collect();
        state.unread.apply_details(&details);

        for dto in &notices {
            let message = state.prepare_message(dto);
            if matches!(
                message.notice,
                SystemNoticeKind::Notice | SystemNoticeKind::Reminder
            ) {
                state.unread.apply_details(std::slice::from_ref(&message));
                state.notices.add(&message);
            }
        }

        // Conversations missing from the list: recent DMs reported by the server and conversations with
        // unread messages; their previews are fetched afterwards
        for conversation in &reg.recent_private_conversations {
            let key = ConversationKey::dm(conversation.user_ids.iter().copied(), self_id);
            if !state.is_bot_dm(&key) {
                state
                    .conversations
                    .bootstrap_known(&key, conversation.max_message_id, "", true);
            }
        }
        for key in state.unread.keys() {
            if let Some(max) = state.unread.max_id(&key)
                && !state.is_bot_dm(&key)
            {
                let title = match &key {
                    ConversationKey::Topic(t) => t.topic.clone(),
                    _ => String::new(),
                };
                state
                    .conversations
                    .bootstrap_known(&key, max, &title, false);
            }
        }
        let system: Vec<ConversationKey> = state
            .conversations
            .iter()
            .filter(|(key, _)| state.is_bot_dm(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in system {
            state.conversations.remove_from_list(&key);
            state.conversations.next_backfill();
        }
        drop(state);
        self.start_backfill();
        Ok(())
    }

    async fn run_events(self: Arc<Self>) {
        let mut backoff = BACKOFF_START;
        loop {
            let position = self.queue.lock().clone();
            let result = self
                .client
                .get_events(
                    &position.queue_id,
                    position.last_event_id,
                    position.longpoll_timeout + LONGPOLL_MARGIN,
                )
                .await;
            match result {
                Ok(events) => {
                    backoff = BACKOFF_START;
                    self.set_connection(ConnectionState::Connected);
                    self.apply_events(&events).await;
                }
                Err(error) if error.is_cancelled() => return,
                Err(error) if error.is_auth_error() => {
                    self.set_connection(ConnectionState::AuthFailed);
                    return;
                }
                Err(error) if error.is_bad_event_queue() => {
                    if !self.resync_until_done(&mut backoff).await {
                        return;
                    }
                }
                Err(error) => {
                    log::info!("取事件失败：{error}");
                    self.set_connection(ConnectionState::Reconnecting);
                    if !self.wait_until_reachable(&mut backoff).await {
                        return;
                    }
                }
            }
        }
    }

    // A long poll may take very long to return after recovery: probe separately so the UI learns
    // promptly that the connection is back
    async fn wait_until_reachable(&self, backoff: &mut Duration) -> bool {
        loop {
            if !self.wait(backoff).await {
                return false;
            }
            match self.client.server_settings().await {
                Ok(_) => {
                    *backoff = BACKOFF_START;
                    self.set_connection(ConnectionState::Connected);
                    return true;
                }
                Err(error) if error.is_cancelled() => return false,
                Err(error) => log::info!("服务器仍不可达：{error}"),
            }
        }
    }

    /// Waits for the backoff time; returns whether to continue.
    async fn wait(&self, backoff: &mut Duration) -> bool {
        let delay = *backoff;
        *backoff = (*backoff * 2).min(BACKOFF_MAX);
        tokio::select! {
            () = self.cancel.cancelled() => false,
            () = tokio::time::sleep(delay) => true,
        }
    }

    async fn resync_until_done(self: &Arc<Self>, backoff: &mut Duration) -> bool {
        loop {
            self.set_connection(ConnectionState::Reconnecting);
            match self.resync().await {
                Ok(()) => {
                    *backoff = BACKOFF_START;
                    self.set_connection(ConnectionState::Connected);
                    self.emit(SessionEvent::Resynced);
                    return true;
                }
                Err(SessionError::Api(error)) if error.is_cancelled() => return false,
                Err(error) if error.is_auth_error() => {
                    self.set_connection(ConnectionState::AuthFailed);
                    return false;
                }
                Err(error) => {
                    log::info!("重新同步失败：{error}");
                    if !self.wait(backoff).await {
                        return false;
                    }
                }
            }
        }
    }

    /// The event queue became invalid: re-register and replace all server state. UI state, open
    /// windows and intercepted messages are kept, see [`State::inherit`].
    async fn resync(self: &Arc<Self>) -> Result<()> {
        let reg = self.client.register().await?;
        check_server(&reg)?;
        let realm = self.client.realm_url().to_string();
        let account_id = self.account_id;
        // Keep the old state when reading fails
        let stored = self.db.call(move |db| Stored::load(db, account_id)).await?;
        let fresh = State::new(&reg, &realm, stored, &self.db, account_id);
        {
            let mut state = self.state.write();
            let previous = std::mem::replace(&mut *state, fresh);
            state.inherit(previous);
        }
        *self.queue.lock() = position_of(&reg);
        self.bootstrap(&reg).await
    }

    fn set_connection(&self, value: ConnectionState) {
        let mut current = self.connection.lock();
        if *current != value {
            *current = value;
            drop(current);
            self.emit(SessionEvent::Connection(value));
            self.notify_hooks(HookEvent::Connection(value));
        }
    }

    async fn apply_events(self: &Arc<Self>, events: &[Value]) {
        for event in events {
            if let Some(id) = event.get("id").and_then(Value::as_i64) {
                let mut queue = self.queue.lock();
                queue.last_event_id = queue.last_event_id.max(id);
            }
            let message = self.hooked_message(event);
            if let (Some(hooks), Some(message)) = (&self.hooks, &message)
                && message.sender_id != self.self_id()
                && hooks.filter_incoming(self, message).await
            {
                self.hide_by_plugin(message);
                continue;
            }
            let applied = router::apply_event(&mut self.state.write(), event);
            self.dispatch(applied);
            if let (Some(hooks), Some(message)) = (&self.hooks, &message) {
                hooks.on_message(self, message);
            }
        }
    }

    /// The message inside a message event, when there are hooks.
    fn hooked_message(&self, event: &Value) -> Option<ChatMessage> {
        self.hooks.as_ref()?;
        if event.get("type").and_then(Value::as_str) != Some("message") {
            return None;
        }
        router::message_of_event(&self.state.read(), event)
    }

    fn hide_by_plugin(self: &Arc<Self>, message: &ChatMessage) {
        let id = message.id;
        self.state.write().messages.hide(id);
        if message.is_read() {
            return;
        }
        // Do not wait for the server: events must be processed in order and should not be slowed down
        // by network requests
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(error) = this.mark_read(&[id]).await {
                log::warn!("标记被插件拦截的消息 {id} 已读失败：{error}");
            }
        });
    }

    async fn run_presence(self: Arc<Self>) {
        let interval = {
            let seconds = self.state.read().realm.presence_ping_interval_seconds;
            if seconds > 0 {
                Duration::from_secs(seconds as u64)
            } else {
                DEFAULT_PRESENCE_INTERVAL
            }
        };
        loop {
            if let Err(error) = self.ping_presence().await {
                if self.cancel.is_cancelled() {
                    return;
                }
                log::info!("上报在线状态失败：{error}");
            }
            tokio::select! {
                () = self.cancel.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
        }
    }

    async fn ping_presence(&self) -> Result<(), ApiError> {
        let status = if self.last_input.lock().elapsed() >= IDLE_AFTER {
            "idle"
        } else {
            "active"
        };
        let new_input = std::mem::take(&mut *self.new_input.lock());
        let last_update_id = self.state.read().users.presence_last_update_id;
        let result = self
            .client
            .update_presence(status, last_update_id, new_input)
            .await?;
        let changed = {
            let mut state = self.state.write();
            state.users.presence_last_update_id = result
                .presence_last_update_id
                .unwrap_or(state.users.presence_last_update_id);
            state.users.apply_presences(&result.presences)
        };
        if !changed.is_empty() {
            self.notify_hooks(HookEvent::PresenceChanged(changed.clone()));
            self.emit(SessionEvent::PresenceChanged(changed));
        }
        Ok(())
    }

    /// UI report of user input (keyboard, mouse), used to determine idleness.
    pub fn report_user_input(&self) {
        *self.last_input.lock() = Instant::now();
        *self.new_input.lock() = true;
    }

    /// Directory where this account's files are stored (`files/{host}`).
    pub(crate) fn files_root(&self) -> PathBuf {
        self.paths.files_dir().join(self.server_folder())
    }

    /// Directory of favorite copies (`favorites/{host}`).
    pub(crate) fn favorites_root(&self) -> PathBuf {
        self.paths.favorites_dir().join(self.server_folder())
    }

    /// Directory name distinguished by server: `host` or `host_port`.
    fn server_folder(&self) -> String {
        let url = self.client.realm_url();
        let host = url.host_str().unwrap_or_default();
        let folder = match url.port() {
            Some(port) => format!("{host}_{port}"),
            None => host.to_owned(),
        };
        zulip_core::file::sanitize_directory_name(&folder)
    }

    /// Only records the message without triggering a notification: used as a fallback for the echo
    /// of sent messages.
    pub(crate) fn accept_fetched(self: &Arc<Self>, dto: &zulip_api::MessageDto, local_id: &str) {
        let mut applied = Applied::default();
        {
            let mut state = self.state.write();
            if state.messages.find_pending(local_id).is_none() {
                return;
            }
            let self_id = state.users.self_id;
            let mut message = message_from_dto(dto, self_id);
            state.classify(&mut message);
            router::accept_message(&mut state, message, Some(local_id), &mut applied);
        }
        self.dispatch(applied);
    }
}

fn position_of(reg: &RegisterResult) -> QueuePosition {
    QueuePosition {
        queue_id: reg.queue_id.clone(),
        last_event_id: reg.last_event_id,
        longpoll_timeout: Duration::from_secs(u64::from(reg.event_queue_longpoll_timeout_seconds)),
    }
}

/// The conversation's narrow.
pub(crate) fn narrow_for(state: &State, key: &ConversationKey) -> Vec<NarrowTerm> {
    match key {
        ConversationKey::Topic(t) => {
            vec![
                NarrowTerm::channel(t.stream_id),
                NarrowTerm::topic(&state.topic_name(key)),
            ]
        }
        ConversationKey::Dm(d) => {
            let self_id = state.users.self_id;
            let ids: HashSet<i64> = if d.is_self_dm() {
                [self_id].into()
            } else {
                d.other_user_ids().iter().copied().collect()
            };
            let mut ids: Vec<i64> = ids.into_iter().collect();
            ids.sort_unstable();
            vec![NarrowTerm::dm(&ids)]
        }
    }
}
