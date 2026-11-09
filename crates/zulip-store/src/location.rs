//! 数据目录（设置中的“聊天信息默认保存到”）的位置与搬迁。
//!
//! 位置记录在默认数据目录的 `location.json` 中（数据库会随数据目录移动，不能记在数据库里）。
//! 更改位置时只记下目标（[`DataLocation::request_move`]）并重启程序；新进程在打开数据库之前搬迁
//! （[`DataLocation::perform_move`]），此时没有任何文件被占用。
//!
//! 搬迁先复制全部文件，再改写数据库中指向旧位置的路径，最后才切换位置并删除旧文件；
//! 复制、改写或切换失败时删除已复制的文件，仍使用旧位置。

use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::core::PCWSTR;

use crate::db::Db;
use crate::error::{Result, StoreError};
use crate::paths::AppPaths;

const CONFIG_FILE: &str = "location.json";

/// 数据目录不能搬到所选位置的原因。
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    #[error("已经是当前位置")]
    SameLocation,

    #[error("不能选择当前聊天信息文件夹之内或包含它的位置")]
    Nested,

    #[error("“{}”不是空文件夹", .0.display())]
    NotEmpty(PathBuf),

    #[error("无法写入该位置")]
    NotWritable,

    #[error("磁盘空间不足")]
    NoSpace,

    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Config {
    data_root: Option<PathBuf>,
    pending_move_to: Option<PathBuf>,
}

pub struct DataLocation {
    default_root: PathBuf,
}

/// [`DataLocation::resolve`] 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// 可以使用的数据目录。
    pub root: PathBuf,
    /// 配置的目录不可用时是配置的目录，此时 `root` 是默认目录。
    pub unavailable: Option<PathBuf>,
}

impl DataLocation {
    /// 选择的文件夹中创建的子目录名：数据目录总是 `所选文件夹\YaZ`。
    pub const FOLDER_NAME: &'static str = "YaZ";

    /// 默认位置 `%LOCALAPPDATA%\YaZ`。
    pub fn system() -> Self {
        let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
        Self::at(base.join(Self::FOLDER_NAME))
    }

    pub fn at(default_root: impl Into<PathBuf>) -> Self {
        Self {
            default_root: default_root.into(),
        }
    }

    /// 默认的数据目录，同时也是 `location.json` 所在的目录（数据目录移走后它仍保留在这里）。
    pub fn default_root(&self) -> &Path {
        &self.default_root
    }

    /// 启动时使用的数据目录：配置的目录存在时用它，否则用默认目录并报告哪个目录不可用。
    /// 配置的目录不存在（例如移动硬盘未连接）时不改写配置，连接后再次解析仍使用它。
    pub fn resolve(&self) -> Resolved {
        resolve_config(&self.load(), &self.default_root, Path::is_dir)
    }

    /// `location.json` 损坏时把它改名为 `location.json.bad`（覆盖旧的），之后就是没有配置。
    /// 没有损坏时为 `None`；否则是改名后的路径或改名失败的原因。
    ///
    /// 日志文件在数据目录中，这时日志还没有初始化，所以由调用方在初始化之后写警告。
    pub fn quarantine_corrupt_config(&self) -> Option<std::io::Result<PathBuf>> {
        let text = std::fs::read(self.config_path()).ok()?;
        parse_config(&text).err()?;
        let bad = self.config_bad_path();
        Some(std::fs::rename(self.config_path(), &bad).map(|()| bad))
    }

    /// 上次请求、还没有完成的搬迁目标；没有时为 `None`。
    pub fn pending_move(&self) -> Option<PathBuf> {
        self.load().pending_move_to
    }

    /// 检查能否把数据目录从 `current` 搬到 `target`。会在目标位置创建目录并试写一个文件。
    pub fn validate(&self, current: &Path, target: &Path) -> std::result::Result<(), MoveError> {
        let (current, target) = (normalize(current), normalize(target));
        if same_path(&target, &current) {
            return Err(MoveError::SameLocation);
        }
        if is_inside(&target, &current) || is_inside(&current, &target) {
            return Err(MoveError::Nested);
        }
        if target.is_dir() && self.entries(&target)?.next().is_some() {
            return Err(MoveError::NotEmpty(target));
        }
        let probe = target.join(".write-test");
        let writable = std::fs::create_dir_all(&target)
            .and_then(|()| std::fs::write(&probe, ""))
            .and_then(|()| std::fs::remove_file(&probe));
        if let Err(error) = writable {
            log::warn!("无法写入数据目录：{}：{error}", target.display());
            return Err(MoveError::NotWritable);
        }
        if volume(&current) != volume(&target) {
            let needed: u64 = self
                .files(&current)?
                .iter()
                .filter_map(|f| f.metadata().ok())
                .map(|m| m.len())
                .sum();
            if free_space(&target).is_some_and(|free| free < needed) {
                return Err(MoveError::NoSpace);
            }
        }
        Ok(())
    }

    /// 记下搬迁目标，下次启动时搬迁。调用前应先 [`validate`](Self::validate)。
    pub fn request_move(&self, target: &Path) -> Result<()> {
        let config = self.load();
        self.save(&Config {
            pending_move_to: Some(normalize(target)),
            ..config
        })
    }

    /// 启动时（打开数据库之前）把 `current` 搬到 `target`。`progress` 报告已复制与总字节数。
    ///
    /// 无论成功与否都清除搬迁请求；失败时数据目录不变，已复制的文件删除。成功后调用方应改用 `target`。
    pub fn perform_move(
        &self,
        current: &Path,
        target: &Path,
        mut progress: impl FnMut(u64, u64),
    ) -> std::result::Result<(), MoveError> {
        let (source, target) = (normalize(current), normalize(target));
        let config = self.load();
        self.save(&Config {
            data_root: config.data_root.clone(),
            pending_move_to: None,
        })?;
        self.validate(&source, &target)?;

        let files = self.files(&source)?;
        let total: u64 = files
            .iter()
            .filter_map(|f| f.metadata().ok())
            .map(|m| m.len())
            .sum();
        let copied = self.copy_files(&source, &target, &files, total, &mut progress);
        if let Err(error) = copied {
            self.remove_copied(&target);
            return Err(error);
        }

        let data_root = (!same_path(&target, &self.default_root)).then(|| target.clone());
        let switched = self.save(&Config {
            data_root,
            pending_move_to: None,
        });
        if let Err(error) = switched {
            self.remove_copied(&target);
            return Err(error.into());
        }
        log::info!(
            "数据目录已从 {} 移动到 {}",
            source.display(),
            target.display()
        );
        self.remove_old(&source, &files);
        Ok(())
    }

    fn copy_files(
        &self,
        source: &Path,
        target: &Path,
        files: &[PathBuf],
        total: u64,
        progress: &mut impl FnMut(u64, u64),
    ) -> std::result::Result<(), MoveError> {
        let mut done = 0;
        for file in files {
            let relative = file.strip_prefix(source).unwrap_or(file);
            let destination = target.join(relative);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            done += std::fs::copy(file, &destination)?;
            progress(done, total);
        }
        // 数据库在新位置打开、改写其中指向旧位置的路径；关闭后即释放文件
        let database = target.join(AppPaths::DATABASE_FILE);
        if database.is_file() {
            let db = Db::open(&database)?;
            rebase_paths(&db, &source.to_string_lossy(), &target.to_string_lossy())?;
        }
        Ok(())
    }

    /// 数据目录中要搬的文件：除 `location.json` 及其临时、损坏备份文件以外的全部文件。
    fn files(&self, root: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            if !dir.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    pending.push(path);
                } else if !self.is_config_file(&path) {
                    files.push(path);
                }
            }
        }
        files.sort();
        Ok(files)
    }

    /// 目录下除 `location.json` 及其临时、损坏备份文件以外的直接内容。
    fn entries(&self, dir: &Path) -> std::io::Result<impl Iterator<Item = PathBuf>> {
        let paths: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        Ok(paths.into_iter().filter(|p| !self.is_config_file(p)))
    }

    /// 已经复制、切换完成：删除旧文件与空目录，删不掉的（被其它程序占用）留下。
    fn remove_old(&self, source: &Path, files: &[PathBuf]) {
        for file in files {
            if let Err(error) = remove_file(file) {
                log::warn!("删除旧数据失败：{}：{error}", file.display());
            }
        }
        let mut dirs = Vec::new();
        collect_dirs(source, &mut dirs);
        dirs.sort_by_key(|d| std::cmp::Reverse(d.as_os_str().len()));
        for dir in dirs {
            // 非空时删除失败，保留即可
            let _ = std::fs::remove_dir(&dir);
        }
        if !same_path(source, &self.default_root) {
            let _ = std::fs::remove_dir(source);
        }
    }

    /// 搬迁失败：删除目标中已复制的内容；目标不是默认目录时，空了的目标文件夹一并删除。
    fn remove_copied(&self, target: &Path) {
        let Ok(entries) = self.entries(target) else {
            return;
        };
        for entry in entries {
            let removed = if entry.is_dir() {
                let mut files = Vec::new();
                collect_files(&entry, &mut files);
                files.iter().for_each(|f| _ = remove_file(f));
                std::fs::remove_dir_all(&entry)
            } else {
                remove_file(&entry)
            };
            if let Err(error) = removed {
                log::warn!("清理未完成的搬迁失败：{}：{error}", entry.display());
            }
        }
        if !same_path(target, &self.default_root) {
            let _ = std::fs::remove_dir(target);
        }
    }

    fn config_path(&self) -> PathBuf {
        self.default_root.join(CONFIG_FILE)
    }

    fn config_temp_path(&self) -> PathBuf {
        self.config_path().with_extension("json.tmp")
    }

    fn config_bad_path(&self) -> PathBuf {
        self.config_path().with_extension("json.bad")
    }

    fn is_config_file(&self, path: &Path) -> bool {
        let path = normalize(path);
        same_path(&path, &normalize(&self.config_path()))
            || same_path(&path, &normalize(&self.config_temp_path()))
            || same_path(&path, &normalize(&self.config_bad_path()))
    }

    /// 文件不存在或损坏时视为没有配置（使用默认位置）。
    fn load(&self) -> Config {
        std::fs::read(self.config_path())
            .ok()
            .and_then(|text| parse_config(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, config: &Config) -> Result<()> {
        std::fs::create_dir_all(&self.default_root)?;
        if config.data_root.is_none() && config.pending_move_to.is_none() {
            return match std::fs::remove_file(self.config_path()) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            };
        }
        let temp = self.config_temp_path();
        std::fs::write(&temp, serde_json::to_string_pretty(config)?)?;
        std::fs::rename(temp, self.config_path())?;
        Ok(())
    }
}

/// `location.json` 的内容。按字节解析：不是 UTF-8 的文件同样算损坏。
fn parse_config(text: &[u8]) -> serde_json::Result<Config> {
    serde_json::from_slice(text)
}

/// 由配置决定数据目录。
fn resolve_config(
    config: &Config,
    default_root: &Path,
    is_available: impl Fn(&Path) -> bool,
) -> Resolved {
    match &config.data_root {
        Some(root) if is_available(root) => Resolved {
            root: root.clone(),
            unavailable: None,
        },
        Some(root) => Resolved {
            root: default_root.to_path_buf(),
            unavailable: Some(root.clone()),
        },
        None => Resolved {
            root: default_root.to_path_buf(),
            unavailable: None,
        },
    }
}

/// 改写数据库中指向旧数据目录的绝对路径：已下载文件的本地路径，以及程序级、账号级设置中的路径
/// （例如主题背景图，设置值可能是 JSON，其中的反斜杠是转义过的）。
pub fn rebase_paths(db: &Db, old_root: &str, new_root: &str) -> Result<()> {
    let rebase = |value: &str| rebase_value(value, old_root, new_root);

    let downloads = db.query(
        "SELECT account_id, upload_path, local_path FROM downloaded_file",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        },
    )?;
    let settings = db.query("SELECT rowid, value FROM setting", [], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;

    db.with(|conn| {
        let tx = conn.transaction()?;
        for (account, path, local) in downloads {
            let rebased = rebase(&local);
            if rebased != local {
                tx.execute(
                    "UPDATE downloaded_file SET local_path = ?1 WHERE account_id = ?2 AND upload_path = ?3",
                    rusqlite::params![rebased, account, path],
                )?;
            }
        }
        for (row, value) in settings {
            let rebased = rebase(&value);
            if rebased != value {
                tx.execute(
                    "UPDATE setting SET value = ?1 WHERE rowid = ?2",
                    rusqlite::params![rebased, row],
                )?;
            }
        }
        tx.commit()
    })?;
    db.reload_settings()
}

/// 把 `value` 中以 `old_root` 开头的路径改为 `new_root`：路径后面必须是分隔符、引号或结尾，
/// 前缀相同但不是同一个目录（`YaZ2`）的不改。纯文本和 JSON 转义（反斜杠加倍）两种写法都处理，
/// 路径的盘符与目录名不区分 ASCII 大小写。
fn rebase_value(value: &str, old_root: &str, new_root: &str) -> String {
    let plain = replace_root(value, old_root, new_root, &["\\", "\""]);
    replace_root(
        &plain,
        &json_escape(old_root),
        &json_escape(new_root),
        &["\\\\", "\""],
    )
}

fn json_escape(path: &str) -> String {
    path.replace('\\', "\\\\")
}

fn replace_root(text: &str, old: &str, new: &str, followers: &[&str]) -> String {
    if old.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = find_ignore_ascii_case(rest, old) {
        let after = &rest[at + old.len()..];
        out.push_str(&rest[..at]);
        if after.is_empty() || followers.iter().any(|f| after.starts_with(f)) {
            out.push_str(new);
        } else {
            out.push_str(&rest[at..at + old.len()]);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    let (hay, needle) = (haystack.as_bytes(), needle.as_bytes());
    (0..=hay.len().checked_sub(needle.len())?).find(|&i| {
        haystack.is_char_boundary(i) && hay[i..i + needle.len()].eq_ignore_ascii_case(needle)
    })
}

fn remove_file(path: &Path) -> std::io::Result<()> {
    // 直接打开过的文件设置了只读属性，删除前先去掉（Windows 上这只清除只读属性）
    if let Ok(metadata) = path.metadata() {
        let mut permissions = metadata.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(path, permissions);
    }
    std::fs::remove_file(path)
}

fn collect_dirs(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for path in entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
    {
        collect_dirs(&path, out);
        out.push(path);
    }
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// 绝对路径，去掉末尾的分隔符。
fn normalize(path: &Path) -> PathBuf {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .components()
        .collect()
}

fn key(path: &Path) -> PathBuf {
    path.to_string_lossy().to_lowercase().into()
}

fn same_path(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

/// `path` 是否在 `root` 之内（不含 `root` 本身）。
fn is_inside(path: &Path, root: &Path) -> bool {
    let (path, root) = (key(path), key(root));
    path != root && path.starts_with(root)
}

/// 盘符（或网络共享）。
fn volume(path: &Path) -> PathBuf {
    key(&path
        .components()
        .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
        .collect::<PathBuf>())
}

/// 路径所在磁盘的可用字节数；查询失败时为 `None`。
fn free_space(path: &Path) -> Option<u64> {
    let root = path.ancestors().last()?;
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: wide 以 0 结尾且在调用期间有效，free 是有效的输出位置。
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&raw mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::{DownloadRecord, DownloadRepo};
    use crate::setting::SettingRepo;

    #[test]
    fn rebase_paths_rewrites_downloads_and_settings() {
        let db = Db::open_in_memory().unwrap();
        let account = crate::account::tests::add_account(&db);
        let downloads = DownloadRepo::new(&db, account);
        let record = |upload: &str, local: &str| DownloadRecord {
            upload_path: upload.into(),
            local_path: local.into(),
            file_name: "f".into(),
            size: 1,
            message_id: 1,
            conv_key: "t:1:x".into(),
            downloaded_at: 1,
            last_opened_at: None,
        };
        downloads.upsert(&record("/user_uploads/a", r"C:\Old\YaZ\files\a\f.txt"));
        downloads.upsert(&record("/user_uploads/b", r"D:\Elsewhere\g.txt"));
        let app = SettingRepo::app(&db);
        // JSON 中的反斜杠是转义过的；前缀相同但不是同一个目录（YaZ2）的不改
        app.set(
            "palette",
            r#"{"background":"C:\\Old\\YaZ\\theme\\bg.png","other":"C:\\Old\\YaZ2\\x"}"#,
        );
        app.set("plain", r"c:\old\yaz\plugin-data\p");
        app.set("exact", r"C:\Old\YaZ");
        SettingRepo::account(&db, account).set("bg", r"C:\Old\YaZ\bg.png");

        rebase_paths(&db, r"C:\Old\YaZ", r"E:\New\YaZ").unwrap();

        assert_eq!(
            downloads
                .find("/user_uploads/a")
                .unwrap()
                .unwrap()
                .local_path,
            r"E:\New\YaZ\files\a\f.txt"
        );
        assert_eq!(
            downloads
                .find("/user_uploads/b")
                .unwrap()
                .unwrap()
                .local_path,
            r"D:\Elsewhere\g.txt"
        );
        assert_eq!(
            app.get("palette").unwrap(),
            r#"{"background":"E:\\New\\YaZ\\theme\\bg.png","other":"C:\\Old\\YaZ2\\x"}"#
        );
        assert_eq!(app.get("plain").unwrap(), r"E:\New\YaZ\plugin-data\p");
        assert_eq!(app.get("exact").unwrap(), r"E:\New\YaZ");
        assert_eq!(
            SettingRepo::account(&db, account).get("bg").unwrap(),
            r"E:\New\YaZ\bg.png"
        );
    }

    #[test]
    fn rebase_value_handles_unescaped_prefix_inside_text() {
        assert_eq!(
            rebase_value(r#"{"a":"C:\Old\YaZ\x"}"#, r"C:\Old\YaZ", r"E:\N"),
            r#"{"a":"E:\N\x"}"#
        );
        assert_eq!(rebase_value("没有路径", r"C:\Old", r"E:\N"), "没有路径");
        assert_eq!(
            rebase_value(r"C:\Old\a;C:\Old", r"C:\Old", r"E:\N"),
            r"E:\N\a;E:\N"
        );
    }

    /// 默认数据目录里放一些文件和数据库，返回（临时目录、位置、数据库里的设置）。
    fn populated() -> (tempfile::TempDir, DataLocation, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("default");
        std::fs::create_dir_all(root.join("cache").join("images")).unwrap();
        std::fs::write(
            root.join("cache").join("images").join("a.png"),
            vec![1u8; 100],
        )
        .unwrap();
        std::fs::write(root.join("files.txt"), "内容").unwrap();
        let db = Db::open(&root.join(AppPaths::DATABASE_FILE)).unwrap();
        SettingRepo::app(&db).set("bg", &root.join("theme.png").to_string_lossy());
        drop(db);
        (dir, DataLocation::at(&root), root)
    }

    #[test]
    fn move_copies_data_rebases_paths_and_switches_location() {
        let (dir, location, root) = populated();
        let target = dir.path().join("moved").join(DataLocation::FOLDER_NAME);
        assert_eq!(location.resolve().root, root);
        location.validate(&root, &target).unwrap();

        location.request_move(&target).unwrap();
        assert_eq!(location.pending_move(), Some(target.clone()));

        let mut reports = Vec::new();
        location
            .perform_move(&root, &target, |done, total| reports.push((done, total)))
            .unwrap();
        assert!(!reports.is_empty() && reports.windows(2).all(|w| w[0].0 <= w[1].0));
        let (done, total) = *reports.last().unwrap();
        assert_eq!(done, total);

        assert_eq!(
            std::fs::read(target.join("cache").join("images").join("a.png"))
                .unwrap()
                .len(),
            100
        );
        assert_eq!(
            std::fs::read_to_string(target.join("files.txt")).unwrap(),
            "内容"
        );
        let db = Db::open(&target.join(AppPaths::DATABASE_FILE)).unwrap();
        assert_eq!(
            SettingRepo::app(&db).get("bg").unwrap(),
            target.join("theme.png").to_string_lossy()
        );

        assert_eq!(location.resolve().root, target);
        assert_eq!(location.pending_move(), None);
        // 旧位置只剩配置文件
        assert!(
            !root.join("files.txt").exists()
                && !root.join("cache").exists()
                && !root.join(AppPaths::DATABASE_FILE).exists()
        );
        assert!(root.join(CONFIG_FILE).is_file());
    }

    #[test]
    fn moving_back_to_default_removes_the_override() {
        let (dir, location, root) = populated();
        let target = dir.path().join("moved");
        location.perform_move(&root, &target, |_, _| {}).unwrap();
        assert_eq!(location.resolve().root, target);

        location.perform_move(&target, &root, |_, _| {}).unwrap();
        assert_eq!(location.resolve().root, root);
        assert!(root.join("files.txt").is_file());
        // 没有自定义位置也没有待办搬迁：配置文件删除
        assert!(!root.join(CONFIG_FILE).exists());
        assert!(!target.exists());
    }

    #[test]
    fn failed_move_cleans_up_and_keeps_old_location() {
        let (dir, location, root) = populated();
        // 损坏的数据库在新位置无法打开
        std::fs::write(
            root.join(AppPaths::DATABASE_FILE),
            b"this is not a database at all",
        )
        .unwrap();
        let target = dir.path().join("moved");
        location.request_move(&target).unwrap();

        let error = location
            .perform_move(&root, &target, |_, _| {})
            .unwrap_err();
        assert!(matches!(error, MoveError::Store(_)), "{error:?}");
        assert!(!target.exists());
        assert!(root.join("files.txt").is_file());
        assert_eq!(location.resolve().root, root);
        assert_eq!(location.pending_move(), None);
    }

    #[test]
    fn failed_switch_cleans_up_and_keeps_old_location() {
        let (dir, location, root) = populated();
        let target = dir.path().join("moved");
        location.request_move(&target).unwrap();

        // 临时文件的位置被目录占住：切换位置时的保存失败
        let blocker = root.join("location.json.tmp");
        let error = location
            .perform_move(&root, &target, |_, _| {
                std::fs::create_dir_all(&blocker).unwrap();
            })
            .unwrap_err();
        assert!(matches!(error, MoveError::Store(_)), "{error:?}");
        assert!(!target.exists());
        assert!(root.join("files.txt").is_file());
        assert!(root.join(AppPaths::DATABASE_FILE).is_file());
        assert_eq!(location.resolve().root, root);
        assert_eq!(location.pending_move(), None);
    }

    #[test]
    fn save_replaces_config_atomically() {
        let (dir, location, root) = populated();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        location.request_move(&first).unwrap();
        assert_eq!(location.pending_move(), Some(first.clone()));
        location.request_move(&second).unwrap();
        assert_eq!(location.pending_move(), Some(second.clone()));
        assert!(!root.join("location.json.tmp").exists());

        // 写不了临时文件时，原来的配置不受影响
        std::fs::create_dir_all(root.join("location.json.tmp")).unwrap();
        assert!(location.request_move(&first).is_err());
        assert_eq!(location.pending_move(), Some(second));
    }

    #[test]
    fn config_file_format_is_unchanged() {
        let (dir, location, root) = populated();
        let target = dir.path().join("target");
        location.request_move(&target).unwrap();
        let text = std::fs::read_to_string(root.join(CONFIG_FILE)).unwrap();
        let expected = format!(
            "{{\n  \"data_root\": null,\n  \"pending_move_to\": {}\n}}",
            serde_json::to_string(&target).unwrap()
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn leftover_temp_file_is_not_data() {
        let (dir, location, root) = populated();
        std::fs::write(root.join("location.json.tmp"), "残留").unwrap();
        let target = dir.path().join("moved");
        location.perform_move(&root, &target, |_, _| {}).unwrap();
        assert!(!target.join("location.json.tmp").exists());

        // 默认目录只剩配置文件和残留的临时文件，可以搬回来
        std::fs::write(root.join("location.json.tmp"), "残留").unwrap();
        location.validate(&target, &root).unwrap();
    }

    #[test]
    fn validate_rejects_bad_targets() {
        let (dir, location, root) = populated();
        assert!(matches!(
            location.validate(&root, &root),
            Err(MoveError::SameLocation)
        ));
        assert!(matches!(
            location.validate(&root, &root.join("cache").join("x")),
            Err(MoveError::Nested)
        ));
        assert!(matches!(
            location.validate(&root.join("cache"), &root),
            Err(MoveError::Nested)
        ));

        let occupied = dir.path().join("occupied");
        std::fs::create_dir_all(&occupied).unwrap();
        std::fs::write(occupied.join("x.txt"), "").unwrap();
        let error = location.validate(&root, &occupied).unwrap_err();
        assert!(matches!(&error, MoveError::NotEmpty(p) if p == &occupied));
        assert_eq!(
            error.to_string(),
            format!("“{}”不是空文件夹", occupied.display())
        );

        // 空文件夹和只有配置文件的默认目录可以作为目标
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        location.validate(&root, &empty).unwrap();

        // 目标的父目录是一个文件：无法创建
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(matches!(
            location.validate(&root, &file.join("sub")),
            Err(MoveError::NotWritable)
        ));
    }

    #[test]
    fn corrupt_config_means_default_location() {
        let (_dir, location, root) = populated();
        std::fs::write(root.join(CONFIG_FILE), "{ not json").unwrap();
        assert_eq!(location.resolve().root, root);
        assert_eq!(location.resolve().unavailable, None);
        assert_eq!(location.pending_move(), None);
    }

    #[test]
    fn missing_custom_root_falls_back_without_forgetting_it() {
        let (dir, location, root) = populated();
        let gone = dir.path().join("unplugged");
        location
            .save(&Config {
                data_root: Some(gone.clone()),
                pending_move_to: None,
            })
            .unwrap();
        let config_before = std::fs::read(root.join(CONFIG_FILE)).unwrap();
        let resolved = location.resolve();
        assert_eq!(resolved.root, root);
        assert_eq!(resolved.unavailable.as_ref(), Some(&gone));
        // 解析不写配置，连接后再次解析仍是原来的目录
        assert_eq!(
            std::fs::read(root.join(CONFIG_FILE)).unwrap(),
            config_before
        );
        std::fs::create_dir_all(&gone).unwrap();
        let resolved = location.resolve();
        assert_eq!(resolved.root, gone);
        assert_eq!(resolved.unavailable, None);
    }

    #[test]
    fn resolve_config_covers_every_case() {
        let default = Path::new(r"C:\Default");
        let custom = PathBuf::from(r"E:\Custom");
        let config = |data_root: Option<PathBuf>| Config {
            data_root,
            pending_move_to: None,
        };
        assert_eq!(
            resolve_config(&config(None), default, |_| panic!("没有配置时不检查目录")),
            Resolved {
                root: default.to_path_buf(),
                unavailable: None
            }
        );
        assert_eq!(
            resolve_config(&config(Some(custom.clone())), default, |_| true),
            Resolved {
                root: custom.clone(),
                unavailable: None
            }
        );
        assert_eq!(
            resolve_config(&config(Some(custom.clone())), default, |_| false),
            Resolved {
                root: default.to_path_buf(),
                unavailable: Some(custom)
            }
        );
    }

    #[test]
    fn parse_config_rejects_everything_that_is_not_the_format() {
        for bad in [
            &b""[..],
            b"{ not json",
            b"[]",
            b"null",
            b"{\"data_root\": 5}",
            b"\xff\xfe",
        ] {
            assert!(parse_config(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_config(b"{}").unwrap().data_root, None);
        assert_eq!(
            parse_config(br#"{"data_root":"E:\\D","pending_move_to":null}"#)
                .unwrap()
                .data_root,
            Some(PathBuf::from(r"E:\D"))
        );
    }

    #[test]
    fn corrupt_config_is_renamed_over_the_old_backup_once() {
        let (_dir, location, root) = populated();
        let (config, bad) = (root.join(CONFIG_FILE), root.join("location.json.bad"));
        assert!(location.quarantine_corrupt_config().is_none());

        std::fs::write(&bad, "更早的备份").unwrap();
        std::fs::write(&config, "{ not json").unwrap();
        assert_eq!(location.quarantine_corrupt_config().unwrap().unwrap(), bad);
        assert!(!config.exists());
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ not json");
        // 只提示一次：坏文件已经移开
        assert!(location.quarantine_corrupt_config().is_none());
        assert_eq!(location.resolve().root, root);
    }

    #[test]
    fn valid_config_is_not_quarantined() {
        let (dir, location, root) = populated();
        location.request_move(&dir.path().join("target")).unwrap();
        assert!(location.quarantine_corrupt_config().is_none());
        assert!(root.join(CONFIG_FILE).is_file());
    }

    #[test]
    fn bad_config_backup_is_not_data() {
        let (dir, location, root) = populated();
        std::fs::write(root.join("location.json.bad"), "坏").unwrap();
        let target = dir.path().join("moved");
        location.perform_move(&root, &target, |_, _| {}).unwrap();
        assert!(!target.join("location.json.bad").exists());
        // 默认目录只剩配置文件和备份，可以搬回来
        location.validate(&target, &root).unwrap();
    }

    #[test]
    fn path_helpers_ignore_case_and_trailing_separators() {
        assert!(same_path(
            &normalize(Path::new(r"C:\A\B\")),
            &normalize(Path::new(r"c:\a\b"))
        ));
        assert!(is_inside(Path::new(r"C:\A\B\C"), Path::new(r"c:\a\b")));
        assert!(!is_inside(Path::new(r"C:\A\B2"), Path::new(r"C:\A\B")));
        assert!(!is_inside(Path::new(r"C:\A\B"), Path::new(r"C:\A\B")));
        assert_eq!(volume(Path::new(r"C:\A\B")), volume(Path::new(r"c:\x")));
        assert_ne!(volume(Path::new(r"C:\A")), volume(Path::new(r"D:\A")));
        assert!(free_space(&std::env::temp_dir()).is_some_and(|free| free > 0));
    }
}
