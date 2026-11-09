//! Code block languages: display names, search, and the list shown below the language box under a
//! code block while typing in it.

use super::control::Ctl;
use crate::shell::language_icon_url;
use crate::ui::*;
use dioxus::prelude::*;
use zulip_core::markdown::code_language;
use zulip_core::{contains_ignore_case, starts_with_ignore_case};

/// Display name: mostly the language name, a few use a more common spelling.
pub fn label(language: &str) -> String {
    match language.to_lowercase().as_str() {
        "cpp" => "c++".to_owned(),
        "csharp" | "cs" => "c#".to_owned(),
        "fsharp" => "f#".to_owned(),
        "vbnet" => "vb.net".to_owned(),
        other => other.to_owned(),
    }
}

/// Languages in the dropdown: those whose name or display name starts with the query come first,
/// then those containing it, each keeping the usual order.
pub fn search(query: &str) -> Vec<&'static str> {
    let query = query.trim();
    let matches = |l: &str, test: fn(&str, &str) -> bool| test(l, query) || test(&label(l), query);
    let prefix = code_language::pickable().filter(|l| matches(l, starts_with_ignore_case));
    let contains = code_language::pickable()
        .filter(|l| !matches(l, starts_with_ignore_case) && matches(l, contains_ignore_case));
    prefix.chain(contains).collect()
}

/// Text typed in the language box of the code block opened on line `line`, and the highlighted
/// item of its list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LangSearch {
    pub line: usize,
    pub query: String,
    pub active: usize,
}

impl Ctl {
    /// Text typed in the language box under a code block (`line`: its opening fence line;
    /// `anchor`: the box). Lists the matching languages below the box; empty text hides the list.
    pub(super) fn code_language_query(mut self, line: usize, query: String, anchor: Rect) {
        if query.trim().is_empty() {
            self.close_language_list();
            return;
        }
        self.lang.set(Some(LangSearch {
            line,
            query,
            active: 0,
        }));
        if self.lang_popover.peek().is_some() {
            return;
        }
        let options = PopoverOptions {
            placement: Placement::BottomStart,
            gap: 4.0,
            min_width: anchor.w,
        };
        let (lang, popover) = (self.lang, self.lang_popover);
        let handle = self.overlays.open_popover_with_close(
            anchor,
            options,
            move |_| rsx! { LanguageList { ctl: self } },
            move || {
                let (mut lang, mut popover) = (lang, popover);
                lang.set(None);
                popover.set(None);
            },
        );
        self.lang_popover.set(Some(handle));
    }

    /// A key pressed in the language box: arrows move the highlight, Enter picks it, Esc closes
    /// the list.
    pub(super) fn code_language_key(mut self, key: &str) {
        let Some(state) = self.lang.peek().clone() else {
            return;
        };
        let items = search(&state.query);
        match key {
            "down" | "up" if !items.is_empty() => {
                let step = if key == "down" { 1 } else { items.len() - 1 };
                if let Some(s) = self.lang.write().as_mut() {
                    s.active = (state.active + step) % items.len();
                }
                document::eval(
                    r#"setTimeout(() => document.querySelector(".lang-list .menu-row.active")?.scrollIntoView({block:"nearest"}))"#,
                );
            }
            "enter" => {
                if let Some(language) = items.get(state.active.min(items.len().saturating_sub(1))) {
                    self.pick_language(state.line, language);
                }
            }
            "escape" => self.close_language_list(),
            _ => {}
        }
    }

    fn pick_language(self, line: usize, language: &str) {
        self.close_language_list();
        self.edit(|core| core.set_code_language(line, language));
    }

    /// Hide the language list (no-op when hidden).
    pub(super) fn close_language_list(mut self) {
        let handle = self.lang_popover.take();
        match handle {
            Some(handle) => handle.close(),
            None => self.lang.set(None),
        }
    }
}

/// Languages matching the text typed in the language box; pressing keeps the focus in the box.
#[component]
fn LanguageList(ctl: Ctl) -> Element {
    let Some(state) = ctl.lang.read().clone() else {
        return rsx! {};
    };
    let items = search(&state.query);
    let chosen = state.active.min(items.len().saturating_sub(1));
    let line = state.line;
    rsx! {
        div {
            class: "lang-list scroll",
            onmousedown: move |e| e.prevent_default(),
            if items.is_empty() {
                div { class: "lang-empty", "没有匹配的语言" }
            }
            for (i , language) in items.into_iter().enumerate() {
                div {
                    key: "{language}",
                    class: if i == chosen { "menu-row active" } else { "menu-row" },
                    onmouseenter: move |_| {
                        let mut lang = ctl.lang;
                        if let Some(s) = lang.write().as_mut() {
                            s.active = i;
                        }
                    },
                    onclick: move |_| ctl.pick_language(line, language),
                    img { class: "lang-icon", src: language_icon_url(language), alt: "" }
                    span { class: "menu-label", "{label(language)}" }
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
