//! 进程级的多账号管理：登录、自动登录、切换、退出和删除账号。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::broadcast;
use zulip_api::{ClientConfig, ZulipClient};
use zulip_store::{AccountRepo, AccountRow, AppPaths, Db, protect_key, unprotect_key};

use crate::error::{Result, SessionError, describe_login_error};
use crate::event::{ConnectionState, SessionEvent};
use crate::hooks::SessionHooks;
use crate::login::{normalize_realm, parse_zuliprc};
use crate::session::{MIN_FEATURE_LEVEL, Session};

const AUTH_EXPIRED: &str = "登录已失效，请重新登录";
const HOOK_ENDING_LIMIT: Duration = Duration::from_secs(2);

/// 账号的状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountStatus {
    Connecting,
    Online,
    /// 登录失败，内容是错误说明。
    Failed(String),
    /// 没有登录（用户退出或未启用启动时登录）。
    Disabled,
}

impl AccountStatus {
    pub fn text(&self) -> &str {
        match self {
            Self::Connecting => "正在登录…",
            Self::Online => "在线",
            Self::Failed(message) => message,
            Self::Disabled => "未启用",
        }
    }
}

/// 账号列表的一行。
#[derive(Clone)]
pub struct AccountEntry {
    pub account: AccountRow,
    pub status: AccountStatus,
    pub session: Option<Arc<Session>>,
}

/// Hub 事件。
#[derive(Clone, Debug, PartialEq)]
pub enum HubEvent {
    /// 账号列表或某个账号的状态变化。
    AccountsChanged,
    ActiveChanged(Option<i64>),
    /// 某个账号的会话事件（已在线的账号都转发，供通知和角标使用）。
    Session(i64, SessionEvent),
}

/// 密码登录的请求。
#[derive(Clone, Debug)]
pub struct PasswordLogin {
    pub realm: String,
    pub email: String,
    pub password: String,
    pub allow_invalid_cert: bool,
}

#[derive(Default)]
struct HubState {
    sessions: HashMap<i64, Arc<Session>>,
    statuses: HashMap<i64, AccountStatus>,
    /// 递增的登录序号：进行中的登录完成时序号已变化，说明用户又操作过，结果丢弃。
    attempts: HashMap<i64, u64>,
    active: Option<i64>,
}

pub struct Hub {
    db: Db,
    paths: AppPaths,
    config: Option<Arc<ClientConfig>>,
    state: Mutex<HubState>,
    tx: broadcast::Sender<HubEvent>,
    hooks: Mutex<Option<Arc<dyn SessionHooks>>>,
}

impl Hub {
    /// `config` 为 `None` 时使用默认客户端配置。
    pub fn new(db: Db, paths: AppPaths, config: Option<Arc<ClientConfig>>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            db,
            paths,
            config,
            state: Mutex::new(HubState::default()),
            tx,
            hooks: Mutex::new(None),
        })
    }

    /// 安装钩子。在登录账号之前调用。
    pub fn set_hooks(&self, hooks: Arc<dyn SessionHooks>) {
        *self.hooks.lock() = Some(hooks);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.tx.subscribe()
    }

    fn emit(&self, event: HubEvent) {
        let _ = self.tx.send(event);
    }

    fn repo(&self) -> AccountRepo {
        AccountRepo::new(&self.db)
    }

    fn client_builder(&self, realm: &str, allow_invalid_cert: bool) -> zulip_api::ClientBuilder {
        let builder = ZulipClient::builder(realm).allow_invalid_cert(allow_invalid_cert);
        match &self.config {
            Some(config) => builder.config(config.clone()),
            None => builder,
        }
    }

    /// 账号列表（显示顺序）。同步读数据库，只在界面线程上调用。
    pub fn accounts(&self) -> Result<Vec<AccountEntry>> {
        let rows = self.repo().get_listed()?;
        let state = self.state.lock();
        Ok(rows
            .into_iter()
            .map(|account| AccountEntry {
                session: state.sessions.get(&account.id).cloned(),
                status: state
                    .statuses
                    .get(&account.id)
                    .cloned()
                    .unwrap_or(AccountStatus::Disabled),
                account,
            })
            .collect())
    }

    pub fn session(&self, account_id: i64) -> Option<Arc<Session>> {
        self.state.lock().sessions.get(&account_id).cloned()
    }

    pub fn online_sessions(&self) -> Vec<Arc<Session>> {
        self.state.lock().sessions.values().cloned().collect()
    }

    pub fn active_session(&self) -> Option<Arc<Session>> {
        let state = self.state.lock();
        state.sessions.get(&state.active?).cloned()
    }

    pub fn active_account(&self) -> Option<i64> {
        self.state.lock().active
    }

    pub fn set_active(&self, account_id: Option<i64>) {
        self.state.lock().active = account_id;
        self.emit(HubEvent::ActiveChanged(account_id));
    }

    fn begin_attempt(&self, account_id: i64) -> u64 {
        let mut state = self.state.lock();
        let attempt = state.attempts.entry(account_id).or_default();
        *attempt += 1;
        *attempt
    }

    fn is_current(&self, account_id: i64, attempt: u64) -> bool {
        self.state.lock().attempts.get(&account_id) == Some(&attempt)
    }

    fn set_status(&self, account_id: i64, status: AccountStatus) {
        self.state.lock().statuses.insert(account_id, status);
        self.emit(HubEvent::AccountsChanged);
    }

    /// 用邮箱和密码登录，保存账号并建立会话。
    pub async fn login_with_password(
        self: &Arc<Self>,
        request: PasswordLogin,
    ) -> Result<Arc<Session>> {
        let realm = normalize_realm(&request.realm)
            .ok_or_else(|| SessionError::invalid("服务器地址无效"))?;
        let anonymous = self
            .client_builder(&realm, request.allow_invalid_cert)
            .build()?;
        let settings = anonymous.server_settings().await?;
        if settings.zulip_feature_level < MIN_FEATURE_LEVEL {
            return Err(SessionError::ServerTooOld);
        }
        if !settings.email_auth_enabled {
            return Err(SessionError::invalid(
                "该组织未开启邮箱密码登录，请使用 zuliprc 登录",
            ));
        }
        let key = anonymous
            .fetch_api_key(request.email.trim(), &request.password)
            .await?;
        self.login_with_key(&realm, &key.email, &key.api_key, request.allow_invalid_cert)
            .await
    }

    /// 用 zuliprc 的内容登录。
    pub async fn login_with_zuliprc(
        self: &Arc<Self>,
        text: &str,
        allow_invalid_cert: bool,
    ) -> Result<Arc<Session>> {
        let parsed = parse_zuliprc(text)?;
        self.login_with_key(
            &parsed.site,
            &parsed.email,
            &parsed.api_key,
            allow_invalid_cert,
        )
        .await
    }

    async fn login_with_key(
        self: &Arc<Self>,
        realm: &str,
        email: &str,
        api_key: &str,
        allow_invalid_cert: bool,
    ) -> Result<Arc<Session>> {
        let row = AccountRow {
            id: 0,
            realm_url: realm.to_owned(),
            email: email.to_owned(),
            user_id: 0,
            full_name: String::new(),
            avatar_url: None,
            api_key: Some(protect_key(api_key)?),
            allow_invalid_cert,
            login_on_startup: true,
            last_login_at: chrono::Utc::now().timestamp(),
        };
        let account_id = self
            .db
            .call(move |db| {
                let repo = AccountRepo::new(db);
                let row = match repo.find(&row.realm_url, &row.email)? {
                    Some(previous) => AccountRow {
                        user_id: previous.user_id,
                        full_name: previous.full_name,
                        avatar_url: previous.avatar_url,
                        ..row
                    },
                    None => row,
                };
                repo.upsert(&row)
            })
            .await?;
        let repo = self.repo();
        repo.add_to_list(account_id);
        repo.set_login_on_startup(account_id, true);
        self.start(account_id).await
    }

    /// 用保存的 key 登录账号。
    pub async fn login_account(self: &Arc<Self>, account_id: i64) -> Result<Arc<Session>> {
        self.start(account_id).await
    }

    async fn start(self: &Arc<Self>, account_id: i64) -> Result<Arc<Session>> {
        if let Some(session) = self.session(account_id) {
            return Ok(session);
        }
        let attempt = self.begin_attempt(account_id);
        self.set_status(account_id, AccountStatus::Connecting);
        let result = self.connect(account_id).await;
        if !self.is_current(account_id, attempt) {
            if let Ok(session) = &result {
                session.shutdown().await;
            }
            return Err(SessionError::invalid("登录已取消"));
        }
        match result {
            Ok(session) => {
                self.state
                    .lock()
                    .sessions
                    .insert(account_id, session.clone());
                self.set_status(account_id, AccountStatus::Online);
                self.forward_events(account_id, attempt, &session);
                if let Some(hooks) = self.hooks.lock().clone() {
                    hooks.session_started(&session);
                }
                if self.active_account().is_none() {
                    self.set_active(Some(account_id));
                }
                Ok(session)
            }
            Err(error) => {
                self.set_status(
                    account_id,
                    AccountStatus::Failed(describe_login_error(&error)),
                );
                Err(error)
            }
        }
    }

    async fn connect(&self, account_id: i64) -> Result<Arc<Session>> {
        let account = self
            .db
            .call(move |db| AccountRepo::new(db).get(account_id))
            .await?
            .ok_or_else(|| SessionError::invalid("账号不存在"))?;
        let Some(api_key) = &account.api_key else {
            return Err(SessionError::invalid("登录信息已失效，请重新输入密码"));
        };
        let api_key = unprotect_key(api_key)?;
        let client = self
            .client_builder(&account.realm_url, account.allow_invalid_cert)
            .credentials(&account.email, &api_key)
            .build()?;
        let hooks = self.hooks.lock().clone();
        Session::connect(
            client,
            account_id,
            self.db.clone(),
            self.paths.clone(),
            hooks,
        )
        .await
    }

    /// 通知钩子后关闭会话。
    async fn stop_session(&self, session: &Arc<Session>) {
        let hooks = self.hooks.lock().clone();
        if let Some(hooks) = hooks {
            let _ = tokio::time::timeout(HOOK_ENDING_LIMIT, hooks.session_ending(session)).await;
        }
        session.shutdown().await;
    }

    fn forward_events(self: &Arc<Self>, account_id: i64, attempt: u64, session: &Arc<Session>) {
        let hub = Arc::downgrade(self);
        let mut rx = session.subscribe();
        tokio::spawn(async move {
            loop {
                let event = match rx.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => SessionEvent::Resynced,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let Some(hub) = hub.upgrade() else { return };
                if !hub.is_current(account_id, attempt) {
                    return;
                }
                if event == SessionEvent::Connection(ConnectionState::AuthFailed) {
                    hub.on_auth_failed(account_id).await;
                }
                hub.emit(HubEvent::Session(account_id, event));
            }
        });
    }

    async fn on_auth_failed(&self, account_id: i64) {
        let session = self.state.lock().sessions.remove(&account_id);
        if let Some(session) = session {
            self.stop_session(&session).await;
        }
        self.set_status(account_id, AccountStatus::Failed(AUTH_EXPIRED.to_owned()));
        self.pick_next_active(account_id);
    }

    fn pick_next_active(&self, ended: i64) {
        let next = {
            let mut state = self.state.lock();
            if state.active != Some(ended) {
                return;
            }
            let next = state.sessions.keys().min().copied();
            state.active = next;
            next
        };
        self.emit(HubEvent::ActiveChanged(next));
    }

    /// 启动时登录所有设置为启动时登录的账号，并发进行。失败的账号记录在状态中。
    pub async fn restore_startup_accounts(self: &Arc<Self>) -> Result<()> {
        let accounts = self
            .db
            .call(|db| AccountRepo::new(db).get_startup_accounts())
            .await?;
        let tasks: Vec<_> = accounts
            .into_iter()
            .map(|account| {
                let hub = self.clone();
                tokio::spawn(async move {
                    if let Err(error) = hub.start(account.id).await {
                        log::warn!("账号 {} 登录失败：{error}", account.id);
                    }
                })
            })
            .collect();
        for task in tasks {
            let _ = task.await;
        }
        Ok(())
    }

    /// 设置启动时登录。开启时立即登录，关闭时结束会话。
    pub async fn set_login_on_startup(
        self: &Arc<Self>,
        account_id: i64,
        enabled: bool,
    ) -> Result<()> {
        self.repo().set_login_on_startup(account_id, enabled);
        if enabled {
            self.start(account_id).await?;
        } else {
            self.end_session(account_id).await;
            self.set_status(account_id, AccountStatus::Disabled);
        }
        Ok(())
    }

    async fn end_session(&self, account_id: i64) {
        self.begin_attempt(account_id);
        let session = self.state.lock().sessions.remove(&account_id);
        if let Some(session) = session {
            self.stop_session(&session).await;
        }
        self.pick_next_active(account_id);
    }

    /// 退出登录：从列表移除，`delete_data` 时同时删除本地数据。
    pub async fn logout(&self, account_id: i64, delete_data: bool) {
        self.end_session(account_id).await;
        self.state.lock().statuses.remove(&account_id);
        let repo = self.repo();
        repo.remove_from_list(account_id);
        if delete_data {
            repo.delete_with_data(account_id);
        }
        self.emit(HubEvent::AccountsChanged);
    }

    /// 删除账号：同时清除保存的 key。等写入执行完才返回，数据库已关闭时返回错误。
    pub async fn delete_account(&self, account_id: i64, delete_data: bool) -> Result<()> {
        self.repo().clear_key(account_id);
        self.logout(account_id, delete_data).await;
        self.db.call(|_| Ok(())).await?;
        Ok(())
    }

    /// 程序退出：结束所有会话，等待本地数据写完。
    pub async fn shutdown(&self) {
        let sessions: Vec<(i64, Arc<Session>)> = {
            let mut state = self.state.lock();
            state.attempts.values_mut().for_each(|a| *a += 1);
            state.sessions.drain().collect()
        };
        for (_, session) in sessions {
            self.stop_session(&session).await;
        }
        if let Err(error) = self.db.call(|_| Ok(())).await {
            log::warn!("等待本地数据写入失败：{error}");
        }
    }
}
