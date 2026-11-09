use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;

use crate::trace::TraceSink;

/// 默认的客户端名称，也是产品名。
pub const CLIENT_NAME: &str = "YaZ";

/// Zulip 桌面版（Electron）的客户端名称。
pub const DESKTOP_CLIENT_NAME: &str = "ZulipElectron";

/// Zulip 网页版的客户端名称（服务器把浏览器发来的请求都记为 website）。
pub const WEB_CLIENT_NAME: &str = "website";

// 服务器拒绝低于 5.4.3 的桌面版，报告的版本要足够新
const DESKTOP_VERSION: &str = "5.12.1";

/// 客户端名称对应的 User-Agent：本程序和桌面版带上版本，其它名称原样使用；为空时是本程序。
///
/// 服务器取第一个“/”或空格之前的部分作为客户端名称，记入消息的发送来源和用户的活动记录。
pub fn user_agent_for(client_name: &str) -> String {
    let name = single_line(client_name.trim());
    match name.as_str() {
        "" | CLIENT_NAME => format!("{CLIENT_NAME}/{}", env!("CARGO_PKG_VERSION")),
        DESKTOP_CLIENT_NAME => format!("{DESKTOP_CLIENT_NAME}/{DESKTOP_VERSION}"),
        _ => name,
    }
}

fn single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(' ');
            }
            '\n' | '\u{85}' | '\u{2028}' | '\u{2029}' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// 同一个进程中所有请求共用的设置：User-Agent 和请求记录。修改后从下一个请求起生效。
pub struct ClientConfig {
    user_agent: RwLock<String>,
    trace: RwLock<Option<Arc<dyn TraceSink>>>,
}

impl ClientConfig {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            user_agent: RwLock::new(user_agent_for("")),
            trace: RwLock::new(None),
        })
    }

    /// 进程级的设置，[`ZulipClient::new`](crate::ZulipClient::new) 默认使用它。
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<ClientConfig>> = OnceLock::new();
        GLOBAL.get_or_init(Self::new).clone()
    }

    /// 按设置中的客户端名称更新 User-Agent，见 [`user_agent_for`]。
    pub fn set_client_name(&self, name: &str) {
        *self.user_agent.write() = user_agent_for(name);
    }

    pub fn user_agent(&self) -> String {
        self.user_agent.read().clone()
    }

    pub fn set_trace_sink(&self, sink: Option<Arc<dyn TraceSink>>) {
        *self.trace.write() = sink;
    }

    pub(crate) fn active_trace_sink(&self) -> Option<Arc<dyn TraceSink>> {
        self.trace.read().clone().filter(|sink| sink.is_enabled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_client_name_has_version() {
        for name in ["", "  ", "YaZ"] {
            assert!(user_agent_for(name).starts_with("YaZ/"), "{name:?}");
        }
    }

    #[test]
    fn desktop_has_supported_version() {
        assert_eq!(user_agent_for("ZulipElectron"), "ZulipElectron/5.12.1");
    }

    #[test]
    fn web_is_website() {
        assert_eq!(user_agent_for("website"), "website");
    }

    #[test]
    fn custom_is_trimmed_and_single_line() {
        assert_eq!(user_agent_for("  MyBot\na "), "MyBot a");
        assert_eq!(user_agent_for("A\r\nB\rC"), "A B C");
    }

    #[test]
    fn config_applies_new_name() {
        let config = ClientConfig::new();
        assert!(config.user_agent().starts_with("YaZ/"));
        config.set_client_name("website");
        assert_eq!(config.user_agent(), "website");
    }
}
