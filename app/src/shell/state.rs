//! 所有窗口共享的内容。

use super::theme::ThemeHub;
use super::window::WindowBoundsStore;
use std::sync::Arc;

/// 所有窗口共享的内容（放在每个 VirtualDom 的根上下文中）。
#[derive(Clone)]
pub struct AppShared {
    pub theme: ThemeHub,
    pub bounds: Arc<dyn WindowBoundsStore>,
}

impl AppShared {
    /// 窗口背景色在主题生效前使用：Windows 的应用主题设置。
    pub fn starts_dark(&self) -> bool {
        crate::platform::system_prefers_dark()
    }
}
