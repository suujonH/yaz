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

/// 替换程序目录中的文件：旧文件先移到备份目录，再复制新文件。只动旧版本和新版本清单中列出的文件，
/// 程序目录中的其它文件（用户自己的文件、自行安装的插件）不受影响。
/// `commit` 删除备份（移除旧版本），`rollback` 恢复原样。
pub struct FileSwap {
    app_dir: PathBuf,
    backup_dir: PathBuf,
    log: Logger,
    moved: Vec<String>,
    installed: Vec<String>,
    /// 文件可能被杀毒软件或刚退出的进程短暂占用，操作失败时按此间隔重试。
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

    /// 执行替换。失败时调用方应调用 `rollback`。
    pub fn apply(&mut self, old: &[FileEntry], staged_dir: &Path, new: &[FileEntry]) -> Result<()> {
        delete_directory(&self.backup_dir)?;
        let new_paths: Vec<&str> = new
            .iter()
            .map(|e| e.path.as_str())
            .chain([FILE_LIST_NAME])
            .collect();
        // 新版本的文件如果和目录中已有的文件同名，也要先备份才能回滚
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
            // 先记录再复制：复制到一半失败时残留的文件也要清除
            self.installed.push(path.to_string());
            self.retry(|| fs::copy(staged_dir.join(path), &target).map(drop))?;
        }
        verify_files(&self.app_dir, new)
    }

    /// 恢复替换前的文件。返回是否全部恢复；个别文件失败时继续恢复其余文件，备份目录保留。
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

    /// 更新成功：删除备份，以及旧版本文件移走后变空的目录。删不掉时只记日志，下次启动再清理。
    pub fn commit(&self) {
        self.try_delete_directory(&self.backup_dir);
        self.prune_empty_directories(&self.moved);
    }

    /// 删除程序目录中残留的备份（更新器被中断时可能留下）。
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
