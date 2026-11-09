//! Image viewer window: zoom (wheel, buttons, fit), pan, rotate, album switching, save as, copy,
//! open externally, forward, fullscreen.

mod zoom;

use crate::platform::{self, ClipboardImage};
use crate::shell::{WindowShell, url_for, use_window_args};
use crate::state::use_app_state;
use crate::ui::*;
use crate::widgets::*;
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::{use_window, use_wry_event_handler};
use dioxus::html::geometry::WheelDelta;
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;
use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use zoom::Zoom;
use zulip_core::html::{IMAGE_EXTENSIONS, normalize_upload_path, upload_link_of};
use zulip_session::{Session, SessionError};

/// How long the zoom ratio hint is shown.
const TIP_DURATION: Duration = Duration::from_millis(1500);
/// After the window size changes, wait for the layout to settle before measuring the display area.
const MEASURE_DELAY: Duration = Duration::from_millis(40);
/// Window background color, consistent with the background of `.viewer-window`.
pub const BACKGROUND: (u8, u8, u8) = (0x1B, 0x1B, 0x1B);
/// Pixels per wheel notch.
const WHEEL_NOTCH: f64 = 100.0;
/// Duration of the double-click zoom animation, consistent with the transition of
/// `.viewer-image.animating`.
const ZOOM_ANIMATION: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq)]
pub struct ImageViewerArgs {
    pub images: Vec<String>,
    pub index: usize,
}

/// The image that finished loading.
#[derive(Clone, Debug, PartialEq)]
struct Loaded {
    /// The address actually loaded (the original address when the large image does not exist).
    url: String,
    path: PathBuf,
    /// Local address for `img`.
    src: String,
    size: (f64, f64),
}

/// Original address used when the large avatar (`-medium.png`) does not exist.
fn fallback_url(url: &str) -> Option<String> {
    const MEDIUM: &str = "-medium.png";
    let (path, query) = url.split_at(url.find('?').unwrap_or(url.len()));
    let stem = path
        .len()
        .checked_sub(MEDIUM.len())
        .filter(|at| path.is_char_boundary(*at))
        .filter(|at| path[*at..].eq_ignore_ascii_case(MEDIUM))?;
    Some(format!("{}.png{query}", &path[..stem]))
}

async fn load_image(session: &Arc<Session>, url: &str) -> Result<Loaded, SessionError> {
    let (used, path) = match session.image_file(url).await {
        Ok(path) => (url.to_owned(), path),
        Err(error) => match fallback_url(url) {
            Some(original) => {
                let path = session.image_file(&original).await?;
                (original, path)
            }
            None => return Err(error),
        },
    };
    let (width, height) = image::image_dimensions(&path)
        .map_err(|e| SessionError::invalid(format!("无法显示这张图片：{e}")))?;
    let src = url_for(&path).ok_or_else(|| SessionError::invalid("无法显示这张图片"))?;
    Ok(Loaded {
        url: used,
        path,
        src,
        size: (f64::from(width), f64::from(height)),
    })
}

/// File name of the image: the last segment of the address; when it has no image extension, one is
/// added based on the local file.
fn file_name_of(url: &str, path: &Path) -> String {
    let without_query = url.split('?').next().unwrap_or(url);
    let last = without_query.rsplit('/').next().unwrap_or_default();
    let mut name = percent_decode_str(last).decode_utf8_lossy().into_owned();
    if name.is_empty() {
        name = "图片".to_owned();
    }
    let has_extension = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)));
    if has_extension {
        return name;
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or("png");
    format!("{name}.{extension}")
}

/// File handed to the system image viewer: when the cache file has no image extension, copy it to
/// the temp directory and add one based on the file header.
async fn external_copy(path: &Path, name: &str) -> std::io::Result<PathBuf> {
    let has_extension = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)));
    if has_extension {
        return Ok(path.to_owned());
    }
    let bytes = tokio::fs::read(path).await?;
    let extension = image::guess_format(&bytes)
        .ok()
        .and_then(|f| f.extensions_str().first().copied())
        .unwrap_or("png");
    let stem = Path::new(name)
        .file_stem()
        .map_or_else(|| "图片".to_owned(), |s| s.to_string_lossy().into_owned());
    let dir = std::env::temp_dir()
        .join("YaZ")
        .join(format!("{:x}", text_id(&path.to_string_lossy())));
    tokio::fs::create_dir_all(&dir).await?;
    let target = dir.join(format!("{stem}.{extension}"));
    tokio::fs::write(&target, bytes).await?;
    Ok(target)
}

/// Content to forward: images uploaded to this organization send the upload link, other images send
/// the image address.
fn forward_markdown(url: &str) -> String {
    match normalize_upload_path(url) {
        Some(path) => {
            let link = upload_link_of(&path, None);
            format!("[{}]({path})", link.file_name)
        }
        None => url.to_owned(),
    }
}

async fn copy_image(overlays: Overlays, path: PathBuf) {
    let decoded = tokio::task::spawn_blocking(move || {
        image::open(&path).map(|img| {
            let rgba = img.to_rgba8();
            ClipboardImage {
                width: rgba.width() as usize,
                height: rgba.height() as usize,
                rgba: rgba.into_raw(),
            }
        })
    })
    .await;
    match decoded {
        Ok(Ok(image)) => match platform::copy_image(&image) {
            Ok(()) => overlays.toast("已复制"),
            Err(error) => overlays.report_error("复制失败", error),
        },
        Ok(Err(error)) => overlays.report_error("复制失败", error),
        Err(error) => overlays.report_error("复制失败", error),
    }
}

#[component]
pub fn ImageViewerWindow() -> Element {
    rsx! {
        WindowShell { title: "图片查看", class: "viewer-window", ViewerBody {} }
    }
}

#[component]
fn ViewerBody() -> Element {
    let args = use_window_args::<ImageViewerArgs>();
    let state = use_app_state();
    let overlays = use_overlays();
    let window = use_window();
    let mut index = use_signal(|| 0usize);
    use_effect(move || {
        let ImageViewerArgs {
            images,
            index: start,
        } = args();
        index.set(start.min(images.len().saturating_sub(1)));
    });
    let count = args.read().images.len();
    use_window_keys(move |key| match key {
        ArrowKey::Left if *index.peek() > 0 => index -= 1,
        ArrowKey::Right if *index.peek() + 1 < args.peek().images.len() => index += 1,
        _ => {}
    });

    let mut zoom = use_signal(Zoom::default);
    let mut stage = use_signal(|| None::<Rc<MountedData>>);
    let mut resized = use_signal(|| 0u32);
    let mut drag = use_signal(|| None::<((f64, f64), (f64, f64))>);
    let mut tip = use_signal(|| 0u32);
    let mut tip_visible = use_signal(|| false);
    let mut fullscreen = use_signal(|| false);
    // Transition animation for double-click zoom; the wheel and dragging have no animation
    let mut animating = use_signal(|| 0u32);

    // The window is hidden at creation and shown by the shell after the content is mounted; measure
    // the display area again after it shows
    use_effect(move || resized.with_mut(|r| *r += 1));

    let loaded = use_resource(move || async move {
        let url = args().images.get(index()).cloned()?;
        let session = state.session.read().clone()?;
        match load_image(&session, &url).await {
            Ok(loaded) => Some(loaded),
            Err(error) => {
                log::warn!("载入图片失败：{error}");
                None
            }
        }
    });
    use_effect(move || {
        if let Some(Some(image)) = &*loaded.read() {
            zoom.write().set_image(image.size);
        }
    });

    // Size of the display area: measured on mount and again after the window size changes
    use_wry_event_handler({
        let window = window.clone();
        move |event, _| {
            if let Event::WindowEvent {
                event: WindowEvent::Resized(_),
                window_id,
                ..
            } = event
                && *window_id == window.id()
            {
                resized += 1;
            }
        }
    });
    use_resource(move || async move {
        let _ = resized();
        tokio::time::sleep(MEASURE_DELAY).await;
        let element = stage.peek().clone();
        if let Some(element) = element
            && let Ok(rect) = element.get_client_rect().await
        {
            zoom.write().set_stage((rect.size.width, rect.size.height));
        }
    });

    let mut show_tip = move || {
        tip += 1;
        tip_visible.set(true);
        let version = *tip.peek();
        spawn(async move {
            tokio::time::sleep(TIP_DURATION).await;
            if *tip.peek() == version {
                tip_visible.set(false);
            }
        });
    };

    let current = loaded.read().clone().flatten();
    let percent = zoom.read().percent();
    let ready = current.is_some();

    let save_as = use_callback(move |()| {
        let Some(image) = loaded.peek().clone().flatten() else {
            return;
        };
        spawn(async move {
            let name = file_name_of(&image.url, &image.path);
            overlays
                .save_as(&name, |target| async move {
                    tokio::fs::copy(&image.path, target).await.map(|_| ())
                })
                .await;
        });
    });
    let copy = use_callback(move |()| {
        if let Some(image) = loaded.peek().clone().flatten() {
            spawn(copy_image(overlays, image.path));
        }
    });
    let forward = use_callback(move |()| {
        let Some(image) = loaded.peek().clone().flatten() else {
            return;
        };
        let Some(session) = state.session.peek().clone() else {
            return;
        };
        spawn(async move {
            forward_text(overlays, &session, forward_markdown(&image.url)).await;
        });
    });
    let open_external = use_callback(move |()| {
        let Some(image) = loaded.peek().clone().flatten() else {
            return;
        };
        spawn(async move {
            let name = file_name_of(&image.url, &image.path);
            match external_copy(&image.path, &name).await {
                Ok(path) => overlays.open_file(&path),
                Err(error) => overlays.report_error("打开失败", error),
            }
        });
    });
    let toggle_fullscreen = {
        let window = window.clone();
        move |_| {
            let next = !fullscreen();
            window.set_fullscreen(next);
            fullscreen.set(next);
        }
    };

    rsx! {
        div { class: "viewer",
            ContextMenuArea {
                class: "viewer-stage",
                items: move |()| {
                    if !ready {
                        return Vec::new();
                    }
                    // The viewer's menus have no icons
                    vec![
                        MenuEntry::item("使用系统图片查看器打开", move || open_external.call(())).plain(),
                        MenuEntry::Separator,
                        MenuEntry::item("复制", move || copy.call(())).plain(),
                        MenuEntry::item("转发", move || forward.call(())).plain(),
                        MenuEntry::item("另存为", move || save_as.call(())).plain(),
                    ]
                },
                div {
                    class: "viewer-surface",
                    onmounted: move |e| {
                        stage.set(Some(e.data()));
                        resized += 1;
                    },
                    onwheel: move |e| {
                        let notches = match e.delta() {
                            WheelDelta::Pixels(v) => v.y / WHEEL_NOTCH,
                            WheelDelta::Lines(v) => v.y,
                            WheelDelta::Pages(v) => v.y * 10.0,
                        };
                        let at = e.element_coordinates();
                        zoom.write().wheel(-notches, (at.x, at.y));
                        show_tip();
                    },
                    onmousedown: move |e| {
                        if e.trigger_button() == Some(MouseButton::Primary) {
                            let at = e.client_coordinates();
                            drag.set(Some(((at.x, at.y), zoom.peek().offset())));
                        }
                    },
                    onmousemove: move |e| {
                        let Some((start, origin)) = *drag.peek() else { return };
                        let at = e.client_coordinates();
                        zoom.write().set_offset((origin.0 + at.x - start.0, origin.1 + at.y - start.1));
                    },
                    onmouseup: move |_| drag.set(None),
                    onmouseleave: move |_| drag.set(None),
                    ondoubleclick: move |e| {
                        let at = e.element_coordinates();
                        zoom.write().toggle_fit_at((at.x, at.y));
                        show_tip();
                        animating += 1;
                        let version = *animating.peek();
                        spawn(async move {
                            tokio::time::sleep(ZOOM_ANIMATION).await;
                            if *animating.peek() == version {
                                animating.set(0);
                            }
                        });
                    },
                    if let Some(image) = current.as_ref().filter(|image| zoom.read().placed(image.size)) {
                        img {
                            class: if animating() > 0 { "viewer-image animating" } else { "viewer-image" },
                            src: "{image.src}",
                            draggable: "false",
                            style: "{zoom.read().style()}",
                        }
                    }
                }
                match &*loaded.read() {
                    None => rsx! { div { class: "viewer-status text-sub", "加载中…" } },
                    Some(None) => rsx! { div { class: "viewer-status text-sub", "图片加载失败" } },
                    Some(Some(_)) => rsx! {},
                }
                if index() > 0 {
                    Tooltip { text: "上一张（←）",
                        IconButton {
                            icon: IconName::ChevronLeft,
                            size: 36.0,
                            icon_size: 20.0,
                            class: "viewer-nav left",
                            onclick: move |_| index -= 1,
                        }
                    }
                }
                if index() + 1 < count {
                    Tooltip { text: "下一张（→）",
                        IconButton {
                            icon: IconName::ChevronRight,
                            size: 36.0,
                            icon_size: 20.0,
                            class: "viewer-nav right",
                            onclick: move |_| index += 1,
                        }
                    }
                }
                if tip_visible() {
                    div { class: "viewer-tip", "{percent}%" }
                }
            }
            div { class: "viewer-toolbar",
                Tooltip { text: "缩小",
                    IconButton {
                        icon: IconName::ZoomOut,
                        disabled: !ready,
                        onclick: move |_| {
                            zoom.write().zoom_out();
                            show_tip();
                        },
                    }
                }
                span { class: "viewer-scale text-sub", "{percent}%" }
                Tooltip { text: "放大",
                    IconButton {
                        icon: IconName::ZoomIn,
                        disabled: !ready,
                        onclick: move |_| {
                            zoom.write().zoom_in();
                            show_tip();
                        },
                    }
                }
                Tooltip { text: "适应窗口",
                    Button {
                        kind: ButtonKind::Flat,
                        disabled: !ready,
                        onclick: move |_| {
                            zoom.write().apply_fit();
                            show_tip();
                        },
                        "适应"
                    }
                }
                Tooltip { text: "原始大小",
                    Button {
                        kind: ButtonKind::Flat,
                        disabled: !ready,
                        onclick: move |_| {
                            zoom.write().actual_size();
                            show_tip();
                        },
                        "1:1"
                    }
                }
                Tooltip { text: "向左旋转",
                    IconButton {
                        icon: IconName::RotateLeft,
                        disabled: !ready,
                        onclick: move |_| zoom.write().rotate_left(),
                    }
                }
                span { class: "viewer-separator" }
                Tooltip { text: "另存为",
                    IconButton { icon: IconName::Download, disabled: !ready, onclick: move |_| save_as.call(()) }
                }
                Tooltip { text: "复制",
                    IconButton { icon: IconName::Copy, disabled: !ready, onclick: move |_| copy.call(()) }
                }
                Tooltip { text: "转发",
                    IconButton { icon: IconName::Share, disabled: !ready, onclick: move |_| forward.call(()) }
                }
                Tooltip { text: "用系统图片查看器打开",
                    IconButton { icon: IconName::OpenExternal, disabled: !ready, onclick: move |_| open_external.call(()) }
                }
                Tooltip { text: if fullscreen() { "退出全屏" } else { "全屏" },
                    IconButton { icon: IconName::Fullscreen, onclick: toggle_fullscreen }
                }
                if count > 1 {
                    span { class: "viewer-count text-sub", "{index() + 1} / {count}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_large_falls_back_to_the_original() {
        assert_eq!(
            fallback_url("/user_avatars/2/abc-medium.png?version=3").as_deref(),
            Some("/user_avatars/2/abc.png?version=3")
        );
        assert_eq!(
            fallback_url("https://cdn.example.com/a-MEDIUM.PNG").as_deref(),
            Some("https://cdn.example.com/a.png")
        );
        assert_eq!(fallback_url("/user_avatars/2/abc.png"), None);
        assert_eq!(fallback_url("/user_uploads/1/ab/截图.png"), None);
        assert_eq!(fallback_url("é"), None);
    }

    #[test]
    fn file_names_come_from_the_url() {
        let local = Path::new("cache/abcdef");
        assert_eq!(
            file_name_of("/user_uploads/1/ab/%E6%88%AA%E5%9B%BE.png", local),
            "截图.png"
        );
        assert_eq!(
            file_name_of("https://x.test/a/photo?x=1", local),
            "photo.png"
        );
        assert_eq!(
            file_name_of("https://x.test/a/photo", Path::new("cache/h.jpg")),
            "photo.jpg"
        );
        assert_eq!(file_name_of("https://x.test/", local), "图片.png");
    }

    #[test]
    fn forwarding_uses_upload_links_for_own_images() {
        assert_eq!(
            forward_markdown("/user_uploads/1/ab/x.png"),
            "[x.png](/user_uploads/1/ab/x.png)"
        );
        assert_eq!(
            forward_markdown("https://x.test/a.png"),
            "https://x.test/a.png"
        );
    }
}
