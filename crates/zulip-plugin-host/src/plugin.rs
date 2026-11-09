//! 一个原生插件：清单、导出函数、状态与日志。DLL 加载后不会卸载（停用只是不再派发事件）。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use chrono::{DateTime, Local};
use libloading::Library;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use zulip_plugin_sdk::abi::{
    self, AccountFn, HostApi, IdJsonFn, InitFn, JsonFn, MenuFn, MessageFn, MessageMenuFn,
    OutgoingFn, RawOutgoing, ResourceRequestFn, VoidFn,
};
use zulip_session::OutgoingKind;

use crate::host::{ContextMenuKind, PluginHost};
use crate::host_api;
use crate::manifest::{
    FILTER_INCOMING, FILTER_OUTGOING, MAX_PRIORITY, NOTIFICATIONS, ON_MESSAGE,
    PluginComposerButton, PluginManifest, PluginMenu, event_permission,
};
use crate::marshal::{Conversation, MessageSnapshot, Scope};
use crate::panel::PluginPanel;
use crate::permission;

const MAX_LOGS: usize = 1000;
const MAX_TIMEOUTS: u32 = 3;

/// 日志行的外观。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogKind {
    Debug,
    Info,
    Success,
    Io,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub time: DateTime<Local>,
    pub level: i32,
    pub category: String,
    pub text: String,
}

impl LogEntry {
    pub fn time_text(&self) -> String {
        self.time.format("%H:%M:%S").to_string()
    }

    pub fn level_name(&self) -> &'static str {
        match self.level {
            abi::LOG_INFO_SUCCESS => "成功",
            abi::LOG_INFO_RECV => "接收",
            abi::LOG_INFO_SEND => "发送",
            level if level < abi::LOG_INFO => "调试",
            level if level < abi::LOG_WARNING => "信息",
            level if level < abi::LOG_ERROR => "警告",
            level if level < abi::LOG_FATAL => "错误",
            _ => "致命",
        }
    }

    pub fn kind(&self) -> LogKind {
        match self.level {
            abi::LOG_INFO_SUCCESS => LogKind::Success,
            abi::LOG_INFO_RECV | abi::LOG_INFO_SEND => LogKind::Io,
            level if level < abi::LOG_INFO => LogKind::Debug,
            level if level < abi::LOG_WARNING => LogKind::Info,
            level if level < abi::LOG_ERROR => LogKind::Warning,
            _ => LogKind::Error,
        }
    }

    /// 复制时的一行文字。
    pub fn line(&self) -> String {
        format!(
            "{} [{}] {}: {}",
            self.time_text(),
            self.level_name(),
            self.category,
            self.text
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Exports {
    on_enable: Option<VoidFn>,
    on_disable: Option<VoidFn>,
    on_account_login: Option<AccountFn>,
    on_account_logout: Option<AccountFn>,
    on_message: Option<MessageFn>,
    filter_incoming: Option<MessageFn>,
    filter_outgoing: Option<OutgoingFn>,
    on_menu: Option<MenuFn>,
    on_message_menu: Option<MessageMenuFn>,
    on_messages_shown: Option<JsonFn>,
    on_settings_changed: Option<VoidFn>,
    on_settings_button: Option<JsonFn>,
    on_resource_request: Option<ResourceRequestFn>,
    on_panel: Option<JsonFn>,
    on_context_menu: Option<IdJsonFn>,
    on_composer_button: Option<IdJsonFn>,
    // 与 NOTIFICATIONS 一一对应。
    notifications: [Option<JsonFn>; NOTIFICATIONS.len()],
}

/// # Safety
/// `T` 是导出函数的真实类型。
unsafe fn export<T: Copy>(library: &Library, name: &[u8]) -> Option<T> {
    // SAFETY: 见函数说明。
    unsafe { library.get::<T>(name) }.ok().map(|symbol| *symbol)
}

impl Exports {
    /// # Safety
    /// `library` 按 C ABI 导出这些函数。
    unsafe fn resolve(library: &Library) -> Self {
        // SAFETY: 见函数说明。
        unsafe {
            Self {
                on_enable: export(library, b"zulip_plugin_on_enable\0"),
                on_disable: export(library, b"zulip_plugin_on_disable\0"),
                on_account_login: export(library, b"zulip_plugin_on_account_login\0"),
                on_account_logout: export(library, b"zulip_plugin_on_account_logout\0"),
                on_message: export(library, b"zulip_plugin_on_message\0"),
                filter_incoming: export(library, b"zulip_plugin_filter_incoming\0"),
                filter_outgoing: export(library, b"zulip_plugin_filter_outgoing\0"),
                on_menu: export(library, b"zulip_plugin_on_menu\0"),
                on_message_menu: export(library, b"zulip_plugin_on_message_menu\0"),
                on_messages_shown: export(library, b"zulip_plugin_on_messages_shown\0"),
                on_settings_changed: export(library, b"zulip_plugin_on_settings_changed\0"),
                on_settings_button: export(library, b"zulip_plugin_on_settings_button\0"),
                on_resource_request: export(library, b"zulip_plugin_on_resource_request\0"),
                on_panel: export(library, b"zulip_plugin_on_panel\0"),
                on_context_menu: export(library, b"zulip_plugin_on_context_menu\0"),
                on_composer_button: export(library, b"zulip_plugin_on_composer_button\0"),
                notifications: NOTIFICATIONS.map(|(event, ..)| {
                    export(library, format!("zulip_plugin_{event}\0").as_bytes())
                }),
            }
        }
    }
}

pub struct NativePlugin {
    directory: PathBuf,
    manifest: Option<PluginManifest>,
    fingerprint: Mutex<Option<String>>,
    approved: Mutex<Option<String>>,
    load_error: Mutex<Option<String>>,
    exports: OnceLock<Exports>,
    enabled: AtomicBool,
    active: AtomicBool,
    // on_disable 回调执行期间。
    disabling: AtomicBool,
    // 已经收到 on_account_login、还没有 on_account_logout / on_disable 的账号。只在插件线程上访问。
    announced_accounts: Mutex<Vec<i64>>,
    unresponsive: AtomicBool,
    timeouts: AtomicU32,
    selected_account: AtomicI64,
    settings_json: Mutex<String>,
    logs: Mutex<VecDeque<LogEntry>>,
    // 日志变化的通知，内容是插件 id。
    log_changes: broadcast::Sender<String>,
}

impl std::fmt::Debug for NativePlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NativePlugin({})", self.id())
    }
}

impl NativePlugin {
    pub(crate) fn new(
        directory: PathBuf,
        manifest: Option<PluginManifest>,
        load_error: Option<String>,
        fingerprint: Option<String>,
        approved: Option<String>,
        log_changes: broadcast::Sender<String>,
    ) -> Self {
        Self {
            directory,
            manifest,
            fingerprint: Mutex::new(fingerprint),
            approved: Mutex::new(approved),
            load_error: Mutex::new(load_error),
            exports: OnceLock::new(),
            enabled: AtomicBool::new(false),
            active: AtomicBool::new(false),
            disabling: AtomicBool::new(false),
            announced_accounts: Mutex::new(Vec::new()),
            unresponsive: AtomicBool::new(false),
            timeouts: AtomicU32::new(0),
            selected_account: AtomicI64::new(0),
            settings_json: Mutex::new("{}".to_owned()),
            logs: Mutex::new(VecDeque::new()),
            log_changes,
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// 清单；plugin.json 无效时为 `None`（此时 [`load_error`](Self::load_error) 说明原因）。
    pub fn manifest(&self) -> Option<&PluginManifest> {
        self.manifest.as_ref()
    }

    fn directory_name(&self) -> String {
        self.directory
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn id(&self) -> String {
        self.manifest
            .as_ref()
            .map_or_else(|| self.directory_name(), |m| m.id.clone())
    }

    pub fn name(&self) -> String {
        self.manifest
            .as_ref()
            .map_or_else(|| self.directory_name(), |m| m.name.clone())
    }

    /// 版本与作者，例如“1.0.0 · YaZ”；清单无效时为 id。
    pub fn meta(&self) -> String {
        match &self.manifest {
            Some(m) => join_meta(&[&m.version, &m.author]),
            None => self.id(),
        }
    }

    /// 插件目录的指纹；读取失败时为 `None`（不能开启）。
    pub fn fingerprint(&self) -> Option<String> {
        self.fingerprint.lock().clone()
    }

    pub(crate) fn set_fingerprint(&self, fingerprint: String) {
        *self.fingerprint.lock() = Some(fingerprint);
    }

    /// 当前文件已经过用户确认。
    pub fn is_approved(&self) -> bool {
        let fingerprint = self.fingerprint.lock();
        fingerprint.is_some() && *self.approved.lock() == *fingerprint
    }

    /// 确认过、但之后文件发生了变化，需要重新确认。
    pub fn needs_reapproval(&self) -> bool {
        self.approved.lock().is_some() && !self.is_approved()
    }

    pub(crate) fn set_approved(&self, fingerprint: String) {
        *self.approved.lock() = Some(fingerprint);
    }

    /// 清单无效、DLL 加载失败或初始化失败的原因。
    pub fn load_error(&self) -> Option<String> {
        self.load_error.lock().clone()
    }

    /// 已加载并成功初始化。
    pub fn is_loaded(&self) -> bool {
        self.exports.get().is_some()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    /// 连续多次超时：本次运行内不再派发事件。
    pub fn is_unresponsive(&self) -> bool {
        self.unresponsive.load(Ordering::Acquire)
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.manifest
            .as_ref()
            .is_some_and(|m| m.has_permission(permission))
    }

    pub(crate) fn depends_on(&self, plugin_id: &str) -> bool {
        self.manifest
            .as_ref()
            .is_some_and(|m| m.dependencies.iter().any(|d| d == plugin_id))
    }

    /// 异常状态的文字（开启、停用由开关表示）；没有异常时为空。
    pub fn status_text(&self) -> String {
        let error = self.load_error();
        if self.manifest.is_none() || (self.is_enabled() && !self.is_loaded() && error.is_some()) {
            format!("加载失败：{}", error.unwrap_or_default())
        } else if self.fingerprint.lock().is_none() {
            "无法读取插件文件".to_owned()
        } else if self.needs_reapproval() {
            "插件文件已变化（更新或被修改），需要重新开启".to_owned()
        } else if self.is_enabled() && self.is_unresponsive() {
            "无响应".to_owned()
        } else {
            String::new()
        }
    }

    pub fn is_error(&self) -> bool {
        self.manifest.is_none()
            || self.is_unresponsive()
            || self.needs_reapproval()
            || (self.is_enabled() && self.load_error().is_some())
    }

    // 已加载、已启用、未被标记为无响应时的导出函数。
    fn live(&self) -> Option<&Exports> {
        if self.is_enabled() && !self.is_unresponsive() {
            self.exports.get()
        } else {
            None
        }
    }

    /// 可以调用的插件菜单。
    pub fn active_menus(&self) -> &[PluginMenu] {
        match (&self.manifest, self.live()) {
            (Some(m), Some(_)) => &m.menus,
            _ => &[],
        }
    }

    /// 消息右键菜单中显示的菜单项。
    pub fn active_message_menus(&self) -> &[PluginMenu] {
        match (&self.manifest, self.live()) {
            (Some(m), Some(exports))
                if exports.on_message_menu.is_some()
                    && self.has_permission(permission::READ_MESSAGE) =>
            {
                &m.message_menus
            }
            _ => &[],
        }
    }

    /// 用户资料卡 / 话题资料面板中显示的分组。
    pub fn active_panels(&self) -> &[PluginPanel] {
        match (&self.manifest, self.live()) {
            (Some(m), Some(exports)) if exports.on_panel.is_some() => &m.panels,
            _ => &[],
        }
    }

    /// 右键菜单中显示的菜单项。
    pub fn active_context_menus(&self, kind: ContextMenuKind) -> &[PluginMenu] {
        let (Some(m), Some(exports)) = (&self.manifest, self.live()) else {
            return &[];
        };
        if exports.on_context_menu.is_none() {
            return &[];
        }
        let (menus, needed) = match kind {
            ContextMenuKind::Conversation => (&m.conversation_menus, permission::READ_MESSAGE),
            ContextMenuKind::User => (&m.user_menus, permission::READ_CONTACTS),
            ContextMenuKind::Channel => (&m.channel_menus, permission::READ_CONTACTS),
        };
        if self.has_permission(needed) {
            menus
        } else {
            &[]
        }
    }

    /// 输入框工具栏中显示的按钮（点击时收到输入框文字，需要 read_message）。
    pub fn active_composer_buttons(&self) -> &[PluginComposerButton] {
        match (&self.manifest, self.live()) {
            (Some(m), Some(exports))
                if exports.on_composer_button.is_some()
                    && self.has_permission(permission::READ_MESSAGE) =>
            {
                &m.composer_buttons
            }
            _ => &[],
        }
    }

    /// 设置中的按钮可以使用。
    pub fn can_use_settings_buttons(&self) -> bool {
        self.live().is_some_and(|e| e.on_settings_button.is_some())
    }

    pub(crate) fn handles_messages_shown(&self) -> bool {
        self.live().is_some_and(|e| e.on_messages_shown.is_some())
            && self.has_permission(permission::READ_MESSAGE)
    }

    pub(crate) fn can_provide_resources(&self) -> bool {
        self.live().is_some_and(|e| e.on_resource_request.is_some())
    }

    /// 接收该事件：可用、导出了函数、声明了事件并拥有所需权限。
    pub(crate) fn handles(&self, event: &str) -> bool {
        let Some(exports) = self.live() else {
            return false;
        };
        let exported = match event {
            ON_MESSAGE => exports.on_message.is_some(),
            FILTER_INCOMING => exports.filter_incoming.is_some(),
            FILTER_OUTGOING => exports.filter_outgoing.is_some(),
            _ => notification_index(event).is_some_and(|i| exports.notifications[i].is_some()),
        };
        exported
            && self
                .manifest
                .as_ref()
                .is_some_and(|m| m.events.contains_key(event))
            && event_permission(event).is_some_and(|p| self.has_permission(p))
    }

    pub(crate) fn priority(&self, event: &str) -> i64 {
        self.manifest
            .as_ref()
            .and_then(|m| m.events.get(event).copied())
            .unwrap_or(MAX_PRIORITY)
    }

    pub(crate) fn selected_account(&self) -> i64 {
        self.selected_account.load(Ordering::Acquire)
    }

    pub(crate) fn select_account(&self, account_id: i64) {
        self.selected_account.store(account_id, Ordering::Release);
    }

    pub(crate) fn settings_json(&self) -> String {
        self.settings_json.lock().clone()
    }

    pub(crate) fn set_settings_json(&self, json: String) {
        *self.settings_json.lock() = json;
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }

    pub(crate) fn set_disabling(&self, disabling: bool) {
        self.disabling.store(disabling, Ordering::Release);
    }

    /// 插件可以调用宿主函数：已启用，或正在执行 on_disable。
    pub(crate) fn accepts_calls(&self) -> bool {
        self.is_enabled() || self.disabling.load(Ordering::Acquire)
    }

    /// 记录账号已通知；已经通知过时返回 false。
    pub(crate) fn mark_announced(&self, account_id: i64) -> bool {
        let mut accounts = self.announced_accounts.lock();
        if accounts.contains(&account_id) {
            return false;
        }
        accounts.push(account_id);
        true
    }

    /// 取消账号的通知记录；没有通知过时返回 false。
    pub(crate) fn unmark_announced(&self, account_id: i64) -> bool {
        let mut accounts = self.announced_accounts.lock();
        let before = accounts.len();
        accounts.retain(|id| *id != account_id);
        accounts.len() != before
    }

    pub(crate) fn clear_announced(&self) {
        self.announced_accounts.lock().clear();
    }

    /// 插件线程上：加载 DLL、准备宿主函数表并调用 init。失败时记录原因。
    pub(crate) fn load(self: &Arc<Self>, host: Weak<PluginHost>) {
        let Some(manifest) = self.manifest.as_ref() else {
            return;
        };
        if self.is_loaded() || !self.is_approved() {
            return;
        }
        match self.try_load(manifest, host) {
            Ok(exports) => {
                let _ = self.exports.set(exports);
                *self.load_error.lock() = None;
            }
            Err(error) => {
                log::warn!("加载插件失败：{}：{error}", manifest.id);
                *self.load_error.lock() = Some(error);
            }
        }
    }

    fn try_load(
        self: &Arc<Self>,
        manifest: &PluginManifest,
        host: Weak<PluginHost>,
    ) -> Result<Exports, String> {
        let path = self.directory.join(&manifest.library);
        // SAFETY: 用户确认开启的插件，加载时运行其初始化代码。
        let library = unsafe { Library::new(&path) }
            .map_err(|e| format!("无法加载 {}：{e}", manifest.library))?;
        // SAFETY: 导出函数的类型由 ABI 约定。
        let init = unsafe { export::<InitFn>(&library, b"zulip_plugin_init\0") }
            .ok_or_else(|| "缺少导出函数 zulip_plugin_init".to_owned())?;
        // SAFETY: 同上。
        let exports = unsafe { Exports::resolve(&library) };
        // 插件可能已经启动了自己的线程，不能卸载。
        std::mem::forget(library);
        let api: &'static HostApi = host_api::create(self.clone(), host);
        // SAFETY: 函数表一直有效。
        let result = unsafe { init(api) };
        if result != abi::OK {
            return Err(format!("初始化失败（zulip_plugin_init 返回 {result}）"));
        }
        Ok(exports)
    }

    fn exports(&self) -> Option<&Exports> {
        self.exports.get()
    }

    // 以下只在插件线程上调用；没有导出的函数视为返回 IGNORE。

    pub(crate) fn call_enable(&self) {
        if let Some(f) = self.exports().and_then(|e| e.on_enable) {
            // SAFETY: ABI 约定。
            unsafe { f() };
        }
    }

    pub(crate) fn call_disable(&self) {
        if let Some(f) = self.exports().and_then(|e| e.on_disable) {
            // SAFETY: ABI 约定。
            unsafe { f() };
        }
    }

    pub(crate) fn call_account_login(&self, account_id: i64) {
        if let Some(f) = self.exports().and_then(|e| e.on_account_login) {
            // SAFETY: ABI 约定。
            unsafe { f(account_id) };
        }
    }

    pub(crate) fn call_account_logout(&self, account_id: i64) {
        if let Some(f) = self.exports().and_then(|e| e.on_account_logout) {
            // SAFETY: ABI 约定。
            unsafe { f(account_id) };
        }
    }

    pub(crate) fn call_settings_changed(&self) {
        if let Some(f) = self.exports().and_then(|e| e.on_settings_changed) {
            // SAFETY: ABI 约定。
            unsafe { f() };
        }
    }

    pub(crate) fn call_menu(&self, menu_id: i32) {
        if let Some(f) = self.exports().and_then(|e| e.on_menu) {
            // SAFETY: ABI 约定。
            unsafe { f(menu_id) };
        }
    }

    pub(crate) fn call_message_menu(&self, menu_id: i32, message: &MessageSnapshot) {
        if let Some(f) = self.exports().and_then(|e| e.on_message_menu) {
            let mut scope = Scope::default();
            let raw = message.to_raw(&mut scope);
            // SAFETY: raw 中的指针在 scope 存活期间有效。
            unsafe { f(menu_id, &raw) };
        }
    }

    fn call_message(export: Option<MessageFn>, message: &MessageSnapshot) -> i32 {
        let Some(f) = export else {
            return abi::EVENT_IGNORE;
        };
        let mut scope = Scope::default();
        let raw = message.to_raw(&mut scope);
        // SAFETY: raw 中的指针在 scope 存活期间有效。
        unsafe { f(&raw) }
    }

    pub(crate) fn call_on_message(&self, message: &MessageSnapshot) -> i32 {
        Self::call_message(self.exports().and_then(|e| e.on_message), message)
    }

    pub(crate) fn call_filter_incoming(&self, message: &MessageSnapshot) -> i32 {
        Self::call_message(self.exports().and_then(|e| e.filter_incoming), message)
    }

    pub(crate) fn call_filter_outgoing(
        &self,
        account_id: i64,
        conv: &Conversation,
        content: &str,
        kind: OutgoingKind,
    ) -> i32 {
        let Some(f) = self.exports().and_then(|e| e.filter_outgoing) else {
            return abi::EVENT_IGNORE;
        };
        let mut scope = Scope::default();
        let conv = conv.to_raw(&mut scope);
        let raw = RawOutgoing {
            struct_size: size_of::<RawOutgoing>() as u32,
            kind: match kind {
                OutgoingKind::Text => abi::OUTGOING_TEXT,
                OutgoingKind::File => abi::OUTGOING_FILE,
                OutgoingKind::Forward => abi::OUTGOING_FORWARD,
            },
            account_id,
            conv: scope.conversation(conv),
            content: scope.text(content),
        };
        // SAFETY: raw 中的指针在 scope 存活期间有效。
        unsafe { f(&raw) }
    }

    fn call_json(export: Option<JsonFn>, json: &str) {
        if let Some(f) = export {
            let mut scope = Scope::default();
            let text = scope.text(json);
            // SAFETY: 字符串在 scope 存活期间有效。
            unsafe { f(text) };
        }
    }

    pub(crate) fn call_messages_shown(&self, json: &str) {
        Self::call_json(self.exports().and_then(|e| e.on_messages_shown), json);
    }

    pub(crate) fn call_settings_button(&self, json: &str) {
        Self::call_json(self.exports().and_then(|e| e.on_settings_button), json);
    }

    pub(crate) fn call_panel(&self, json: &str) {
        Self::call_json(self.exports().and_then(|e| e.on_panel), json);
    }

    pub(crate) fn call_notification(&self, event: &str, json: &str) {
        let export =
            notification_index(event).and_then(|i| self.exports().and_then(|e| e.notifications[i]));
        Self::call_json(export, json);
    }

    fn call_id_json(export: Option<IdJsonFn>, id: i32, json: &str) {
        if let Some(f) = export {
            let mut scope = Scope::default();
            let text = scope.text(json);
            // SAFETY: 字符串在 scope 存活期间有效。
            unsafe { f(id, text) };
        }
    }

    pub(crate) fn call_context_menu(&self, menu_id: i32, json: &str) {
        Self::call_id_json(
            self.exports().and_then(|e| e.on_context_menu),
            menu_id,
            json,
        );
    }

    pub(crate) fn call_composer_button(&self, button_id: i32, json: &str) {
        Self::call_id_json(
            self.exports().and_then(|e| e.on_composer_button),
            button_id,
            json,
        );
    }

    pub(crate) fn call_resource_request(&self, request_id: i64, json: &str) {
        if let Some(f) = self.exports().and_then(|e| e.on_resource_request) {
            let mut scope = Scope::default();
            let text = scope.text(json);
            // SAFETY: 字符串在 scope 存活期间有效。
            unsafe { f(request_id, text) };
        }
    }

    /// 调用超时：连续多次后标记为无响应。
    pub(crate) fn on_timeout(&self) {
        if self.timeouts.fetch_add(1, Ordering::AcqRel) + 1 >= MAX_TIMEOUTS {
            self.unresponsive.store(true, Ordering::Release);
        }
    }

    /// 调用在超时时间内返回：超时计数清零。
    pub(crate) fn on_returned(&self) {
        self.timeouts.store(0, Ordering::Release);
    }

    pub fn add_log(&self, level: i32, category: &str, text: &str) {
        {
            let mut logs = self.logs.lock();
            logs.push_back(LogEntry {
                time: Local::now(),
                level,
                category: category.to_owned(),
                text: text.to_owned(),
            });
            while logs.len() > MAX_LOGS {
                logs.pop_front();
            }
        }
        let _ = self.log_changes.send(self.id());
    }

    pub fn logs(&self) -> Vec<LogEntry> {
        self.logs.lock().iter().cloned().collect()
    }

    pub fn clear_logs(&self) {
        self.logs.lock().clear();
        let _ = self.log_changes.send(self.id());
    }
}

fn notification_index(event: &str) -> Option<usize> {
    NOTIFICATIONS.iter().position(|(name, ..)| *name == event)
}

fn join_meta(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 开启确认中的元信息。
pub(crate) fn approval_meta(manifest: &PluginManifest) -> String {
    join_meta(&[&manifest.version, &manifest.author, &manifest.id])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(level: i32) -> LogEntry {
        LogEntry {
            time: Local::now(),
            level,
            category: "c".into(),
            text: "t".into(),
        }
    }

    #[test]
    fn log_levels() {
        let names = [
            (abi::LOG_DEBUG, "调试", LogKind::Debug),
            (abi::LOG_INFO, "信息", LogKind::Info),
            (abi::LOG_INFO_SUCCESS, "成功", LogKind::Success),
            (abi::LOG_INFO_RECV, "接收", LogKind::Io),
            (abi::LOG_INFO_SEND, "发送", LogKind::Io),
            (abi::LOG_WARNING, "警告", LogKind::Warning),
            (abi::LOG_ERROR, "错误", LogKind::Error),
            (abi::LOG_FATAL, "致命", LogKind::Error),
        ];
        for (level, name, kind) in names {
            assert_eq!(entry(level).level_name(), name);
            assert_eq!(entry(level).kind(), kind);
        }
        assert!(entry(abi::LOG_WARNING).line().ends_with("[警告] c: t"));
    }

    #[test]
    fn logs_are_capped() {
        let (events, _) = broadcast::channel(4);
        let plugin = NativePlugin::new("a.b.c".into(), None, None, None, None, events);
        for i in 0..MAX_LOGS + 5 {
            plugin.add_log(abi::LOG_INFO, "c", &i.to_string());
        }
        let logs = plugin.logs();
        assert_eq!(logs.len(), MAX_LOGS);
        assert_eq!(logs[0].text, "5");
        plugin.clear_logs();
        assert!(plugin.logs().is_empty());
    }

    #[test]
    fn approval_state() {
        let (events, _) = broadcast::channel(4);
        let plugin = NativePlugin::new(
            "a.b.c".into(),
            None,
            None,
            Some("new".into()),
            Some("old".into()),
            events.clone(),
        );
        assert!(!plugin.is_approved());
        assert!(plugin.needs_reapproval());
        plugin.set_approved("new".into());
        assert!(plugin.is_approved());
        assert!(!plugin.needs_reapproval());

        let fresh = NativePlugin::new("a.b.c".into(), None, None, Some("x".into()), None, events);
        assert!(!fresh.is_approved());
        assert!(!fresh.needs_reapproval());
    }

    #[test]
    fn timeouts_mark_unresponsive() {
        let (events, _) = broadcast::channel(4);
        let plugin = NativePlugin::new("a.b.c".into(), None, None, None, None, events);
        plugin.on_timeout();
        plugin.on_timeout();
        plugin.on_returned();
        plugin.on_timeout();
        plugin.on_timeout();
        assert!(!plugin.is_unresponsive());
        plugin.on_timeout();
        assert!(plugin.is_unresponsive());
    }
}
