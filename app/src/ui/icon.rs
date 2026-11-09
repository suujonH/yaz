use dioxus::prelude::*;

pub use super::icon_data::IconName;

/// 与 [`Icon`] 相同的 SVG 文本，用于拼接的 HTML（消息内容、输入框）。
pub fn icon_html(name: IconName, size: u32) -> String {
    let (stroke, fill, even_odd) = name.paths();
    let mut out =
        format!(r#"<svg class="icon" width="{size}" height="{size}" viewBox="0 0 24 24">"#);
    if !fill.is_empty() {
        let rule = if even_odd { "evenodd" } else { "nonzero" };
        out.push_str(&format!(
            r#"<path class="solid" d="{fill}" fill-rule="{rule}"></path>"#
        ));
    }
    if !stroke.is_empty() {
        out.push_str(&format!(r#"<path class="stroke" d="{stroke}"></path>"#));
    }
    out.push_str("</svg>");
    out
}

/// TDesign 线框图标，颜色继承 `color`。
#[component]
pub fn Icon(
    name: IconName,
    #[props(default = 16.0)] size: f64,
    #[props(default, into)] class: String,
) -> Element {
    let (stroke, fill, even_odd) = name.paths();
    rsx! {
        svg {
            class: "icon {class}",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            if !fill.is_empty() {
                path { class: "solid", d: fill, fill_rule: if even_odd { "evenodd" } else { "nonzero" } }
            }
            if !stroke.is_empty() {
                path { class: "stroke", d: stroke }
            }
        }
    }
}
