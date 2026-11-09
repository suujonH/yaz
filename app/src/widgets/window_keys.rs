//! Window-level keys: with no menu, card, or dialog open, Esc closes the window and the left/right
//! arrow keys go to the page.

use crate::services::plugins;
use crate::state::use_plugin_events;
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use zulip_plugin_host::{PluginEvent, ShortcutRow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowKey {
    Left,
    Right,
}

// The capture phase runs before a popup's own Esc handling, so check synchronously here whether a
// popup exists
const KEYS_JS: &str = r#"
window.zulipWindowKeys?.abort();
const control = new AbortController();
window.zulipWindowKeys = control;
document.addEventListener("keydown", (e) => {
  if (e.isComposing || e.ctrlKey || e.altKey || e.metaKey || e.shiftKey) return;
  if (document.querySelector(".dialog-layer, .overlay-card, .menu")) return;
  const typing = e.target instanceof Element && e.target.closest("input, textarea, [contenteditable=true]");
  if (e.key === "Escape" || (!typing && (e.key === "ArrowLeft" || e.key === "ArrowRight"))) dioxus.send(e.key);
}, { capture: true, signal: control.signal });
await new Promise(() => {});
"#;

/// Esc closes the window; `on_arrow` receives the left/right arrow keys (not forwarded when focus
/// is in the composer).
pub fn use_window_keys(on_arrow: impl FnMut(ArrowKey) + 'static) {
    let window = use_window();
    let on_arrow = use_callback(on_arrow);
    use_future(move || {
        let window = window.clone();
        async move {
            let mut eval = document::eval(KEYS_JS);
            while let Ok(key) = eval.recv::<String>().await {
                match key.as_str() {
                    "Escape" => window.close(),
                    "ArrowLeft" => on_arrow.call(ArrowKey::Left),
                    "ArrowRight" => on_arrow.call(ArrowKey::Right),
                    _ => {}
                }
            }
        }
    });
}

// Decided in the capture phase; on a hit, neither the composer nor the page receives this key
// press. Key and Digit are named by physical key, independent of the character produced with Shift
// held.
const PLUGIN_KEYS_JS: &str = r#"
window.zulipPluginKeys?.abort();
const control = new AbortController();
window.zulipPluginKeys = control;
let bound = new Set();
const keyName = (code) => {
  const named = /^(?:Key|Digit|Numpad)([A-Z0-9])$/.exec(code);
  return named ? named[1] : /^F\d+$/.test(code) ? code : null;
};
document.addEventListener("keydown", (e) => {
  if (e.isComposing || e.repeat || e.metaKey) return;
  if (document.querySelector(".dialog-layer")) return;
  if (e.target instanceof Element && e.target.closest(".shortcut-recorder")) return;
  const key = keyName(e.code);
  if (!key) return;
  const gesture = (e.ctrlKey ? "Ctrl+" : "") + (e.altKey ? "Alt+" : "") + (e.shiftKey ? "Shift+" : "") + key;
  if (bound.has(gesture)) {
    e.preventDefault();
    e.stopPropagation();
    dioxus.send(gesture);
  }
}, { capture: true, signal: control.signal });
for (;;) { bound = new Set(await dioxus.recv()); }
"#;

/// Shortcut bound to a menu item.
fn bound_gestures(rows: Vec<ShortcutRow>) -> Vec<String> {
    rows.into_iter()
        .map(|row| row.gesture)
        .filter(|gesture| !gesture.is_empty())
        .collect()
}

/// Keyboard shortcuts of plugin menus: called once each by the main window and standalone chat
/// windows.
pub fn use_plugin_key_shortcuts() {
    let eval = use_hook(|| document::eval(PLUGIN_KEYS_JS));
    let send_bound = move || {
        if let Err(error) = eval.send(bound_gestures(plugins().key_shortcuts())) {
            log::warn!("更新快捷键失败：{error}");
        }
    };
    use_plugin_events(move |event| {
        if matches!(event, PluginEvent::PluginsChanged) {
            send_bound();
        }
    });
    use_future(move || async move {
        send_bound();
        let mut eval = eval;
        while let Ok(gesture) = eval.recv::<String>().await {
            plugins().invoke_key_shortcut(&gesture);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bound_rows_are_gestures() {
        let row = |gesture: &str| ShortcutRow {
            key: String::new(),
            title: String::new(),
            is_mouse: false,
            gesture: gesture.to_owned(),
        };
        assert_eq!(
            bound_gestures(vec![row(""), row("Ctrl+Shift+E"), row("F5")]),
            ["Ctrl+Shift+E", "F5"]
        );
    }
}
