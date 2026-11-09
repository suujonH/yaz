//! 插件宿主。插件随程序启动加载（登录之前），账号登录 / 结束时通知插件，
//! 新消息、收到的消息过滤、发送前过滤按优先级依次调用插件（事件链）。
//! 公开方法可以在任何线程调用；不会同步等待插件线程。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde_json::{Map, Value, json};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use zulip_core::ConversationKey;
use zulip_core::model::ChatMessage;
use zulip_plugin_sdk::abi;
use zulip_session::{HookEvent, HookFuture, Hub, OutgoingKind, Session, SessionHooks};
use zulip_store::{AppPaths, Db, PluginRecord, PluginRepo, SettingRepo};

use crate::annotation::AnnotationStore;
use crate::chain::{ChainState, Prefiltered};
use crate::event::PluginEvent;
use crate::fingerprint;
use crate::host_api::HOST_CATEGORY;
use crate::manifest::{ComposerIcon, PluginManifest, event_permission, event_title};
use crate::marshal::{MessageSnapshot, conversation_json};
use crate::panel::{
    DialogResult, DialogSpec, PanelError, PanelRequest, PluginTarget, PluginTargetKind,
};
use crate::permission::{self, PermissionGroup};
use crate::plugin::{NativePlugin, approval_meta};
use crate::resources::Resources;
use crate::settings;
use crate::shortcuts::{self, Bindings};
use crate::thread::{self, PluginThread};

pub use crate::annotation::{
    ActionOutcome, ActionView, AnnotationCard, MessageAnnotations, RenderedContent,
};
pub use crate::chain::{CHAIN_TIMEOUT, OutgoingRejected, PREFILTER_LIMIT};
pub use crate::resources::ResourceOption;
pub use crate::shortcuts::ShortcutRow;

const HOST_API_VERSION: u32 = abi::API_VERSION;
const EVENT_CAPACITY: usize = 256;
const LOG_CHANGE_CAPACITY: usize = 64;
pub const DIALOG_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error(transparent)]
    Store(#[from] zulip_store::StoreError),
    #[error("无法加密插件设置：{0}")]
    Encrypt(String),
}

/// 切换开关的结果。
#[derive(Clone, Debug)]
pub enum ToggleOutcome {
    /// 已经切换。
    Done,
    /// 需要用户确认，确认后调用 [`PluginHost::approve`]。
    NeedsApproval(Box<ApprovalInfo>),
    /// 依赖的插件未开启或未安装（名称）。
    MissingDependencies(Vec<String>),
    /// 无法读取插件文件。
    Unreadable,
}

impl ToggleOutcome {
    /// 需要提示用户的文字。
    pub fn message(&self) -> Option<String> {
        match self {
            Self::MissingDependencies(names) => {
                Some(format!("请先开启依赖的插件：{}", names.join("、")))
            }
            Self::Unreadable => Some("无法读取插件文件".to_owned()),
            _ => None,
        }
    }
}

/// 开启确认的内容：插件可以读取什么、可以做什么、会自动打开哪些链接 / 程序。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalInfo {
    pub title: String,
    pub meta: String,
    /// 插件更新后的提示。
    pub notice: Option<String>,
    pub reads: Vec<String>,
    pub acts: Vec<String>,
    pub declared: Vec<String>,
    pub events: Vec<String>,
    pub launch_rules: Vec<String>,
    pub dependencies: Vec<String>,
    /// 指纹的短格式。
    pub fingerprint: String,
}

/// 选择对象时列表的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetCandidate {
    pub target: PluginTarget,
    pub title: String,
    pub subtitle: String,
}

/// 右键菜单的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextMenuKind {
    Conversation,
    User,
    Channel,
}

/// 右键菜单的对象。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextMenuTarget {
    Conversation(ConversationKey),
    User(i64),
    Channel(i64),
}

impl ContextMenuTarget {
    pub fn kind(&self) -> ContextMenuKind {
        match self {
            Self::Conversation(_) => ContextMenuKind::Conversation,
            Self::User(_) => ContextMenuKind::User,
            Self::Channel(_) => ContextMenuKind::Channel,
        }
    }

    fn to_json(&self, session: &Session) -> Value {
        let (kind, target) = match self {
            Self::Conversation(key) => ("conversation", conversation_json(Some(key), session)),
            Self::User(id) => (
                "user",
                json!({"user": id, "name": session.display_name(*id)}),
            ),
            Self::Channel(id) => (
                "channel",
                json!({"stream": id, "name": session.channel_display_name(*id)}),
            ),
        };
        json!({"account": session.account_id(), "kind": kind, "target": target})
    }
}

/// 输入框工具栏的一个按钮。`icon` 为 [`ComposerIcon::Svg`] 时路径已是绝对路径。
#[derive(Clone, Debug)]
pub struct ComposerButton {
    pub plugin: Arc<NativePlugin>,
    pub id: i32,
    pub icon: ComposerIcon,
    pub tooltip: String,
}

/// 主窗口连接提示位置显示的插件文字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStatusText {
    pub plugin_name: String,
    pub text: String,
}

/// 设置中的按钮被点击（值尚未保存）。
#[derive(Clone, Debug, Default)]
pub struct SettingsButtonClick {
    pub key: String,
    /// list 的行号。
    pub row: Option<i64>,
    /// tags 的项（item_button）。
    pub item: Option<String>,
    /// 所在一层当前的值。
    pub values: Map<String, Value>,
    /// 全部当前的值。
    pub settings: Map<String, Value>,
}

/// 等待用户回复的对话框。
#[derive(Clone, Debug)]
pub struct PendingDialog {
    pub request_id: i64,
    pub plugin_id: String,
    pub account_id: i64,
    pub spec: DialogSpec,
}

struct DialogWait {
    dialog: PendingDialog,
    reply: SyncSender<DialogResult>,
}

pub struct PluginHostConfig {
    pub plugins_dir: PathBuf,
    pub db: Db,
    pub paths: AppPaths,
    /// 宿主函数用它等待网络请求。必须是多线程运行时。
    pub runtime: Handle,
    /// 插件对话框等待回复的时限，到期时关闭对话框并向插件返回错误。程序用 [`DIALOG_TIMEOUT`]。
    pub dialog_timeout: Duration,
}

/// 插件安装目录：程序目录下的 plugins。
pub fn default_plugins_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("plugins")))
        .unwrap_or_else(|| PathBuf::from("plugins"))
}

pub struct PluginHost {
    plugins_dir: PathBuf,
    pub(crate) db: Db,
    paths: AppPaths,
    pub(crate) runtime: Handle,
    dialog_timeout: Duration,
    pub(crate) weak: Weak<PluginHost>,
    pub(crate) thread: PluginThread,
    pub(crate) plugins: RwLock<Vec<Arc<NativePlugin>>>,
    // 已登录的会话（按登录顺序，第一个是插件的默认账号）。
    pub(crate) sessions: RwLock<Vec<Arc<Session>>>,
    events: broadcast::Sender<PluginEvent>,
    // 日志量可能很大，单独一个通道，不挤掉 events 中的其它事件。
    log_changes: broadcast::Sender<String>,
    pub(crate) annotations: AnnotationStore,
    pub(crate) resources: Resources,
    pub(crate) shortcuts: Mutex<Bindings>,
    pub(crate) chain: ChainState,
    // 插件的状态栏提示（插件 id，文字），最后设置的在后。
    status_texts: Mutex<Vec<(String, String)>>,
    // filter_outgoing 通过的内容，紧接着的发送不再过滤一次。
    pub(crate) prefiltered: Mutex<Vec<Prefiltered>>,
    // 等待界面回复的 show_dialog，回复或超时时移除。
    dialogs: Mutex<HashMap<i64, DialogWait>>,
    next_dialog: AtomicI64,
    // 插件设置“读取 → 修改 → 写入”的互斥。持有期间不调用插件，也不发事件。
    settings_lock: Mutex<()>,
}

impl PluginHost {
    pub fn new(config: PluginHostConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (log_changes, _) = broadcast::channel(LOG_CHANGE_CAPACITY);
        let shortcuts = shortcuts::load(&SettingRepo::app(&config.db));
        Arc::new_cyclic(|weak| Self {
            plugins_dir: config.plugins_dir,
            db: config.db,
            paths: config.paths,
            runtime: config.runtime,
            dialog_timeout: config.dialog_timeout,
            weak: weak.clone(),
            thread: PluginThread::start(),
            plugins: RwLock::new(Vec::new()),
            sessions: RwLock::new(Vec::new()),
            events,
            log_changes,
            annotations: AnnotationStore::default(),
            resources: Resources::default(),
            shortcuts: Mutex::new(shortcuts),
            chain: ChainState::default(),
            status_texts: Mutex::new(Vec::new()),
            prefiltered: Mutex::new(Vec::new()),
            dialogs: Mutex::new(HashMap::new()),
            next_dialog: AtomicI64::new(0),
            settings_lock: Mutex::new(()),
        })
    }

    /// 接入会话层。要在账号登录之前调用。
    pub fn attach(self: &Arc<Self>, hub: &Hub) {
        hub.set_hooks(self.clone());
    }

    pub fn plugins_dir(&self) -> &std::path::Path {
        &self.plugins_dir
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PluginEvent> {
        self.events.subscribe()
    }

    /// 插件的日志有变化（新日志或被清空），内容是插件 id。`Lagged` 时全量重读。
    pub fn subscribe_logs(&self) -> broadcast::Receiver<String> {
        self.log_changes.subscribe()
    }

    pub(crate) fn emit(&self, event: PluginEvent) {
        let _ = self.events.send(event);
    }

    pub(crate) fn runtime(&self) -> &Handle {
        &self.runtime
    }

    pub(crate) fn plugin_data_dir(&self, id: &str) -> PathBuf {
        self.paths.plugin_data_dir(id)
    }

    pub(crate) fn is_plugin_thread(&self) -> bool {
        self.thread.is_current()
    }

    pub fn plugins(&self) -> Vec<Arc<NativePlugin>> {
        self.plugins.read().clone()
    }

    pub fn plugin(&self, id: &str) -> Option<Arc<NativePlugin>> {
        self.plugins.read().iter().find(|p| p.id() == id).cloned()
    }

    /// 已登录的会话。
    pub fn sessions(&self) -> Vec<Arc<Session>> {
        self.sessions.read().clone()
    }

    fn session(&self, account_id: i64) -> Option<Arc<Session>> {
        self.sessions
            .read()
            .iter()
            .find(|s| s.account_id() == account_id)
            .cloned()
    }

    // 插件 select_account 选择的账号，没有选择或已退出时为最先登录的账号。
    fn default_session(&self, plugin: &NativePlugin) -> Option<Arc<Session>> {
        let sessions = self.sessions.read();
        let selected = plugin.selected_account();
        sessions
            .iter()
            .find(|s| s.account_id() == selected)
            .or_else(|| sessions.first())
            .cloned()
    }

    // 回调账号 > 线程绑定 > select_account > 最先登录。
    // 线程绑定的账号已退出时为 None：不改用其他账号，与回调账号一样。
    pub(crate) fn caller_session(&self, plugin: &NativePlugin) -> Option<Arc<Session>> {
        let in_callback = self.is_plugin_thread().then(thread::call_session).flatten();
        in_callback.or_else(|| match thread::bound_account(plugin) {
            Some(account_id) => self.session(account_id),
            None => self.default_session(plugin),
        })
    }

    /// 扫描插件目录并加载已启用、且文件与用户确认开启时一致的插件；新插件默认停用。
    /// 所有插件加载完成后返回。只能调用一次。
    pub async fn load_all(&self) -> Result<(), PluginError> {
        let Some(host) = self.weak.upgrade() else {
            return Ok(());
        };
        let Ok(entries) = std::fs::read_dir(&self.plugins_dir) else {
            return Ok(());
        };
        let mut directories: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        directories.sort_by_key(|d| d.to_string_lossy().to_lowercase());

        let records = Arc::new(self.db.call(|db| PluginRepo::new(db).load_all()).await?);
        let settings_repo = SettingRepo::app(&self.db);
        let mut to_load = Vec::new();
        for directory in directories {
            // 调用方可能在界面线程上，读文件和计算指纹放到阻塞线程。
            let (reader, scanned) = (host.clone(), records.clone());
            let read = self
                .runtime
                .spawn_blocking(move || reader.read_plugin(directory, &scanned))
                .await;
            let plugin = match read {
                Ok(plugin) => Arc::new(plugin),
                Err(error) => {
                    log::warn!("读取插件失败：{error}");
                    continue;
                }
            };
            if let Some(manifest) = plugin.manifest() {
                let (values, undecryptable) = settings::load(&settings_repo, manifest);
                plugin.set_settings_json(Value::Object(values).to_string());
                for key in undecryptable {
                    plugin.add_log(
                        abi::LOG_WARNING,
                        HOST_CATEGORY,
                        &format!("无法解密设置项 {key}（可能换了 Windows 账户），保存时保留原值"),
                    );
                }
                if plugin.needs_reapproval() {
                    plugin.add_log(
                        abi::LOG_WARNING,
                        HOST_CATEGORY,
                        "插件文件与上次确认开启时不同，需要重新开启",
                    );
                }
                if plugin.is_approved() && records.get(&manifest.id).is_some_and(|r| r.enabled) {
                    plugin.set_enabled(true);
                    to_load.push(plugin.clone());
                }
            }
            self.plugins.write().push(plugin);
        }
        self.drop_missing_dependencies(&mut to_load);
        self.emit(PluginEvent::PluginsChanged);
        for plugin in to_load {
            self.load(&plugin).await;
        }
        Ok(())
    }

    // 启动时依赖未开启的插件保持未激活，不改变保存的开关。依赖可能也是这样，所以重复到没有变化。
    fn drop_missing_dependencies(&self, to_load: &mut Vec<Arc<NativePlugin>>) {
        loop {
            let before = to_load.len();
            to_load.retain(|plugin| {
                let missing = self.missing_dependencies(plugin);
                if missing.is_empty() {
                    return true;
                }
                plugin.set_enabled(false);
                plugin.add_log(
                    abi::LOG_WARNING,
                    HOST_CATEGORY,
                    &format!("依赖的插件未开启：{}，本次不激活", missing.join("、")),
                );
                false
            });
            if to_load.len() == before {
                return;
            }
        }
    }

    fn read_plugin(
        &self,
        directory: PathBuf,
        records: &std::collections::HashMap<String, PluginRecord>,
    ) -> NativePlugin {
        let name = directory
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let invalid = |directory, error: String| {
            log::warn!("插件清单无效：{name}：{error}");
            NativePlugin::new(
                directory,
                None,
                Some(error),
                None,
                None,
                self.log_changes.clone(),
            )
        };
        let text = match std::fs::read_to_string(directory.join("plugin.json")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return invalid(directory, "缺少 plugin.json".to_owned());
            }
            Err(e) => return invalid(directory, e.to_string()),
        };
        let mut warnings = Vec::new();
        let manifest = match PluginManifest::parse(&text, &name, HOST_API_VERSION, &mut |w| {
            warnings.push(w)
        }) {
            Ok(manifest) => manifest,
            Err(e) => return invalid(directory, e.to_string()),
        };
        let fingerprint = fingerprint::compute(&directory)
            .inspect_err(|e| log::warn!("无法读取插件文件：{name}：{e}"))
            .ok();
        let approved = records
            .get(&manifest.id)
            .and_then(|r| r.fingerprint.clone());
        let plugin = NativePlugin::new(
            directory,
            Some(manifest),
            None,
            fingerprint,
            approved,
            self.log_changes.clone(),
        );
        for warning in warnings {
            plugin.add_log(abi::LOG_WARNING, HOST_CATEGORY, &warning);
        }
        plugin
    }

    async fn load(&self, plugin: &Arc<NativePlugin>) {
        let Some(host) = self.weak.upgrade() else {
            return;
        };
        let target = plugin.clone();
        let done = self
            .thread
            .run(self.default_session(plugin), move || {
                target.load(host.weak.clone());
                host.activate_and_announce(&target);
            })
            .await;
        if done.is_err() {
            log::warn!("加载插件失败：{}", plugin.id());
        }
        self.emit(PluginEvent::PluginsChanged);
    }

    // 插件线程上：至少有一个账号登录且插件已启用、已加载时调用 on_enable。
    fn activate(&self, plugin: &Arc<NativePlugin>) {
        let logged_in = thread::call_session()
            .is_some_and(|s| self.sessions.read().iter().any(|x| Arc::ptr_eq(x, &s)));
        if logged_in && plugin.is_enabled() && plugin.is_loaded() && !plugin.is_active() {
            plugin.set_active(true);
            self.invoke(plugin, || plugin.call_enable());
        }
    }

    // 插件线程上：开启时账号可能已经登录，调用 on_enable 后为每个已登录账号补发 on_account_login。
    fn activate_and_announce(&self, plugin: &Arc<NativePlugin>) {
        self.activate(plugin);
        if plugin.is_active() {
            for session in self.sessions() {
                self.announce_login(plugin, &session);
            }
        }
    }

    // 插件线程上：以该账号调用 on_account_login，每个账号只调用一次。
    fn announce_login(&self, plugin: &Arc<NativePlugin>, session: &Arc<Session>) {
        let account_id = session.account_id();
        if !plugin.mark_announced(account_id) {
            return;
        }
        let previous = thread::call_session();
        thread::replace_call_session(session);
        self.invoke(plugin, || plugin.call_account_login(account_id));
        if let Some(previous) = previous {
            thread::replace_call_session(&previous);
        }
    }

    // 插件线程上：收到过 on_enable 的插件调用 on_disable。
    fn deactivate(&self, plugin: &Arc<NativePlugin>) {
        if plugin.is_active() {
            plugin.set_active(false);
            plugin.clear_announced();
            plugin.set_disabling(true);
            self.invoke(plugin, || plugin.call_disable());
            plugin.set_disabling(false);
        }
    }

    /// 切换开关。依赖未开启、需要确认时不切换，由调用方提示或显示确认后再调用 [`approve`](Self::approve)。
    /// 停用时依赖它的插件随之停用，重新开启时不恢复。开启已确认的插件时重新计算指纹，文件变化后走确认。
    pub async fn toggle(&self, plugin: &Arc<NativePlugin>, enabled: bool) -> ToggleOutcome {
        if !enabled {
            for dependent in self.enabled_dependents(plugin) {
                dependent.add_log(
                    abi::LOG_WARNING,
                    HOST_CATEGORY,
                    &format!("依赖的插件“{}”已停用，本插件随之停用", plugin.name()),
                );
                self.set_enabled(&dependent, false).await;
            }
            self.set_enabled(plugin, false).await;
            return ToggleOutcome::Done;
        }
        let missing = self.missing_dependencies(plugin);
        if !missing.is_empty() {
            return ToggleOutcome::MissingDependencies(missing);
        }
        if !plugin.is_enabled() && plugin.is_approved() {
            let Some(current) = self.current_fingerprint(plugin).await else {
                return ToggleOutcome::Unreadable;
            };
            if plugin.fingerprint().as_deref() != Some(current.as_str()) {
                plugin.add_log(
                    abi::LOG_WARNING,
                    HOST_CATEGORY,
                    "插件文件与上次确认开启时不同，需要重新开启",
                );
                plugin.set_fingerprint(current);
            }
        }
        if !plugin.is_approved() {
            if plugin.fingerprint().is_none() {
                return ToggleOutcome::Unreadable;
            }
            return match self.approval_info(plugin) {
                Some(info) => ToggleOutcome::NeedsApproval(Box::new(info)),
                None => ToggleOutcome::Unreadable,
            };
        }
        self.set_enabled(plugin, true).await;
        ToggleOutcome::Done
    }

    // 依赖这个插件（直接或间接）且已开启的插件，依赖链远的在前。
    fn enabled_dependents(&self, plugin: &NativePlugin) -> Vec<Arc<NativePlugin>> {
        let plugins = self.plugins();
        let mut result: Vec<Arc<NativePlugin>> = Vec::new();
        let mut ids = vec![plugin.id()];
        let mut next = 0;
        while next < ids.len() {
            let id = ids[next].clone();
            next += 1;
            for candidate in &plugins {
                if candidate.is_enabled()
                    && candidate.depends_on(&id)
                    && !ids.contains(&candidate.id())
                {
                    ids.push(candidate.id());
                    result.push(candidate.clone());
                }
            }
        }
        result.reverse();
        result
    }

    // 未开启（或未安装）的依赖插件的名称。
    fn missing_dependencies(&self, plugin: &NativePlugin) -> Vec<String> {
        let Some(manifest) = plugin.manifest() else {
            return Vec::new();
        };
        manifest
            .dependencies
            .iter()
            .filter_map(|id| match self.plugin(id) {
                Some(dep) if dep.is_enabled() => None,
                Some(dep) => Some(dep.name()),
                None => Some(format!("{id}（未安装）")),
            })
            .collect()
    }

    // 磁盘上现在的清单（运行期间文件可能已变化，内存中的清单不再更新）。
    fn current_manifest(&self, plugin: &NativePlugin) -> Option<PluginManifest> {
        let name = plugin.directory().file_name()?.to_string_lossy();
        let text = std::fs::read_to_string(plugin.directory().join("plugin.json")).ok()?;
        PluginManifest::parse(&text, &name, HOST_API_VERSION, &mut |_| {}).ok()
    }

    fn approval_info(&self, plugin: &NativePlugin) -> Option<ApprovalInfo> {
        let current = self.current_manifest(plugin)?;
        let m = &current;
        // 妥协：DLL 不卸载、内存中的清单不替换，运行期间确认的变化要到重启后才完全生效。
        let takes_effect_later = plugin.is_loaded() || plugin.manifest() != Some(m);
        let names = |group| {
            permission::ALL
                .iter()
                .filter(|p| p.group == group && m.has_permission(p.id))
                .map(|p| p.name.to_owned())
                .collect::<Vec<_>>()
        };
        let mut events: Vec<String> = m
            .events
            .keys()
            .filter(|e| event_permission(e).is_some_and(|p| m.has_permission(p)))
            .map(|e| event_title(e).to_owned())
            .collect();
        if m.has_permission(permission::READ_MESSAGE) {
            events.extend(
                m.message_menus
                    .iter()
                    .map(|menu| format!("消息右键菜单“{}”", menu.name)),
            );
            events.extend(
                m.conversation_menus
                    .iter()
                    .map(|menu| format!("会话右键菜单“{}”", menu.name)),
            );
            events.extend(m.composer_buttons.iter().map(|button| {
                format!(
                    "输入框工具栏按钮“{}”（点击时读取输入框文字）",
                    button.tooltip
                )
            }));
        }
        if m.has_permission(permission::READ_CONTACTS) {
            events.extend(
                m.user_menus
                    .iter()
                    .map(|menu| format!("用户右键菜单“{}”", menu.name)),
            );
            events.extend(
                m.channel_menus
                    .iter()
                    .map(|menu| format!("频道右键菜单“{}”", menu.name)),
            );
        }
        Some(ApprovalInfo {
            title: format!("开启插件“{}”", m.name),
            meta: approval_meta(m),
            notice: plugin.needs_reapproval().then(|| {
                let suffix = if takes_effect_later {
                    "变化在重启程序后完全生效。"
                } else {
                    ""
                };
                format!("插件文件与上次开启时不同（已更新或被修改），请重新确认。{suffix}")
            }),
            reads: names(PermissionGroup::Read),
            acts: names(PermissionGroup::Act),
            declared: names(PermissionGroup::Declared),
            events,
            launch_rules: if m.has_permission(permission::LAUNCH) {
                m.launch.iter().map(|r| r.pattern.clone()).collect()
            } else {
                Vec::new()
            },
            dependencies: m
                .dependencies
                .iter()
                .map(|id| self.plugin(id).map_or_else(|| id.clone(), |p| p.name()))
                .collect(),
            fingerprint: plugin
                .fingerprint()
                .as_deref()
                .map(fingerprint::short)
                .unwrap_or_default(),
        })
    }

    /// 用户在开启确认中同意：重新计算指纹，与扫描时（确认框显示的清单）一致才记录并启用。
    /// 返回 false 时提示“插件文件已变化，请重启程序后再开启”。
    pub async fn approve(&self, plugin: &Arc<NativePlugin>) -> bool {
        let Some(expected) = plugin.fingerprint() else {
            return false;
        };
        if plugin.manifest().is_none()
            || self.current_fingerprint(plugin).await.as_deref() != Some(expected.as_str())
        {
            return false;
        }
        PluginRepo::new(&self.db).approve(&plugin.id(), &expected);
        plugin.set_approved(expected);
        self.set_enabled(plugin, true).await;
        true
    }

    // 读取插件目录的全部文件，放到阻塞线程上，不占住调用方（可能是界面线程）。读取失败时为 None。
    async fn current_fingerprint(&self, plugin: &NativePlugin) -> Option<String> {
        let directory = plugin.directory().to_path_buf();
        self.runtime
            .spawn_blocking(move || fingerprint::compute(&directory))
            .await
            .ok()?
            .ok()
    }

    /// 启用或停用插件（保存状态）。启用只对已确认的插件有效。
    async fn set_enabled(&self, plugin: &Arc<NativePlugin>, enabled: bool) {
        if plugin.manifest().is_none()
            || plugin.is_enabled() == enabled
            || (enabled && !plugin.is_approved())
        {
            return;
        }
        PluginRepo::new(&self.db).set_enabled(&plugin.id(), enabled);
        plugin.set_enabled(enabled);
        self.emit(PluginEvent::PluginsChanged);
        if enabled && !plugin.is_loaded() {
            self.load(plugin).await;
            return;
        }
        let Some(host) = self.weak.upgrade() else {
            return;
        };
        let target = plugin.clone();
        // 生命周期回调在插件线程上按当时的状态决定，期间又切换回来时不会重复或遗漏。
        let _ = self
            .thread
            .run(self.default_session(plugin), move || {
                if target.is_enabled() {
                    host.activate_and_announce(&target);
                } else {
                    host.deactivate(&target);
                }
            })
            .await;
    }

    /// 插件设置的当前值（解密后，含默认值）。
    pub fn load_settings(&self, plugin: &NativePlugin) -> Map<String, Value> {
        plugin
            .manifest()
            .map(|m| settings::load(&SettingRepo::app(&self.db), m).0)
            .unwrap_or_default()
    }

    /// 保存插件设置并通知插件。无法加密时不保存。
    pub fn save_settings(
        &self,
        plugin: &Arc<NativePlugin>,
        values: &Map<String, Value>,
    ) -> Result<(), PluginError> {
        {
            let _guard = self.settings_lock.lock();
            self.store_settings(plugin, values)?;
        }
        if plugin.is_enabled() && plugin.is_loaded() && !plugin.is_unresponsive() {
            self.post(plugin, |p| p.call_settings_changed());
        }
        Ok(())
    }

    // 调用方持有 settings_lock。
    fn store_settings(
        &self,
        plugin: &NativePlugin,
        values: &Map<String, Value>,
    ) -> Result<(), PluginError> {
        let Some(manifest) = plugin.manifest() else {
            return Ok(());
        };
        let repo = SettingRepo::app(&self.db);
        settings::save(&repo, manifest, values).map_err(PluginError::Encrypt)?;
        plugin.set_settings_json(Value::Object(settings::load(&repo, manifest).0).to_string());
        Ok(())
    }

    // 插件修改自己的设置：合并键，不回调 on_settings_changed。
    pub(crate) fn merge_settings(
        &self,
        plugin: &NativePlugin,
        changes: Map<String, Value>,
    ) -> Result<(), PluginError> {
        {
            let _guard = self.settings_lock.lock();
            let mut values = self.load_settings(plugin);
            values.extend(changes);
            self.store_settings(plugin, &values)?;
        }
        self.emit(PluginEvent::SettingsChanged(plugin.id()));
        Ok(())
    }

    // 请求界面显示对话框并等待回复（阻塞，不能在插件线程上调用）。超时时错误为 ABI 错误码。
    pub(crate) fn show_dialog(
        &self,
        plugin: &NativePlugin,
        account_id: i64,
        spec: DialogSpec,
    ) -> Result<DialogResult, i32> {
        self.wait_dialog(plugin, account_id, spec, self.dialog_timeout)
    }

    fn wait_dialog(
        &self,
        plugin: &NativePlugin,
        account_id: i64,
        spec: DialogSpec,
        timeout: Duration,
    ) -> Result<DialogResult, i32> {
        let request_id = self.next_dialog.fetch_add(1, Ordering::AcqRel) + 1;
        let (sender, reply) = sync_channel(1);
        let dialog = PendingDialog {
            request_id,
            plugin_id: plugin.id(),
            account_id,
            spec,
        };
        self.dialogs.lock().insert(
            request_id,
            DialogWait {
                dialog: dialog.clone(),
                reply: sender,
            },
        );
        self.emit(PluginEvent::ShowDialog {
            request_id,
            plugin_id: dialog.plugin_id,
            account_id,
            spec: dialog.spec,
        });
        if let Ok(result) = reply.recv_timeout(timeout) {
            return Ok(result);
        }
        if self.dialogs.lock().remove(&request_id).is_some() {
            self.emit(PluginEvent::DialogClosed { request_id });
            return Err(abi::ERR_NOT_FOUND);
        }
        // 登记已被 reply_dialog 取走：回复恰好赶在超时之后，结果已在通道里。
        reply.try_recv().map_err(|_| abi::ERR_NOT_FOUND)
    }

    /// 还在等待回复的对话框，按请求的先后排列。[`PluginEvent::ShowDialog`] 可能错过
    /// （界面尚未订阅、事件落后），界面据此补上。
    pub fn pending_dialogs(&self) -> Vec<PendingDialog> {
        let mut pending: Vec<PendingDialog> = self
            .dialogs
            .lock()
            .values()
            .map(|wait| wait.dialog.clone())
            .collect();
        pending.sort_by_key(|dialog| dialog.request_id);
        pending
    }

    /// 回复 [`PluginEvent::ShowDialog`]。请求不存在或已超时时返回 false。
    pub fn reply_dialog(&self, request_id: i64, result: DialogResult) -> bool {
        self.dialogs
            .lock()
            .remove(&request_id)
            .is_some_and(|wait| wait.reply.try_send(result).is_ok())
    }

    // 以默认会话投递一个调用，不等待。
    pub(crate) fn post(
        &self,
        plugin: &Arc<NativePlugin>,
        call: impl FnOnce(&Arc<NativePlugin>) + Send + 'static,
    ) {
        self.post_with(self.default_session(plugin), plugin, call);
    }

    fn post_with(
        &self,
        session: Option<Arc<Session>>,
        plugin: &Arc<NativePlugin>,
        call: impl FnOnce(&Arc<NativePlugin>) + Send + 'static,
    ) {
        let Some(host) = self.weak.upgrade() else {
            return;
        };
        let target = plugin.clone();
        drop(
            self.thread
                .run(session, move || host.invoke(&target, || call(&target))),
        );
    }

    /// 点击插件菜单。
    pub fn invoke_menu(&self, plugin: &Arc<NativePlugin>, menu_id: i32) {
        if plugin.is_enabled() && plugin.is_loaded() && !plugin.is_unresponsive() {
            self.post(plugin, move |p| p.call_menu(menu_id));
        }
    }

    /// 点击消息右键菜单：以消息所属的账号调用插件。
    pub fn invoke_message_menu(
        &self,
        plugin: &Arc<NativePlugin>,
        menu_id: i32,
        message: &ChatMessage,
        session: &Arc<Session>,
    ) {
        if plugin.active_message_menus().is_empty() {
            return;
        }
        let Some(snapshot) = MessageSnapshot::new(message, session, true) else {
            return;
        };
        self.post_with(Some(session.clone()), plugin, move |p| {
            p.call_message_menu(menu_id, &snapshot);
        });
    }

    /// 点击会话、用户或频道的右键菜单：以目标所属的账号调用插件。
    pub fn invoke_context_menu(
        &self,
        plugin: &Arc<NativePlugin>,
        menu_id: i32,
        session: &Arc<Session>,
        target: &ContextMenuTarget,
    ) {
        if plugin.active_context_menus(target.kind()).is_empty() {
            return;
        }
        let json = target.to_json(session).to_string();
        self.post_with(Some(session.clone()), plugin, move |p| {
            p.call_context_menu(menu_id, &json);
        });
    }

    /// 已开启的插件的输入框工具栏按钮。
    pub fn composer_buttons(&self) -> Vec<ComposerButton> {
        self.plugins()
            .iter()
            .flat_map(|plugin| {
                plugin
                    .active_composer_buttons()
                    .iter()
                    .map(|b| ComposerButton {
                        plugin: plugin.clone(),
                        id: b.id,
                        icon: match &b.icon {
                            ComposerIcon::Svg(file) => ComposerIcon::Svg(
                                plugin.directory().join(file).to_string_lossy().into_owned(),
                            ),
                            builtin => builtin.clone(),
                        },
                        tooltip: b.tooltip.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// 点击工具栏按钮，带当前会话和输入框文字；插件用 fill_composer 替换输入框内容，
    /// 结果走 [`PluginEvent::FillComposer`]。
    pub fn invoke_composer_button(
        &self,
        plugin: &Arc<NativePlugin>,
        button_id: i32,
        session: &Arc<Session>,
        key: &ConversationKey,
        text: &str,
    ) {
        if plugin.active_composer_buttons().is_empty() {
            return;
        }
        let json = json!({
            "account": session.account_id(),
            "conv": conversation_json(Some(key), session),
            "text": text,
        })
        .to_string();
        self.post_with(Some(session.clone()), plugin, move |p| {
            p.call_composer_button(button_id, &json);
        });
    }

    pub(crate) fn set_status_text(&self, plugin_id: &str, text: &str) {
        {
            let mut texts = self.status_texts.lock();
            texts.retain(|(id, _)| id != plugin_id);
            if !text.is_empty() {
                texts.push((plugin_id.to_owned(), text.to_owned()));
            }
        }
        self.emit(PluginEvent::StatusTextChanged);
    }

    /// 要显示在主窗口连接提示位置的文字：已开启的插件中最后设置的。
    pub fn status_text(&self) -> Option<PluginStatusText> {
        let texts = self.status_texts.lock().clone();
        texts.iter().rev().find_map(|(id, text)| {
            let plugin = self.plugin(id).filter(|p| p.is_enabled())?;
            Some(PluginStatusText {
                plugin_name: plugin.name(),
                text: text.clone(),
            })
        })
    }

    /// 插件面板：以 target 所属的账号调用 on_panel，返回插件回复的对象。
    /// 插件正常返回但没有回复或回复格式错误时为 `Ok(None)`；超时不计入超时次数。
    /// 结果用 [`parse_fields`](crate::panel::parse_fields)、[`parse_entries`](crate::panel::parse_entries) 解析。
    pub async fn panel(
        &self,
        plugin: &Arc<NativePlugin>,
        request: PanelRequest,
    ) -> Result<Option<Value>, PanelError> {
        if plugin.active_panels().is_empty() {
            return Err(PanelError::Inactive);
        }
        let host = self.weak.upgrade().ok_or(PanelError::Inactive)?;
        let session = request
            .account_id()
            .and_then(|id| self.session(id))
            .or_else(|| self.default_session(plugin));
        let json = request.to_json().to_string();
        let target = plugin.clone();
        let reply = self.thread.run(session, move || {
            thread::in_panel(|| host.invoke(&target, || target.call_panel(&json)))
        });
        let text = tokio::time::timeout(CHAIN_TIMEOUT, reply)
            .await
            .map_err(|_| PanelError::Timeout)?
            .map_err(|_| PanelError::Timeout)?;
        Ok(text
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .filter(Value::is_object))
    }

    /// 聊天窗口中显示了这些消息（打开会话、加载历史、收到新消息），插件可以重新附加保存的结果。
    pub fn notify_messages_shown(&self, session: &Arc<Session>, message_ids: &[i64]) {
        let targets: Vec<_> = self
            .plugins()
            .into_iter()
            .filter(|p| p.handles_messages_shown())
            .collect();
        if message_ids.is_empty() || targets.is_empty() {
            return;
        }
        let json = json!({"account": session.account_id(), "messages": message_ids}).to_string();
        let Some(host) = self.weak.upgrade() else {
            return;
        };
        drop(self.thread.run(Some(session.clone()), move || {
            for plugin in targets.iter().filter(|p| p.handles_messages_shown()) {
                host.invoke(plugin, || plugin.call_messages_shown(&json));
            }
        }));
    }

    /// 设置中的按钮。返回 false 表示插件未开启或不支持。
    pub fn invoke_settings_button(
        &self,
        plugin: &Arc<NativePlugin>,
        click: &SettingsButtonClick,
    ) -> bool {
        if !plugin.can_use_settings_buttons() {
            return false;
        }
        let mut json =
            json!({"key": click.key, "values": click.values, "settings": click.settings});
        if let Some(row) = click.row {
            json["row"] = json!(row);
        }
        if let Some(item) = &click.item {
            json["item"] = json!(item);
        }
        let json = json.to_string();
        self.post(plugin, move |p| p.call_settings_button(&json));
        true
    }

    /// 为面板列表选择对象：所有已登录账号的用户（不含自己和机器人），或已订阅频道的话题（没有缓存时加载）。
    pub async fn target_candidates(&self, kind: PluginTargetKind) -> Vec<TargetCandidate> {
        let sessions = self.sessions();
        let show_account = sessions.len() > 1;
        let mut result = Vec::new();
        for session in &sessions {
            let suffix = if show_account {
                format!(" · {}", session.email())
            } else {
                String::new()
            };
            match kind {
                PluginTargetKind::User => {
                    let self_id = session.self_id();
                    let mut users: Vec<_> = session
                        .users()
                        .into_iter()
                        .filter(|u| u.is_active && !u.is_bot && u.id != self_id)
                        .map(|u| (session.display_name(u.id), u.id, u.email))
                        .collect();
                    users.sort();
                    result.extend(users.into_iter().map(|(name, id, email)| TargetCandidate {
                        target: PluginTarget::User {
                            account_id: session.account_id(),
                            user_id: id,
                            name: name.clone(),
                        },
                        title: name,
                        subtitle: format!("{email}{suffix}"),
                    }));
                }
                PluginTargetKind::Topic => {
                    let mut channels = session.subscribed_channels();
                    channels.sort_by(|a, b| a.name.cmp(&b.name));
                    for channel in channels {
                        if session.topics(channel.stream_id).is_none()
                            && let Err(error) = session.load_topics(channel.stream_id).await
                        {
                            log::warn!("加载话题失败：{}：{error}", channel.name);
                        }
                        for topic in session.topics(channel.stream_id).unwrap_or_default() {
                            result.push(TargetCandidate {
                                target: PluginTarget::Topic {
                                    account_id: session.account_id(),
                                    stream_id: channel.stream_id,
                                    name: format!("{} / {}", channel.name, topic.name),
                                    topic: topic.name.clone(),
                                },
                                title: topic.name,
                                subtitle: format!("{}{suffix}", channel.name),
                            });
                        }
                    }
                }
            }
        }
        result
    }

    /// 程序退出：插件线程不再接受新的调用。
    pub fn shutdown(&self) {
        self.thread.stop();
        self.dialogs.lock().clear();
    }
}

impl SessionHooks for PluginHost {
    fn session_started(&self, session: &Arc<Session>) {
        {
            let mut sessions = self.sessions.write();
            if sessions.iter().any(|s| Arc::ptr_eq(s, session)) {
                return;
            }
            sessions.push(session.clone());
        }
        let Some(host) = self.weak.upgrade() else {
            return;
        };
        let started = session.clone();
        drop(self.thread.run(Some(session.clone()), move || {
            for plugin in host.plugins() {
                host.activate(&plugin);
                if plugin.is_active() {
                    host.announce_login(&plugin, &started);
                }
            }
        }));
    }

    fn session_ending(&self, session: &Arc<Session>) -> HookFuture<()> {
        let removed = {
            let mut sessions = self.sessions.write();
            let before = sessions.len();
            sessions.retain(|s| !Arc::ptr_eq(s, session));
            sessions.len() != before
        };
        let host = self.weak.upgrade();
        let (true, Some(host)) = (removed, host) else {
            return Box::pin(async {});
        };
        let last = self.sessions.read().is_empty();
        let account_id = session.account_id();
        let job_host = host.clone();
        let done = host.thread.run(Some(session.clone()), move || {
            for plugin in job_host.plugins() {
                if plugin.is_active() && plugin.unmark_announced(account_id) {
                    job_host.invoke(&plugin, || plugin.call_account_logout(account_id));
                }
                if last {
                    job_host.deactivate(&plugin);
                }
            }
        });
        Box::pin(async move {
            let _ = done.await;
        })
    }

    fn filter_incoming(&self, session: &Arc<Session>, message: &ChatMessage) -> HookFuture<bool> {
        self.dispatch_filter_incoming(session, message)
    }

    fn on_message(&self, session: &Arc<Session>, message: &ChatMessage) {
        self.dispatch_on_message(session, message);
    }

    fn on_event(&self, session: &Arc<Session>, event: HookEvent) {
        self.dispatch_event(session, event);
    }

    fn filter_outgoing(
        &self,
        session: &Arc<Session>,
        key: &ConversationKey,
        content: &str,
        kind: OutgoingKind,
    ) -> HookFuture<Result<String, String>> {
        self.dispatch_filter_outgoing(session, key, content, kind)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// 只有内存数据库的宿主，不加载任何插件。要在 tokio 运行时内调用。
    pub(crate) fn host() -> Arc<PluginHost> {
        PluginHost::new(PluginHostConfig {
            plugins_dir: PathBuf::from("不存在的插件目录"),
            db: Db::open_in_memory().unwrap(),
            paths: AppPaths::new(PathBuf::from("不存在的数据目录")),
            runtime: Handle::current(),
            dialog_timeout: DIALOG_TIMEOUT,
        })
    }

    /// 已启用的插件（未加载 DLL），`extra` 合并到清单中。
    pub(crate) fn plugin(host: &PluginHost, extra: &Value) -> Arc<NativePlugin> {
        const ID: &str = "test.plugin";
        let mut root = json!({
            "id": ID,
            "name": "测试",
            "version": "1",
            "api_version": 1,
            "library": "plugin.dll",
        });
        if let (Some(root), Some(extra)) = (root.as_object_mut(), extra.as_object()) {
            root.extend(extra.clone());
        }
        let manifest =
            PluginManifest::parse(&root.to_string(), ID, HOST_API_VERSION, &mut |_| {}).unwrap();
        let plugin = NativePlugin::new(
            PathBuf::from(ID),
            Some(manifest),
            None,
            None,
            None,
            host.log_changes.clone(),
        );
        plugin.set_enabled(true);
        Arc::new(plugin)
    }
}

#[cfg(test)]
mod tests {
    use crate::panel::DialogKind;
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    use super::*;

    const LONG: Duration = Duration::from_secs(30);

    fn spec() -> DialogSpec {
        DialogSpec {
            kind: DialogKind::Confirm,
            title: "标题".to_owned(),
            text: String::new(),
            fields: Vec::new(),
        }
    }

    fn pending_ids(host: &PluginHost) -> Vec<i64> {
        host.pending_dialogs()
            .iter()
            .map(|d| d.request_id)
            .collect()
    }

    fn start_dialog(
        host: &Arc<PluginHost>,
        plugin: &Arc<NativePlugin>,
        timeout: Duration,
    ) -> tokio::task::JoinHandle<Result<DialogResult, i32>> {
        let (host, plugin) = (host.clone(), plugin.clone());
        tokio::task::spawn_blocking(move || host.wait_dialog(&plugin, 7, spec(), timeout))
    }

    async fn wait_pending(host: &PluginHost, count: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while host.pending_dialogs().len() != count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dialog_shown_without_subscribers_can_be_picked_up_later() {
        let host = test_support::host();
        let plugin = test_support::plugin(&host, &json!({}));
        let first = start_dialog(&host, &plugin, LONG);
        wait_pending(&host, 1).await;
        let second = start_dialog(&host, &plugin, LONG);
        wait_pending(&host, 2).await;

        // 界面这时才订阅：事件已经错过，从待显示表取。
        let mut events = host.subscribe();
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
        let pending = host.pending_dialogs();
        assert_eq!(pending.len(), 2);
        assert!(pending[0].request_id < pending[1].request_id);
        assert_eq!(pending[0].plugin_id, "test.plugin");
        assert_eq!(pending[0].account_id, 7);
        assert_eq!(pending[0].spec.title, "标题");

        let answer = DialogResult {
            ok: true,
            values: Map::new(),
        };
        assert!(host.reply_dialog(pending[1].request_id, answer.clone()));
        assert!(host.reply_dialog(pending[0].request_id, answer.clone()));
        assert!(first.await.unwrap().unwrap().ok);
        assert!(second.await.unwrap().unwrap().ok);
        assert!(host.pending_dialogs().is_empty());
        assert!(!host.reply_dialog(pending[0].request_id, answer));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dialog_stays_pending_when_events_lag() {
        let host = test_support::host();
        let plugin = test_support::plugin(&host, &json!({}));
        let mut events = host.subscribe();
        let waiting = start_dialog(&host, &plugin, LONG);
        wait_pending(&host, 1).await;
        for _ in 0..EVENT_CAPACITY + 1 {
            host.emit(PluginEvent::StatusTextChanged);
        }
        assert!(matches!(events.recv().await, Err(RecvError::Lagged(_))));
        let ids = pending_ids(&host);
        assert_eq!(ids.len(), 1);
        assert!(host.reply_dialog(ids[0], DialogResult::default()));
        assert!(waiting.await.unwrap().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dialog_timeout_removes_the_request_and_tells_the_ui() {
        let host = test_support::host();
        let plugin = test_support::plugin(&host, &json!({}));
        let mut events = host.subscribe();
        let waiting = start_dialog(&host, &plugin, Duration::from_millis(300));
        let Ok(PluginEvent::ShowDialog { request_id, .. }) = events.recv().await else {
            panic!("应先收到 ShowDialog");
        };
        assert_eq!(pending_ids(&host), [request_id]);

        let timed_out = waiting.await.unwrap();
        assert!(matches!(timed_out, Err(code) if code == abi::ERR_NOT_FOUND));
        assert!(host.pending_dialogs().is_empty());
        assert!(matches!(
            events.recv().await,
            Ok(PluginEvent::DialogClosed { request_id: closed }) if closed == request_id
        ));
        assert!(!host.reply_dialog(request_id, DialogResult::default()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replied_dialog_does_not_report_closed() {
        let host = test_support::host();
        let plugin = test_support::plugin(&host, &json!({}));
        let mut events = host.subscribe();
        let waiting = start_dialog(&host, &plugin, LONG);
        let Ok(PluginEvent::ShowDialog { request_id, .. }) = events.recv().await else {
            panic!("应先收到 ShowDialog");
        };
        assert!(host.reply_dialog(request_id, DialogResult::default()));
        assert!(waiting.await.unwrap().is_ok());
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_releases_waiting_dialogs() {
        let host = test_support::host();
        let plugin = test_support::plugin(&host, &json!({}));
        let waiting = start_dialog(&host, &plugin, LONG);
        wait_pending(&host, 1).await;
        host.shutdown();
        assert!(matches!(waiting.await.unwrap(), Err(code) if code == abi::ERR_NOT_FOUND));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn logs_have_their_own_channel() {
        let host = test_support::host();
        let plugin = test_support::plugin(&host, &json!({}));
        let mut events = host.subscribe();
        let mut logs = host.subscribe_logs();
        plugin.add_log(abi::LOG_INFO, "c", "第一条");
        assert_eq!(logs.recv().await.unwrap(), "test.plugin");

        // 日志再多也不占其它事件的容量。
        host.emit(PluginEvent::StatusTextChanged);
        for _ in 0..EVENT_CAPACITY * 2 {
            plugin.add_log(abi::LOG_INFO, "c", "刷屏");
        }
        assert!(matches!(
            events.try_recv(),
            Ok(PluginEvent::StatusTextChanged)
        ));
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
        assert!(matches!(logs.recv().await, Err(RecvError::Lagged(_))));

        plugin.clear_logs();
        while !matches!(logs.try_recv(), Err(TryRecvError::Empty)) {}
        plugin.clear_logs();
        assert_eq!(logs.recv().await.unwrap(), "test.plugin");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_merges_keep_every_key() {
        const KEYS: usize = 8;
        const ROUNDS: usize = 20;
        let host = test_support::host();
        let fields: Vec<Value> = (0..KEYS)
            .map(|i| json!({"key": format!("k{i}"), "label": "x"}))
            .collect();
        let plugin = test_support::plugin(&host, &json!({"settings": fields}));
        let tasks: Vec<_> = (0..KEYS)
            .map(|i| {
                let (host, plugin) = (host.clone(), plugin.clone());
                tokio::task::spawn_blocking(move || {
                    for round in 0..ROUNDS {
                        let changes = Map::from_iter([(format!("k{i}"), json!(round.to_string()))]);
                        host.merge_settings(&plugin, changes).unwrap();
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        let last = (ROUNDS - 1).to_string();
        let values = host.load_settings(&plugin);
        for i in 0..KEYS {
            assert_eq!(values[&format!("k{i}")], last.as_str(), "k{i}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn settings_changed_is_emitted_after_the_lock_is_released() {
        let host = test_support::host();
        let plugin =
            test_support::plugin(&host, &json!({"settings": [{"key": "a", "label": "x"}]}));
        let mut events = host.subscribe();
        host.merge_settings(&plugin, Map::from_iter([("a".to_owned(), json!("1"))]))
            .unwrap();
        assert!(host.settings_lock.try_lock().is_some());
        assert!(matches!(
            events.try_recv(),
            Ok(PluginEvent::SettingsChanged(id)) if id == "test.plugin"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn current_fingerprint_runs_on_the_runtime() {
        let host = test_support::host();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "内容").unwrap();
        let plugin = NativePlugin::new(
            dir.path().to_path_buf(),
            None,
            None,
            None,
            None,
            host.log_changes.clone(),
        );
        assert_eq!(
            host.current_fingerprint(&plugin).await.unwrap(),
            fingerprint::compute(dir.path()).unwrap()
        );
        let missing = NativePlugin::new(
            dir.path().join("不存在"),
            None,
            None,
            None,
            None,
            host.log_changes.clone(),
        );
        assert_eq!(host.current_fingerprint(&missing).await, None);
    }
}
