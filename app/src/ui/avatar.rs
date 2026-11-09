use dioxus::prelude::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

thread_local! {
    /// 加载成功过的图片地址。重新挂载时直接显示，不再经过文字色块。
    static LOADED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

const PALETTE: [&str; 7] = [
    "#0052D9", "#2BA471", "#E37318", "#D54941", "#8E56DD", "#029CD4", "#E851B3",
];

/// 名称稳定地映射到色板。
fn palette_color(text: &str) -> &'static str {
    let hash = text
        .encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)));
    PALETTE[(hash & i32::MAX) as usize % PALETTE.len()]
}

/// 第一个字符（大写）。
fn initial(text: &str) -> String {
    text.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}

/// 图片状态：已显示的地址、加载失败的地址、属于谁。
#[derive(Default, PartialEq, Debug)]
struct ImageState {
    owner: String,
    shown: Option<String>,
    failed: Option<String>,
}

impl ImageState {
    /// 同一个人换地址时保留旧图直到新图加载完；换人或没有地址时立刻清除。
    fn sync(&mut self, owner: &str, src: Option<&str>) {
        if self.owner != owner {
            self.owner = owner.to_string();
            self.shown = None;
            self.failed = None;
        }
        if src.is_none() {
            self.shown = None;
            self.failed = None;
        }
    }

    /// 正在加载、还没有显示的地址。
    fn loading<'a>(&self, src: Option<&'a str>) -> Option<&'a str> {
        src.filter(|s| self.shown.as_deref() != Some(s) && self.failed.as_deref() != Some(s))
    }

    fn loaded(&mut self, src: &str) {
        self.shown = Some(src.to_string());
        self.failed = None;
    }

    /// 失败时回到文字头像，不显示旧图。
    fn load_failed(&mut self, src: &str) {
        self.failed = Some(src.to_string());
        self.shown = None;
    }
}

/// 圆形头像。图片加载前和失败时显示文字色块。
///
/// `owner` 标识头像属于谁（用户 ID 等）：列表复用组件换人时要传不同的值，旧图才会立刻清除；
/// 缺省时用 `text` 判断。
#[component]
pub fn Avatar(
    #[props(into)] text: String,
    #[props(into)] src: Option<String>,
    #[props(into)] owner: Option<String>,
    #[props(default = 40.0)] size: f64,
    /// 文字色块的背景；缺省按名称取色板颜色。
    #[props(into)]
    fill: Option<String>,
    #[props(default)] online: bool,
    onclick: Option<EventHandler<MouseEvent>>,
) -> Element {
    let state = use_hook(|| Rc::new(RefCell::new(ImageState::default())));
    let mut tick = use_signal(|| 0u32);
    let _ = tick();

    let src_ref = src.as_deref().filter(|s| !s.is_empty());
    let owner_key = owner.as_deref().unwrap_or(&text);
    let (shown, loading) = {
        let mut st = state.borrow_mut();
        st.sync(owner_key, src_ref);
        if let Some(url) = st.loading(src_ref)
            && LOADED.with_borrow(|l| l.contains(url))
        {
            st.loaded(url);
        }
        (st.shown.clone(), st.loading(src_ref).map(str::to_string))
    };

    let background = fill.as_deref().unwrap_or_else(|| palette_color(&text));
    let font_size = size * 0.42;
    let on_load = {
        let state = state.clone();
        move |url: String| {
            state.borrow_mut().loaded(&url);
            LOADED.with_borrow_mut(|l| l.insert(url));
            tick += 1;
        }
    };
    let on_error = {
        let state = state.clone();
        move |url: String| {
            state.borrow_mut().load_failed(&url);
            LOADED.with_borrow_mut(|l| l.remove(&url));
            tick += 1;
        }
    };

    let avatar = rsx! {
        div {
            class: if onclick.is_some() { "avatar clickable" } else { "avatar" },
            style: "width:{size}px;height:{size}px;font-size:{font_size}px;background:{background}",
            onclick: move |e| {
                if let Some(h) = onclick {
                    h.call(e);
                }
            },
            "{initial(&text)}"
            if let Some(url) = shown {
                img {
                    key: "{url}",
                    src: "{url}",
                    draggable: "false",
                    alt: "",
                    decoding: "sync",
                    onerror: {
                        let mut on_error = on_error.clone();
                        let url = url.clone();
                        move |_| on_error(url.clone())
                    },
                }
            }
            if let Some(url) = loading {
                img {
                    key: "{url}",
                    class: "loading",
                    src: "{url}",
                    draggable: "false",
                    alt: "",
                    onload: {
                        let mut on_load = on_load.clone();
                        let url = url.clone();
                        move |_| on_load(url.clone())
                    },
                    onerror: {
                        let mut on_error = on_error.clone();
                        let url = url.clone();
                        move |_| on_error(url.clone())
                    },
                }
            }
        }
    };
    if online {
        rsx! {
            span { class: "avatar-wrap",
                {avatar}
                span { class: "online-dot" }
            }
        }
    } else {
        avatar
    }
}

/// 从左向右重叠排列（投票人头像）：后一个压在前一个下面，放不下时被裁掉。
#[component]
pub fn OverlapStack(
    #[props(default = 12.0)] step: f64,
    #[props(default = 24.0)] item_size: f64,
    items: Vec<Element>,
) -> Element {
    let overlap = step - item_size;
    let count = items.len();
    rsx! {
        div { class: "overlap-stack",
            for (i, item) in items.into_iter().enumerate() {
                div {
                    key: "{i}",
                    style: if i == 0 { format!("position:relative;z-index:{count}") } else { format!("position:relative;z-index:{};margin-left:{overlap}px", count - i) },
                    {item}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_is_stable() {
        assert_eq!(palette_color(""), "#0052D9");
        assert_eq!(palette_color("张三"), palette_color("张三"));
    }

    #[test]
    fn initial_takes_first_character() {
        assert_eq!(initial("alice"), "A");
        assert_eq!(initial("张三"), "张");
        assert_eq!(initial("😀 x"), "😀");
        assert_eq!(initial(""), "");
    }

    #[test]
    fn keeps_old_image_until_new_one_loads() {
        let mut s = ImageState::default();
        s.sync("u1", Some("a.png"));
        assert_eq!(s.loading(Some("a.png")), Some("a.png"));
        s.loaded("a.png");
        s.sync("u1", Some("b.png"));
        assert_eq!(s.shown.as_deref(), Some("a.png"));
        assert_eq!(s.loading(Some("b.png")), Some("b.png"));
        s.loaded("b.png");
        assert_eq!(s.loading(Some("b.png")), None);
    }

    #[test]
    fn clears_immediately_when_owner_changes() {
        let mut s = ImageState::default();
        s.sync("u1", Some("a.png"));
        s.loaded("a.png");
        s.sync("u2", Some("b.png"));
        assert_eq!(s.shown, None);
    }

    #[test]
    fn failed_load_falls_back_to_text_and_does_not_retry() {
        let mut s = ImageState::default();
        s.sync("u1", Some("a.png"));
        s.loaded("a.png");
        s.sync("u1", Some("b.png"));
        s.load_failed("b.png");
        assert_eq!(s.shown, None);
        assert_eq!(s.loading(Some("b.png")), None);
    }

    #[test]
    fn removing_source_clears_image() {
        let mut s = ImageState::default();
        s.sync("u1", Some("a.png"));
        s.loaded("a.png");
        s.sync("u1", None);
        assert_eq!(s.shown, None);
    }
}
