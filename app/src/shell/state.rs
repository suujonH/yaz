//! Content shared by all windows.

use super::theme::ThemeHub;
use super::window::WindowBoundsStore;
use std::sync::Arc;

/// Content shared by all windows (placed in the root context of every VirtualDom).
#[derive(Clone)]
pub struct AppShared {
    pub theme: ThemeHub,
    pub bounds: Arc<dyn WindowBoundsStore>,
}

impl AppShared {
    /// Window background color used before the theme takes effect: the Windows app theme setting.
    pub fn starts_dark(&self) -> bool {
        crate::platform::system_prefers_dark()
    }
}
