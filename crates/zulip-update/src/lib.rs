//! 自动更新：版本比较、更新包校验、文件替换与回滚、更新器流程，以及主程序一侧的检查与下载。
#![deny(unsafe_code)]

mod applier;
#[cfg(feature = "service")]
mod archive;
mod error;
#[cfg(feature = "service")]
mod idle;
mod layout;
mod manifest;
mod package;
mod process;
#[cfg(feature = "service")]
mod service;
mod state;
mod swap;
#[cfg(test)]
mod testing;
mod version;

use std::sync::Arc;

/// 日志回调，可能在任意线程调用。
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

pub use applier::{ApplyResult, UpdateApplier, UpdateRequest};
pub use error::{Result, UpdateError};
#[cfg(feature = "service")]
pub use idle::user_idle_time;
pub use layout::{
    APP_EXE_NAME, BACKUP_DIR_NAME, FILE_LIST_NAME, PROBE_ARG, UPDATER_EXE_NAME, UpdateLayout,
};
pub use manifest::{MANIFEST_FILE_NAME, MAX_PACKAGE_SIZE, UpdateManifest};
pub use package::FileEntry;
pub use process::{ProcessHost, StartedApp, SystemProcessHost};
#[cfg(feature = "service")]
pub use service::{
    LogLevel, RELEASES_URL, UpdateCheckResult, UpdateCheckStatus, UpdateConfig, UpdateHost,
    UpdateService, UpdateStatus,
};
pub use state::{MAX_FAILURES, UpdateState};
pub use version::{Version, parse_version};
