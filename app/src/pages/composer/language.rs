//! 代码块语言：显示名、搜索，以及代码块右下角语言按钮打开的下拉框。

use super::control::Ctl;
use crate::shell::language_icon_url;
use crate::ui::*;
use dioxus::prelude::*;
use zulip_core::markdown::code_language;
use zulip_core::{contains_ignore_case, starts_with_ignore_case};

/// 显示名：多数就是语言名，少数换成常见写法。
pub fn label(language: &str) -> String {
    match language.to_lowercase().as_str() {
        "cpp" => "c++".to_owned(),
        "csharp" | "cs" => "c#".to_owned(),
        "fsharp" => "f#".to_owned(),
        "vbnet" => "vb.net".to_owned(),
        other => other.to_owned(),
    }
}

/// 下拉框里的语言：名称或显示名以查询词开头的在前，其次是包含查询词的，各自保持常用顺序。
pub fn search(query: &str) -> Vec<&'static str> {
    let query = query.trim();
    let matches = |l: &str, test: fn(&str, &str) -> bool| test(l, query) || test(&label(l), query);
    let prefix = code_language::pickable().filter(|l| matches(l, starts_with_ignore_case));
    let contains = code_language::pickable()
        .filter(|l| !matches(l, starts_with_ignore_case) && matches(l, contains_ignore_case));
    prefix.chain(contains).collect()
}

impl Ctl {
    /// 代码块右下角的语言按钮：在按钮下方打开下拉框，选中后改写开头围栏的语言。
    pub(super) fn pick_code_language(self, open_line: usize, anchor: Rect) {
        let current = self.core.peek().code_language(open_line);
        let options = PopoverOptions {
            placement: Placement::BottomStart,
            gap: 4.0,
            min_width: 0.0,
        };
        self.overlays.open_popover(anchor, options, move |handle| {
            rsx! {
                LanguagePicker {
                    current: current.clone(),
                    on_pick: move |language: &'static str| {
                        handle.close();
                        self.edit(|core| core.set_code_language(open_line, language));
                    },
                }
            }
        });
    }
}

#[component]
fn LanguagePicker(current: String, on_pick: EventHandler<&'static str>) -> Element {
    let mut query = use_signal(String::new);
    let mut active = use_signal(|| search("").iter().position(|l| *l == current).unwrap_or(0));
    let items = search(&query.read());
    let count = items.len();
    let chosen = items.get(active().min(count.saturating_sub(1))).copied();
    // 方向键移动后把选中的一项滚进可见范围
    let reveal = move || {
        document::eval(
            r#"document.querySelector(".lang-list .menu-row.active")?.scrollIntoView({block:"nearest"})"#,
        );
    };
    rsx! {
        div {
            class: "lang-picker",
            onkeydown: move |e: KeyboardEvent| {
                if count == 0 {
                    return;
                }
                let step = match e.key() {
                    Key::ArrowDown => 1,
                    Key::ArrowUp => count - 1,
                    _ => return,
                };
                e.prevent_default();
                active.set((active() + step) % count);
                reveal();
            },
            TextInput {
                value: query(),
                placeholder: "搜索语言",
                autofocus: true,
                oninput: move |v: String| {
                    query.set(v);
                    active.set(0);
                },
                onsubmit: move |_| {
                    if let Some(language) = chosen {
                        on_pick.call(language);
                    }
                },
            }
            div { class: "lang-list scroll",
                if items.is_empty() {
                    div { class: "lang-empty", "没有匹配的语言" }
                }
                for (i , language) in items.into_iter().enumerate() {
                    div {
                        key: "{language}",
                        class: if Some(language) == chosen { "menu-row active" } else { "menu-row" },
                        onmouseenter: move |_| active.set(i),
                        onclick: move |_| on_pick.call(language),
                        img { class: "lang-icon", src: language_icon_url(language), alt: "" }
                        span { class: "menu-label", "{label(language)}" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_name_and_label() {
        assert_eq!(search("c#"), ["csharp"]);
        assert_eq!(&search("c")[..3], ["c", "cpp", "csharp"]);
        assert_eq!(search("PYTH"), ["python"]);
        assert!(search("zzzz").is_empty());
        assert!(!search("").contains(&"math"));
    }

    #[test]
    fn labels() {
        assert_eq!(label("cpp"), "c++");
        assert_eq!(label("Rust"), "rust");
    }
}
