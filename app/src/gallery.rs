//! 隐藏的组件预览页（`ZULIP_GALLERY=1`）：展示所有基础组件和平台能力，用来验证。

use crate::platform::{self, ClipboardImage, FileFilter, LaunchAtStartup, TrayEvent, TrayItem};
use crate::shell::{
    self, ColorScheme, PRESETS, Palette, ThemeConfig, WindowShell, WindowSpec, open_singleton,
    use_theme,
};
use crate::ui::*;
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use std::rc::Rc;
use std::time::Duration;
use zulip_store::{MAX_FONT_SIZE, MIN_FONT_SIZE};

const SAMPLE_AVATAR: &str = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 40 40'><rect width='40' height='40' fill='%232BA471'/><circle cx='20' cy='16' r='7' fill='white'/><rect x='8' y='26' width='24' height='14' rx='7' fill='white'/></svg>";
const SAMPLE_AVATAR_2: &str = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 40 40'><rect width='40' height='40' fill='%238E56DD'/><circle cx='20' cy='20' r='9' fill='white'/></svg>";
const BROKEN_AVATAR: &str = "http://zulip-file.localhost/not-registered.png";
const SLOW_AVATAR: &str = "http://zulip-file.localhost/slow.png";

#[component]
pub fn Gallery() -> Element {
    rsx! {
        WindowShell {
            title: "YaZ 组件预览",
            GalleryBody {}
        }
    }
}

#[component]
fn GalleryBody() -> Element {
    rsx! {
        div { class: "gallery scroll",
            ThemeSection {}
            IconSection {}
            AvatarSection {}
            ButtonSection {}
            InputSection {}
            ControlSection {}
            OverlaySection {}
            FlowSection {}
            MenuSection {}
            ExpanderSection {}
            ListSection {}
            WindowSection {}
            PlatformSection {}
        }
    }
}

#[component]
fn ThemeSection() -> Element {
    let theme = use_theme();
    let config: ThemeConfig = theme.config();
    let scheme = match config.scheme {
        ColorScheme::System => "system",
        ColorScheme::Dark => "dark",
        ColorScheme::Light => "light",
    };
    let theme_for_scheme = theme.clone();
    let theme_for_size = theme.clone();
    let theme_for_preview = theme.clone();
    let theme_for_opacity = theme.clone();
    let theme_for_image = theme.clone();
    let palette_for_opacity = config.palette.clone();
    let palette_for_image = config.palette.clone();
    let swatches = PRESETS.iter().map(|preset| {
        let theme = theme.clone();
        let palette = config.palette.clone();
        rsx! {
            Button {
                key: "{preset.key}",
                kind: if palette.preset == preset.key { ButtonKind::Primary } else { ButtonKind::Secondary },
                onclick: move |_| theme.set_palette(Palette { preset: preset.key.into(), ..palette.clone() }),
                "{preset.name}"
            }
        }
    });
    rsx! {
        h2 { "主题" }
        div { class: "gallery-row",
            div { style: "width:140px",
                Select {
                    value: scheme,
                    options: vec![
                        SelectOption::new("system", "跟随系统"),
                        SelectOption::new("light", "浅色"),
                        SelectOption::new("dark", "深色"),
                    ],
                    onchange: move |v: String| {
                        theme_for_scheme.set_scheme(match v.as_str() {
                            "dark" => ColorScheme::Dark,
                            "light" => ColorScheme::Light,
                            _ => ColorScheme::System,
                        });
                    },
                }
            }
            Toggle {
                checked: config.preview_dark,
                onchange: move |on| theme_for_preview.set_preview_dark(on),
                "预览夜间"
            }
            span { "字号 {config.font_size}" }
            div { style: "width:140px",
                Slider {
                    value: f64::from(config.font_size),
                    min: f64::from(MIN_FONT_SIZE),
                    max: f64::from(MAX_FONT_SIZE),
                    oninput: move |v: f64| theme_for_size.set_font_size(v as u32),
                }
            }
        }
        div { class: "gallery-row", {swatches} }
        div { class: "gallery-row",
            Button {
                onclick: move |_| {
                    let theme = theme_for_image.clone();
                    let palette = palette_for_image.clone();
                    async move {
                        let filter = FileFilter { name: "图片", extensions: &["png", "jpg", "jpeg", "webp"] };
                        if let Some(path) = platform::pick_file("选择背景图", &[filter]).await {
                            theme.set_palette(Palette { background_image: Some(path), ..palette });
                        }
                    }
                },
                "选择背景图"
            }
            span { "面板不透明度" }
            div { style: "width:140px",
                Slider {
                    value: config.palette.panel_opacity,
                    min: 0.6,
                    max: 1.0,
                    step: 0.05,
                    oninput: move |v: f64| {
                        theme_for_opacity.set_palette(Palette { panel_opacity: v, ..palette_for_opacity.clone() });
                    },
                }
            }
        }
    }
}

#[component]
fn IconSection() -> Element {
    rsx! {
        h2 { "图标（{IconName::ALL.len()}）" }
        div { class: "gallery-icons",
            for name in IconName::ALL.iter().copied() {
                div { class: "gallery-icon bounce-icon", key: "{name:?}",
                    Icon { name, size: 20.0 }
                    "{name:?}"
                }
            }
        }
    }
}

#[component]
fn AvatarSection() -> Element {
    let overlays = use_overlays();
    let mut swap_src = use_signal(|| SAMPLE_AVATAR.to_string());
    let mut owner = use_signal(|| 1);
    rsx! {
        h2 { "头像" }
        div { class: "gallery-row",
            Avatar { text: "文字色块" }
            Avatar { text: "alice", size: 32.0 }
            Avatar { text: "bob", size: 24.0, online: true }
            Avatar { text: "图片", src: SAMPLE_AVATAR, onclick: move |_| overlays.toast("点击了头像") }
            Avatar { text: "失败", src: BROKEN_AVATAR }
            Avatar { text: "助手", fill: "var(--helper-avatar)" }
            span { id: "avatar-swap",
                Avatar { text: "换图", owner: "{owner}", src: "{swap_src}" }
            }
            Button { onclick: move |_| swap_src.set(SAMPLE_AVATAR_2.to_string()), "同一人换地址" }
            Button { onclick: move |_| swap_src.set(SLOW_AVATAR.to_string()), "换到加载失败的地址" }
            Button {
                onclick: move |_| {
                    owner += 1;
                    swap_src.set(SAMPLE_AVATAR.to_string());
                },
                "换人"
            }
            OverlapStack {
                items: vec![
                    rsx! { Avatar { text: "甲", size: 24.0 } },
                    rsx! { Avatar { text: "乙", size: 24.0 } },
                    rsx! { Avatar { text: "丙", size: 24.0 } },
                ],
            }
        }
    }
}

#[component]
fn ButtonSection() -> Element {
    rsx! {
        h2 { "按钮、徽标" }
        div { class: "gallery-row",
            Button { kind: ButtonKind::Primary, "主要" }
            Button { kind: ButtonKind::Secondary, "次要" }
            Button { kind: ButtonKind::Danger, "危险" }
            Button { kind: ButtonKind::Flat, icon: IconName::Add, "扁平" }
            Button { kind: ButtonKind::Primary, disabled: true, "禁用" }
            Button { kind: ButtonKind::Secondary, disabled: true, "禁用" }
            IconButton { icon: IconName::Setting }
            IconButton { icon: IconName::Delete, disabled: true }
            IconButton {
                icon: IconName::More,
                image: "data:image/svg+xml;charset=utf-8,%3Csvg%20xmlns%3D%22http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%22%20viewBox%3D%220%200%2024%2024%22%3E%3Ccircle%20cx%3D%2212%22%20cy%3D%2212%22%20r%3D%2210%22%2F%3E%3C%2Fsvg%3E".to_owned(),
            }
            Badge { count: 3 }
            Badge { count: 120 }
            Badge { count: 8, muted: true }
            Badge { dot: true }
            RoleBadge { owner: true, "群主" }
            RoleBadge { "管理员" }
            Tooltip { text: "这是文字提示",
                Button { "悬停看提示" }
            }
        }
    }
}

#[component]
fn InputSection() -> Element {
    let mut text = use_signal(String::new);
    let mut search = use_signal(|| "可清除".to_string());
    let mut area = use_signal(String::new);
    let mut name = use_signal(|| "行内编辑的文字".to_string());
    let mut editing = use_signal(|| false);
    let mut shortcut = use_signal(|| "Ctrl+K".to_string());
    let overlays = use_overlays();
    rsx! {
        h2 { "输入" }
        div { class: "gallery-row",
            div { style: "width:200px",
                TextInput {
                    value: text(),
                    placeholder: "普通输入框",
                    oninput: move |v| text.set(v),
                    onsubmit: move |v: String| overlays.toast(format!("提交：{v}")),
                }
            }
            div { style: "width:200px",
                TextInput { value: "", placeholder: "错误状态", invalid: true }
            }
            div { style: "width:200px",
                TextInput { value: "", placeholder: "密码", password: true }
            }
            div { style: "width:200px",
                TextInput {
                    value: search(),
                    style_kind: InputStyle::Search,
                    placeholder: "搜索",
                    icon: IconName::Search,
                    clearable: true,
                    oninput: move |v| search.set(v),
                }
            }
            div { style: "width:200px",
                TextInput {
                    value: "",
                    style_kind: InputStyle::SearchRing,
                    placeholder: "带焦点边框",
                    icon: IconName::Search,
                }
            }
        }
        div { class: "gallery-row",
            div { style: "width:300px",
                TextInput {
                    value: area(),
                    multiline: true,
                    placeholder: "多行输入",
                    oninput: move |v| area.set(v),
                    onblur: move |_| {},
                }
            }
            div { style: "width:200px",
                if editing() {
                    InlineEdit {
                        value: name(),
                        oncommit: move |v| {
                            name.set(v);
                            editing.set(false);
                        },
                        oncancel: move |_| editing.set(false),
                    }
                } else {
                    span { id: "inline-text", ondoubleclick: move |_| editing.set(true), "{name}" }
                }
            }
            Button { onclick: move |_| editing.set(true), "开始编辑" }
            div { style: "width:160px",
                ShortcutRecorder { value: shortcut(), onchange: move |v| shortcut.set(v) }
            }
        }
    }
}

#[component]
fn ControlSection() -> Element {
    let mut checked = use_signal(|| true);
    let mut radio = use_signal(|| 0);
    let mut toggled = use_signal(|| true);
    let mut slider = use_signal(|| 40.0);
    let mut select = use_signal(|| "b".to_string());
    let mut tab = use_signal(|| "one".to_string());
    let mut pill = use_signal(|| "all".to_string());
    rsx! {
        h2 { "选择控件" }
        div { class: "gallery-row",
            Checkbox { checked: checked(), onchange: move |v| checked.set(v), "复选框" }
            Checkbox { checked: false, disabled: true, "禁用" }
            Checkbox { checked: radio() == 0, radio: true, onchange: move |_| radio.set(0), "单选一" }
            Checkbox { checked: radio() == 1, radio: true, onchange: move |_| radio.set(1), "单选二" }
            Toggle { checked: toggled(), onchange: move |v| toggled.set(v), "开关" }
            Toggle { checked: false, disabled: true, "禁用" }
            div { style: "width:180px", Slider { value: slider(), oninput: move |v| slider.set(v) } }
            span { "{slider}" }
            div { style: "width:160px",
                Select {
                    value: select(),
                    options: vec![
                        SelectOption::new("a", "选项 A"),
                        SelectOption::new("b", "选项 B"),
                        SelectOption::new("c", "很长很长很长很长很长很长的选项 C"),
                    ],
                    onchange: move |v| select.set(v),
                }
            }
            div { style: "width:160px",
                Select { value: "", placeholder: "未选择", options: vec![SelectOption::new("x", "选项 X")] }
            }
        }
        Tabs {
            value: tab(),
            items: vec![
                TabItem::new("one", "消息").icon(IconName::Chat),
                TabItem::new("two", "联系人"),
                TabItem::new("three", "文件"),
            ],
            onchange: move |v| tab.set(v),
        }
        div { style: "padding:8px 0", "当前标签：{tab}" }
        Tabs {
            pill: true,
            value: pill(),
            items: vec![TabItem::new("all", "全部"), TabItem::new("unread", "未读"), TabItem::new("at", "@我")],
            onchange: move |v| pill.set(v),
        }
    }
}

#[component]
fn OverlaySection() -> Element {
    let overlays = use_overlays();
    let mut result = use_signal(|| "尚未确认".to_string());
    rsx! {
        h2 { "弹层" }
        div { class: "gallery-row",
            Button {
                onclick: move |_| async move {
                    let ok = overlays.confirm(Confirm::new("确定要退出当前账号吗？").danger("退出")).await;
                    result.set(format!("确认框结果：{ok}"));
                },
                "确认框"
            }
            Button {
                kind: ButtonKind::Danger,
                onclick: move |_| async move {
                    let ok = overlays.confirm(Confirm::delete("这条消息", Some("删除后无法恢复。"))).await;
                    result.set(format!("确认框结果：{ok}"));
                },
                "危险确认框"
            }
            Button {
                onclick: move |_| {
                    overlays.open_dialog(true, |handle| rsx! {
                        DialogFrame {
                            title: "自定义对话框",
                            width: 420.0,
                            on_close: move |_| handle.close(),
                            footer: rsx! { Button { kind: ButtonKind::Primary, onclick: move |_| handle.close(), "完成" } },
                            DialogSample {}
                        }
                    });
                },
                "对话框"
            }
            Button { onclick: move |_| overlays.toast("这是一条提示"), "提示" }
            Button { onclick: move |_| overlays.toast_kind(ToastKind::Success, "保存成功"), "成功提示" }
            Button { onclick: move |_| overlays.toast_kind(ToastKind::Error, "发送失败"), "错误提示" }
            Popover {
                content: move |handle: PopoverHandle| rsx! { PopoverSample { handle } },
                Button { "弹出卡片" }
            }
            Popover {
                options: PopoverOptions { placement: Placement::Top, ..PopoverOptions::default() },
                content: move |handle: PopoverHandle| rsx! { PopoverSample { handle } },
                Avatar { text: "名片", size: 32.0, onclick: move |_| {} }
            }
            span { id: "confirm-result", "{result}" }
        }
    }
}

/// 统一流程和复用块。
#[component]
fn FlowSection() -> Element {
    let overlays = use_overlays();
    let mut flow_result = use_signal(|| "尚未操作".to_string());
    let mut dirty = use_signal(|| false);
    let people = || {
        (1..=2000u64)
            .map(|i| {
                PickerItem::new(i, format!("成员 {i}"))
                    .subtitle(format!("user{i}@example.com"))
                    .avatar("http://zulip-file.localhost/missing.png")
            })
            .collect::<Vec<_>>()
    };
    rsx! {
        h2 { "统一流程" }
        div { class: "gallery-row",
            Button {
                onclick: move |_| async move {
                    let done = overlays
                        .delete("话题「示例」", Some("话题中的所有消息将被永久删除。"), || async { Ok::<_, String>(()) })
                        .await;
                    flow_result.set(format!("删除成功：{}", done.is_some()));
                },
                "删除（成功）"
            }
            Button {
                onclick: move |_| async move {
                    let done = overlays
                        .delete("草稿", None, || async { Err::<(), _>("服务器拒绝".to_string()) })
                        .await;
                    flow_result.set(format!("删除成功：{}", done.is_some()));
                },
                "删除（失败）"
            }
            Button {
                onclick: move |_| async move {
                    let answer = overlays.ask(Confirm::new("退出账号？").confirm_text("退出").check("同时删除所有数据")).await;
                    flow_result.set(format!("复选框结果：{answer:?}"));
                },
                "带复选框的确认"
            }
            Button {
                onclick: move |_| async move {
                    let name = overlays.prompt_text(TextPrompt::new("重命名话题", "话题名称", "旧名称").max_len(20)).await;
                    flow_result.set(format!("重命名：{name:?}"));
                },
                "重命名"
            }
            Button {
                onclick: move |_| async move {
                    let remark = overlays.prompt_text(TextPrompt::remark("设置备注", "张三", Some("老张"))).await;
                    flow_result.set(format!("备注：{remark:?}"));
                },
                "备注"
            }
            Button {
                onclick: move |_| async move {
                    let leave = overlays.confirm_discard().await;
                    flow_result.set(format!("放弃修改：{leave}"));
                },
                "未保存确认"
            }
            Button { onclick: move |_| overlays.copy_text("复制的文字"), "复制" }
            Button {
                onclick: move |_| async move {
                    let picked = overlays.pick(Picker::single("选择用户", people())).await;
                    flow_result.set(format!("单选：{picked:?}"));
                },
                "单选选择器"
            }
            Button {
                onclick: move |_| async move {
                    let picked = overlays
                        .pick(Picker::multi("转发", people(), vec![2]).max_selected(3).confirm_text("发送"))
                        .await;
                    flow_result.set(format!("多选：{picked:?}"));
                },
                "多选选择器"
            }
            Button { onclick: move |_| dirty.toggle(), "未保存提示条" }
            span { id: "flow-result", "{flow_result}" }
        }
        div { class: "card", style: "position:relative;margin-bottom:8px",
            ListRow {
                density: RowDensity::Large,
                pinned: true,
                leading: rsx! { Avatar { text: "置顶", size: 44.0 } },
                title: "置顶的会话",
                meta: "12:30",
                subtitle: "最后一条消息",
                trailing: rsx! { Badge { count: 5 } },
                onclick: move |_| {},
            }
            ListRow {
                density: RowDensity::Large,
                selected: true,
                leading: rsx! { Avatar { text: "选中", size: 44.0 } },
                title: "选中的会话",
                subtitle_prefix: rsx! { span { style: "color:var(--danger);font-size:var(--fs-sub)", "[有人@我]" } },
                subtitle: "预览文字",
                onclick: move |_| {},
                menu: move |_| vec![MenuEntry::item("置顶", move || overlays.toast("置顶")).icon(IconName::Backtop)],
            }
            ListRow {
                density: RowDensity::Compact,
                cells: rsx! { span { "自定义内容（表格的一行）" } },
                trailing: rsx! { Icon { name: IconName::ArrowRightUp, size: 14.0 } },
                onclick: move |_| {},
            }
            ListRow {
                leading: rsx! { Avatar { text: "联系人", size: 36.0 } },
                title: "搜索命中 Zulip 的联系人",
                query: "zulip",
                onclick: move |_| {},
            }
            ListRow {
                density: RowDensity::Compact,
                checked: true,
                leading: rsx! { Avatar { text: "成员", size: 28.0 } },
                title: "可勾选的成员",
                onclick: move |_| {},
            }
            UnsavedBar {
                visible: dirty(),
                on_save: move |_| dirty.set(false),
                on_revert: move |_| dirty.set(false),
            }
        }
        div { class: "gallery-row",
            div { class: "card",
                ProfileCard {
                    avatar: rsx! { Avatar { text: "张三", size: 64.0, online: true } },
                    name: "张三",
                    badge: rsx! { RoleBadge { "管理员" } },
                    subtitle: rsx! { div { class: "text-sub", "zhang@example.com" } },
                    actions: rsx! { Button { kind: ButtonKind::Primary, "发消息" } },
                    ProfileField { label: "部门", "研发" }
                    ProfileField { label: "本地时间", "12:30" }
                }
            }
        }
    }
}

#[component]
fn DialogSample() -> Element {
    let mut value = use_signal(String::new);
    rsx! {
        div { style: "margin-bottom:8px", "对话框里的内容是普通组件，可以有自己的状态。" }
        TextInput { value: value(), placeholder: "输入点什么", autofocus: true, oninput: move |v| value.set(v) }
    }
}

#[component]
fn PopoverSample(handle: PopoverHandle) -> Element {
    rsx! {
        div { style: "width:240px;padding:12px",
            div { style: "display:flex;gap:8px;align-items:center;margin-bottom:8px",
                Avatar { text: "名片", size: 40.0, src: SAMPLE_AVATAR }
                div {
                    div { class: "text-title", "名片卡片" }
                    div { class: "text-sub", "点击外部或按 Esc 关闭" }
                }
            }
            Button { onclick: move |_| handle.close(), "关闭" }
        }
    }
}

fn sample_menu(overlays: Overlays, checked: Signal<bool>) -> Vec<MenuEntry> {
    vec![
        MenuEntry::item("复制", move || overlays.toast("复制"))
            .icon(IconName::Copy)
            .hint("Ctrl+C"),
        MenuEntry::item("引用", move || overlays.toast("引用")).icon(IconName::Quote),
        MenuEntry::item("转发（禁用）", || {})
            .icon(IconName::Share)
            .disabled(true),
        MenuEntry::Separator,
        MenuEntry::item("显示已读", move || {
            let mut checked = checked;
            checked.toggle();
        })
        .checked(checked()),
        MenuEntry::submenu(
            "设置免打扰",
            vec![
                MenuEntry::item("一小时", move || overlays.toast("一小时")),
                MenuEntry::item("一天", move || overlays.toast("一天")),
                MenuEntry::submenu(
                    "更多时长",
                    vec![MenuEntry::item("一周", move || overlays.toast("一周"))],
                ),
            ],
        )
        .icon(IconName::Notification),
        MenuEntry::Separator,
        MenuEntry::item("删除", move || overlays.toast("删除"))
            .icon(IconName::Delete)
            .danger(),
    ]
}

#[component]
fn MenuSection() -> Element {
    let overlays = use_overlays();
    let checked = use_signal(|| true);
    rsx! {
        h2 { "右键菜单" }
        ContextMenuArea {
            class: "card",
            items: move |_| sample_menu(overlays, checked),
            div { id: "menu-area", style: "padding:24px;text-align:center", "在这里点右键" }
        }
    }
}

#[component]
fn ExpanderSection() -> Element {
    let overlays = use_overlays();
    let mut slide = use_signal(|| false);
    let mut reveal = use_signal(|| true);
    let mut trace = use_signal(|| false);
    let hit = "搜索结果中标出 Zulip 命中的部分，zulip 不区分大小写";
    rsx! {
        h2 { "展开、滑出、其它" }
        Expander { title: "折叠区",
            div { style: "padding:8px 28px", "折叠区的内容，高度过渡，收起后不能获得焦点。" }
        }
        div { class: "gallery-row",
            Button { onclick: move |_| slide.toggle(), "底部滑出栏" }
            Button { onclick: move |_| reveal.toggle(), "圆形揭示" }
            Button { onclick: move |_| {
                trace.set(false);
                spawn(async move {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    trace.set(true);
                });
            }, "描边动画" }
            CircleReveal { open: reveal(),
                div { class: "card", style: "padding:6px 12px", "圆形揭示的内容" }
            }
            div { class: "card", style: "position:relative;padding:6px 12px",
                "贴表情描边"
                TraceBorder { play: trace() }
            }
            FileCard {
                name: "一个文件名很长很长很长很长很长很长很长很长很长很长的文档.pdf",
                icon: IconName::FilePdf,
                detail: "1.2 MB",
                progress: 0.4,
                menu: move |_| vec![MenuEntry::item("打开", move || overlays.toast("打开"))],
            }
        }
        div { style: "margin-bottom:8px", HighlightText { text: hit, query: "zulip" } }
        SlideBar { open: slide(),
            div { class: "card", style: "padding:8px 12px", "多选操作栏" }
        }
        SettingCard {
            SettingRow { label: "设置项", description: "左边文字、右边控件", Toggle { checked: true } }
            SettingRow { label: "另一个设置项", Button { "操作" } }
        }
    }
}

const LIST_COUNT: u64 = 5000;
const HISTORY_BATCH: u64 = 500;

/// 演示用的变高内容：行数由键决定。
fn row_lines(key: u64) -> u64 {
    (key.wrapping_mul(2654435761) >> 7) % 6 + 1
}

#[component]
fn DemoRow(row_key: u64) -> Element {
    let lines = row_lines(row_key);
    rsx! {
        div { class: "gallery-vl-row",
            div { style: "display:flex;gap:8px;align-items:center",
                Avatar { text: "{row_key}", owner: "{row_key}", size: 24.0 }
                span { class: "text-title", "第 {row_key} 条" }
                Badge { count: (row_key % 7) as u32 }
            }
            for line in 0..lines {
                div { key: "{line}", class: "text-sub", "变高内容 {line}" }
            }
        }
    }
}

#[component]
fn ListSection() -> Element {
    rsx! {
        h2 { "虚拟列表（{LIST_COUNT} 条变高）" }
        div { class: "gallery-two",
            MessageListDemo {}
            ConversationListDemo {}
        }
    }
}

/// 消息列表的用法：粘底、向上加载历史。
#[component]
fn MessageListDemo() -> Element {
    let handle = use_virtual_list(VirtualListConfig {
        stick_to_bottom: true,
        start_at_bottom: true,
        ..VirtualListConfig::default()
    });
    let mut first = use_signal(|| LIST_COUNT * 2);
    let mut last = use_signal(|| LIST_COUNT * 3);
    let keys = use_memo(move || Rc::new((first()..last()).collect::<Vec<u64>>()));
    let handle_for_top = handle.clone();
    let handle_for_bottom = handle.clone();
    let handle_for_jump = handle.clone();
    let handle_for_end = handle.clone();
    let at_bottom = handle.at_bottom();
    rsx! {
        div {
            div { class: "gallery-row",
                Button {
                    onclick: move |_| last += 1,
                    "新消息"
                }
                Button {
                    onclick: move |_| first -= HISTORY_BATCH,
                    "加载历史"
                }
                Button {
                    onclick: move |_| handle_for_top.scroll_to_key(first() + 1200, ScrollAlign::Center),
                    "滚动到一条"
                }
                Button {
                    onclick: move |_| handle_for_end.scroll_to_key(first() + 300, ScrollAlign::End),
                    "滚动到另一条（底对齐）"
                }
                Button {
                    onclick: move |_| handle_for_bottom.scroll_to_bottom(),
                    "回到底部"
                }
                span { id: "vl-state", "{first()}..{last()} 底部:{at_bottom}" }
            }
            div { class: "gallery-vl", id: "vl-messages",
                VirtualList {
                    handle: handle_for_jump,
                    keys: keys(),
                    render: move |i: usize| {
                        let key = first() + i as u64;
                        rsx! { DemoRow { row_key: key } }
                    },
                    on_reach_top: move |_| {
                        if first() >= HISTORY_BATCH {
                            first -= HISTORY_BATCH;
                        }
                    },
                }
            }
        }
    }
}

/// 会话列表的用法：普通滚动、右键菜单。
#[component]
fn ConversationListDemo() -> Element {
    let checked = use_signal(|| false);
    let handle = use_virtual_list(VirtualListConfig {
        estimate: 64.0,
        ..VirtualListConfig::default()
    });
    let keys = use_memo(|| Rc::new((0..LIST_COUNT).collect::<Vec<u64>>()));
    let selected = use_signal(|| None::<u64>);
    rsx! {
        div {
            div { class: "gallery-row",
                span { "选中：{selected:?}" }
            }
            div { class: "gallery-vl", id: "vl-conversations",
                VirtualList {
                    handle,
                    keys: keys(),
                    render: move |i: usize| rsx! {
                        ConversationRow { row_key: i as u64, selected, checked }
                    },
                }
            }
        }
    }
}

/// 条目是读取自身状态的组件：键不变时 `VirtualList` 不会重新调用 `render`。
#[component]
fn ConversationRow(row_key: u64, selected: Signal<Option<u64>>, checked: Signal<bool>) -> Element {
    let overlays = use_overlays();
    let background = if selected() == Some(row_key) {
        "var(--selected-row)"
    } else {
        "transparent"
    };
    rsx! {
        ContextMenuArea {
            items: move |_| sample_menu(overlays, checked),
            div {
                style: "display:flex;gap:8px;align-items:center;padding:12px;background:{background}",
                onclick: move |_| selected.set(Some(row_key)),
                Avatar { text: "会话 {row_key}", owner: "{row_key}", size: 40.0 }
                div { style: "min-width:0;flex:1",
                    div { class: "text-title", "会话 {row_key}" }
                    div { class: "text-sub", "最后一条消息的预览" }
                }
            }
        }
    }
}

#[component]
fn SecondWindow() -> Element {
    rsx! {
        WindowShell { title: "单例窗口",
            div { style: "padding:20px",
                "这是一个单例窗口。再次点击预览页的按钮会激活它而不是新开。主题与主窗口同步。"
                div { style: "margin-top:12px", SecondWindowTheme {} }
            }
        }
    }
}

#[component]
fn SecondWindowTheme() -> Element {
    let theme = use_theme();
    let dark = theme.is_dark();
    rsx! {
        Toggle {
            checked: dark,
            onchange: move |on: bool| theme.set_scheme(if on { ColorScheme::Dark } else { ColorScheme::Light }),
            "深色（所有窗口同步）"
        }
    }
}

#[component]
fn WindowSection() -> Element {
    let mut title_hint = use_signal(|| None::<String>);
    rsx! {
        h2 { "窗口" }
        div { class: "gallery-row",
            Button {
                onclick: move |_| {
                    open_singleton(
                        WindowSpec::new("gallery-second", "单例窗口", (420.0, 300.0)),
                        SecondWindow,
                        |dom| dom,
                    );
                },
                "打开单例窗口"
            }
            Button {
                onclick: move |_| shell::move_window(40.0, 0.0),
                "移到屏幕上边缘"
            }
            Button {
                onclick: move |_| title_hint.set(Some("已写入窗口状态".into())),
                "写窗口状态"
            }
            if let Some(hint) = title_hint.read().as_ref() {
                span { "{hint}" }
            }
        }
    }
}

#[component]
fn PlatformSection() -> Element {
    let overlays = use_overlays();
    let window = use_window();
    let mut events = use_signal(Vec::<String>::new);
    let mut edge_enabled = use_signal(|| false);
    let mut idle = use_signal(|| Duration::ZERO);
    let mut clip = use_signal(String::new);
    let mut startup = use_signal(|| LaunchAtStartup::current().is_ok_and(|s| s.is_enabled()));
    let mut hidden_position = use_signal(String::new);

    let tray = platform::use_tray(
        "YaZ 组件预览",
        &[
            TrayItem::Action("open", "打开主面板"),
            TrayItem::Separator,
            TrayItem::Action("exit", "退出"),
        ],
        move |event| {
            let text = match &event {
                TrayEvent::LeftClick => "左键单击".to_string(),
                TrayEvent::Hover(p) => format!("闪烁时悬停 {},{}", p.x, p.y),
                TrayEvent::FlashingStopped => "闪烁停止".to_string(),
                TrayEvent::Menu(id) => format!("菜单 {id}"),
            };
            events.write().push(text);
        },
    );
    let edge = platform::use_edge_hide(
        edge_enabled.into(),
        Callback::new(move |_| overlays.any_open()),
    );
    use_future(move || async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            idle.set(platform::idle_time());
            hidden_position.set(format!("{:?}", edge.resting_position()));
        }
    });
    let flash_window = window.clone();
    let foreground_window = window.clone();
    let tray_for_start = tray.clone();
    let tray_for_stop = tray.clone();
    let tray_for_tip = tray.clone();
    let tray_for_visible = tray.clone();
    let mut tray_visible = use_signal(|| true);
    rsx! {
        h2 { "平台" }
        div { class: "gallery-row",
            Button { onclick: move |_| {
                if let Some(t) = &tray_for_start { t.start_flashing(); }
            }, "托盘闪烁" }
            Button { onclick: move |_| {
                if let Some(t) = &tray_for_stop { t.stop_flashing(); }
            }, "停止闪烁" }
            Button { onclick: move |_| {
                if let Some(t) = &tray_for_tip { t.set_tooltip("新的提示文字"); }
            }, "改托盘提示" }
            Button { onclick: move |_| {
                tray_visible.toggle();
                if let Some(t) = &tray_for_visible { t.set_visible(tray_visible()); }
            }, "显示 / 隐藏托盘" }
            span { id: "tray-events", "托盘：{tray.is_some()} {events.read().join(\" / \")}" }
        }
        div { class: "gallery-row",
            Button { onclick: move |_| {
                let window = flash_window.clone();
                spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    platform::flash_taskbar(&window);
                });
            }, "2 秒后闪烁任务栏" }
            Button { onclick: move |_| overlays.toast(format!("前台：{}", platform::is_foreground(&foreground_window))), "是否前台" }
            Button { onclick: move |_| platform::play_notification_sound(), "提示音" }
            span { id: "idle", "空闲 {idle().as_secs()} 秒" }
        }
        div { class: "gallery-row",
            Toggle {
                checked: startup(),
                onchange: move |on: bool| {
                    match LaunchAtStartup::current().and_then(|s| s.set(on).map(|()| s)) {
                        Ok(s) => startup.set(s.is_enabled()),
                        Err(e) => overlays.toast_kind(ToastKind::Error, e.to_string()),
                    }
                },
                "开机自动运行"
            }
            Toggle {
                checked: edge_enabled(),
                onchange: move |on| edge_enabled.set(on),
                "贴边隐藏"
            }
            span { id: "edge-state", "正常位置：{hidden_position}" }
            Button { onclick: move |_| edge.reveal(), "移回" }
        }
        div { class: "gallery-row",
            Button { onclick: move |_| {
                match platform::copy_text("来自 Zulip 的文字") {
                    Ok(()) => overlays.toast("已复制文字"),
                    Err(e) => overlays.toast_kind(ToastKind::Error, e.to_string()),
                }
            }, "复制文字" }
            Button { onclick: move |_| clip.set(platform::paste_text().unwrap_or_default()), "读取文字" }
            Button { onclick: move |_| {
                let image = ClipboardImage { width: 2, height: 2, rgba: [255, 0, 0, 255].repeat(4) };
                if let Err(e) = platform::copy_image(&image) {
                    overlays.toast_kind(ToastKind::Error, e.to_string());
                }
            }, "复制图片" }
            Button { onclick: move |_| {
                let text = platform::paste_image().map_or("无图片".to_string(), |i| {
                    format!("图片 {}x{}，PNG {} 字节", i.width, i.height, i.to_png().map_or(0, |p| p.len()))
                });
                clip.set(text);
            }, "读取图片" }
            span { id: "clip", "{clip}" }
        }
        div { class: "gallery-row",
            Button { onclick: move |_| async move {
                let path = platform::pick_file("选择文件", &[]).await;
                overlays.toast(format!("{path:?}"));
            }, "选择文件" }
            Button { onclick: move |_| async move {
                let paths = platform::pick_files("选择多个文件", &[]).await;
                overlays.toast(format!("{} 个文件", paths.len()));
            }, "选择多个" }
            Button { onclick: move |_| async move {
                let path = platform::pick_folder("选择文件夹").await;
                overlays.toast(format!("{path:?}"));
            }, "选择文件夹" }
            Button { onclick: move |_| async move {
                let saved = overlays
                    .save_as("文件.txt", |path| async move { tokio::fs::write(path, "内容").await })
                    .await;
                if let Some(path) = saved {
                    overlays.open_file(&path);
                }
            }, "另存为后打开" }
        }
    }
}
