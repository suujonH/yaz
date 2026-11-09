//! 进程级服务：数据库、设置、账号 Hub、更新。数据目录搬迁完成后才创建。

use crate::pages::data_move::DataProblem;
use crate::platform;
#[cfg(debug_assertions)]
use crate::request_log::sync_request_log;
use crate::shell::{ColorScheme, Palette, ThemeConfig, ThemeHub, WindowBoundsStore};
use crate::startup::Boot;
use crate::state::{AppCommand, send_command};
use log::LevelFilter;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use zulip_api::ClientConfig;
use zulip_core::window::WindowBounds;
use zulip_plugin_host::{DIALOG_TIMEOUT, PluginHost, PluginHostConfig, default_plugins_dir};
use zulip_session::{Hub, Session};
use zulip_store::{AccountSettings, AppPaths, DataLocation, Db, FileLogger, LocalSettings, keys};
use zulip_update::{
    LogLevel, UpdateConfig, UpdateHost, UpdateService, UpdateStatus, parse_version,
};

pub const MAIN_BOUNDS_KEY: &str = "main";

static BOOT: OnceLock<Boot> = OnceLock::new();
static SERVICES: OnceLock<Services> = OnceLock::new();
/// 登录、搬迁和主界面是同一个窗口，只有主界面阶段才读取和保存位置。
static MAIN_BOUNDS_ENABLED: AtomicBool = AtomicBool::new(false);
static PLUGINS: OnceLock<Arc<PluginHost>> = OnceLock::new();
static PLUGINS_LOADED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
static UNAVAILABLE_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub struct Services {
    pub db: Db,
    pub paths: AppPaths,
    pub settings: LocalSettings,
    pub hub: Arc<Hub>,
}

pub fn set_boot(boot: Boot) {
    if BOOT.set(boot).is_err() {
        log::warn!("重复设置启动信息");
    }
}

pub fn boot() -> &'static Boot {
    BOOT.get().expect("main 已设置启动信息")
}

/// 进程级服务。搬迁窗口之外的代码都可以直接用。
pub fn services() -> &'static Services {
    SERVICES.get().expect("服务已初始化")
}

pub fn try_services() -> Option<&'static Services> {
    SERVICES.get()
}

/// 插件宿主。第一次调用时挂到 Hub 上，所以要早于任何登录；需要在运行时内调用。
pub fn plugins() -> &'static Arc<PluginHost> {
    PLUGINS.get_or_init(|| {
        let services = services();
        let host = PluginHost::new(PluginHostConfig {
            plugins_dir: default_plugins_dir(),
            db: services.db.clone(),
            paths: services.paths.clone(),
            runtime: tokio::runtime::Handle::current(),
            dialog_timeout: DIALOG_TIMEOUT,
        });
        host.attach(&services.hub);
        host
    })
}

/// 登录前等插件加载完，新消息不会漏过过滤。
pub async fn load_plugins() {
    PLUGINS_LOADED
        .get_or_init(|| async {
            if let Err(error) = plugins().load_all().await {
                log::warn!("加载插件失败：{error}");
            }
        })
        .await;
}

/// 开发者模式的“显示不可用的功能”。
#[cfg(debug_assertions)]
pub fn show_unavailable() -> bool {
    services().settings.show_unavailable()
}

/// Release 不编译开发者模式：已保存的开发者选项不生效。
#[cfg(not(debug_assertions))]
pub const fn show_unavailable() -> bool {
    false
}

/// 修改本机设置，应用它的副作用，并通知所有窗口的 [`use_local_setting`](crate::state::use_local_setting)。
pub fn write_setting(change: impl FnOnce(&LocalSettings)) {
    let settings = &services().settings;
    let before = EffectInputs::read(settings);
    change(settings);
    apply_setting_effects(&before, &EffectInputs::read(settings));
    send_command(AppCommand::SettingsChanged);
}

/// 修改账号的本机设置并通知所有窗口：设置页随之刷新，其它读取方下次读到新值。
/// 已经打开的聊天输入框不跟随“源码模式”切换，以免打断编辑。
pub fn write_account_setting(account_id: i64, change: impl FnOnce(&AccountSettings)) {
    change(&AccountSettings::new(&services().db, account_id));
    send_command(AppCommand::SettingsChanged);
}

/// 决定设置副作用的那些设置值。
#[derive(Clone, Debug, PartialEq, Eq)]
struct EffectInputs {
    client_name: String,
    #[cfg(debug_assertions)]
    log_requests: bool,
    #[cfg(debug_assertions)]
    request_log_limit: usize,
    collapse_resolved: bool,
    topic_title_format: String,
}

impl EffectInputs {
    fn read(settings: &LocalSettings) -> Self {
        Self {
            client_name: settings.client_name(),
            #[cfg(debug_assertions)]
            log_requests: settings.log_requests(),
            #[cfg(debug_assertions)]
            request_log_limit: settings.request_log_limit(),
            collapse_resolved: settings.collapse_resolved_topics(),
            topic_title_format: settings.topic_title_format(),
        }
    }
}

/// 一次写入需要的副作用。
#[derive(Debug, Default, PartialEq, Eq)]
struct SettingEffects {
    client_name: bool,
    #[cfg(debug_assertions)]
    request_log: bool,
    conversations: bool,
}

fn setting_effects(before: &EffectInputs, after: &EffectInputs) -> SettingEffects {
    SettingEffects {
        client_name: before.client_name != after.client_name,
        #[cfg(debug_assertions)]
        request_log: before.log_requests != after.log_requests
            || before.request_log_limit != after.request_log_limit,
        conversations: before.collapse_resolved != after.collapse_resolved
            || before.topic_title_format != after.topic_title_format,
    }
}

fn apply_setting_effects(before: &EffectInputs, after: &EffectInputs) {
    let effects = setting_effects(before, after);
    if effects.client_name {
        ClientConfig::global().set_client_name(&after.client_name);
    }
    #[cfg(debug_assertions)]
    if effects.request_log {
        sync_request_log(&services().settings);
    }
    if effects.conversations {
        for session in services().hub.online_sessions() {
            session.refresh_conversations();
        }
    }
}

struct AppUpdateHost;

impl UpdateHost for AppUpdateHost {
    fn idle_time(&self) -> Duration {
        platform::idle_time()
    }

    fn exit_for_update(&self) {
        send_command(AppCommand::ExitForUpdate);
    }

    fn log(&self, level: LogLevel, message: &str) {
        match level {
            LogLevel::Info => log::info!("{message}"),
            LogLevel::Warn => log::warn!("{message}"),
            LogLevel::Error => log::error!("{message}"),
        }
    }

    fn status_changed(&self, _status: &UpdateStatus) {
        send_command(AppCommand::UpdateStatusChanged);
    }
}

pub fn create_update_service(location: &DataLocation) -> Option<Arc<UpdateService>> {
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;
    let app_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let updater_log = location.default_root().join("updater.log");
    let config = UpdateConfig::new(current, app_dir, updater_log);
    match UpdateService::new(config, Arc::new(AppUpdateHost)) {
        Ok(service) => Some(Arc::new(service)),
        Err(error) => {
            log::warn!("创建更新服务失败：{error}");
            None
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum InitError {
    #[error("无法创建数据目录“{0}”：{1}")]
    CreateRoot(PathBuf, std::io::Error),
    #[error("无法打开数据库：{0}")]
    Store(#[from] zulip_store::StoreError),
}

/// 启动前检查数据目录的配置，有问题时返回要提示用户的问题；此时还没有打开数据库。
pub fn check_data_location() -> Option<DataProblem> {
    let location = &boot().location;
    if let Some(quarantined) = location.quarantine_corrupt_config() {
        return Some(DataProblem::Corrupted(
            quarantined.map_err(|e| e.to_string()),
        ));
    }
    location.resolve().unavailable.map(DataProblem::Unavailable)
}

/// 配置的目录不可用时，用户选择本次使用默认目录：不改配置，下次启动再次检查。
/// 返回值同 [`init_or_report`]。
pub fn init_default_or_report(problem: &DataProblem) -> bool {
    let location = &boot().location;
    let root = location.default_root().to_path_buf();
    if !init_at_or_report(root) {
        return false;
    }
    match problem {
        DataProblem::Unavailable(path) => {
            let _ = UNAVAILABLE_ROOT.set(path.clone());
            log::warn!(
                "配置的数据目录“{}”不可用，本次使用默认目录“{}”",
                path.display(),
                location.default_root().display()
            );
        }
        DataProblem::Corrupted(Ok(bad)) => {
            log::warn!("数据目录配置已损坏，已改名为“{}”", bad.display());
        }
        DataProblem::Corrupted(Err(error)) => {
            log::warn!("数据目录配置已损坏，改名失败：{error}");
        }
    }
    true
}

/// 这次运行回退到默认目录时，配置的那个目录。回退期间不能搬迁：搬迁会覆盖配置，忘记原位置。
pub fn unavailable_data_root() -> Option<&'static PathBuf> {
    UNAVAILABLE_ROOT.get()
}

/// 创建 [`Services`]；失败时提示并返回 false，调用方应退出。
pub fn init_or_report() -> bool {
    init_at_or_report(boot().location.resolve().root)
}

fn init_at_or_report(root: PathBuf) -> bool {
    match init(root) {
        Ok(()) => true,
        Err(error) => {
            log::error!("启动失败：{error}");
            rfd::MessageDialog::new()
                .set_title("YaZ")
                .set_level(rfd::MessageLevel::Error)
                .set_description(error.to_string())
                .show();
            false
        }
    }
}

/// 框架和第三方库的 info 日志很多，只记它们的警告和错误。
struct AppLogger(FileLogger);

impl log::Log for AppLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
            || (metadata.target().starts_with("yaz") || metadata.target().starts_with("zulip"))
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0.log(record);
        }
    }

    fn flush(&self) {
        self.0.flush();
    }
}

fn init_app_logging(paths: &AppPaths) {
    let logger = AppLogger(FileLogger::new(paths.log_dir(), LevelFilter::Info));
    match log::set_boxed_logger(Box::new(logger)) {
        Ok(()) => log::set_max_level(LevelFilter::Info),
        Err(error) => eprintln!("初始化日志失败：{error}"),
    }
}

fn init(root: PathBuf) -> Result<(), InitError> {
    std::fs::create_dir_all(&root).map_err(|e| InitError::CreateRoot(root.clone(), e))?;
    let paths = AppPaths::new(root);
    init_app_logging(&paths);
    log::info!("YaZ {} 启动", env!("CARGO_PKG_VERSION"));
    let db = Db::open(&paths.database())?;
    let settings = LocalSettings::new(&db);
    let client_config = ClientConfig::global();
    client_config.set_client_name(&settings.client_name());
    let hub = Hub::new(db.clone(), paths.clone(), Some(client_config));
    let services = Services {
        db,
        paths,
        settings,
        hub,
    };
    if SERVICES.set(services).is_err() {
        log::warn!("重复初始化服务");
    }
    Ok(())
}

/// 主题：调色盘和字号在本机保存，深浅色跟随账号的 Zulip 设置。
pub fn create_theme() -> ThemeHub {
    let hub = ThemeHub::new(ThemeConfig::default(), |config| {
        let Some(services) = try_services() else {
            return;
        };
        let palette = serde_json::to_string(&config.palette).unwrap_or_default();
        services.settings.set_palette(&palette);
        services.settings.set_font_size(config.font_size as i32);
    });
    load_theme(&hub);
    hub
}

/// 读取已保存的调色盘和字号。数据目录搬迁完成后也要调用一次。
pub fn load_theme(hub: &ThemeHub) {
    let Some(services) = try_services() else {
        return;
    };
    let palette = services
        .settings
        .palette()
        .and_then(|json| serde_json::from_str::<Palette>(&json).ok())
        .unwrap_or_default();
    hub.load(palette, services.settings.font_size().max(0) as u32);
}

/// Zulip 设置 `color_scheme` 对应的方案；没有账号时跟随系统。
pub fn scheme_of(session: Option<&Session>) -> ColorScheme {
    match session.map(|s| s.settings().color_scheme) {
        Some(2) => ColorScheme::Dark,
        Some(3) => ColorScheme::Light,
        _ => ColorScheme::System,
    }
}

/// 进入主界面阶段后才读取和保存主窗口位置。
pub fn enable_main_bounds() {
    MAIN_BOUNDS_ENABLED.store(true, Ordering::Relaxed);
}

fn main_bounds_blocked(key: &str) -> bool {
    key == MAIN_BOUNDS_KEY && !MAIN_BOUNDS_ENABLED.load(Ordering::Relaxed)
}

/// 窗口位置保存在本机设置中：主窗口的经典模式和默认模式分开记，其它窗口一个键一条。
pub struct SettingsBounds;

fn bounds_slot<'a>(settings: &LocalSettings, key: &'a str) -> &'a str {
    match key {
        MAIN_BOUNDS_KEY if settings.classic_mode() => keys::MAIN_BOUNDS_CLASSIC,
        MAIN_BOUNDS_KEY => keys::MAIN_BOUNDS_NORMAL,
        other => other,
    }
}

impl WindowBoundsStore for SettingsBounds {
    fn load(&self, key: &str) -> Option<WindowBounds> {
        if main_bounds_blocked(key) {
            return None;
        }
        let settings = &try_services()?.settings;
        WindowBounds::parse(&settings.window_bounds(bounds_slot(settings, key))?)
    }

    fn save(&self, key: &str, bounds: WindowBounds) {
        let Some(services) = try_services() else {
            return;
        };
        if main_bounds_blocked(key) {
            return;
        }
        let settings = &services.settings;
        settings.set_window_bounds(bounds_slot(settings, key), &bounds.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> EffectInputs {
        EffectInputs {
            client_name: String::new(),
            #[cfg(debug_assertions)]
            log_requests: false,
            #[cfg(debug_assertions)]
            request_log_limit: 1000,
            collapse_resolved: false,
            topic_title_format: "{Topic}".to_owned(),
        }
    }

    #[test]
    fn unchanged_values_have_no_effects() {
        assert_eq!(
            setting_effects(&inputs(), &inputs()),
            SettingEffects::default()
        );
    }

    #[test]
    fn client_name_change_only_updates_client_config() {
        let after = EffectInputs {
            client_name: "website".to_owned(),
            ..inputs()
        };
        assert_eq!(
            setting_effects(&inputs(), &after),
            SettingEffects {
                client_name: true,
                ..SettingEffects::default()
            }
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn request_log_settings_only_sync_request_log() {
        let expected = SettingEffects {
            request_log: true,
            ..SettingEffects::default()
        };
        let logging = EffectInputs {
            log_requests: true,
            ..inputs()
        };
        assert_eq!(setting_effects(&inputs(), &logging), expected);
        let limit = EffectInputs {
            request_log_limit: 100,
            ..inputs()
        };
        assert_eq!(setting_effects(&inputs(), &limit), expected);
    }

    #[test]
    fn collapse_and_title_format_refresh_conversations() {
        let expected = SettingEffects {
            conversations: true,
            ..SettingEffects::default()
        };
        let collapse = EffectInputs {
            collapse_resolved: true,
            ..inputs()
        };
        assert_eq!(setting_effects(&inputs(), &collapse), expected);
        let format = EffectInputs {
            topic_title_format: "{Channel} / {Topic}".to_owned(),
            ..inputs()
        };
        assert_eq!(setting_effects(&inputs(), &format), expected);
    }
}
