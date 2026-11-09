use std::path::{Path, PathBuf};

/// Locations of the items in the data directory. The data directory is determined by
/// [`DataLocation`](crate::DataLocation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    pub const DATABASE_FILE: &'static str = "zulip.db";

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Chat info (data directory): database, logs, cache, opened files, plugin data, etc.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database(&self) -> PathBuf {
        self.root.join(Self::DATABASE_FILE)
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn image_cache_dir(&self) -> PathBuf {
        self.root.join("cache").join("images")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Chat files opened directly: `files\<server>\<directory part of path_id>\<file name>`.
    pub fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    /// Local copies of images and files in favorite messages: `favorites\<server>\<message
    /// id>\path_id`.
    pub fn favorites_dir(&self) -> PathBuf {
        self.root.join("favorites")
    }

    pub fn plugin_data_dir(&self, plugin_id: &str) -> PathBuf {
        self.root.join("plugin-data").join(plugin_id)
    }
}

/// Default directory for received or downloaded files: the current user's Downloads folder.
pub fn default_save_root() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("Downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_are_under_root() {
        let paths = AppPaths::new(r"C:\Data\YaZ");
        assert_eq!(paths.database(), Path::new(r"C:\Data\YaZ\zulip.db"));
        assert_eq!(
            paths.image_cache_dir(),
            Path::new(r"C:\Data\YaZ\cache\images")
        );
        assert_eq!(paths.log_dir(), Path::new(r"C:\Data\YaZ\logs"));
        assert_eq!(paths.files_dir(), Path::new(r"C:\Data\YaZ\files"));
        assert_eq!(
            paths.plugin_data_dir("com.a"),
            Path::new(r"C:\Data\YaZ\plugin-data\com.a")
        );
    }
}
