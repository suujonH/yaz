//! 主题：深浅色、超级调色盘、字号。配置由 [`ThemeHub`] 在所有窗口间共享，每个窗口经 [`ThemeContext`] 读写。

use super::local_files;
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::tao::window::Theme;
use dioxus::desktop::{use_window, use_wry_event_handler};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;
use zulip_core::color::Rgb;
use zulip_store::{DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE};

pub const MIN_PANEL_OPACITY: f64 = 0.6;

/// 超级调色盘的预设。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePreset {
    pub key: &'static str,
    pub name: &'static str,
    pub color: &'static str,
}

pub const PRESETS: [ThemePreset; 6] = [
    ThemePreset {
        key: "default",
        name: "默认",
        color: "#0099FF",
    },
    ThemePreset {
        key: "mint",
        name: "薄荷",
        color: "#2BA471",
    },
    ThemePreset {
        key: "orange",
        name: "暖橙",
        color: "#E37318",
    },
    ThemePreset {
        key: "sakura",
        name: "樱花",
        color: "#E851B3",
    },
    ThemePreset {
        key: "grape",
        name: "葡萄",
        color: "#8E56DD",
    },
    ThemePreset {
        key: "graphite",
        name: "石墨",
        color: "#4B4B4B",
    },
];

/// 深浅色方案（Zulip 的 color_scheme）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorScheme {
    #[default]
    System,
    Dark,
    Light,
}

/// 超级调色盘的配置。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Palette {
    /// [`PRESETS`] 的键。
    pub preset: String,
    /// “随心调”选的强调色 `#RRGGBB`；有值时覆盖预设。
    pub accent: Option<String>,
    pub background_image: Option<PathBuf>,
    /// 有背景图时主面板的不透明度，0.6～1。
    pub panel_opacity: f64,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            preset: PRESETS[0].key.into(),
            accent: None,
            background_image: None,
            panel_opacity: 1.0,
        }
    }
}

impl Palette {
    pub fn accent_color(&self) -> Rgb {
        self.accent
            .as_deref()
            .and_then(Rgb::parse)
            .or_else(|| {
                PRESETS
                    .iter()
                    .find(|p| p.key == self.preset)
                    .and_then(|p| Rgb::parse(p.color))
            })
            .unwrap_or(Rgb::new(0x00, 0x99, 0xFF))
    }
}

/// 主题配置。调色盘和字号由 [`ThemeHub`] 的 `on_change` 分别保存到本机设置；
/// 深浅色跟随账号的 Zulip 设置，“预览夜间”只是临时状态，都不保存。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub scheme: ColorScheme,
    /// 临时切换为深色，不修改 Zulip 设置（“预览夜间”）。
    pub preview_dark: bool,
    pub palette: Palette,
    pub font_size: u32,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            scheme: ColorScheme::System,
            preview_dark: false,
            palette: Palette::default(),
            font_size: DEFAULT_FONT_SIZE as u32,
        }
    }
}

impl ThemeConfig {
    pub fn is_dark(&self, system_dark: bool) -> bool {
        self.preview_dark
            || match self.scheme {
                ColorScheme::System => system_dark,
                ColorScheme::Dark => true,
                ColorScheme::Light => false,
            }
    }
}

/// 写到 `.app-root` 行内样式的变量覆盖。
fn root_style(config: &ThemeConfig, dark: bool) -> String {
    let mut style = format!("--fs:{}px;", config.font_size);
    let accent = config.palette.accent_color();
    let default_accent = Rgb::parse(PRESETS[0].color);
    if Some(accent) != default_accent {
        let toward = |light: f64, deep: f64| {
            if dark {
                accent.mix(Rgb::BLACK, deep)
            } else {
                accent.mix(Rgb::WHITE, light)
            }
        };
        style.push_str(&format!(
            "--brand:{};--brand-hover:{};--brand-disabled:{};--own-bubble:{};--own-bubble-selected:{};",
            accent.to_hex(),
            accent.lighten(0.1).to_hex(),
            toward(0.65, 0.6).to_hex(),
            toward(0.8, 0.55).to_hex(),
            toward(0.7, 0.45).to_hex(),
        ));
    }
    if let Some(url) = config
        .palette
        .background_image
        .as_deref()
        .and_then(local_files::url_for)
    {
        style.push_str(&format!("--bg-image:url(\"{url}\");"));
        let opacity = config.palette.panel_opacity.clamp(MIN_PANEL_OPACITY, 1.0);
        if opacity < 1.0 {
            // 主面板半透明，露出下面的背景图
            let (panel, sidebar) = if dark { (0x26, 0x23) } else { (0xF5, 0xEF) };
            style.push_str(&format!(
                "--panel-bg:rgba({panel},{panel},{panel},{opacity});--sidebar-bg:rgba({sidebar},{sidebar},{sidebar},{opacity});"
            ));
        }
    }
    style
}

/// 窗口间共享的主题配置。`on_change` 在调色盘或字号变化后调用，用来保存。
#[derive(Clone)]
pub struct ThemeHub {
    tx: watch::Sender<ThemeConfig>,
    on_change: Arc<dyn Fn(&ThemeConfig) + Send + Sync>,
}

impl ThemeHub {
    pub fn new(
        initial: ThemeConfig,
        on_change: impl Fn(&ThemeConfig) + Send + Sync + 'static,
    ) -> Self {
        Self {
            tx: watch::Sender::new(initial),
            on_change: Arc::new(on_change),
        }
    }

    fn update(&self, change: impl FnOnce(&mut ThemeConfig)) {
        let mut changed = false;
        self.tx.send_modify(|config| {
            let (palette, font_size) = (config.palette.clone(), config.font_size);
            change(config);
            changed = config.palette != palette || config.font_size != font_size;
        });
        if changed {
            (self.on_change)(&self.tx.borrow());
        }
    }

    /// 应用已保存的调色盘和字号（不触发保存）。
    pub fn load(&self, palette: Palette, font_size: u32) {
        self.tx.send_modify(|c| {
            c.palette = palette;
            c.font_size = font_size;
        });
    }

    /// 深浅色跟随当前账号的 Zulip 设置（不触发保存）。
    pub fn apply_scheme(&self, scheme: ColorScheme) {
        self.tx.send_if_modified(|c| {
            let changed = c.scheme != scheme;
            c.scheme = scheme;
            changed
        });
    }
}

/// 当前窗口的主题读写入口。
#[derive(Clone)]
pub struct ThemeContext {
    hub: ThemeHub,
    config: Signal<ThemeConfig>,
    system_dark: Signal<bool>,
}

/// 读写主题（在窗口外壳内使用）。
pub fn use_theme() -> ThemeContext {
    use_context::<ThemeContext>()
}

impl ThemeContext {
    /// 当前配置（响应式）。
    pub fn config(&self) -> ThemeConfig {
        (self.config)()
    }

    /// 当前实际是否深色（响应式）。
    pub fn is_dark(&self) -> bool {
        self.config.read().is_dark((self.system_dark)())
    }

    pub fn set_scheme(&self, scheme: ColorScheme) {
        self.hub.update(|c| c.scheme = scheme);
    }

    pub fn set_preview_dark(&self, on: bool) {
        self.hub.update(|c| c.preview_dark = on);
    }

    pub fn set_palette(&self, palette: Palette) {
        self.hub.update(|c| {
            c.palette = Palette {
                panel_opacity: palette.panel_opacity.clamp(MIN_PANEL_OPACITY, 1.0),
                ..palette
            };
        });
    }

    pub fn set_font_size(&self, size: u32) {
        self.hub
            .update(|c| c.font_size = size.clamp(MIN_FONT_SIZE as u32, MAX_FONT_SIZE as u32));
    }
}

/// 提供 [`ThemeContext`] 并渲染 `.app-root`。窗口外壳已包含。
#[component]
pub fn ThemeProvider(children: Element) -> Element {
    let hub = use_context::<ThemeHub>();
    let window = use_window();
    let mut config = use_signal(|| hub.tx.borrow().clone());
    let mut system_dark = use_signal(|| window.theme() == Theme::Dark);
    use_context_provider(|| ThemeContext {
        hub: hub.clone(),
        config,
        system_dark,
    });
    use_future({
        let hub = hub.clone();
        move || {
            let mut rx = hub.tx.subscribe();
            async move {
                while rx.changed().await.is_ok() {
                    let next = rx.borrow_and_update().clone();
                    config.set(next);
                }
            }
        }
    });
    use_wry_event_handler(move |event, _| {
        if let Event::WindowEvent {
            event: WindowEvent::ThemeChanged(theme),
            ..
        } = event
        {
            system_dark.set(*theme == Theme::Dark);
        }
    });

    let current = config.read();
    let dark = current.is_dark(system_dark());
    let style = root_style(&current, dark);
    let has_image = style.contains("--bg-image");
    rsx! {
        div {
            class: if has_image { "app-root has-bg-image" } else { "app-root" },
            "data-theme": if dark { "dark" } else { "light" },
            style,
            {children}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn scheme_resolves() {
        let mut c = ThemeConfig::default();
        assert!(c.is_dark(true));
        assert!(!c.is_dark(false));
        c.scheme = ColorScheme::Light;
        assert!(!c.is_dark(true));
        c.preview_dark = true;
        assert!(c.is_dark(false));
    }

    #[test]
    fn accent_prefers_custom_color_over_preset() {
        let mut p = Palette {
            preset: "mint".into(),
            ..Palette::default()
        };
        assert_eq!(p.accent_color().to_hex(), "#2BA471");
        p.accent = Some("#112233".into());
        assert_eq!(p.accent_color().to_hex(), "#112233");
        p.accent = Some("bad".into());
        assert_eq!(p.accent_color().to_hex(), "#2BA471");
        p.preset = "unknown".into();
        assert_eq!(p.accent_color().to_hex(), "#0099FF");
    }

    #[test]
    fn default_accent_adds_no_overrides() {
        let style = root_style(&ThemeConfig::default(), false);
        assert_eq!(style, "--fs:14px;");
    }

    #[test]
    fn preset_overrides_brand_by_scheme() {
        let config = ThemeConfig {
            palette: Palette {
                preset: "grape".into(),
                ..Palette::default()
            },
            ..ThemeConfig::default()
        };
        let accent = Rgb::parse("#8E56DD").unwrap();
        let light = root_style(&config, false);
        let dark = root_style(&config, true);
        assert!(light.contains("--brand:#8E56DD"));
        assert!(light.contains(&format!(
            "--own-bubble:{}",
            accent.mix(Rgb::WHITE, 0.8).to_hex()
        )));
        assert!(dark.contains(&format!(
            "--own-bubble:{}",
            accent.mix(Rgb::BLACK, 0.55).to_hex()
        )));
    }

    #[test]
    fn root_style_uses_font_size() {
        let config = ThemeConfig {
            font_size: 16,
            ..ThemeConfig::default()
        };
        assert!(root_style(&config, false).starts_with("--fs:16px;"));
    }

    #[test]
    fn only_palette_and_font_size_changes_are_saved() {
        let saved = Arc::new(AtomicUsize::new(0));
        let counter = saved.clone();
        let hub = ThemeHub::new(ThemeConfig::default(), move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        });

        hub.update(|c| c.preview_dark = true);
        hub.update(|c| c.scheme = ColorScheme::Dark);
        hub.update(|c| c.font_size = DEFAULT_FONT_SIZE as u32);
        hub.update(|c| c.palette = Palette::default());
        assert_eq!(saved.load(Ordering::Relaxed), 0);
        assert!(hub.tx.borrow().preview_dark);

        hub.update(|c| c.font_size = 16);
        assert_eq!(saved.load(Ordering::Relaxed), 1);
        hub.update(|c| c.palette.preset = "mint".into());
        assert_eq!(saved.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn config_round_trips_and_tolerates_missing_fields() {
        let config = ThemeConfig {
            scheme: ColorScheme::Dark,
            font_size: 16,
            ..ThemeConfig::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<ThemeConfig>(&json).unwrap(), config);
        assert_eq!(
            serde_json::from_str::<ThemeConfig>("{}").unwrap(),
            ThemeConfig::default()
        );
    }
}
