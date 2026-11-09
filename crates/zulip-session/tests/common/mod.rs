// 每个测试文件只用其中一部分
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use test_server::{ME_EMAIL, TestServer};
use tokio::sync::broadcast;
use zulip_session::{Hub, PasswordLogin, Session, SessionEvent};
use zulip_store::{AppPaths, Db};

pub struct Fixture {
    pub server: TestServer,
    pub hub: Arc<Hub>,
    pub session: Arc<Session>,
    pub _dir: TempDir,
}

pub fn password_request(server: &TestServer) -> PasswordLogin {
    PasswordLogin {
        realm: server.url(),
        email: ME_EMAIL.to_owned(),
        password: "任意密码".to_owned(),
        allow_invalid_cert: false,
    }
}

pub fn new_hub(dir: &TempDir) -> Arc<Hub> {
    let paths = AppPaths::new(dir.path());
    let db = Db::open(&paths.database()).unwrap();
    Hub::new(db, paths, None)
}

pub async fn fixture() -> Fixture {
    let server = TestServer::start(0).await.unwrap();
    let dir = TempDir::new().unwrap();
    let hub = new_hub(&dir);
    let session = hub
        .login_with_password(password_request(&server))
        .await
        .unwrap();
    Fixture {
        server,
        hub,
        session,
        _dir: dir,
    }
}

/// 等到满足条件的事件出现。
pub async fn wait_for(
    rx: &mut broadcast::Receiver<SessionEvent>,
    what: &str,
    mut matches: impl FnMut(&SessionEvent) -> bool,
) -> SessionEvent {
    let wait = async {
        loop {
            match rx.recv().await {
                Ok(event) if matches(&event) => return event,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("事件通道关闭：{what}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(15), wait)
        .await
        .unwrap_or_else(|_| panic!("等待超时：{what}"))
}

/// 等到读接口的条件成立。
pub async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("等待超时：{what}");
}
