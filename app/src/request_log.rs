//! 开发者模式的请求记录：所有请求共用的记录、变化通知、设置同步。

use crate::services::services;
use std::sync::{Arc, LazyLock, OnceLock};
use tokio::sync::watch;
use zulip_api::{ClientConfig, RequestLog, TraceSink};
use zulip_store::LocalSettings;

static LOG: OnceLock<Arc<RequestLog>> = OnceLock::new();
static CHANGES: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::Sender::new(0));

/// 请求记录的接入点：把记录交给所有请求共用的客户端配置。
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

/// 记录变化的通知。
pub fn subscribe_request_log() -> watch::Receiver<u64> {
    CHANGES.subscribe()
}

/// 让请求记录使用设置中的开关和条数。
pub fn sync_request_log(settings: &LocalSettings) {
    request_log().configure(settings.log_requests(), settings.request_log_limit());
}
