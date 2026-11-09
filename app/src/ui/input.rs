//! 文字输入：输入框、行内编辑、快捷键录制。

use super::icon::{Icon, IconName};
use super::next_dom_id;
use dioxus::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputStyle {
    /// 白底灰边框，聚焦时品牌色边框。
    #[default]
    Normal,
    /// 灰底圆角搜索框。
    Search,
    /// 同搜索框，聚焦时品牌色边框。
    SearchRing,
}

#[component]
pub fn TextInput(
    #[props(into)] value: String,
    #[props(default, into)] placeholder: String,
    #[props(default)] style_kind: InputStyle,
    #[props(default)] password: bool,
    #[props(default)] multiline: bool,
    #[props(default = 3)] rows: u32,
    #[props(default)] disabled: bool,
    #[props(default)] invalid: bool,
    /// 有内容时显示清除按钮。
    #[props(default)]
    clearable: bool,
    #[props(default)] autofocus: bool,
    /// 自动获得焦点时光标放在最前面（缺省在末尾）。
    #[props(default)]
    caret_at_start: bool,
    icon: Option<IconName>,
    oninput: Option<EventHandler<String>>,
    /// 按下回车（多行时为 Ctrl+回车）。
    onsubmit: Option<EventHandler<String>>,
    onblur: Option<EventHandler<()>>,
) -> Element {
    let class = {
        let mut c = String::from(match style_kind {
            InputStyle::Normal => "input",
            InputStyle::Search => "input search",
            InputStyle::SearchRing => "input search ring",
        });
        if disabled {
            c.push_str(" disabled");
        }
        if invalid {
            c.push_str(" invalid");
        }
        c
    };
    let id = use_hook(next_dom_id);
    let focus_id = id.clone();
    // autofocus 属性对动态插入的元素不生效。弹出卡片定位前不可见（visibility: hidden），
    // 这时拿不到焦点：每帧重试，直到拿到焦点（最多约 20 帧）
    let focus_if_needed = move |_: MountedEvent| {
        if autofocus {
            document::eval(&format!(
                r#"(() => {{
                    const el = document.getElementById({focus_id:?});
                    let tries = 0;
                    const go = () => {{
                        if (!el || !el.isConnected) return;
                        el.focus({{ preventScroll: true }});
                        if (document.activeElement !== el) {{
                            if (++tries < 20) requestAnimationFrame(go);
                        }} else if ({caret_at_start}) {{
                            el.setSelectionRange(0, 0);
                        }}
                    }};
                    go();
                }})()"#
            ));
        }
    };
    let handle_input = move |e: FormEvent| {
        if let Some(h) = oninput {
            h.call(e.value());
        }
    };
    let submit_value = value.clone();
    let handle_key = move |e: KeyboardEvent| {
        let submit = e.key() == Key::Enter && (!multiline || e.modifiers().ctrl());
        if submit
            && !e.is_composing()
            && let Some(h) = onsubmit
        {
            h.call(submit_value.clone());
        }
    };
    rsx! {
        div { class,
            if let Some(name) = icon {
                span { class: "adorn", Icon { name, size: 14.0 } }
            }
            if multiline {
                textarea {
                    value: "{value}",
                    placeholder: "{placeholder}",
                    rows: "{rows}",
                    disabled,
                    onmounted: focus_if_needed,
                    oninput: handle_input,
                    onkeydown: handle_key,
                    onblur: move |_| {
                        if let Some(h) = onblur {
                            h.call(());
                        }
                    },
                }
            } else {
                input {
                    id: "{id}",
                    r#type: if password { "password" } else { "text" },
                    value: "{value}",
                    placeholder: "{placeholder}",
                    disabled,
                    spellcheck: "false",
                    onmounted: focus_if_needed,
                    oninput: handle_input,
                    onkeydown: handle_key,
                    onblur: move |_| {
                        if let Some(h) = onblur {
                            h.call(());
                        }
                    },
                }
            }
            if clearable && !value.is_empty() {
                button {
                    class: "input-clear",
                    r#type: "button",
                    tabindex: "-1",
                    onclick: move |_| {
                        if let Some(h) = oninput {
                            h.call(String::new());
                        }
                    },
                    Icon { name: IconName::Close, size: 12.0 }
                }
            }
        }
    }
}

/// 行内编辑框：出现时获得焦点并全选；回车或失去焦点时提交，Esc 取消。每次编辑只触发一次。
#[component]
pub fn InlineEdit(
    #[props(into)] value: String,
    oncommit: EventHandler<String>,
    oncancel: EventHandler<()>,
) -> Element {
    let id = use_hook(next_dom_id);
    let mut text = use_signal(|| value.clone());
    let mut finished = use_signal(|| false);
    let mut commit = move || {
        if !finished() {
            finished.set(true);
            oncommit.call(text());
        }
    };
    let select_id = id.clone();
    rsx! {
        input {
            id: "{id}",
            class: "inline-edit",
            value: "{text}",
            spellcheck: "false",
            onmounted: move |_| {
                document::eval(&format!("window.zulipUi.selectInput({select_id:?})"));
            },
            oninput: move |e| text.set(e.value()),
            onblur: move |_| commit(),
            onkeydown: move |e| {
                if e.is_composing() {
                    return;
                }
                match e.key() {
                    Key::Enter => {
                        e.prevent_default();
                        commit();
                    }
                    Key::Escape => {
                        e.prevent_default();
                        if !finished() {
                            finished.set(true);
                            oncancel.call(());
                        }
                    }
                    _ => {}
                }
            },
        }
    }
}

/// 录制按键组合的只读输入框：获得焦点后按组合键录制，Backspace / Delete 清除，Esc 结束。
#[component]
pub fn ShortcutRecorder(
    #[props(into)] value: String,
    onchange: EventHandler<String>,
    #[props(default = "按下组合键".to_string(), into)] prompt: String,
) -> Element {
    let mut waiting = use_signal(|| false);
    let shown = if waiting() && value.is_empty() {
        prompt.as_str()
    } else {
        value.as_str()
    };
    rsx! {
        div { class: "input shortcut-recorder",
            input {
                r#type: "text",
                readonly: true,
                value: "{shown}",
                placeholder: "{prompt}",
                onfocus: move |_| waiting.set(true),
                onblur: move |_| waiting.set(false),
                onkeydown: move |e| {
                    e.prevent_default();
                    match e.key() {
                        Key::Backspace | Key::Delete => onchange.call(String::new()),
                        Key::Escape => {
                            waiting.set(false);
                        }
                        key => {
                            if let Some(text) = format_gesture(&key, e.modifiers()) {
                                onchange.call(text);
                            }
                        }
                    }
                },
            }
        }
    }
}

/// 只按修饰键时为 `None`。
fn format_gesture(key: &Key, modifiers: Modifiers) -> Option<String> {
    if matches!(
        key,
        Key::Control | Key::Shift | Key::Alt | Key::Meta | Key::Process | Key::Unidentified
    ) {
        return None;
    }
    let mut parts = Vec::new();
    if modifiers.ctrl() {
        parts.push("Ctrl".to_string());
    }
    if modifiers.shift() {
        parts.push("Shift".to_string());
    }
    if modifiers.alt() {
        parts.push("Alt".to_string());
    }
    let name = match key {
        Key::Character(c) if c == " " => "Space".to_string(),
        Key::Character(c) => c.to_uppercase(),
        other => other.to_string(),
    };
    parts.push(name);
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_gestures() {
        assert_eq!(
            format_gesture(&Key::Character("k".into()), Modifiers::CONTROL),
            Some("Ctrl+K".into())
        );
        assert_eq!(
            format_gesture(&Key::Enter, Modifiers::CONTROL | Modifiers::SHIFT),
            Some("Ctrl+Shift+Enter".into())
        );
        assert_eq!(
            format_gesture(&Key::Character(" ".into()), Modifiers::empty()),
            Some("Space".into())
        );
        assert_eq!(format_gesture(&Key::Control, Modifiers::CONTROL), None);
    }
}
