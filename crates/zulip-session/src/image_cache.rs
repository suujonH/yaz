//! 图片磁盘缓存与下载去重。界面在 WebView 中，只需要文件路径或字节，不做内存解码。

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use url::Url;
use zulip_api::ZulipClient;

use crate::error::{Result, SessionError};

/// 外部图片的缓存时长；本组织的上传文件内容不变，一直有效。
const EXTERNAL_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

pub type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>>;

/// 取图片内容。`same_realm` 为 true 时 `url` 是本组织的地址，需要带认证。
pub trait ImageFetcher: Send + Sync {
    fn fetch<'a>(&'a self, url: &'a Url, same_realm: bool) -> FetchFuture<'a>;
}

/// 本组织的地址用账号认证，外部地址不带认证。
pub struct ClientFetcher {
    pub client: ZulipClient,
    pub http: reqwest::Client,
}

impl ImageFetcher for ClientFetcher {
    fn fetch<'a>(&'a self, url: &'a Url, same_realm: bool) -> FetchFuture<'a> {
        Box::pin(async move {
            if same_realm {
                let mut relative = url.path().trim_start_matches('/').to_owned();
                if let Some(query) = url.query() {
                    relative.push('?');
                    relative.push_str(query);
                }
                let mut bytes = Vec::new();
                self.client
                    .download(&relative, &mut bytes, |_, _| {})
                    .await?;
                return Ok(bytes);
            }
            let response = self
                .http
                .get(url.clone())
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|e| SessionError::invalid(format!("下载图片失败：{e}")))?;
            let bytes = response
                .bytes()
                .await
                .map_err(|e| SessionError::invalid(format!("下载图片失败：{e}")))?;
            Ok(bytes.to_vec())
        })
    }
}

pub struct ImageCache {
    dir: PathBuf,
    realm: Url,
    fetcher: Box<dyn ImageFetcher>,
    in_flight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    next_temp: AtomicU64,
}

impl ImageCache {
    pub fn new(dir: PathBuf, realm: Url, fetcher: Box<dyn ImageFetcher>) -> Self {
        Self {
            dir,
            realm,
            fetcher,
            in_flight: Mutex::new(HashMap::new()),
            next_temp: AtomicU64::new(0),
        }
    }

    fn resolve(&self, url: &str) -> Result<Url> {
        self.realm
            .join(url)
            .map_err(|_| SessionError::invalid(format!("无效的图片地址：{url}")))
    }

    fn is_same_realm(&self, url: &Url) -> bool {
        url.scheme() == self.realm.scheme()
            && url.host() == self.realm.host()
            && url.port_or_known_default() == self.realm.port_or_known_default()
    }

    /// 缓存文件路径（不保证存在）。文件名是完整地址的哈希。
    fn cache_path(&self, url: &Url) -> PathBuf {
        let hash = hex::encode(Sha256::digest(url.as_str().as_bytes()));
        let extension = Path::new(url.path())
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| e.len() <= 5 && e.chars().all(char::is_alphanumeric))
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        self.dir.join(format!("{hash}{extension}"))
    }

    fn is_fresh(path: &Path, same_realm: bool) -> bool {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        if same_realm {
            return true;
        }
        metadata
            .modified()
            .ok()
            .and_then(|at| SystemTime::now().duration_since(at).ok())
            .is_some_and(|age| age < EXTERNAL_MAX_AGE)
    }

    /// 写入失败不影响显示：旧文件保留，临时文件清除。
    async fn store(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let serial = self.next_temp.fetch_add(1, Ordering::Relaxed);
        let temp = self.dir.join(format!("{serial}.tmp"));
        let written = async {
            tokio::fs::write(&temp, bytes).await?;
            tokio::fs::rename(&temp, path).await
        }
        .await;
        if written.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        written
    }

    /// 取图片。同一地址同时只下载一次，等待者直接读缓存。
    /// 返回 `(内容, 缓存文件)`；写缓存失败时文件为 `None`。
    async fn load(&self, url: &str, need_bytes: bool) -> Result<(Vec<u8>, Option<PathBuf>)> {
        let url = self.resolve(url)?;
        let same_realm = self.is_same_realm(&url);
        let path = self.cache_path(&url);
        let gate = self
            .in_flight
            .lock()
            .entry(url.as_str().to_owned())
            .or_default()
            .clone();
        let result = {
            let _guard = gate.lock().await;
            if Self::is_fresh(&path, same_realm) {
                let bytes = if need_bytes {
                    tokio::fs::read(&path).await?
                } else {
                    Vec::new()
                };
                Ok((bytes, Some(path)))
            } else {
                match self.fetcher.fetch(&url, same_realm).await {
                    Ok(bytes) => match self.store(&path, &bytes).await {
                        Ok(()) => Ok((bytes, Some(path))),
                        Err(error) => {
                            log::warn!("写入图片缓存失败：{error}");
                            Ok((bytes, None))
                        }
                    },
                    Err(error) => Err(error),
                }
            }
        };
        let mut in_flight = self.in_flight.lock();
        if Arc::strong_count(&gate) <= 2 {
            in_flight.remove(url.as_str());
        }
        result
    }

    /// 图片的本地文件。`url` 可以是相对本组织的地址。
    pub async fn file(&self, url: &str) -> Result<PathBuf> {
        let (_, path) = self.load(url, false).await?;
        path.ok_or_else(|| SessionError::invalid("无法写入图片缓存"))
    }

    pub async fn bytes(&self, url: &str) -> Result<Vec<u8>> {
        Ok(self.load(url, true).await?.0)
    }

    #[cfg(test)]
    fn in_flight_count(&self) -> usize {
        self.in_flight.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct CountingFetcher {
        calls: Arc<AtomicUsize>,
        authed: Arc<Mutex<Vec<bool>>>,
    }

    impl ImageFetcher for CountingFetcher {
        fn fetch<'a>(&'a self, _: &'a Url, same_realm: bool) -> FetchFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.authed.lock().push(same_realm);
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(vec![1, 2, 3])
            })
        }
    }

    fn cache(dir: PathBuf) -> (ImageCache, Arc<AtomicUsize>, Arc<Mutex<Vec<bool>>>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let authed = Arc::new(Mutex::new(Vec::new()));
        let fetcher = CountingFetcher {
            calls: calls.clone(),
            authed: authed.clone(),
        };
        let realm = Url::parse("https://z.test:8443/").unwrap();
        (
            ImageCache::new(dir, realm, Box::new(fetcher)),
            calls,
            authed,
        )
    }

    fn file_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, Iterator::count)
    }

    #[tokio::test]
    async fn concurrent_loads_download_once() {
        let dir = tempfile::tempdir().unwrap();
        let (cache, calls, _) = cache(dir.path().join("c"));
        let cache = Arc::new(cache);
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            tasks.push(tokio::spawn(async move {
                cache.bytes("/user_uploads/a.png").await.unwrap()
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), [1, 2, 3]);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(file_count(&dir.path().join("c")), 1);
        assert_eq!(cache.in_flight_count(), 0);
    }

    #[tokio::test]
    async fn file_returns_cached_path() {
        let dir = tempfile::tempdir().unwrap();
        let (cache, calls, _) = cache(dir.path().to_path_buf());
        let first = cache.file("/user_uploads/a.png").await.unwrap();
        assert!(first.exists());
        assert_eq!(first.extension().unwrap(), "png");
        assert_eq!(cache.file("/user_uploads/a.png").await.unwrap(), first);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn host_and_port_decide_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let (cache, _, authed) = cache(dir.path().to_path_buf());
        cache.bytes("/user_uploads/a.png").await.unwrap();
        cache.bytes("https://z.test:8443/x.png").await.unwrap();
        cache.bytes("https://z.test/x.png").await.unwrap();
        cache.bytes("https://other.test:8443/x.png").await.unwrap();
        cache.bytes("http://z.test:8443/y.png").await.unwrap();
        assert_eq!(*authed.lock(), [true, true, false, false, false]);
    }

    #[tokio::test]
    async fn path_with_scheme_is_refused_before_authenticated_download() {
        let dir = tempfile::tempdir().unwrap();
        // 地址不可达：一旦发出请求，错误就不是“地址不属于本组织”。
        let client = ZulipClient::new("https://z.test:8443/", "a@z.test", "KEY", false).unwrap();
        let fetcher = ClientFetcher {
            client,
            http: reqwest::Client::new(),
        };
        let realm = Url::parse("https://z.test:8443/").unwrap();
        let cache = ImageCache::new(dir.path().to_path_buf(), realm, Box::new(fetcher));
        let error = cache.bytes("/https://evil.test/a.png").await.unwrap_err();
        assert!(error.to_string().contains("不属于本组织"), "{error}");
    }

    #[tokio::test]
    async fn store_failure_still_returns_bytes_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        // 缓存目录的位置是一个文件：无法创建目录
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"x").unwrap();
        let (cache, _, _) = cache(blocked.clone());
        assert_eq!(cache.bytes("/a.png").await.unwrap(), [1, 2, 3]);
        assert!(cache.file("/a.png").await.is_err());
        assert_eq!(cache.in_flight_count(), 0);
        assert!(blocked.is_file());
    }

    #[tokio::test]
    async fn invalid_url_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (cache, calls, _) = cache(dir.path().to_path_buf());
        assert!(cache.bytes("http://[bad").await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
