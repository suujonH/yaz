use std::path::{Path, PathBuf};

pub const APP_EXE_NAME: &str = "yaz.exe";
pub const UPDATER_EXE_NAME: &str = "yaz-updater.exe";

/// 程序目录中的文件清单（sha256sum 格式，由 tools/pack-release.ps1 生成）；更新时只动清单中的文件。
pub const FILE_LIST_NAME: &str = "yaz.files.sha256";

/// 替换时存放旧文件的目录，位于程序目录内：移动文件很快，也不需要上级目录的写权限。
pub const BACKUP_DIR_NAME: &str = ".update-backup";

/// 更新器启动新版本时的参数，其后是启动标记的路径；新版本启动成功后把自己的版本号写入该文件。
pub const PROBE_ARG: &str = "--update-probe";

/// 更新工作目录中各文件的位置。
#[derive(Debug, Clone)]
pub struct UpdateLayout {
    root: PathBuf,
}

impl UpdateLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `%LOCALAPPDATA%\YaZ.Update`
    pub fn default_root() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("YaZ.Update")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// 已下载、校验并解压好的新版本，等待替换。
    pub fn staged_dir(&self) -> PathBuf {
        self.root.join("staged")
    }

    /// 解压到位后写入的清单；存在即表示 `staged_dir` 完整可用。
    pub fn ready_path(&self) -> PathBuf {
        self.root.join("ready.json")
    }

    /// 下载与解压的临时目录。
    pub fn work_dir(&self) -> PathBuf {
        self.root.join("work")
    }

    /// 更新器运行时的副本所在目录：更新器要替换程序目录，不能在程序目录内运行。
    pub fn runner_dir(&self) -> PathBuf {
        self.root.join("runner")
    }

    /// 新版本启动成功后写入的标记。
    pub fn probe_path(&self) -> PathBuf {
        self.root.join("started.txt")
    }
}
