//! 窗口级按键：没有菜单、卡片、对话框打开时，Esc 关闭窗口，左右方向键交给页面。

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

// 捕获阶段先于弹层自己的 Esc 处理，所以这里同步检查弹层是否存在
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

/// Esc 关闭窗口；`on_arrow` 收左右方向键（焦点在输入框时不转发）。
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

// 捕获阶段判断，命中时输入框和页面都收不到这次按键。Key、Digit 按物理键取名，与按下 Shift 后的字符无关。
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

/// 绑定了菜单项的快捷键。
fn bound_gestures(rows: Vec<ShortcutRow>) -> Vec<String> {
    rows.into_iter()
        .map(|row| row.gesture)
        .filter(|gesture| !gesture.is_empty())
        .collect()
}

/// 插件菜单的键盘快捷键：主窗口和独立聊天窗口各调用一次。
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
