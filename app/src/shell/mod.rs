//! 窗口外壳：无边框窗口、标题栏、调整大小、主题、多窗口。

mod bundled;
mod caption;
mod local_files;
mod state;
mod theme;
mod window;

pub use bundled::language_icon_url;
pub use caption::{CaptionBar, DragRegion};
pub use local_files::url_for;
pub use state::AppShared;
pub use theme::{
    ColorScheme, PRESETS, Palette, ThemeConfig, ThemeContext, ThemeHub, ThemeProvider, use_theme,
};
pub use window::{
    ShowWhenReady, WindowBoundsStore, WindowKey, WindowSpec, activate, close_all_windows,
    local_app_data, move_window, open_singleton, open_with_args, use_save_window_bounds,
    use_window_args, window_config,
};

use crate::ui::OverlayHost;
use dioxus::desktop::tao::window::ResizeDirection;
use dioxus::desktop::{DesktopService, use_window};
use dioxus::prelude::*;

/// 窗口是否缩进在屏幕边缘（此时不保存位置）。
pub(crate) fn edge_hidden(window: &DesktopService) -> bool {
    crate::platform::is_hidden_at_edge(window)
}

/// 窗口外壳：主题根、弹层、调整大小区域、标题栏。每个窗口的根组件都用它包住内容。
///
/// `caption_overlay` 为 true 时标题栏浮在内容上方（内容顶到窗口上沿）。
#[component]
pub fn WindowShell(
    #[props(default, into)] title: String,
    #[props(default)] caption_overlay: bool,
    /// `.window` 上的附加样式类。
    #[props(default)]
    class: &'static str,
    #[props(default = true)] resizable: bool,
    /// 关闭按钮的动作；缺省关闭窗口。
    on_close: Option<EventHandler<()>>,
    /// 标题栏左侧内容（在拖动区内）。
    caption_left: Option<Element>,
    /// 最小化按钮左边的内容。
    caption_right: Option<Element>,
    children: Element,
) -> Element {
    use_save_window_bounds();
    crate::state::use_provide_app_state();
    // 先隐藏的窗口在内容挂载后显示
    let window = use_window();
    let ready = try_use_context::<ShowWhenReady>().is_some_and(|ready| ready.0);
    use_effect(move || {
        if ready {
            activate(&window);
        }
    });
    rsx! {
        ThemeProvider {
            OverlayHost {
                div { class: if caption_overlay { "window caption-overlay {class}" } else { "window {class}" },
                    if resizable {
                        ResizeEdges {}
                    }
                    CaptionBar { title, resizable, on_close, left: caption_left, right: caption_right }
                    div { class: "window-body", {children} }
                }
            }
        }
    }
}

/// 上边和上方两角的调整大小区域。WebView 盖住了窗口的客户区，系统收不到这部分的命中测试；
/// 左、右、下落在窗口的不可见边框上，系统能直接处理。
#[component]
fn ResizeEdges() -> Element {
    let window = use_window();
    let edge = |class: &'static str, direction: ResizeDirection| {
        let window = window.clone();
        rsx! {
            div {
                class: "resize-edge {class}",
                onmousedown: move |e| {
                    e.stop_propagation();
                    if !window.is_maximized() {
                        let _ = window.drag_resize_window(direction);
                    }
                },
            }
        }
    };
    rsx! {
        {edge("n", ResizeDirection::North)}
        {edge("nw", ResizeDirection::NorthWest)}
        {edge("ne", ResizeDirection::NorthEast)}
    }
}
