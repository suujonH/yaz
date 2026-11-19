//! Transparent text layer over the image: the recognized words at their places, so the text on the
//! image can be selected and copied.

use crate::platform::OcrLine;
use dioxus::prelude::*;
use std::rc::Rc;

/// Stretches each word horizontally to the width of its rectangle on the image (the layout width
/// is not affected by the layer's transform).
const FIT_JS: &str = r#"
requestAnimationFrame(() => {
  for (const el of document.querySelectorAll(".viewer-word")) {
    el.style.transform = "";
    const width = el.offsetWidth;
    const target = Number(el.dataset.w);
    if (width > 0 && target > 0) el.style.transform = `scaleX(${target / width})`;
  }
});
"#;

/// Reports the text selected in the text layer (an empty string when none), and copies it as
/// plain text only (the HTML would carry the transparent color into where it is pasted).
pub const SELECTION_JS: &str = r#"
window.zulipViewerText?.abort();
const control = new AbortController();
window.zulipViewerText = control;
const selected = () => {
  const sel = document.getSelection();
  if (!sel || sel.isCollapsed || !sel.anchorNode) return "";
  const anchor = sel.anchorNode instanceof Element ? sel.anchorNode : sel.anchorNode.parentElement;
  return anchor && anchor.closest(".viewer-text-layer") ? sel.toString() : "";
};
let last = "";
document.addEventListener("selectionchange", () => {
  const text = selected();
  if (text !== last) {
    last = text;
    dioxus.send(text);
  }
}, { signal: control.signal });
document.addEventListener("copy", (e) => {
  const text = selected();
  if (!text) return;
  e.clipboardData.setData("text/plain", text);
  e.preventDefault();
}, { signal: control.signal });
await new Promise(() => {});
"#;

/// Chinese, Japanese and Korean characters and full-width forms: no space is put between them and
/// their neighbors.
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{2E80}'..='\u{9FFF}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE30}'..='\u{FE4F}'
            | '\u{FF00}'..='\u{FFEF}')
}

/// Whether a space goes between two neighboring words: only when neither side is CJK.
pub fn needs_space(before: &str, after: &str) -> bool {
    match (before.chars().next_back(), after.chars().next()) {
        (Some(last), Some(first)) => !is_cjk(last) && !is_cjk(first),
        _ => false,
    }
}

/// The text each word of the line shows: the word, plus a space when the next word needs one.
pub fn line_texts(line: &OcrLine) -> Vec<String> {
    line.words
        .iter()
        .enumerate()
        .map(|(i, word)| match line.words.get(i + 1) {
            Some(next) if needs_space(&word.text, &next.text) => format!("{} ", word.text),
            _ => word.text.clone(),
        })
        .collect()
}

/// All the text, one line per recognized line.
pub fn plain_text(lines: &[OcrLine]) -> String {
    lines
        .iter()
        .map(|line| line_texts(line).concat())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `style`: the image's size and transform ([`Zoom::style`](super::zoom::Zoom::style)), so the
/// words follow the image as it zooms, moves and rotates.
#[component]
pub fn TextLayer(lines: Rc<Vec<OcrLine>>, style: String, animating: bool) -> Element {
    use_effect(use_reactive!(|lines| {
        let _ = lines;
        document::eval(FIT_JS);
    }));
    let count = lines.len();
    rsx! {
        div {
            class: if animating { "viewer-text-layer animating" } else { "viewer-text-layer" },
            style: "{style}",
            for (row , line) in lines.iter().enumerate() {
                Fragment { key: "{row}",
                    for (word , text) in line.words.iter().zip(line_texts(line)) {
                        span {
                            class: "viewer-word",
                            "data-w": "{word.width}",
                            style: "left:{word.x}px;top:{word.y}px;height:{word.height}px;font-size:{word.height}px;line-height:{word.height}px",
                            // Dragging on a word selects text instead of moving the image, and a
                            // double click selects the word instead of zooming
                            onmousedown: move |e| e.stop_propagation(),
                            ondoubleclick: move |e| e.stop_propagation(),
                            "{text}"
                        }
                    }
                    if row + 1 < count {
                        br {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::OcrWord;

    fn line(words: &[&str]) -> OcrLine {
        OcrLine {
            words: words
                .iter()
                .map(|text| OcrWord {
                    text: (*text).to_owned(),
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                })
                .collect(),
        }
    }

    #[test]
    fn spaces_only_between_non_cjk_words() {
        assert!(needs_space("Hello", "world"));
        assert!(!needs_space("你", "好"));
        assert!(!needs_space("Hello", "你"));
        assert!(!needs_space("，", "a"));
        assert!(!needs_space("", "a"));
    }

    #[test]
    fn words_carry_their_spaces() {
        assert_eq!(
            line_texts(&line(&["Hello", "world", "你", "好"])),
            ["Hello ", "world", "你", "好"]
        );
        assert_eq!(
            plain_text(&[line(&["Hello", "world"]), line(&["你", "好"])]),
            "Hello world\n你好"
        );
    }
}
