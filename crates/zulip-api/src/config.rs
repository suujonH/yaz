use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;

use crate::trace::TraceSink;

/// Default client name, also the product name.
pub const CLIENT_NAME: &str = "YaZ";

/// Client name of the Zulip desktop app (Electron).
pub const DESKTOP_CLIENT_NAME: &str = "ZulipElectron";

/// Client name of the Zulip web app (the server records all requests from browsers as website).
pub const WEB_CLIENT_NAME: &str = "website";

// The server rejects desktop versions below 5.4.3, so the reported version must be new enough
const DESKTOP_VERSION: &str = "5.12.1";

/// User-Agent for a client name: this app and the desktop app carry a version, other names are used
/// as-is; empty means this app.
///
/// The server takes the part before the first "/" or space as the client name and records it as the
/// message's send source and in the user's activity log.
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

/// Settings shared by all requests in the same process: User-Agent and request trace. Changes take
/// effect from the next request.
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

    /// Process-wide settings, used by default by [`ZulipClient::new`](crate::ZulipClient::new).
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<ClientConfig>> = OnceLock::new();
        GLOBAL.get_or_init(Self::new).clone()
    }

    /// Update the User-Agent from the client name in the settings; see [`user_agent_for`].
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
