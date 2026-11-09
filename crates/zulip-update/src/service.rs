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

/// This repository's Releases. `latest/download` points only to the latest stable release
/// (excluding pre-releases and drafts) and is not subject to the GitHub API's rate limit.
pub const RELEASES_URL: &str = "https://github.com/suujonH/yaz/releases/";

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MANIFEST_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCheckStatus {
    /// Already the latest version.
    UpToDate,
    /// The new version has been downloaded, verified and extracted, awaiting replacement.
    Ready,
    /// There is a new version, but this app was not installed from a release package (or the
    /// directory is not writable), so it cannot auto-update.
    ManualOnly,
    /// There is a new version, but it failed to update before, so auto-update no longer tries it; a
    /// manual check retries.
    Skipped,
    /// No release has been published on GitHub yet.
    NoRelease,
    /// Check or download failed.
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

/// Progress and result of the check.
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

/// What the update service depends on from the app, implemented by the app layer. Callbacks may
/// fire on a background thread.
pub trait UpdateHost: Send + Sync {
    /// How long since the user last used the keyboard or mouse (system-wide).
    fn idle_time(&self) -> Duration;

    /// The new version is ready and the user is idle: request the app to exit. The app should end
    /// its sessions like a normal exit, call `UpdateService::start_updater` during cleanup, and
    /// restart itself if it returns false.
    fn exit_for_update(&self);

    fn log(&self, level: LogLevel, message: &str);

    fn status_changed(&self, _status: &UpdateStatus) {}
}

#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// Releases URL ending with `/`.
    pub releases_url: String,
    pub layout: UpdateLayout,
    pub app_dir: PathBuf,
    pub current: Version,
    /// The updater's log file.
    pub updater_log: PathBuf,
    /// Overrides the "can auto-update" detection; when `None`, it is judged from the release
    /// package's characteristics.
    pub self_update: Option<bool>,
    /// Wait this long after startup before the automatic check, so it does not compete with login
    /// and loading for resources.
    pub startup_delay: Duration,
    /// Restart to update only after the user has been inactive this long, so typing in progress is
    /// not interrupted; if the user is never idle, it is applied at the next start.
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

/// Auto-update (main app side): checks the latest version from GitHub Releases, downloads, verifies
/// and extracts it to the update working directory, and requests the app to exit and starts the
/// standalone updater when the user is idle. Replacement and rollback are done by the updater (see
/// `UpdateApplier`).
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

    /// Uses a custom HTTP client.
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

    /// Whether this app can auto-update: it must be installed from a release package (the app
    /// directory has the updater and file manifest) and the app directory must be writable. Debug
    /// builds do not auto-update.
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

    /// Reason the last update failed; `None` after an update succeeds or the user manually retries
    /// a skipped version.
    pub fn last_error(&self) -> Option<String> {
        self.load_state().last_error
    }

    /// First step of the program entry (before the UI starts): if a new version was already
    /// prepared last time, starts the updater and returns true, and the caller should exit
    /// immediately so the user never sees the old version's UI. A new version started as the
    /// updater's target does nothing until it confirms a successful start.
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
        // No longer needed: not newer than the current version, or it failed before and is no
        // longer tried
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

    /// Starts the standalone updater (copied to the update working directory and then run; it waits
    /// for this process to exit). Returns whether it started successfully; on failure this process
    /// should keep running or restart itself.
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

    /// Called after the window finishes its first layout: a new version started by the updater
    /// reports a successful start; otherwise writes the updater's result to the log and cleans up
    /// leftovers from the last update.
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

    /// Waits a moment after startup and then runs one automatic check. A new version just started
    /// by the updater (with `PROBE_ARG`) is skipped by the caller.
    pub async fn check_after_startup(&self) {
        tokio::select! {
            () = self.inner.cancel.cancelled() => {}
            () = sleep(self.config().startup_delay) => {
                self.check(false).await;
            }
        }
    }

    /// Checks the latest version; if there is a new version and auto-update is possible, downloads,
    /// verifies and extracts it, then waits for the user to be idle before requesting exit. Runs
    /// only once at a time; calling again while running returns the same result. When `manual` is
    /// true (the user clicked "Confirm version"), retries a version that failed before.
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

    /// Cancels the running check and the wait for idle; called when the app exits.
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

    // Returns None when there is no release (404)
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

    // download -> verify -> extract to a temp directory -> verify -> move to staged -> write
    // ready.json; if any step fails, no "ready" state is left behind
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
        // Time out only when no data is read for a continuous period: a large file on a slow
        // network can take a long time to download
        while let Some(chunk) = stall_limited(response.chunk()).await? {
            done += chunk.len() as u64;
            // Larger than the manifest declares, so no need to continue
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

    // Leftovers from the last update: the updater copy (it cannot delete itself while running) and
    // the backup left when the updater was interrupted. Not cleaned while other instances are
    // running: the updater may be waiting for it to confirm startup, and the backup is still needed
    // for rollback
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
