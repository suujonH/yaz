use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::Logger;
use crate::error::Result;
use crate::layout::{BACKUP_DIR_NAME, FILE_LIST_NAME};
use crate::package::{FileEntry, verify_files};

/// Replaces files in the app directory: old files are first moved to the backup directory, then the
/// new files are copied. Only files listed in the old and new versions' manifests are touched, and
/// other files in the app directory (the user's own files, plugins installed by themselves) are
/// unaffected. `commit` deletes the backup (removing the old version) and `rollback` restores the
/// original state.
pub struct FileSwap {
    app_dir: PathBuf,
    backup_dir: PathBuf,
    log: Logger,
    moved: Vec<String>,
    installed: Vec<String>,
    /// Files may be briefly held by antivirus software or a process that just exited; when an
    /// operation fails, retry at this interval.
    pub retry_delay: Duration,
    pub retry_count: u32,
}

impl FileSwap {
    pub fn new(app_dir: &Path, log: Logger) -> Self {
        Self {
            app_dir: app_dir.to_path_buf(),
            backup_dir: app_dir.join(BACKUP_DIR_NAME),
            log,
            moved: Vec::new(),
            installed: Vec::new(),
            retry_delay: Duration::from_millis(300),
            retry_count: 10,
        }
    }

    /// Performs the replacement. On failure the caller should call `rollback`.
    pub fn apply(&mut self, old: &[FileEntry], staged_dir: &Path, new: &[FileEntry]) -> Result<()> {
        delete_directory(&self.backup_dir)?;
        let new_paths: Vec<&str> = new
            .iter()
            .map(|e| e.path.as_str())
            .chain([FILE_LIST_NAME])
            .collect();
        // If a file of the new version has the same name as an existing file in the directory, it
        // must be backed up first so it can be rolled back
        let mut seen = HashSet::new();
        let existing = old
            .iter()
            .map(|e| e.path.as_str())
            .chain([FILE_LIST_NAME])
            .chain(new_paths.iter().copied())
            .filter(|path| seen.insert(path.to_ascii_lowercase()));
        for path in existing {
            let source = self.app_dir.join(path);
            if !source.is_file() {
                continue;
            }
            let target = self.backup_dir.join(path);
            create_parent(&target)?;
            self.retry(|| fs::rename(&source, &target))?;
            self.moved.push(path.to_string());
        }
        for path in new_paths {
            let target = self.app_dir.join(path);
            create_parent(&target)?;
            // Record first, then copy: files left over from a copy that fails halfway must also be
            // cleaned up
            self.installed.push(path.to_string());
            self.retry(|| fs::copy(staged_dir.join(path), &target).map(drop))?;
        }
        verify_files(&self.app_dir, new)
    }

    /// Restores the files from before the replacement. Returns whether all were restored; if some
    /// files fail, the rest are still restored and the backup directory is kept.
    pub fn rollback(&self) -> bool {
        let mut ok = true;
        for path in &self.installed {
            let target = self.app_dir.join(path);
            if target.is_file()
                && let Err(error) = self.retry(|| fs::remove_file(&target))
            {
                ok = false;
                self.log(&format!("回滚时无法删除新文件 {path}：{error}"));
            }
        }
        for path in &self.moved {
            let backup = self.backup_dir.join(path);
            let target = self.app_dir.join(path);
            if let Err(error) = self.retry(|| fs::rename(&backup, &target)) {
                ok = false;
                self.log(&format!("回滚时无法恢复旧文件 {path}：{error}"));
            }
        }
        self.prune_empty_directories(&self.installed);
        if ok {
            self.try_delete_directory(&self.backup_dir);
        }
        ok
    }

    /// Update succeeded: deletes the backup and the directories that became empty after the old
    /// version's files were moved away. If deletion fails, only logs it and cleans up at the next
    /// start.
    pub fn commit(&self) {
        self.try_delete_directory(&self.backup_dir);
        self.prune_empty_directories(&self.moved);
    }

    /// Deletes leftover backups in the app directory (possibly left when the updater was
    /// interrupted).
    #[cfg(feature = "service")]
    pub fn remove_leftover_backup(app_dir: &Path) -> io::Result<()> {
        delete_directory(&app_dir.join(BACKUP_DIR_NAME))
    }

    fn prune_empty_directories(&self, paths: &[String]) {
        for path in paths {
            let mut dir = Path::new(path).parent();
            while let Some(relative) = dir.filter(|d| !d.as_os_str().is_empty()) {
                let full = self.app_dir.join(relative);
                let is_empty = fs::read_dir(&full).map(|mut entries| entries.next().is_none());
                if !matches!(is_empty, Ok(true)) || fs::remove_dir(&full).is_err() {
                    break;
                }
                dir = relative.parent();
            }
        }
    }

    fn try_delete_directory(&self, dir: &Path) {
        if let Err(error) = delete_directory(dir) {
            self.log(&format!("无法删除 {}：{error}", dir.display()));
        }
    }

    fn retry<T>(&self, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
        let mut attempt = 1;
        loop {
            match operation() {
                Err(_) if attempt < self.retry_count => {
                    attempt += 1;
                    thread::sleep(self.retry_delay);
                }
                result => return result,
            }
        }
    }

    fn log(&self, message: &str) {
        (self.log)(message);
    }
}

pub(crate) fn delete_directory(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

fn create_parent(path: &Path) -> io::Result<()> {
    path.parent().map_or(Ok(()), fs::create_dir_all)
}

#[cfg(test)]
mod tests;
