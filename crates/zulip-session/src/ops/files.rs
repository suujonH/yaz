//! 文件上传下载、打开方式、图片和表情数据。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zulip_core::emoji::{self, EmojiCatalog, EmojiEntry};
use zulip_core::file::{next_free_name, sanitize_directory_name};
use zulip_core::html::{UploadLink, extract_uploads, original_upload_path};
use zulip_core::model::UploadedFile;
use zulip_store::{AccountSettings, DownloadRecord, DownloadRepo, LocalSettings};

use crate::error::{Result, SessionError};
use crate::event::SessionEvent;
use crate::session::Session;

const RECENT_EMOJI_MAX: usize = 16;
const DEFAULT_REACTIONS: [&str; 6] = ["+1", "heart", "joy", "open_mouth", "cry", "pray"];

// Windows 标记为可执行的扩展名：只在资源管理器中显示，不直接打开
const EXECUTABLE_EXTENSIONS: [&str; 23] = [
    "exe",
    "com",
    "bat",
    "cmd",
    "scr",
    "pif",
    "msi",
    "msp",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "hta",
    "lnk",
    "jar",
    "reg",
    "cpl",
    "url",
    "appref-ms",
];

/// 打开文件的方式。真正的打开由 app 执行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenAction {
    Open(PathBuf),
    /// 可执行文件只在资源管理器中定位。
    Reveal(PathBuf),
}

pub fn open_action_for(path: &Path) -> OpenAction {
    let executable = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        EXECUTABLE_EXTENSIONS
            .iter()
            .any(|x| x.eq_ignore_ascii_case(e))
    });
    if executable {
        OpenAction::Reveal(path.to_owned())
    } else {
        OpenAction::Open(path.to_owned())
    }
}

/// `/user_uploads/2/ab/x.png` → 缓存目录下的相对路径。
fn cache_relative(upload_path: &str) -> PathBuf {
    upload_path
        .trim_start_matches('/')
        .trim_start_matches("user_uploads/")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(sanitize_directory_name)
        .collect()
}

// 互联网来源标记：Windows 打开时会提示来源
fn write_zone_identifier(path: &Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    if let Err(error) = std::fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\n") {
        log::info!("写入来源标记失败：{error}");
    }
}

impl Session {
    /// 文件超过上传限制时的错误。
    pub fn upload_too_large(&self) -> SessionError {
        let max = self.state.read().realm.max_file_upload_size_mib;
        SessionError::invalid(format!("文件超过 {max}MB，无法发送"))
    }

    /// 检查文件大小是否在服务器的上传限制内。
    pub fn check_upload_size(&self, len: u64) -> Result<()> {
        let max = self.state.read().realm.max_file_upload_size_mib;
        if len > u64::try_from(max).unwrap_or(0) * 1024 * 1024 {
            return Err(self.upload_too_large());
        }
        Ok(())
    }

    /// 上传附件。超过上传限制时不发请求，返回 `Invalid`。`progress` 收到已发送的字节数。
    pub async fn upload_file(
        &self,
        path: &Path,
        progress: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<UploadedFile> {
        let file = tokio::fs::File::open(path).await?;
        let len = file.metadata().await?.len();
        self.check_upload_size(len)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_owned();
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        Ok(self
            .client
            .upload_file(file, len, &name, &content_type, progress)
            .await?)
    }

    /// 图片的本地文件（带认证下载并缓存）。`url` 可以是相对本组织的地址。
    /// 下载失败（服务器上已删除、离线）时用收藏的副本。
    pub async fn image_file(&self, url: &str) -> Result<PathBuf> {
        match self.images.file(url).await {
            Ok(path) => Ok(path),
            Err(error) => self.favorite_copy(url).await.ok_or(error),
        }
    }

    /// 图片内容。下载失败时用收藏的副本。
    pub async fn image_bytes(&self, url: &str) -> Result<Vec<u8>> {
        match self.images.bytes(url).await {
            Ok(bytes) => Ok(bytes),
            Err(error) => match self.favorite_copy(url).await {
                Some(copy) => Ok(tokio::fs::read(copy).await?),
                None => Err(error),
            },
        }
    }

    /// 收藏后保存消息里的图片和文件的本机副本，取消收藏后删除。失败只记日志：不影响收藏本身。
    pub(crate) async fn sync_favorite_copies(&self, ids: &[i64], starred: bool) {
        for &id in ids {
            let dir = self.favorites_root().join(id.to_string());
            if !starred {
                if let Err(error) = tokio::fs::remove_dir_all(&dir).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    log::warn!("删除收藏副本 {} 失败：{error}", dir.display());
                }
                continue;
            }
            let html = match self.message_html(id).await {
                Ok(html) => html,
                Err(error) => {
                    log::warn!("取收藏的消息 {id} 失败：{error}");
                    continue;
                }
            };
            for upload in extract_uploads(&html) {
                let target = dir.join(cache_relative(&upload.path));
                if tokio::fs::try_exists(&target).await.unwrap_or(false) {
                    continue;
                }
                if let Err(error) = self.download_file(&upload.path, &target, |_, _| {}).await {
                    log::warn!("保存收藏的文件 {} 失败：{error}", upload.path);
                }
            }
        }
    }

    async fn message_html(&self, id: i64) -> Result<String> {
        let cached = self
            .state
            .read()
            .messages
            .get(id)
            .map(|message| message.content_html.clone());
        match cached {
            Some(html) => Ok(html),
            None => Ok(self.client.get_message(id, true).await?.content),
        }
    }

    /// 收藏时保存的副本。`url` 可以是缩略图地址（取原图的副本）或本组织的完整地址。
    pub async fn favorite_copy(&self, url: &str) -> Option<PathBuf> {
        let realm = self.client.realm_url();
        let relative = url
            .strip_prefix(realm.as_str().trim_end_matches('/'))
            .unwrap_or(url);
        let path = original_upload_path(relative)?;
        let relative = cache_relative(&path);
        let mut messages = tokio::fs::read_dir(self.favorites_root()).await.ok()?;
        while let Ok(Some(message)) = messages.next_entry().await {
            let candidate = message.path().join(&relative);
            if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
                return Some(candidate);
            }
        }
        None
    }

    fn downloads(&self) -> DownloadRepo {
        DownloadRepo::new(&self.db, self.account_id)
    }

    /// 文件已下载时的本地路径。文件被移走后记录作废。
    pub async fn downloaded_path(&self, upload_path: &str) -> Option<PathBuf> {
        let account_id = self.account_id;
        let upload_path = upload_path.to_owned();
        let record = self
            .db
            .call(move |db| DownloadRepo::new(db, account_id).find(&upload_path))
            .await
            .ok()
            .flatten()?;
        let path = PathBuf::from(record.local_path);
        tokio::fs::try_exists(&path)
            .await
            .unwrap_or(false)
            .then_some(path)
    }

    pub async fn recent_downloads(&self, limit: usize) -> Result<Vec<DownloadRecord>> {
        let account_id = self.account_id;
        Ok(self
            .db
            .call(move |db| DownloadRepo::new(db, account_id).recent(limit))
            .await?)
    }

    pub(crate) fn forget_download(&self, upload_path: &str) {
        self.downloads().delete(upload_path);
    }

    pub async fn file_size(&self, upload_path: &str) -> Result<Option<u64>> {
        Ok(self
            .client
            .head_size(upload_path.trim_start_matches('/'))
            .await?)
    }

    /// 下载到账号的文件缓存。同一文件并发下载时只下载一次。
    pub async fn download_to_cache(
        &self,
        upload_path: &str,
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<PathBuf> {
        let target = self.files_root().join(cache_relative(upload_path));
        let gate = self.gate(format!("file:{upload_path}"));
        let _guard = gate.lock().await;
        if target.exists() {
            return Ok(target);
        }
        if let Err(error) = self.download_file(upload_path, &target, progress).await {
            return self.favorite_copy(upload_path).await.ok_or(error);
        }
        Ok(target)
    }

    /// 下载到默认保存目录，文件名冲突时自动编号，并记录到“最近文件”。
    pub async fn save_to_default_folder(
        &self,
        link: &UploadLink,
        message_id: i64,
        conv_key: &str,
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<PathBuf> {
        if let Some(existing) = self.downloaded_path(&link.path).await {
            return Ok(existing);
        }
        let root = LocalSettings::new(&self.db).save_root();
        let name = next_free_name(&link.file_name, |candidate| root.join(candidate).exists())
            .ok_or_else(|| SessionError::invalid("无法确定保存的文件名"))?;
        let target = root.join(name);
        self.save_as(link, &target, message_id, conv_key, progress)
            .await?;
        Ok(target)
    }

    /// 另存为指定路径。
    pub async fn save_as(
        &self,
        link: &UploadLink,
        target: &Path,
        message_id: i64,
        conv_key: &str,
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<()> {
        let gate = self.gate(format!("file:{}", link.path));
        let _guard = gate.lock().await;
        if let Err(error) = self.download_file(&link.path, target, progress).await {
            let copy = self.favorite_copy(&link.path).await.ok_or(error)?;
            if let Some(dir) = target.parent() {
                tokio::fs::create_dir_all(dir).await?;
            }
            tokio::fs::copy(&copy, target).await?;
        }
        let size = tokio::fs::metadata(target).await?.len();
        self.downloads().upsert(&DownloadRecord {
            upload_path: link.path.clone(),
            local_path: target.to_string_lossy().into_owned(),
            file_name: link.file_name.clone(),
            size: size as i64,
            message_id,
            conv_key: conv_key.to_owned(),
            downloaded_at: chrono::Utc::now().timestamp(),
            last_opened_at: None,
        });
        Ok(())
    }

    async fn download_file(
        &self,
        upload_path: &str,
        target: &Path,
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<()> {
        if let Some(dir) = target.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut part = target.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let result = async {
            let mut file = tokio::fs::File::create(&part).await?;
            self.client
                .download(upload_path.trim_start_matches('/'), &mut file, progress)
                .await?;
            drop(file);
            tokio::fs::rename(&part, target).await?;
            Ok::<(), SessionError>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&part).await;
        }
        result?;
        write_zone_identifier(target);
        Ok(())
    }

    /// 打开已下载的文件：记录打开时间并返回打开方式。
    pub async fn open_downloaded(&self, upload_path: &str) -> Result<OpenAction> {
        let path = self
            .downloaded_path(upload_path)
            .await
            .ok_or_else(|| SessionError::invalid("文件不存在，请重新下载"))?;
        self.downloads()
            .mark_opened(upload_path, chrono::Utc::now().timestamp());
        Ok(open_action_for(&path))
    }

    pub fn emoji_catalog(&self) -> Arc<EmojiCatalog> {
        self.state.read().emoji.clone()
    }

    /// 下载 Unicode 表情数据。先用本地缓存；地址变化后重新下载。
    pub(crate) async fn ensure_unicode_emoji(&self) -> Result<()> {
        let Some(url) = self.state.read().realm.server_emoji_data_url.clone() else {
            return Ok(());
        };
        let dir = self.paths.cache_dir();
        let data_path = dir.join("emoji-data.json");
        let url_path = dir.join("emoji-data.json.url");
        let cached = match tokio::fs::read_to_string(&url_path).await {
            Ok(saved) if saved == url => tokio::fs::read_to_string(&data_path).await.ok(),
            _ => None,
        };
        let json = match cached {
            Some(json) => json,
            None => {
                let bytes = self.images.bytes(&url).await?;
                let json = String::from_utf8_lossy(&bytes).into_owned();
                tokio::fs::create_dir_all(&dir).await?;
                tokio::fs::write(&data_path, &json).await?;
                tokio::fs::write(&url_path, &url).await?;
                json
            }
        };
        let unicode = emoji::parse_unicode(&json)
            .map_err(|e| SessionError::invalid(format!("表情数据无效：{e}")))?;
        {
            let mut state = self.state.write();
            state.unicode_emoji = unicode;
            state.rebuild_emoji();
        }
        self.emit(SessionEvent::EmojiChanged);
        Ok(())
    }

    fn emoji_settings(&self) -> AccountSettings {
        AccountSettings::new(&self.db, self.account_id)
    }

    /// 最近使用的表情，最新的在前。
    pub fn recent_emoji(&self) -> Vec<EmojiEntry> {
        let catalog = self.emoji_catalog();
        self.emoji_settings()
            .recent_emoji()
            .iter()
            .filter_map(|name| catalog.get(name).cloned())
            .collect()
    }

    pub fn add_recent_emoji(&self, name: &str) {
        let settings = self.emoji_settings();
        let mut names = settings.recent_emoji();
        names.retain(|n| n != name);
        names.insert(0, name.to_owned());
        names.truncate(RECENT_EMOJI_MAX);
        settings.set_recent_emoji(&names);
    }

    /// 消息菜单的快捷回应：最近使用的在前，用默认表情补足 6 个。
    pub fn quick_reactions(&self) -> Vec<EmojiEntry> {
        let catalog = self.emoji_catalog();
        let mut list = self.recent_emoji();
        for name in DEFAULT_REACTIONS {
            if let Some(entry) = catalog.get(name)
                && !list.iter().any(|e| e.name == entry.name)
            {
                list.push(entry.clone());
            }
        }
        list.truncate(DEFAULT_REACTIONS.len());
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executables_are_only_revealed() {
        for name in ["a.EXE", "b.ps1", "c.appref-ms", "d.lnk"] {
            assert!(
                matches!(open_action_for(Path::new(name)), OpenAction::Reveal(_)),
                "{name}"
            );
        }
        for name in ["a.txt", "b.png", "noext"] {
            assert!(
                matches!(open_action_for(Path::new(name)), OpenAction::Open(_)),
                "{name}"
            );
        }
    }

    #[test]
    fn cache_path_keeps_upload_folders() {
        let path = cache_relative("/user_uploads/2/ab/报告.pdf");
        let parts: Vec<_> = path
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(parts, ["2", "ab", "报告.pdf"]);
    }
}
