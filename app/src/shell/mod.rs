//! Window shell: borderless window, title bar, resizing, theme, multiple windows.

mod bundled;
mod caption;
mod local_files;
mod spare;
mod state;
mod theme;
mod window;

pub use bundled::language_icon_url;
pub use caption::{CaptionBar, DragRegion};
pub use local_files::url_for;
pub use spare::{create_spare, use_presented};
pub use state::AppShared;
pub use theme::{
    ColorScheme, PRESETS, Palette, ThemeConfig, ThemeContext, ThemeHub, ThemeProvider, use_theme,
};
pub use window::{
    ShowWhenReady, WindowBoundsStore, WindowKey, WindowSpec, activate, close_all_windows,
    close_window, local_app_data, move_window, open_singleton, open_with_args, prepare_with_args,
    use_save_window_bounds, use_window_args, window_config,
};

use crate::ui::OverlayHost;
use dioxus::desktop::tao::window::ResizeDirection;
use dioxus::desktop::{DesktopService, use_window};
use dioxus::prelude::*;

/// Whether the window is tucked into the screen edge (its position is not saved then).
pub(crate) fn edge_hidden(window: &DesktopService) -> bool {
    crate::platform::is_hidden_at_edge(window)
}

/// Window shell: theme root, overlays, resize areas, title bar. The root component of every window
/// wraps its content with it.
///
/// When `caption_overlay` is true the title bar floats above the content (the content extends to
/// the top edge of the window).
#[component]
pub fn WindowShell(
    #[props(default, into)] title: String,
    #[props(default)] caption_overlay: bool,
    /// Extra style classes on `.window`.
    #[props(default)]
    class: &'static str,
    #[props(default = true)] resizable: bool,
    /// Action of the close button; closes the window by default.
    on_close: Option<EventHandler<()>>,
    /// Content on the left of the title bar (inside the drag area).
    caption_left: Option<Element>,
    /// Content to the left of the minimize button.
    caption_right: Option<Element>,
    children: Element,
) -> Element {
    use_save_window_bounds();
    crate::state::use_provide_app_state();
    // A window hidden at first is shown after its content mounts
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

/// Resize areas for the top edge and the two top corners. The WebView covers the window's client
/// area, so the system does not get hit tests there; left, right and bottom fall on the window's
/// invisible border, which the system handles directly.
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
