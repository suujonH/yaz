//! 导航项下的二级列表：当前页的分组标题。页面超过两屏高时才显示，点击滚动到分组，滚动时高亮所在的分组。

use crate::ui::*;
use dioxus::prelude::*;
use serde::Deserialize;

/// 页面滚动区域的元素 ID。
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
  const collect = () => {
    const heads = [...scroll.querySelectorAll(".setting-section")];
    const titles = scroll.scrollHeight > scroll.clientHeight * 2 ? heads.map((h) => h.textContent) : [];
    const top = scroll.getBoundingClientRect().top;
    let active = 0;
    heads.forEach((h, i) => { if (h.getBoundingClientRect().top <= top + 8) active = i; });
    const message = JSON.stringify({ titles, active });
    if (message !== last) { last = message; dioxus.send({ titles, active }); }
  };
  scroll.addEventListener("scroll", collect, { passive: true });
  const observer = new MutationObserver(collect);
  observer.observe(scroll, { childList: true, subtree: true });
  const resize = new ResizeObserver(collect);
  resize.observe(scroll);
  scroll.__sectionCleanup = () => {
    scroll.removeEventListener("scroll", collect);
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
         const h = s && s.querySelectorAll('.setting-section')[{index}]; \
         if (h) s.scrollTo({{ top: s.scrollTop + h.getBoundingClientRect().top - s.getBoundingClientRect().top - 4, behavior: 'smooth' }});"
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
