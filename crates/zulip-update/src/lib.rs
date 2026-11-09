//! Auto-update: version comparison, update package verification, file replacement and rollback, the
//! updater flow, and the check and download on the main app side.
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

/// Log callback; may be called on any thread.
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
