//! General: how chats are opened, topic title, main UI, client identifier, font size.

use super::field::*;
use crate::platform::LaunchAtStartup;
use dioxus::prelude::*;
use zulip_api::{CLIENT_NAME, DESKTOP_CLIENT_NAME, WEB_CLIENT_NAME, user_agent_for};
use zulip_core::text::topic_title;
use zulip_store::{CloseAction, MAX_FONT_SIZE, MIN_FONT_SIZE};

/// Variables of the topic title template.
const CHANNEL_TOKEN: &str = "{Channel}";
const TOPIC_TOKEN: &str = topic_title::DEFAULT;

const CLIENT_NAMES: [(&str, &str); 3] = [
    (CLIENT_NAME, "YaZ"),
    (DESKTOP_CLIENT_NAME, "桌面版"),
    (WEB_CLIENT_NAME, "网页版"),
];

/// Two options, in display order.
fn pair(first: (bool, &str), second: (bool, &str)) -> Vec<(bool, String)> {
    vec![
        (first.0, first.1.to_owned()),
        (second.0, second.1.to_owned()),
    ]
}

#[component]
pub fn General() -> Element {
    let classic = field(Source::App(
        |s| s.classic_mode(),
        |s, v| s.set_classic_mode(v),
    ));
    let tray = field(Source::App(
        |s| s.show_tray_icon(),
        |s, v| s.set_show_tray_icon(v),
    ));
    let open_in_window = field(Source::App(
        |s| s.open_chat_in_new_window(),
        |s, v| s.set_open_chat_in_new_window(v),
    ));
    let title_format = field(Source::App(
        |s| s.topic_title_format(),
        |s, v| s.set_topic_title_format(&v),
    ));
    let preview = topic_title::format(Some(&title_format.value), "频道名", "话题名");
    let collapse = field(Source::App(
        |s| s.collapse_resolved_topics(),
        |s, v| s.set_collapse_resolved_topics(v),
    ));
    let startup = field(Source::Custom(
        Scope::App,
        |_| LaunchAtStartup::current().is_ok_and(|l| l.is_enabled()),
        |_, enabled| {
            LaunchAtStartup::current()
                .and_then(|l| l.set(enabled))
                .map_err(|e| e.to_string())
        },
    ));
    let edge = field(Source::App(
        |s| s.edge_auto_hide(),
        |s, v| s.set_edge_auto_hide(v),
    ));
    let to_tray = field(Source::App(
        |s| s.effective_close_action() == CloseAction::MinimizeToTray,
        |s, v| {
            s.set_close_action(if v {
                CloseAction::MinimizeToTray
            } else {
                CloseAction::Exit
            });
        },
    ));
    let font = field(Source::Theme(
        |c| c.font_size as i32,
        |t, v| t.set_font_size(v as u32),
    ));
    rsx! {
        SettingsPage { title: "通用",
            Group { title: "聊天",
                ChoiceRow {
                    label: "打开新聊天",
                    bind: open_in_window,
                    options: pair((false, "在主窗口中"), (true, "在独立窗口中")),
                    disabled: classic.value,
                }
                TextRow {
                    label: "话题标题",
                    bind: title_format,
                    // Empty is allowed: topic titles are then empty
                    placeholder: "",
                    description: format!("预览：{preview}"),
                    tokens: vec![
                        ("频道名".to_owned(), CHANNEL_TOKEN.to_owned()),
                        ("话题名".to_owned(), TOPIC_TOKEN.to_owned()),
                    ],
                }
                ToggleRow { label: "收起已解决话题", bind: collapse }
            }
            Group { title: "主界面",
                ToggleRow { label: "开机自动运行", bind: startup }
                ToggleRow { label: "经典模式", bind: classic.clone() }
                ToggleRow { label: "停靠在屏幕边缘时自动隐藏", bind: edge }
                ToggleRow { label: "显示托盘图标", bind: tray.clone() }
                ChoiceRow {
                    label: "关闭主面板时",
                    bind: to_tray,
                    options: pair((true, "最小化到托盘"), (false, "退出程序")),
                    disabled: !tray.value,
                }
            }
            Group { title: "连接", ClientNameRows {} }
            Group { title: "字体大小",
                SliderRow {
                    label: "正文字号",
                    bind: font,
                    min: MIN_FONT_SIZE,
                    max: MAX_FONT_SIZE,
                    unit: "",
                }
            }
        }
    }
}

/// Client name reported to the server: preset or custom; the User-Agent actually sent is shown
/// below.
#[component]
fn ClientNameRows() -> Element {
    let bind = field(Source::App(
        |s| s.client_name(),
        |s, v| s.set_client_name(&v),
    ));
    let name = bind.value.trim().to_owned();
    let effective = if name.is_empty() { CLIENT_NAME } else { &name };
    let preset = CLIENT_NAMES.iter().find(|(n, _)| *n == effective);
    let custom = use_signal(|| preset.is_none());
    let is_custom = custom() || preset.is_none();
    let user_agent = format!("User-Agent：{}", user_agent_for(&name));
    let choice = bind.clone().map(
        |_| {
            if is_custom {
                String::new()
            } else {
                effective.to_owned()
            }
        },
        move |key: String| {
            let mut custom = custom;
            custom.set(key.is_empty());
            (!key.is_empty()).then_some(key)
        },
    );
    let mut options: Vec<(String, String)> = CLIENT_NAMES
        .iter()
        .map(|(n, text)| ((*n).to_owned(), (*text).to_owned()))
        .collect();
    options.push((String::new(), "自定义".to_owned()));
    let custom_name = bind.map(
        |v| v.clone(),
        |v: String| (!v.trim().is_empty()).then_some(v),
    );
    rsx! {
        ChoiceRow { label: "客户端标识", bind: choice, options, description: user_agent }
        if is_custom {
            TextRow { label: "自定义名称", bind: custom_name, placeholder: "客户端名称" }
        }
    }
}
