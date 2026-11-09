//! Secondary list under a navigation item: section titles of the current page. Shown only when the
//! page is taller than two screens; clicking scrolls to a section, and the section in view is
//! highlighted while scrolling. A section that cannot scroll up to the top (the last ones on the
//! page) is still highlighted: the clicked section stays highlighted until the user scrolls, and at
//! the bottom of the page the last section is.

use crate::ui::*;
use dioxus::prelude::*;
use serde::Deserialize;

/// Element ID of the page's scroll area.
pub const SCROLL_ID: &str = "settings-scroll";

const WATCH_JS: &str = r#"
let scroll = null;
for (let i = 0; i < 20 && !scroll; i++) {
  scroll = document.getElementById("settings-scroll");
  if (!scroll) await new Promise((resolve) => setTimeout(resolve, 50));
}
if (scroll) {
  if (scroll.__sectionCleanup) scroll.__sectionCleanup();
  let last = "";
  // Section clicked in the navigation: highlighted until the user scrolls by themselves
  let target = null;
  const collect = () => {
    const heads = [...scroll.querySelectorAll(".setting-section")];
    const titles = scroll.scrollHeight > scroll.clientHeight * 2 ? heads.map((h) => h.textContent) : [];
    const top = scroll.getBoundingClientRect().top;
    let active = 0;
    heads.forEach((h, i) => { if (h.getBoundingClientRect().top <= top + 8) active = i; });
    if (target !== null && target < heads.length) active = target;
    else if (heads.length && scroll.scrollTop + scroll.clientHeight >= scroll.scrollHeight - 2) active = heads.length - 1;
    const message = JSON.stringify({ titles, active });
    if (message !== last) { last = message; dioxus.send({ titles, active }); }
  };
  scroll.addEventListener("scroll", collect, { passive: true });
  const userScroll = () => { if (target !== null) { target = null; collect(); } };
  const userEvents = ["wheel", "pointerdown", "keydown", "touchstart"];
  userEvents.forEach((name) => scroll.addEventListener(name, userScroll, { passive: true }));
  scroll.__sectionSelect = (index) => {
    const h = scroll.querySelectorAll(".setting-section")[index];
    if (!h) return;
    target = index;
    collect();
    scroll.scrollTo({ top: scroll.scrollTop + h.getBoundingClientRect().top - scroll.getBoundingClientRect().top - 4, behavior: "smooth" });
  };
  const observer = new MutationObserver(collect);
  observer.observe(scroll, { childList: true, subtree: true });
  const resize = new ResizeObserver(collect);
  resize.observe(scroll);
  scroll.__sectionCleanup = () => {
    scroll.removeEventListener("scroll", collect);
    userEvents.forEach((name) => scroll.removeEventListener(name, userScroll));
    scroll.__sectionSelect = null;
    observer.disconnect();
    resize.disconnect();
    scroll.__sectionCleanup = null;
  };
  collect();
}
"#;

#[derive(Deserialize)]
struct Sections {
    titles: Vec<String>,
    active: usize,
}

fn scroll_to(index: usize) {
    document::eval(&format!(
        "const s = document.getElementById('{SCROLL_ID}'); \
         if (s && s.__sectionSelect) s.__sectionSelect({index});"
    ));
}

#[component]
pub fn SectionNav() -> Element {
    let mut sections = use_signal(|| None::<Sections>);
    use_future(move || async move {
        let mut eval = document::eval(WATCH_JS);
        while let Ok(next) = eval.recv::<serde_json::Value>().await {
            if let Ok(next) = serde_json::from_value::<Sections>(next) {
                sections.set(Some(next));
            }
        }
    });
    let sections = sections.read();
    let Some(sections) = sections.as_ref().filter(|s| !s.titles.is_empty()) else {
        return rsx! {};
    };
    rsx! {
        div { class: "settings-sections",
            for (index, title) in sections.titles.iter().enumerate() {
                ListRow {
                    key: "{index}",
                    density: RowDensity::Compact,
                    selected: index == sections.active,
                    title: title.clone(),
                    onclick: move |_| scroll_to(index),
                }
            }
        }
    }
}
