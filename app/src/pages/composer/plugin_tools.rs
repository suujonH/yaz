//! 输入框工具栏中的插件按钮（plugin.json 的 composer_buttons）。

use super::control::Ctl;
use crate::services::plugins;
use crate::state::use_plugin_events;
use crate::ui::*;
use crate::widgets::PluginRef;
use dioxus::prelude::*;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use std::path::Path;
use zulip_plugin_host::{ComposerIcon, PluginEvent};

/// 太大的文件不是图标，不读进界面。
const MAX_SVG_BYTES: u64 = 256 * 1024;

fn builtin_icon(name: &str) -> IconName {
    match name {
        "add" => IconName::Add,
        "chat" => IconName::Chat,
        "check" => IconName::Check,
        "edit" => IconName::Edit,
        "file" => IconName::File,
        "folder" => IconName::Folder,
        "history" => IconName::History,
        "image" => IconName::Image,
        "lock" => IconName::Lock,
        "search" => IconName::Search,
        "smile" => IconName::Smile,
        "time" => IconName::Time,
        "user" => IconName::User,
        _ => IconName::More,
    }
}

/// 插件目录内 SVG 文件的 data 地址，放进 `img` 显示（其中的脚本不会执行）。
/// 解析符号链接后不在目录内、读取失败或文件太大时为 `None`。
fn svg_data_uri(directory: &Path, file: &Path) -> Option<String> {
    let directory = directory.canonicalize().ok()?;
    let file = file.canonicalize().ok()?;
    if !file.starts_with(&directory) || std::fs::metadata(&file).ok()?.len() > MAX_SVG_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&file).ok()?;
    Some(format!(
        "data:image/svg+xml;charset=utf-8,{}",
        utf8_percent_encode(&text, NON_ALPHANUMERIC)
    ))
}

#[derive(Clone, PartialEq)]
struct ToolButton {
    plugin: PluginRef,
    id: i32,
    icon: IconName,
    image: Option<String>,
    tooltip: String,
}

fn tool_buttons() -> Vec<ToolButton> {
    plugins()
        .composer_buttons()
        .into_iter()
        .map(|button| {
            let (icon, image) = match &button.icon {
                ComposerIcon::Builtin(name) => (builtin_icon(name), None),
                ComposerIcon::Svg(file) => (
                    IconName::More,
                    svg_data_uri(button.plugin.directory(), Path::new(file)),
                ),
            };
            ToolButton {
                plugin: PluginRef(button.plugin),
                id: button.id,
                icon,
                image,
                tooltip: button.tooltip,
            }
        })
        .collect()
}

#[component]
pub(super) fn PluginTools() -> Element {
    let ctl = use_context::<Ctl>();
    let mut buttons = use_signal(tool_buttons);
    use_plugin_events(move |event| {
        if matches!(event, PluginEvent::PluginsChanged) {
            let next = tool_buttons();
            if *buttons.peek() != next {
                buttons.set(next);
            }
        }
    });
    rsx! {
        for button in buttons.read().iter().cloned() {
            Tooltip {
                key: "{button.plugin.0.id()}:{button.id}",
                text: button.tooltip.clone(),
                IconButton {
                    icon: button.icon,
                    image: button.image.clone(),
                    onclick: move |_| {
                        let text = ctl.core.peek().text().to_owned();
                        plugins().invoke_composer_button(
                            &button.plugin.0,
                            button.id,
                            &ctl.session(),
                            &ctl.key(),
                            &text,
                        );
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zulip_plugin_host::manifest::BUILTIN_ICONS;

    #[test]
    fn every_builtin_icon_is_mapped() {
        for name in BUILTIN_ICONS {
            assert_eq!(
                builtin_icon(name) == IconName::More,
                name == "more",
                "{name}"
            );
        }
    }

    #[test]
    fn svg_must_stay_inside_the_plugin_directory() {
        let root = TempDir::new().unwrap();
        let plugin = root.path().join("plugin");
        std::fs::create_dir_all(plugin.join("icons")).unwrap();
        std::fs::write(plugin.join("icons/a.svg"), "<svg/>").unwrap();
        std::fs::write(root.path().join("b.svg"), "<svg/>").unwrap();

        let inside = svg_data_uri(&plugin, &plugin.join("icons/a.svg")).unwrap();
        assert_eq!(inside, "data:image/svg+xml;charset=utf-8,%3Csvg%2F%3E");
        assert_eq!(svg_data_uri(&plugin, &plugin.join("../b.svg")), None);
        assert_eq!(svg_data_uri(&plugin, &root.path().join("b.svg")), None);
        assert_eq!(svg_data_uri(&plugin, &plugin.join("missing.svg")), None);
    }

    #[test]
    fn oversized_svg_is_not_read() {
        let plugin = TempDir::new().unwrap();
        let file = plugin.path().join("big.svg");
        std::fs::write(&file, vec![b' '; MAX_SVG_BYTES as usize + 1]).unwrap();
        assert_eq!(svg_data_uri(plugin.path(), &file), None);
    }
}
