//! The account's avatar (downloaded with this account's session) and the cropping dialog when
//! changing the avatar.

use crate::shell::url_for;
use crate::ui::*;
use crate::widgets::SessionRef;
use dioxus::html::geometry::WheelDelta;
use dioxus::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use zulip_core::avatar::AvatarCrop;
use zulip_session::AvatarImage;

/// Side length of the crop frame.
const VIEWPORT: f64 = 320.0;
const ZOOM_STEP: f64 = 0.1;
const WHEEL_STEP: f64 = 0.08;

/// The account's avatar: the account the window follows is not necessarily this one, so it
/// downloads with the specified session; with no session only text is shown.
#[component]
pub fn AccountAvatar(
    session: Option<SessionRef>,
    #[props(into)] text: String,
    #[props(into)] url: Option<String>,
    #[props(default = 32.0)] size: f64,
    /// An account that is not online.
    #[props(default)]
    dimmed: bool,
    onclick: Option<EventHandler<MouseEvent>>,
) -> Element {
    let src = use_resource(use_reactive!(|session, url| async move {
        let (session, url) = (session?, url?);
        let path = session.0.image_file(&url).await.ok()?;
        url_for(&path)
    }));
    rsx! {
        span { class: if dimmed { "settings-dimmed" } else { "settings-avatar" },
            Avatar { text, src: src.read().clone().flatten(), size, onclick }
        }
    }
}

/// The image in the dialog, compared by pointer.
#[derive(Clone)]
struct CropSource {
    image: Arc<AvatarImage>,
    url: String,
}

impl PartialEq for CropSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.image, &other.image)
    }
}

/// Read an image and let the user crop it, returning the cropped PNG; `Ok(None)` on cancel.
pub async fn crop_avatar(overlays: Overlays, path: PathBuf) -> Result<Option<Vec<u8>>, String> {
    let url = url_for(&path).ok_or("无法读取这张图片")?;
    let source = path.clone();
    let image = tokio::task::spawn_blocking(move || AvatarImage::load(&source))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let source = CropSource {
        image: Arc::new(image),
        url,
    };
    Ok(overlays
        .ask_dialog(move |answer| rsx! { CropDialog { source: source.clone(), answer } })
        .await)
}

#[component]
fn CropDialog(source: CropSource, answer: Answer<Vec<u8>>) -> Element {
    let overlays = use_overlays();
    let mut crop = use_signal(|| {
        AvatarCrop::new(source.image.width(), source.image.height(), VIEWPORT)
            .expect("图片和取景框的尺寸为正数")
    });
    let mut drag = use_signal(|| None::<(f64, f64)>);
    let mut saving = use_signal(|| false);
    let rect = crop.read().image_rect();
    let zoom = crop.read().zoom_position();
    let can_zoom = crop.read().can_zoom();
    let (ok, cancel, close) = (answer.clone(), answer.clone(), answer);
    let image = source.image.clone();
    rsx! {
        DialogFrame {
            title: "裁剪图片",
            width: VIEWPORT + 32.0,
            on_close: move |_| close.cancel(),
            footer: rsx! {
                DialogButtons {
                    confirm_text: "保存",
                    disabled: saving(),
                    on_confirm: move |_| {
                        let area = crop.peek().crop_rect();
                        let image = image.clone();
                        let ok = ok.clone();
                        saving.set(true);
                        spawn(async move {
                            let png = tokio::task::spawn_blocking(move || image.render_png(area)).await;
                            saving.set(false);
                            match png {
                                Ok(Ok(png)) => ok.send(png),
                                Ok(Err(error)) => overlays.report_error("裁剪失败", error),
                                Err(error) => overlays.report_error("裁剪失败", error),
                            }
                        });
                    },
                    on_cancel: move |_| cancel.cancel(),
                }
            },
            div {
                class: "settings-crop",
                style: "width:{VIEWPORT}px;height:{VIEWPORT}px",
                onmousedown: move |e| {
                    let at = e.client_coordinates();
                    drag.set(Some((at.x, at.y)));
                },
                onmousemove: move |e| {
                    let Some((x, y)) = drag() else { return };
                    let at = e.client_coordinates();
                    drag.set(Some((at.x, at.y)));
                    crop.write().pan(at.x - x, at.y - y);
                },
                onmouseup: move |_| drag.set(None),
                onmouseleave: move |_| drag.set(None),
                onwheel: move |e| {
                    e.prevent_default();
                    let up = match e.delta() {
                        WheelDelta::Pixels(v) => v.y < 0.0,
                        WheelDelta::Lines(v) => v.y < 0.0,
                        WheelDelta::Pages(v) => v.y < 0.0,
                    };
                    let at = e.element_coordinates();
                    let position = crop.peek().zoom_position() + if up { WHEEL_STEP } else { -WHEEL_STEP };
                    crop.write().set_zoom(position, at.x, at.y);
                },
                img {
                    src: "{source.url}",
                    draggable: "false",
                    style: "left:{rect.x}px;top:{rect.y}px;width:{rect.width}px;height:{rect.height}px",
                }
                div { class: "settings-crop-mask" }
            }
            div { class: "settings-crop-zoom",
                IconButton {
                    icon: IconName::ZoomOut,
                    disabled: !can_zoom,
                    onclick: move |_| {
                        let position = crop.peek().zoom_position() - ZOOM_STEP;
                        crop.write().set_zoom_position(position);
                    },
                }
                Slider {
                    value: zoom,
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    disabled: !can_zoom,
                    oninput: move |v| {
                        crop.write().set_zoom_position(v);
                    },
                }
                IconButton {
                    icon: IconName::ZoomIn,
                    disabled: !can_zoom,
                    onclick: move |_| {
                        let position = crop.peek().zoom_position() + ZOOM_STEP;
                        crop.write().set_zoom_position(position);
                    },
                }
            }
        }
    }
}
