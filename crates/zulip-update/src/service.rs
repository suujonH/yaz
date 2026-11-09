use std::ffi::OsStr;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::applier::remove_file_if_exists;
use crate::archive::{extract_safely, verify_archive};
use crate::error::{Result, UpdateError, invalid};
use crate::layout::{FILE_LIST_NAME, PROBE_ARG, UPDATER_EXE_NAME, UpdateLayout};
use crate::manifest::{MANIFEST_FILE_NAME, UpdateManifest};
use crate::package::verify_tree;
use crate::process::{ProcessHost, SystemProcessHost};
use crate::state::UpdateState;
use crate::swap::{FileSwap, delete_directory};
use crate::version::{Version, parse_version};

/// 本仓库的 Releases。`latest/download` 只指向最新的正式版（不含预发布版和草稿），也不受 GitHub API 的请求频率限制。
pub const RELEASES_URL: &str = "https://github.com/suujonH/yaz/releases/";

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MANIFEST_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCheckStatus {
    /// 已是最新版本。
    UpToDate,
    /// 新版本已下载、校验并解压好，等待替换。
    Ready,
    /// 有新版本，但这个程序不是发布包安装的（或目录不可写），不能自动更新。
    ManualOnly,
    /// 有新版本，但它之前更新失败过，自动更新不再尝试；手动检查时会重试。
    Skipped,
    /// GitHub 上还没有发布版本。
    NoRelease,
    /// 检查或下载失败。
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckResult {
    pub status: UpdateCheckStatus,
    pub message: String,
}

impl UpdateCheckResult {
    fn new(status: UpdateCheckStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self::new(UpdateCheckStatus::Failed, message)
    }
}

/// 检查的进度和结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateStatus {
    pub text: String,
    pub busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// 更新服务对程序的依赖，由 app 层实现。回调可能在后台线程触发。
pub trait UpdateHost: Send + Sync {
    /// 用户多久没有操作键盘鼠标（整个系统）。
    fn idle_time(&self) -> Duration;

    /// 新版本已准备好且用户空闲：请求程序退出。
    /// 程序应像普通退出一样结束会话，收尾时调用 `UpdateService::start_updater`，返回 false 则重启自己。
    fn exit_for_update(&self);

    fn log(&self, level: LogLevel, message: &str);

    fn status_changed(&self, _status: &UpdateStatus) {}
}

#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// 以 `/` 结尾的 Releases 地址。
    pub releases_url: String,
    pub layout: UpdateLayout,
    pub app_dir: PathBuf,
    pub current: Version,
    /// 更新器的日志文件。
    pub updater_log: PathBuf,
    /// 覆盖“能否自动更新”的检测；`None` 时按发布包的特征判断。
    pub self_update: Option<bool>,
    /// 启动后等这么久再自动检查，不和登录、加载抢资源。
    pub startup_delay: Duration,
    /// 用户这么久没有操作才重启更新，不打断正在进行的输入；一直没有空闲时，下次启动时应用。
    pub idle_threshold: Duration,
    pub idle_poll: Duration,
}

impl UpdateConfig {
    pub fn new(current: Version, app_dir: PathBuf, updater_log: PathBuf) -> Self {
        Self {
            releases_url: RELEASES_URL.to_string(),
            layout: UpdateLayout::new(UpdateLayout::default_root()),
            app_dir,
            current,
            updater_log,
            self_update: None,
            startup_delay: Duration::from_secs(5),
            idle_threshold: Duration::from_secs(60),
            idle_poll: Duration::from_secs(15),
        }
    }
}

/// 自动更新（主程序一侧）：从 GitHub Releases 检查最新版本，下载、校验并解压到更新工作目录，
/// 用户空闲时请求程序退出并启动独立更新器。替换和回滚由更新器完成（见 `UpdateApplier`）。
#[derive(Clone)]
pub struct UpdateService {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    config: UpdateConfig,
    host: Arc<dyn UpdateHost>,
    can_self_update: OnceLock<bool>,
    status: Mutex<UpdateStatus>,
    running: Mutex<Option<watch::Receiver<Option<UpdateCheckResult>>>>,
    waiting_for_idle: AtomicBool,
    cancel: CancellationToken,
}

struct ClearRunning(Arc<Inner>);

impl Drop for ClearRunning {
    fn drop(&mut self) {
        *self.0.running.lock() = None;
    }
}

impl UpdateService {
    pub fn new(config: UpdateConfig, host: Arc<dyn UpdateHost>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("YaZ-Updater")
            .build()?;
        Ok(Self::with_client(config, host, http))
    }

    /// 使用自定义的 HTTP 客户端。
    pub fn with_client(
        config: UpdateConfig,
        host: Arc<dyn UpdateHost>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                config,
                host,
                can_self_update: OnceLock::new(),
                status: Mutex::new(UpdateStatus::default()),
                running: Mutex::new(None),
                waiting_for_idle: AtomicBool::new(false),
                cancel: CancellationToken::new(),
            }),
        }
    }

    /// 这个程序能否自动更新：必须是发布包安装的（程序目录中有更新器和文件清单）且程序目录可写。
    /// 调试构建不自动更新。
    pub fn can_self_update(&self) -> bool {
        *self.inner.can_self_update.get_or_init(|| {
            self.config().self_update.unwrap_or_else(|| {
                let dir = &self.config().app_dir;
                !cfg!(debug_assertions)
                    && dir.join(UPDATER_EXE_NAME).is_file()
                    && dir.join(FILE_LIST_NAME).is_file()
                    && is_directory_writable(dir)
            })
        })
    }

    pub fn status(&self) -> UpdateStatus {
        self.inner.status.lock().clone()
    }

    /// 上次更新失败的原因；更新成功或用户手动重试被跳过的版本后为 `None`。
    pub fn last_error(&self) -> Option<String> {
        self.load_state().last_error
    }

    /// 程序入口的第一步（界面启动之前）：上次已经准备好新版本时启动更新器，返回 true，调用方应立即退出，
    /// 用户看不到旧版本的界面。作为更新器启动的新版本确认启动成功之前不做任何事。
    pub fn try_apply_staged(&self, args: &[String]) -> bool {
        if args.iter().any(|a| a == PROBE_ARG) || !self.can_self_update() {
            return false;
        }
        self.cleanup();
        let Some(manifest) = self.load_ready() else {
            return false;
        };
        let newer =
            parse_version(&manifest.version).is_some_and(|ready| ready > self.config().current);
        // 已经不需要了：不比当前版本新，或失败过不再尝试
        if !newer || !self.load_state().allows_auto_attempt(&manifest.version) {
            self.clear_staged();
            return false;
        }
        if self.another_instance_running() {
            self.log(LogLevel::Info, "其它实例正在运行，本次启动不应用更新");
            return false;
        }
        self.start_updater()
    }

    /// 启动独立更新器（复制到更新工作目录后运行，它会等本进程退出）。返回是否启动成功；
    /// 失败时本进程应继续运行或重启自己。
    pub fn start_updater(&self) -> bool {
        match self.try_start_updater() {
            Ok(version) => {
                self.log(
                    LogLevel::Info,
                    &format!("已启动更新器，准备更新到 {version}"),
                );
                true
            }
            Err(error) => {
                self.log(LogLevel::Error, &format!("启动更新器失败：{error}"));
                false
            }
        }
    }

    /// 窗口完成首次布局后调用：作为更新器启动的新版本报告启动成功；
    /// 否则把更新器的结果写入日志，清理上次更新的残留。
    pub fn on_started(&self, args: &[String]) {
        if let Some(index) = args.iter().position(|a| a == PROBE_ARG)
            && let Some(path) = args.get(index + 1)
        {
            if let Err(error) = fs::write(path, self.config().current.to_string()) {
                self.log(LogLevel::Error, &format!("无法报告启动成功：{error}"));
            }
            return;
        }
        self.cleanup();
        let mut state = self.load_state();
        if let Some(report) = state.pending_report.take() {
            self.log(LogLevel::Info, &format!("自动更新：{report}"));
            if let Err(error) = state.save(&self.config().layout.state_path()) {
                self.log(LogLevel::Warn, &format!("无法保存更新状态：{error}"));
            }
        }
    }

    /// 启动后稍等再自动检查一次。刚被更新器启动的新版本（带 `PROBE_ARG`）由调用方跳过。
    pub async fn check_after_startup(&self) {
        tokio::select! {
            () = self.inner.cancel.cancelled() => {}
            () = sleep(self.config().startup_delay) => {
                self.check(false).await;
            }
        }
    }

    /// 检查最新版本；有新版本且可以自动更新时下载、校验并解压，准备好后等用户空闲再请求退出。
    /// 同时只运行一次，运行中再次调用得到同一个结果。
    /// `manual` 为 true 时（用户点击“确认版本”）重试之前失败过的版本。
    pub async fn check(&self, manual: bool) -> UpdateCheckResult {
        let mut receiver = {
            let mut running = self.inner.running.lock();
            match running.as_ref() {
                Some(receiver) => receiver.clone(),
                None => {
                    let (sender, receiver) = watch::channel(None);
                    *running = Some(receiver.clone());
                    let service = self.clone();
                    tokio::spawn(async move {
                        let clear = ClearRunning(service.inner.clone());
                        let result = service.run_check(manual).await;
                        drop(clear);
                        let _ = sender.send(Some(result));
                    });
                    receiver
                }
            }
        };
        let result = receiver
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|value| value.clone());
        result.unwrap_or_else(|| UpdateCheckResult::failed("检查更新失败，请检查网络后重试"))
    }

    /// 取消正在进行的检查和等待空闲；程序退出时调用。
    pub fn shutdown(&self) {
        self.inner.cancel.cancel();
    }

    fn config(&self) -> &UpdateConfig {
        &self.inner.config
    }

    fn log(&self, level: LogLevel, message: &str) {
        self.inner.host.log(level, message);
    }

    fn set_status(&self, text: impl Into<String>, busy: bool) {
        let status = UpdateStatus {
            text: text.into(),
            busy,
        };
        *self.inner.status.lock() = status.clone();
        self.inner.host.status_changed(&status);
    }

    fn load_state(&self) -> UpdateState {
        UpdateState::load(&self.config().layout.state_path())
    }

    fn load_ready(&self) -> Option<UpdateManifest> {
        let json = fs::read_to_string(self.config().layout.ready_path()).ok()?;
        UpdateManifest::parse(&json)
    }

    fn another_instance_running(&self) -> bool {
        SystemProcessHost.is_app_running(&self.config().app_dir, std::process::id())
    }

    async fn run_check(&self, manual: bool) -> UpdateCheckResult {
        self.set_status("正在检查更新…", true);
        let outcome = tokio::select! {
            () = self.inner.cancel.cancelled() => Ok(UpdateCheckResult::failed("已取消")),
            outcome = self.check_core(manual) => outcome,
        };
        let result = outcome.unwrap_or_else(|error| {
            self.log(LogLevel::Warn, &format!("检查更新失败：{error}"));
            UpdateCheckResult::failed("检查更新失败，请检查网络后重试")
        });
        self.log(
            LogLevel::Info,
            &format!("检查更新：{:?}，{}", result.status, result.message),
        );
        self.set_status(result.message.clone(), false);
        if result.status == UpdateCheckStatus::Ready {
            self.spawn_idle_watch();
        }
        result
    }

    async fn check_core(&self, manual: bool) -> Result<UpdateCheckResult> {
        let config = self.config();
        let Some(manifest) = self.fetch_manifest().await? else {
            return Ok(UpdateCheckResult::new(
                UpdateCheckStatus::NoRelease,
                "GitHub 上还没有发布版本",
            ));
        };
        let latest = parse_version(&manifest.version).ok_or(UpdateError::ManifestInvalid)?;
        if latest <= config.current {
            return Ok(UpdateCheckResult::new(
                UpdateCheckStatus::UpToDate,
                format!("已是最新版本（{}）", config.current),
            ));
        }
        if !self.can_self_update() {
            return Ok(UpdateCheckResult::new(
                UpdateCheckStatus::ManualOnly,
                format!(
                    "发现新版本 {}。当前程序不支持自动更新（不是发布包安装或程序目录不可写），请到 GitHub Releases 下载",
                    manifest.version
                ),
            ));
        }

        let state = self.load_state();
        if !state.allows_auto_attempt(&manifest.version) {
            if !manual {
                return Ok(UpdateCheckResult::new(
                    UpdateCheckStatus::Skipped,
                    format!(
                        "新版本 {} 更新失败过，不再自动尝试，点击“确认版本”可以重试",
                        manifest.version
                    ),
                ));
            }
            state.cleared().save(&config.layout.state_path())?;
        }

        if self.is_ready(&manifest) {
            return Ok(ready_result(&manifest));
        }

        let prepared = self.prepare(&manifest).await;
        self.try_delete_directory(&config.layout.work_dir());
        match prepared {
            Ok(()) => Ok(ready_result(&manifest)),
            Err(UpdateError::PackageInvalid(message)) => {
                self.log(
                    LogLevel::Warn,
                    &format!("更新包校验失败：{}，{message}", manifest.version),
                );
                self.load_state()
                    .with_failure(&manifest.version, &message, false)
                    .save(&config.layout.state_path())?;
                Ok(UpdateCheckResult::failed(format!(
                    "新版本 {} 校验失败：{message}",
                    manifest.version
                )))
            }
            Err(error) => Err(error),
        }
    }

    // 没有发布版本（404）时返回 None
    async fn fetch_manifest(&self) -> Result<Option<UpdateManifest>> {
        let url = self.release_url(&format!("latest/download/{MANIFEST_FILE_NAME}"));
        let fetch = async {
            let response = self.inner.http.get(url).send().await?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            let mut response = response.error_for_status()?;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_MANIFEST_SIZE {
                    return Err(UpdateError::ManifestInvalid);
                }
            }
            let json = String::from_utf8_lossy(&body);
            UpdateManifest::parse(&json)
                .map(Some)
                .ok_or(UpdateError::ManifestInvalid)
        };
        timeout(CHECK_TIMEOUT, fetch)
            .await
            .map_err(|_| UpdateError::Timeout)?
    }

    // 下载 → 校验 → 解压到临时目录 → 校验 → 移到 staged → 写入 ready.json；任何一步失败都不会留下“已准备好”的状态
    async fn prepare(&self, manifest: &UpdateManifest) -> Result<()> {
        let layout = self.config().layout.clone();
        self.clear_staged();
        fs::create_dir_all(layout.work_dir())?;
        let zip = layout.work_dir().join("package.zip");
        self.download(manifest, &zip).await?;

        let manifest = manifest.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            verify_archive(&zip, &manifest)?;
            let extracted = layout.work_dir().join("package");
            extract_safely(&zip, &extracted)?;
            verify_tree(&extracted)?;
            fs::rename(&extracted, layout.staged_dir())?;
            fs::write(layout.ready_path(), manifest.to_json())?;
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }

    async fn download(&self, manifest: &UpdateManifest, path: &Path) -> Result<()> {
        let url = self.release_url(&format!(
            "download/v{}/{}",
            manifest.version, manifest.package
        ));
        let mut response = stall_limited(self.inner.http.get(url).send())
            .await?
            .error_for_status()?;
        let mut file = tokio::fs::File::create(path).await?;
        let mut done = 0u64;
        let mut last_percent = None;
        // 连续一段时间读不到数据才算超时：大文件在慢速网络上可以下载很久
        while let Some(chunk) = stall_limited(response.chunk()).await? {
            done += chunk.len() as u64;
            // 比清单声明的大就不用继续了
            if done > manifest.size {
                return Err(invalid(format!("更新包大小不符：应为 {}", manifest.size)));
            }
            file.write_all(&chunk).await?;
            let percent = done * 100 / manifest.size;
            if last_percent != Some(percent) {
                last_percent = Some(percent);
                self.set_status(
                    format!("正在下载新版本 {}（{percent}%）", manifest.version),
                    true,
                );
            }
        }
        file.flush().await?;
        Ok(())
    }

    fn release_url(&self, path: &str) -> String {
        format!(
            "{}/{path}",
            self.config().releases_url.trim_end_matches('/')
        )
    }

    fn is_ready(&self, manifest: &UpdateManifest) -> bool {
        self.config().layout.staged_dir().is_dir()
            && self
                .load_ready()
                .is_some_and(|ready| ready.sha256.eq_ignore_ascii_case(&manifest.sha256))
    }

    fn clear_staged(&self) {
        let layout = &self.config().layout;
        if let Err(error) = remove_file_if_exists(&layout.ready_path()) {
            self.log(LogLevel::Warn, &format!("清理更新文件失败：{error}"));
        }
        self.try_delete_directory(&layout.staged_dir());
        self.try_delete_directory(&layout.work_dir());
    }

    // 上次更新的残留：更新器副本（它运行时不能删除自己），更新器被中断时留下的备份。
    // 还有其它实例在运行时不清理：可能是更新器正在等它确认启动，备份还要用于回滚
    fn cleanup(&self) {
        if !self.can_self_update() || self.another_instance_running() {
            return;
        }
        self.try_delete_directory(&self.config().layout.runner_dir());
        if let Err(error) = FileSwap::remove_leftover_backup(&self.config().app_dir) {
            self.log(LogLevel::Warn, &format!("清理更新备份失败：{error}"));
        }
    }

    fn try_delete_directory(&self, dir: &Path) {
        if let Err(error) = delete_directory(dir) {
            self.log(
                LogLevel::Warn,
                &format!("清理更新文件失败：{}：{error}", dir.display()),
            );
        }
    }

    fn try_start_updater(&self) -> Result<String> {
        let config = self.config();
        let manifest = self
            .load_ready()
            .ok_or_else(|| invalid("没有准备好的新版本"))?;
        let runner_dir = config.layout.runner_dir();
        fs::create_dir_all(&runner_dir)?;
        let updater = runner_dir.join(UPDATER_EXE_NAME);
        fs::copy(config.app_dir.join(UPDATER_EXE_NAME), &updater)?;

        let pid = std::process::id().to_string();
        let current = config.current.to_string();
        let args: [&OsStr; 12] = [
            "--pid".as_ref(),
            pid.as_ref(),
            "--app-dir".as_ref(),
            config.app_dir.components().as_path().as_os_str(),
            "--update-root".as_ref(),
            config.layout.root().as_os_str(),
            "--from".as_ref(),
            current.as_ref(),
            "--to".as_ref(),
            manifest.version.as_ref(),
            "--log".as_ref(),
            config.updater_log.as_os_str(),
        ];
        SystemProcessHost.start(&updater, &args)?;
        Ok(manifest.version)
    }

    fn spawn_idle_watch(&self) {
        if self.inner.waiting_for_idle.swap(true, Ordering::SeqCst) {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let config = service.config();
            while service.inner.host.idle_time() < config.idle_threshold {
                tokio::select! {
                    () = service.inner.cancel.cancelled() => return,
                    () = sleep(config.idle_poll) => {}
                }
            }
            if !service.inner.cancel.is_cancelled() {
                service.inner.host.exit_for_update();
            }
        });
    }
}

fn ready_result(manifest: &UpdateManifest) -> UpdateCheckResult {
    UpdateCheckResult::new(
        UpdateCheckStatus::Ready,
        format!("新版本 {} 已下载，空闲时自动重启完成更新", manifest.version),
    )
}

async fn stall_limited<T>(future: impl Future<Output = reqwest::Result<T>>) -> Result<T> {
    timeout(STALL_TIMEOUT, future)
        .await
        .map_err(|_| UpdateError::Timeout)?
        .map_err(UpdateError::from)
}

fn is_directory_writable(dir: &Path) -> bool {
    let probe = dir.join(".update-write-test");
    fs::write(&probe, "").is_ok() && fs::remove_file(probe).is_ok()
}

#[cfg(test)]
mod tests;
