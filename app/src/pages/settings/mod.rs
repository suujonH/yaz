//! 设置窗口：程序级的页（通用、数据目录、调色盘、插件……）和“Zulip 账号”（账号列表、账号的设置页）。
//!
//! 所有设置项用 [`field`](field::field) 绑定，页面只组合行组件；有手动保存的页用 `UnsavedBar`，
//! 离开时经 [`Nav`] 统一确认。

mod about;
mod account_pages;
mod accounts;
mod avatar;
mod channels;
#[cfg(debug_assertions)]
pub mod developer;
pub(crate) mod field;
mod files;
mod general;
mod levels;
mod me;
mod organization;
mod palette;
mod plugin_form;
pub mod plugin_log;
mod plugins;
mod preferences;
mod push;
mod resources;
mod section_nav;
mod server;

use crate::shell::{WindowShell, use_window_args};
use crate::state::{AppCommand, use_app_command};
use crate::ui::*;
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use zulip_session::{Session, SessionEvent};

/// 事件成批到达时合并后再重读。
const COALESCE: Duration = Duration::from_millis(30);

/// 程序级的页。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    General,
    /// 数据目录。
    Files,
    /// 超级调色盘。
    Palette,
    Accounts,
    Plugins,
    /// 开发者模式（只有调试版有）。
    #[cfg(debug_assertions)]
    Developer,
    About,
}

/// “Zulip 账号”中选中账号的页。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountPage {
    #[default]
    Server,
    /// 我。
    Me,
    Preferences,
    Notifications,
    EnhancedPush,
    GroupMessages,
    Privacy,
    AlertWords,
    Resources,
    Organization,
}

/// 打开设置时定位到哪里。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTarget {
    #[default]
    General,
    /// 超级调色盘。
    Palette,
    /// Zulip 账号 → 这个账号 → “我”。
    AccountProfile(i64),
    #[allow(dead_code)]
    Page(Page),
    /// Zulip 账号 → 这个账号 → 指定页。
    #[allow(dead_code)]
    Account(i64, AccountPage),
}

/// 窗口当前显示的位置。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Place {
    page: Page,
    /// “Zulip 账号”中选中的账号；没有时是主窗口显示的账号。
    account: Option<i64>,
    account_page: AccountPage,
}

impl SettingsTarget {
    fn place(self, current: Place) -> Place {
        match self {
            Self::General => Place {
                page: Page::General,
                ..current
            },
            Self::Palette => Place {
                page: Page::Palette,
                ..current
            },
            Self::AccountProfile(id) => Self::Account(id, AccountPage::Me).place(current),
            Self::Page(page) => Place { page, ..current },
            Self::Account(id, account_page) => Place {
                page: Page::Accounts,
                account: Some(id),
                account_page,
            },
        }
    }
}

/// 导航：切换页、账号前统一确认未保存的修改。
#[derive(Clone, Copy)]
struct Nav {
    place: Signal<Place>,
    /// 当前页有未保存的修改，由手动保存的页设置。
    dirty: Signal<bool>,
}

impl Nav {
    /// 可以离开当前页：没有未保存的修改，或用户选择放弃。
    async fn leave(self, overlays: Overlays) -> bool {
        if !*self.dirty.peek() {
            return true;
        }
        let ok = overlays.confirm_discard().await;
        if ok {
            let mut dirty = self.dirty;
            dirty.set(false);
        }
        ok
    }

    async fn go(self, overlays: Overlays, to: Place) {
        if *self.place.peek() != to && self.leave(overlays).await {
            let mut place = self.place;
            place.set(to);
        }
    }
}

/// 当前页是否有未保存的修改（手动保存的页用）。
fn use_dirty(dirty: bool) {
    let nav = use_context::<Nav>();
    use_effect(use_reactive!(|dirty| {
        let mut flag = nav.dirty;
        flag.set(dirty);
    }));
    use_drop(move || {
        let mut flag = nav.dirty;
        flag.set(false);
    });
}

/// 设置页读取的上下文：选中的账号会话（程序级的页没有），以及需要重新读取的版本号。
#[derive(Clone)]
struct Store {
    pub session: Option<Arc<Session>>,
    /// 账号 ID；不在线的账号没有会话。
    pub account: Option<i64>,
    pub rev: Signal<u64>,
}

impl Store {
    /// 账号的页使用，一定有会话。
    pub fn session(&self) -> Arc<Session> {
        self.session.clone().expect("账号的设置页有会话")
    }
}

fn relevant(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Connection(_)
            | SessionEvent::Resynced
            | SessionEvent::UserChanged(_)
            | SessionEvent::UsersChanged
            | SessionEvent::MutedUsersChanged
            | SessionEvent::ChannelChanged(_)
            | SessionEvent::ChannelsChanged
            | SessionEvent::TopicsChanged(_)
            | SessionEvent::UserTopicChanged { .. }
            | SessionEvent::RealmChanged
            | SessionEvent::PermissionsChanged
            | SessionEvent::SettingsChanged(_)
            | SessionEvent::AlertWordsChanged
            | SessionEvent::ConversationChanged(_)
            | SessionEvent::ConversationsChanged
            | SessionEvent::AttachmentsChanged
    )
}

/// 提供 [`Store`]：本机设置变化和这个账号的相关事件都会让它的版本号变化。
fn use_store(session: Option<Arc<Session>>, account: Option<i64>) -> Store {
    let mut rev = use_signal(|| 0u64);
    use_app_command(move |command| {
        if command == AppCommand::SettingsChanged {
            rev += 1;
        }
    });
    let events = session.clone();
    use_future(move || {
        let session = events.clone();
        async move {
            let Some(session) = session else { return };
            let mut rx = session.subscribe();
            loop {
                match rx.recv().await {
                    Ok(event) if relevant(&event) => {}
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
                tokio::time::sleep(COALESCE).await;
                while !matches!(
                    rx.try_recv(),
                    Err(TryRecvError::Empty | TryRecvError::Closed)
                ) {}
                rev += 1;
            }
        }
    });
    use_context_provider(|| Store {
        session,
        account,
        rev,
    })
}

const PAGES: &[(Page, &str)] = &[
    (Page::General, "通用"),
    (Page::Files, "数据目录"),
    (Page::Palette, "超级调色盘"),
    (Page::Accounts, "Zulip 账号"),
    (Page::Plugins, "插件"),
    #[cfg(debug_assertions)]
    (Page::Developer, "开发者模式"),
    (Page::About, "关于"),
];

/// 窗口外壳在组件外，关闭请求经这个计数传给里面的组件处理。
#[derive(Clone, Copy)]
struct CloseRequest(Signal<u64>);

#[component]
pub fn SettingsWindow() -> Element {
    let mut request = use_signal(|| 0u64);
    use_context_provider(|| CloseRequest(request));
    rsx! {
        WindowShell {
            title: "设置",
            caption_left: rsx! { Icon { name: IconName::Setting, size: 14.0 } },
            on_close: move |_| request += 1,
            SettingsRoot {}
        }
    }
}

#[component]
fn SettingsRoot() -> Element {
    let target = use_window_args::<SettingsTarget>();
    let overlays = use_overlays();
    let window = use_window();
    let close = use_context::<CloseRequest>().0;
    let nav = use_context_provider(|| Nav {
        place: Signal::new(target.peek().place(Place::default())),
        dirty: Signal::new(false),
    });
    use_store(None, None);
    #[cfg(debug_assertions)]
    use_hook(crate::request_log::install_request_log);
    use_effect(move || {
        let to = target().place(*nav.place.peek());
        spawn(nav.go(overlays, to));
    });
    use_effect(move || {
        if close() > 0 {
            let window = window.clone();
            spawn(async move {
                if nav.leave(overlays).await {
                    window.close();
                }
            });
        }
    });
    let place = (nav.place)();
    rsx! {
        div { class: "settings",
            div { class: "settings-nav scroll",
                for &(page, title) in PAGES {
                    div { key: "{title}",
                        ListRow {
                            density: RowDensity::Compact,
                            selected: page == place.page,
                            title,
                            onclick: move |_| {
                                spawn(nav.go(overlays, Place { page, ..place }));
                            },
                        }
                        if page == place.page && page != Page::Accounts {
                            section_nav::SectionNav { key: "{title}" }
                        }
                    }
                }
            }
            div { class: "settings-main",
                match place.page {
                    Page::General => rsx! { Scrolled { general::General {} } },
                    Page::Files => rsx! { Scrolled { files::Files {} } },
                    Page::Palette => rsx! { Scrolled { palette::Palette {} } },
                    Page::Accounts => rsx! { accounts::AccountsPanel { place } },
                    Page::Plugins => rsx! { Scrolled { plugins::Plugins {} } },
                    #[cfg(debug_assertions)]
                    Page::Developer => rsx! { Scrolled { developer::Developer {} } },
                    Page::About => rsx! { Scrolled { about::About {} } },
                }
            }
        }
    }
}

/// 页面的滚动区域：二级导航据此滚动和定位。
#[component]
fn Scrolled(children: Element) -> Element {
    rsx! {
        div { id: section_nav::SCROLL_ID, class: "settings-scroll scroll", {children} }
    }
}
