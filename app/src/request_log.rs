//! Developer-mode request log: the log shared by all requests, change notification, settings sync.

use crate::services::services;
use std::sync::{Arc, LazyLock, OnceLock};
use tokio::sync::watch;
use zulip_api::{ClientConfig, RequestLog, TraceSink};
use zulip_store::LocalSettings;

static LOG: OnceLock<Arc<RequestLog>> = OnceLock::new();
static CHANGES: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::Sender::new(0));

/// Hook-in point of the request log: hands the log to the client config shared by all requests.
pub fn install_request_log() {
    LOG.get_or_init(|| {
        let settings = &services().settings;
        let log = RequestLog::new(settings.log_requests(), settings.request_log_limit());
        log.set_on_change(Some(Arc::new(|| {
            CHANGES.send_modify(|n| *n += 1);
        })));
        ClientConfig::global().set_trace_sink(Some(log.clone() as Arc<dyn TraceSink>));
        log
    });
}

pub fn request_log() -> Arc<RequestLog> {
    install_request_log();
    LOG.get().cloned().expect("请求记录已安装")
}

/// Notification of log changes.
pub fn subscribe_request_log() -> watch::Receiver<u64> {
    CHANGES.subscribe()
}

/// Makes the request log use the switch and count from the settings.
pub fn sync_request_log(settings: &LocalSettings) {
    request_log().configure(settings.log_requests(), settings.request_log_limit());
}
