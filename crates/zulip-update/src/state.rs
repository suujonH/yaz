use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::version::parse_version;

/// After a version's package verification and replacement have failed this many times in total,
/// auto-update stops trying it. Network failures do not count.
pub const MAX_FAILURES: u32 = 3;

/// Update results and failure records, to keep retrying at every start after a failed update. Only
/// the most recently attempted version is recorded; once a higher version is released, the record
/// naturally becomes stale.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateState {
    /// The version the failure record belongs to.
    pub version: Option<String>,
    pub failures: u32,
    /// The new version cannot start after replacement: retrying will not succeed either, so
    /// auto-update stops trying after the first failure.
    pub blocked: bool,
    pub last_error: Option<String>,
    /// Result of the updater's last run; the main app writes it to the log at its next start and
    /// then clears it.
    pub pending_report: Option<String>,
}

impl UpdateState {
    /// Whether auto-update may try `version`. A manual check by the user is not subject to this.
    pub fn allows_auto_attempt(&self, version: &str) -> bool {
        !self.is_same_version(version) || (!self.blocked && self.failures < MAX_FAILURES)
    }

    /// Records one failure; when `block` is true, auto-update no longer tries that version.
    #[must_use]
    pub fn with_failure(self, version: &str, error: &str, block: bool) -> Self {
        let same = self.is_same_version(version);
        Self {
            version: Some(version.to_string()),
            failures: if same { self.failures } else { 0 } + 1,
            blocked: block || (same && self.blocked),
            last_error: Some(error.to_string()),
            pending_report: self.pending_report,
        }
    }

    /// Update succeeded or the user manually retried: clears that version's failure record.
    #[must_use]
    pub fn cleared(self) -> Self {
        Self {
            pending_report: self.pending_report,
            ..Self::default()
        }
    }

    /// A missing or corrupt file is treated as having no record.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, serde_json::to_string_pretty(self)?)?;
        fs::rename(temp, path)
    }

    fn is_same_version(&self, version: &str) -> bool {
        match (
            parse_version(version),
            self.version.as_deref().and_then(parse_version),
        ) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn failures_below_limit_still_allow_auto_attempt() {
        let mut state = UpdateState::default();
        for _ in 1..MAX_FAILURES {
            state = state.with_failure("1.0.0", "x", false);
            assert!(state.allows_auto_attempt("1.0.0"));
        }
        state = state.with_failure("1.0.0", "x", false);
        assert!(!state.allows_auto_attempt("1.0.0"));
    }

    #[test]
    fn blocked_version_is_skipped_after_first_failure() {
        let state = UpdateState::default().with_failure("1.0.0", "启动失败", true);
        assert!(!state.allows_auto_attempt("1.0.0"));
        assert!(!state.allows_auto_attempt("v1.0.0"));
    }

    #[test]
    fn higher_version_is_allowed_and_resets_the_count() {
        let state = UpdateState::default().with_failure("1.0.0", "x", true);
        assert!(state.allows_auto_attempt("1.0.1"));

        let state = state.with_failure("1.0.1", "y", false);
        assert_eq!(state.failures, 1);
        assert!(!state.blocked);
    }

    #[test]
    fn clear_allows_retry_and_keeps_pending_report() {
        let state = UpdateState {
            pending_report: Some("r".into()),
            ..UpdateState::default()
        }
        .with_failure("1.0.0", "x", true)
        .cleared();
        assert!(state.allows_auto_attempt("1.0.0"));
        assert_eq!(state.last_error, None);
        assert_eq!(state.pending_report.as_deref(), Some("r"));
    }

    #[test]
    fn persists_and_treats_corrupt_file_as_empty() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.json");
        assert_eq!(UpdateState::load(&path), UpdateState::default());

        let state = UpdateState::default().with_failure("1.0.0", "原因", true);
        state.save(&path).unwrap();
        assert_eq!(UpdateState::load(&path), state);

        fs::write(&path, "{ broken").unwrap();
        assert_eq!(UpdateState::load(&path), UpdateState::default());
    }
}
