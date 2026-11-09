//! Palette: night preview, accent color, background image and panel opacity, preset swatches.

use super::field::*;
use crate::platform;
use crate::services::services;
use crate::shell::{PRESETS, Palette as ThemePalette, ThemeContext, use_theme};
use crate::ui::*;
use dioxus::prelude::*;
use std::path::{Path, PathBuf};

/// Lower bound of the panel opacity (percentage).
const MIN_PANEL_OPACITY_PERCENT: i32 = 60;
/// The first time a background image is set, the panel is slightly transparent by default.
const FIRST_BACKGROUND_OPACITY: f64 = 0.85;

fn theme_dir() -> PathBuf {
    services().paths.root().join("theme")
}

/// Imported background images are placed under theme in the data directory, with a fresh file name
/// each time.
fn import_background(theme: &ThemeContext, source: &str) -> Result<(), String> {
    let mut palette = theme.config().palette;
    let previous = palette.background_image.take();
    if source.is_empty() {
        theme.set_palette(palette);
    } else {
        let dir = theme_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let ext = Path::new(source)
            .extension()
            .map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
        let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
        let target = dir.join(format!("background-{stamp}{ext}"));
        std::fs::copy(source, &target).map_err(|e| e.to_string())?;
        if previous.is_none() && palette.panel_opacity >= 1.0 {
            palette.panel_opacity = FIRST_BACKGROUND_OPACITY;
        }
        palette.background_image = Some(target);
        theme.set_palette(palette);
    }
    if let Some(old) = previous.filter(|p| p.parent() == Some(theme_dir().as_path())) {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

/// Native color picker.
#[component]
fn ColorRow(#[props(into)] label: String, bind: Bind<String>) -> Element {
    rsx! {
        SettingRow { label, description: bind.hint.clone(),
            input {
                class: "settings-color",
                r#type: "color",
                value: "{bind.value}",
                oninput: move |e| bind.set(e.value().to_uppercase()),
            }
        }
    }
}

#[component]
pub fn Palette() -> Element {
    let theme = use_theme();
    let drop_theme = theme.clone();
    use_drop(move || drop_theme.set_preview_dark(false));
    let preview = field(Source::Theme(
        |c| c.preview_dark,
        |t, v| t.set_preview_dark(v),
    ));
    let accent = field(Source::Theme(
        |c| c.palette.accent_color().to_hex(),
        |t, hex| {
            let mut palette = t.config().palette;
            palette.accent = Some(hex);
            t.set_palette(palette);
        },
    ));
    let background = field(Source::Custom(
        Scope::App,
        |env| {
            env.theme
                .config()
                .palette
                .background_image
                .map_or_else(String::new, |p| p.display().to_string())
        },
        |env, source| import_background(&env.theme, &source),
    ));
    let opacity = field(Source::Theme(
        |c| (c.palette.panel_opacity * 100.0).round() as i32,
        |t, percent| {
            let mut palette = t.config().palette;
            palette.panel_opacity = f64::from(percent) / 100.0;
            t.set_palette(palette);
        },
    ));
    let config = theme.config();
    let has_background = !background.value.is_empty();
    let reset = theme.clone();
    let pick_background = background.clone();
    rsx! {
        SettingsPage {
            title: "超级调色盘",
            actions: rsx! {
                div { class: "settings-actions",
                    Toggle {
                        checked: preview.value,
                        onchange: move |v| preview.set(v),
                        "预览夜间"
                    }
                    Button {
                        onclick: move |_| {
                            reset.set_preview_dark(false);
                            reset.set_palette(ThemePalette::default());
                        },
                        "恢复默认"
                    }
                }
            },
            Group { title: "随心调", ColorRow { label: "强调色", bind: accent } }
            Group { title: "背景图片",
                SettingRow { label: "主窗口背景", description: background.hint.clone(),
                    div { class: "settings-actions",
                        Button {
                            onclick: move |_| {
                                let bind = pick_background.clone();
                                spawn(async move {
                                    if let Some(path) = platform::pick_file("选择背景图片", &[platform::IMAGE_FILTER]).await {
                                        bind.set(path.display().to_string());
                                    }
                                });
                            },
                            "上传图片"
                        }
                        if has_background {
                            Button {
                                onclick: {
                                    let bind = background.clone();
                                    move |_| bind.set(String::new())
                                },
                                "移除"
                            }
                        }
                    }
                }
                if has_background {
                    SliderRow {
                        label: "面板不透明度",
                        bind: opacity,
                        min: MIN_PANEL_OPACITY_PERCENT,
                        max: 100,
                        unit: "%",
                    }
                }
            }
            div { class: "setting-section", "定制" }
            div { class: "settings-presets",
                for preset in PRESETS {
                    {
                        let selected = config.palette.accent.is_none() && config.palette.preset == preset.key;
                        let theme = theme.clone();
                        rsx! {
                            button {
                                key: "{preset.key}",
                                class: if selected { "settings-preset selected" } else { "settings-preset" },
                                r#type: "button",
                                onclick: move |_| {
                                    let mut palette = theme.config().palette;
                                    palette.preset = preset.key.to_owned();
                                    palette.accent = None;
                                    theme.set_palette(palette);
                                },
                                div { class: "settings-swatch", style: "background:{preset.color}" }
                                span { class: "text-sub", "{preset.name}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
