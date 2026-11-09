use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::Logger;
use crate::error::{Result, invalid};
use crate::layout::{APP_EXE_NAME, FILE_LIST_NAME, PROBE_ARG, UpdateLayout};
use crate::manifest::UpdateManifest;
use crate::package::{FileEntry, verify_tree};
use crate::process::{ProcessHost, StartedApp};
use crate::state::UpdateState;
use crate::swap::{FileSwap, delete_directory};
use crate::version::parse_version;

/// Parameters of one update, passed from the main app to the updater on the command line.
#[derive(Debug, Clone)]
pub struct UpdateRequest {
    pub process_id: u32,
    pub app_dir: PathBuf,
    pub update_root: PathBuf,
    pub from_version: String,
    pub to_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyResult {
    /// The new version has started and been confirmed working, and the old version has been
    /// removed.
    Updated,
    /// The update failed; the old version was restored and started (or an attempt was made to start
    /// it).
    RolledBack,
    /// Another instance is running from the app directory; nothing was replaced or started, and it
    /// will be retried at the next start.
    Skipped,
}

/// Main flow of the updater: wait for the old app to exit -> verify the new version -> replace
/// files -> start the new version and wait for it to confirm a successful start -> remove the old
/// version. If any step fails, the old files are restored, the failure is recorded, and the old app
/// is started.
pub struct UpdateApplier<'a> {
    request: UpdateRequest,
    host: &'a dyn ProcessHost,
    log: Logger,
    layout: UpdateLayout,
    pub exit_timeout: Duration,
    /// Longest wait for the new version to confirm a successful start.
    pub probe_timeout: Duration,
    /// After the new version confirms it started, it must keep running this long to count as a
    /// successful start.
    pub stable_time: Duration,
    pub poll_interval: Duration,
    pub file_retry_delay: Duration,
}

impl<'a> UpdateApplier<'a> {
    pub fn new(request: UpdateRequest, host: &'a dyn ProcessHost, log: Logger) -> Self {
        Self {
            layout: UpdateLayout::new(&request.update_root),
            request,
            host,
            log,
            exit_timeout: Duration::from_secs(60),
            probe_timeout: Duration::from_secs(90),
            stable_time: Duration::from_secs(3),
            poll_interval: Duration::from_millis(200),
            file_retry_delay: Duration::from_millis(300),
        }
    }

    pub fn run(&self) -> ApplyResult {
        let request = &self.request;
        self.log(&format!(
            "开始更新：{} → {}",
            request.from_version, request.to_version
        ));
        self.host
            .wait_for_exit(request.process_id, self.exit_timeout);
        if self
            .host
            .is_app_running(&request.app_dir, request.process_id)
        {
            self.log("程序目录中还有其它实例在运行，本次不更新");
            return ApplyResult::Skipped;
        }

        let (old_entries, new_entries) = match self.prepare() {
            Ok(entries) => entries,
            Err(error) => return self.fail(&format!("校验新版本失败：{error}"), false),
        };

        let mut swap = FileSwap::new(&request.app_dir, self.log.clone());
        swap.retry_delay = self.file_retry_delay;
        if let Err(error) = swap.apply(&old_entries, &self.layout.staged_dir(), &new_entries) {
            self.roll_back(&swap);
            return self.fail(&format!("替换文件失败：{error}"), false);
        }

        if let Some(error) = self.start_new_version() {
            self.roll_back(&swap);
            return self.fail(&error, true);
        }

        swap.commit();
        self.clean_staged();
        self.report(
            UpdateState::load(&self.layout.state_path()).cleared(),
            &format!(
                "已从 {} 更新到 {}",
                request.from_version, request.to_version
            ),
        );
        ApplyResult::Updated
    }

    fn prepare(&self) -> Result<(Vec<FileEntry>, Vec<FileEntry>)> {
        let ready = fs::read_to_string(self.layout.ready_path())
            .ok()
            .and_then(|json| UpdateManifest::parse(&json))
            .and_then(|manifest| parse_version(&manifest.version));
        if ready.is_none() || ready != parse_version(&self.request.to_version) {
            return Err(invalid("没有准备好的新版本"));
        }
        let old_list = self.request.app_dir.join(FILE_LIST_NAME);
        if !old_list.is_file() {
            return Err(invalid(format!("程序目录中没有文件清单 {FILE_LIST_NAME}")));
        }
        let old_entries = FileEntry::parse_list(&fs::read_to_string(old_list)?)?;
        Ok((old_entries, verify_tree(&self.layout.staged_dir())?))
    }

    // Start the new version and wait for it to write the start marker with the version number;
    // returns the failure reason, or None on success
    fn start_new_version(&self) -> Option<String> {
        let exe = self.request.app_dir.join(APP_EXE_NAME);
        let probe = self.layout.probe_path();
        if let Err(error) = remove_file_if_exists(&probe) {
            return Some(format!("无法启动新版本：{error}"));
        }
        let mut app = match self
            .host
            .start(&exe, &[OsStr::new(PROBE_ARG), probe.as_os_str()])
        {
            Ok(app) => app,
            Err(error) => return Some(format!("无法启动新版本：{error}")),
        };
        let error = self.wait_for_probe(app.as_mut());
        if error.is_some() {
            self.kill_quietly(app.as_mut());
        }
        error
    }

    fn wait_for_probe(&self, app: &mut dyn StartedApp) -> Option<String> {
        let start = Instant::now();
        while start.elapsed() < self.probe_timeout {
            if let Some(marker) = self.read_probe() {
                // If the app's actual version does not match the manifest, it must roll back;
                // otherwise the new app would think it is still the old version and keep updating
                let expected = parse_version(&self.request.to_version);
                if expected.is_none() || parse_version(&marker) != expected {
                    return Some(format!(
                        "新版本报告的版本号 {marker} 与 {} 不符",
                        self.request.to_version
                    ));
                }
                return self.wait_stable(app);
            }
            if app.has_exited() {
                return Some("新版本启动后退出了".to_string());
            }
            thread::sleep(self.poll_interval);
        }
        Some(format!(
            "新版本在 {} 秒内没有确认启动成功",
            self.probe_timeout.as_secs()
        ))
    }

    fn wait_stable(&self, app: &mut dyn StartedApp) -> Option<String> {
        let start = Instant::now();
        while start.elapsed() < self.stable_time {
            if app.has_exited() {
                break;
            }
            thread::sleep(self.poll_interval);
        }
        app.has_exited()
            .then(|| "新版本确认启动后很快退出了".to_string())
    }

    // The content may be empty while the write is not finished; read again next round
    fn read_probe(&self) -> Option<String> {
        let text = fs::read_to_string(self.layout.probe_path()).ok()?;
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn kill_quietly(&self, app: &mut dyn StartedApp) {
        if !app.has_exited()
            && let Err(error) = app.kill()
        {
            self.log(&format!("无法结束新版本：{error}"));
        }
    }

    fn roll_back(&self, swap: &FileSwap) {
        self.log("正在恢复旧版本文件");
        if !swap.rollback() {
            self.log(&format!(
                "部分旧文件没能恢复，备份保留在程序目录的 {}",
                crate::layout::BACKUP_DIR_NAME
            ));
        }
    }

    fn fail(&self, reason: &str, block: bool) -> ApplyResult {
        let request = &self.request;
        self.log(&format!("更新失败：{reason}"));
        // Record the failure before starting the old app: when the old app starts, it uses this to
        // stop retrying automatically
        self.clean_staged();
        let state = UpdateState::load(&self.layout.state_path()).with_failure(
            &request.to_version,
            reason,
            block,
        );
        self.report(
            state,
            &format!(
                "更新到 {} 失败，已恢复 {}：{reason}",
                request.to_version, request.from_version
            ),
        );
        match self.host.start(&request.app_dir.join(APP_EXE_NAME), &[]) {
            Ok(_) => self.log(&format!("已启动旧版本 {}", request.from_version)),
            Err(error) => self.log(&format!("无法启动旧版本：{error}")),
        }
        ApplyResult::RolledBack
    }

    fn report(&self, mut state: UpdateState, message: &str) {
        self.log(message);
        state.pending_report = Some(message.to_string());
        if let Err(error) = state.save(&self.layout.state_path()) {
            self.log(&format!("无法保存更新状态：{error}"));
        }
    }

    fn clean_staged(&self) {
        for file in [self.layout.ready_path(), self.layout.probe_path()] {
            if let Err(error) = remove_file_if_exists(&file) {
                self.log(&format!("无法删除 {}：{error}", file.display()));
            }
        }
        for dir in [self.layout.staged_dir(), self.layout.work_dir()] {
            if let Err(error) = delete_directory(&dir) {
                self.log(&format!("无法删除 {}：{error}", dir.display()));
            }
        }
    }

    fn log(&self, message: &str) {
        (self.log)(message);
    }
}

pub(crate) fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
