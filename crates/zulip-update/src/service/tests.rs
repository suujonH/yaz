use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::layout::BACKUP_DIR_NAME;
use crate::manifest::MANIFEST_FILE_NAME;
use crate::state::MAX_FAILURES;
use crate::testing::{version_files, zip_package};

type Responses = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// 本地的假 Releases：按路径返回内容，没有的路径返回 404。
struct FakeFeed {
    base: String,
    responses: Responses,
    requests: Arc<Mutex<Vec<String>>>,
    offline: Arc<AtomicBool>,
}

impl FakeFeed {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let feed = Self {
            base: format!("http://127.0.0.1:{port}/releases/"),
            responses: Responses::default(),
            requests: Arc::default(),
            offline: Arc::default(),
        };
        let (responses, requests, offline) = (
            feed.responses.clone(),
            feed.requests.clone(),
            feed.offline.clone(),
        );
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                if !offline.load(Ordering::SeqCst) {
                    tokio::spawn(serve(socket, responses.clone(), requests.clone()));
                }
            }
        });
        feed
    }

    fn respond(&self, relative: &str, body: impl Into<Vec<u8>>) {
        self.responses
            .lock()
            .insert(format!("/releases/{relative}"), body.into());
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().clone()
    }

    fn count_requests(&self, suffix: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.ends_with(suffix))
            .count()
    }
}

async fn serve(mut socket: TcpStream, responses: Responses, requests: Arc<Mutex<Vec<String>>>) {
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&buffer[..n]),
        }
    }
    let request = String::from_utf8_lossy(&request);
    let path = request.split_whitespace().nth(1).unwrap_or_default();
    requests.lock().push(path.to_string());
    let (status, body) = match responses.lock().get(path) {
        Some(body) => ("200 OK", body.clone()),
        None => ("404 Not Found", Vec::new()),
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(header.as_bytes()).await;
    let _ = socket.write_all(&body).await;
    let _ = socket.shutdown().await;
}

#[derive(Default)]
struct TestHost {
    idle: Mutex<Duration>,
    exits: AtomicUsize,
    logs: Mutex<Vec<(LogLevel, String)>>,
    statuses: Mutex<Vec<UpdateStatus>>,
}

impl UpdateHost for TestHost {
    fn idle_time(&self) -> Duration {
        *self.idle.lock()
    }

    fn exit_for_update(&self) {
        self.exits.fetch_add(1, Ordering::SeqCst);
    }

    fn log(&self, level: LogLevel, message: &str) {
        self.logs.lock().push((level, message.to_string()));
    }

    fn status_changed(&self, status: &UpdateStatus) {
        self.statuses.lock().push(status.clone());
    }
}

struct Harness {
    temp: TempDir,
    feed: FakeFeed,
    host: Arc<TestHost>,
    layout: UpdateLayout,
}

impl Harness {
    async fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temp.path().join("update"));
        fs::create_dir_all(temp.path().join("app")).unwrap();
        Self {
            temp,
            feed: FakeFeed::start().await,
            host: Arc::default(),
            layout,
        }
    }

    fn app_dir(&self) -> PathBuf {
        self.temp.path().join("app")
    }

    fn service(&self, current: &str) -> UpdateService {
        self.service_with(current, true)
    }

    fn service_with(&self, current: &str, self_update: bool) -> UpdateService {
        let mut config = UpdateConfig::new(
            parse_version(current).unwrap(),
            self.app_dir(),
            self.temp.path().join("updater.log"),
        );
        config.releases_url = self.feed.base.clone();
        config.layout = self.layout.clone();
        config.self_update = Some(self_update);
        config.startup_delay = Duration::from_millis(10);
        config.idle_threshold = Duration::from_millis(50);
        config.idle_poll = Duration::from_millis(5);
        // 不经过系统代理，直接访问本地服务
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        UpdateService::with_client(config, self.host.clone(), http)
    }

    fn publish(&self, version: &str) -> UpdateManifest {
        let (zip, manifest) = zip_package(version, &version_files(version, &[]));
        self.publish_manifest(&manifest);
        self.feed
            .respond(&format!("download/v{version}/{}", manifest.package), zip);
        manifest
    }

    fn publish_manifest(&self, manifest: &UpdateManifest) {
        self.feed.respond(
            &format!("latest/download/{MANIFEST_FILE_NAME}"),
            manifest.to_json(),
        );
    }

    fn state(&self) -> UpdateState {
        UpdateState::load(&self.layout.state_path())
    }

    fn stage(&self, version: &str) {
        fs::create_dir_all(self.layout.staged_dir()).unwrap();
        let manifest = UpdateManifest {
            version: version.into(),
            package: "p.zip".into(),
            sha256: "a".repeat(64),
            size: 1,
        };
        fs::write(self.layout.ready_path(), manifest.to_json()).unwrap();
    }

    async fn status_of(&self, service: &UpdateService, manual: bool) -> UpdateCheckStatus {
        service.check(manual).await.status
    }
}

async fn wait_until(condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < Duration::from_secs(10), "等待条件超时");
        sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn same_or_older_release_is_up_to_date() {
    let h = Harness::new().await;
    h.publish("1.0.0");
    assert_eq!(
        h.status_of(&h.service("1.0.0"), false).await,
        UpdateCheckStatus::UpToDate
    );
    assert_eq!(
        h.status_of(&h.service("2.0.0"), false).await,
        UpdateCheckStatus::UpToDate
    );
    assert!(!h.layout.ready_path().exists());
}

#[tokio::test]
async fn prerelease_current_is_older_than_its_release() {
    let h = Harness::new().await;
    h.publish("1.0.0");
    assert_eq!(
        h.status_of(&h.service("1.0.0-beta.1"), false).await,
        UpdateCheckStatus::Ready
    );
}

#[tokio::test]
async fn missing_release_is_reported() {
    let h = Harness::new().await;
    assert_eq!(
        h.status_of(&h.service("1.0.0"), true).await,
        UpdateCheckStatus::NoRelease
    );
}

#[tokio::test]
async fn network_failure_does_not_count_against_the_version() {
    let h = Harness::new().await;
    h.publish("1.1.0");
    h.feed.offline.store(true, Ordering::SeqCst);

    assert_eq!(
        h.status_of(&h.service("1.0.0"), false).await,
        UpdateCheckStatus::Failed
    );
    assert_eq!(h.state().failures, 0);
}

#[tokio::test]
async fn invalid_manifest_fails_without_download() {
    let h = Harness::new().await;
    h.feed
        .respond(&format!("latest/download/{MANIFEST_FILE_NAME}"), "not json");

    assert_eq!(
        h.status_of(&h.service("1.0.0"), false).await,
        UpdateCheckStatus::Failed
    );
    assert_eq!(h.feed.requests().len(), 1);
    assert_eq!(h.state().failures, 0);
}

#[tokio::test]
async fn newer_release_is_downloaded_verified_and_staged() {
    let h = Harness::new().await;
    let manifest = h.publish("1.1.0");
    let service = h.service("1.0.0");

    let result = service.check(false).await;

    assert_eq!(result.status, UpdateCheckStatus::Ready);
    let ready = fs::read_to_string(h.layout.ready_path()).unwrap();
    assert_eq!(UpdateManifest::parse(&ready), Some(manifest));
    verify_tree(&h.layout.staged_dir()).unwrap();
    assert!(!h.layout.work_dir().exists());
    assert!(service.status().text.contains("1.1.0"));
    assert!(!service.status().busy);
    let statuses = h.host.statuses.lock().clone();
    assert!(statuses.first().is_some_and(|s| s.busy));
    assert!(statuses.iter().any(|s| s.text.contains("100%")));
}

#[tokio::test]
async fn already_staged_version_is_not_downloaded_again() {
    let h = Harness::new().await;
    let manifest = h.publish("1.1.0");
    h.service("1.0.0").check(false).await;
    assert_eq!(h.feed.count_requests(&manifest.package), 1);

    assert_eq!(
        h.status_of(&h.service("1.0.0"), false).await,
        UpdateCheckStatus::Ready
    );
    assert_eq!(h.feed.count_requests(&manifest.package), 1);
}

#[tokio::test]
async fn without_self_update_only_reports_the_new_version() {
    let h = Harness::new().await;
    h.publish("1.1.0");

    let result = h.service_with("1.0.0", false).check(true).await;

    assert_eq!(result.status, UpdateCheckStatus::ManualOnly);
    assert!(result.message.contains("1.1.0"));
    assert!(!h.layout.ready_path().exists());
}

#[tokio::test]
async fn hash_mismatch_is_not_staged_and_stops_after_max_failures() {
    let h = Harness::new().await;
    let manifest = h.publish("1.1.0");
    // 大小相同内容不同：SHA-256 不符
    h.feed.respond(
        &format!("download/v1.1.0/{}", manifest.package),
        vec![0u8; 10],
    );
    h.publish_manifest(&UpdateManifest {
        size: 10,
        ..manifest
    });

    for _ in 0..MAX_FAILURES {
        assert_eq!(
            h.status_of(&h.service("1.0.0"), false).await,
            UpdateCheckStatus::Failed
        );
    }

    assert!(!h.layout.ready_path().exists());
    assert!(!h.layout.staged_dir().exists());
    assert_eq!(h.state().failures, MAX_FAILURES);

    let before = h.feed.requests().len();
    assert_eq!(
        h.status_of(&h.service("1.0.0"), false).await,
        UpdateCheckStatus::Skipped
    );
    assert_eq!(h.feed.requests().len(), before + 1);
}

#[tokio::test]
async fn package_larger_than_manifest_is_rejected() {
    let h = Harness::new().await;
    let manifest = h.publish("1.1.0");
    h.publish_manifest(&UpdateManifest {
        size: 5,
        ..manifest
    });

    assert_eq!(
        h.status_of(&h.service("1.0.0"), false).await,
        UpdateCheckStatus::Failed
    );
    assert_eq!(h.state().failures, 1);
}

#[tokio::test]
async fn blocked_version_is_skipped_automatically_but_manual_check_retries() {
    let h = Harness::new().await;
    h.publish("1.1.0");
    UpdateState::default()
        .with_failure("1.1.0", "启动失败", true)
        .save(&h.layout.state_path())
        .unwrap();

    assert_eq!(
        h.status_of(&h.service("1.0.0"), false).await,
        UpdateCheckStatus::Skipped
    );
    assert!(!h.layout.ready_path().exists());

    assert_eq!(
        h.status_of(&h.service("1.0.0"), true).await,
        UpdateCheckStatus::Ready
    );
    assert!(h.state().allows_auto_attempt("1.1.0"));
}

#[tokio::test]
async fn higher_release_than_blocked_version_is_attempted() {
    let h = Harness::new().await;
    h.publish("1.2.0");
    UpdateState::default()
        .with_failure("1.1.0", "启动失败", true)
        .save(&h.layout.state_path())
        .unwrap();

    assert_eq!(
        h.status_of(&h.service("1.0.0"), false).await,
        UpdateCheckStatus::Ready
    );
}

#[tokio::test]
async fn last_error_reports_recorded_failure() {
    let h = Harness::new().await;
    let service = h.service("1.0.0");
    assert_eq!(service.last_error(), None);

    UpdateState::default()
        .with_failure("1.1.0", "启动失败", true)
        .save(&h.layout.state_path())
        .unwrap();
    assert_eq!(service.last_error().as_deref(), Some("启动失败"));
}

#[tokio::test]
async fn concurrent_checks_share_one_run() {
    let h = Harness::new().await;
    h.publish("1.0.0");
    let service = h.service("1.0.0");

    let (a, b) = tokio::join!(service.check(false), service.check(true));

    assert_eq!(a, b);
    assert_eq!(h.feed.count_requests(MANIFEST_FILE_NAME), 1);
    // 结束后可以再次检查
    service.check(false).await;
    assert_eq!(h.feed.count_requests(MANIFEST_FILE_NAME), 2);
}

#[tokio::test]
async fn ready_update_requests_exit_once_when_user_is_idle() {
    let h = Harness::new().await;
    h.publish("1.1.0");
    let service = h.service("1.0.0");

    assert_eq!(service.check(false).await.status, UpdateCheckStatus::Ready);
    sleep(Duration::from_millis(100)).await;
    assert_eq!(h.host.exits.load(Ordering::SeqCst), 0);

    *h.host.idle.lock() = Duration::from_secs(1);
    wait_until(|| h.host.exits.load(Ordering::SeqCst) == 1).await;

    // 再次检查得到同一个已准备好的版本，不会重复请求退出
    assert_eq!(service.check(true).await.status, UpdateCheckStatus::Ready);
    sleep(Duration::from_millis(100)).await;
    assert_eq!(h.host.exits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shutdown_stops_waiting_for_idle() {
    let h = Harness::new().await;
    h.publish("1.1.0");
    let service = h.service("1.0.0");
    service.check(false).await;

    service.shutdown();
    *h.host.idle.lock() = Duration::from_secs(1);

    sleep(Duration::from_millis(150)).await;
    assert_eq!(h.host.exits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn startup_check_runs_once_unless_shut_down() {
    let h = Harness::new().await;
    h.publish("1.0.0");
    let service = h.service("1.0.0");

    service.check_after_startup().await;
    assert_eq!(h.feed.count_requests(MANIFEST_FILE_NAME), 1);

    service.shutdown();
    service.check_after_startup().await;
    assert_eq!(h.feed.count_requests(MANIFEST_FILE_NAME), 1);
}

#[tokio::test]
async fn staged_update_is_not_applied_when_app_cannot_self_update_or_during_probe() {
    let h = Harness::new().await;
    h.stage("1.1.0");

    assert!(!h.service_with("1.0.0", false).try_apply_staged(&[]));
    let probe = vec![PROBE_ARG.to_string(), "x".to_string()];
    assert!(!h.service("1.0.0").try_apply_staged(&probe));
    assert!(h.layout.ready_path().exists());
}

#[tokio::test]
async fn no_staged_version_means_nothing_to_apply() {
    let h = Harness::new().await;
    assert!(!h.service("1.0.0").try_apply_staged(&[]));
}

#[tokio::test]
async fn staged_version_no_longer_needed_is_discarded() {
    let h = Harness::new().await;
    h.stage("1.0.0");

    assert!(!h.service("1.0.0").try_apply_staged(&[]));

    assert!(!h.layout.ready_path().exists());
    assert!(!h.layout.staged_dir().exists());
}

#[tokio::test]
async fn staged_blocked_version_is_discarded() {
    let h = Harness::new().await;
    h.stage("1.1.0");
    UpdateState::default()
        .with_failure("1.1.0", "x", true)
        .save(&h.layout.state_path())
        .unwrap();

    assert!(!h.service("1.0.0").try_apply_staged(&[]));
    assert!(!h.layout.ready_path().exists());
}

#[tokio::test]
async fn staged_update_starts_updater_copy_from_runner_dir() {
    let h = Harness::new().await;
    h.stage("1.1.0");
    let service = h.service("1.0.0");

    // 没有更新器时启动失败，调用方继续运行
    assert!(!service.try_apply_staged(&[]));
    assert!(
        h.host
            .logs
            .lock()
            .iter()
            .any(|(level, _)| *level == LogLevel::Error)
    );
    assert!(h.layout.ready_path().exists());

    // 用 ping.exe 充当更新器：参数不认识，启动后立即退出
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    fs::copy(
        Path::new(&system).join(r"System32\ping.exe"),
        h.app_dir().join(UPDATER_EXE_NAME),
    )
    .unwrap();
    assert!(service.try_apply_staged(&[]));
    assert!(h.layout.runner_dir().join(UPDATER_EXE_NAME).is_file());
}

#[tokio::test]
async fn start_updater_requires_a_ready_version() {
    let h = Harness::new().await;
    assert!(!h.service("1.0.0").start_updater());
}

#[tokio::test]
async fn probe_reports_version_without_touching_leftovers() {
    let h = Harness::new().await;
    let probe = h.temp.path().join("started.txt");
    let backup = h.app_dir().join(BACKUP_DIR_NAME);
    fs::create_dir_all(&backup).unwrap();

    h.service("1.1.0")
        .on_started(&[PROBE_ARG.to_string(), probe.to_string_lossy().into_owned()]);

    assert_eq!(fs::read_to_string(probe).unwrap(), "1.1.0");
    assert!(backup.exists());
}

#[tokio::test]
async fn normal_start_removes_leftover_backup_and_consumes_report() {
    let h = Harness::new().await;
    let backup = h.app_dir().join(BACKUP_DIR_NAME);
    fs::create_dir_all(&backup).unwrap();
    fs::create_dir_all(h.layout.runner_dir()).unwrap();
    UpdateState {
        pending_report: Some("结果".into()),
        ..UpdateState::default()
    }
    .save(&h.layout.state_path())
    .unwrap();

    h.service("1.0.0").on_started(&[]);

    assert!(!backup.exists());
    assert!(!h.layout.runner_dir().exists());
    assert_eq!(h.state().pending_report, None);
    assert!(
        h.host
            .logs
            .lock()
            .iter()
            .any(|(_, message)| message.contains("结果"))
    );
}

#[tokio::test]
async fn self_update_requires_release_layout() {
    let h = Harness::new().await;
    let mut config = UpdateConfig::new(
        parse_version("1.0.0").unwrap(),
        h.app_dir(),
        h.temp.path().join("updater.log"),
    );
    config.self_update = None;
    let service = UpdateService::with_client(config, h.host.clone(), reqwest::Client::new());

    // 调试构建或缺少更新器与文件清单时都不能自动更新
    assert!(!service.can_self_update());
}
