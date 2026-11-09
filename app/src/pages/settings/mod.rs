//! Settings window: app-level pages (General, Data directory, Palette, Plugins, ...) and "Zulip
//! accounts" (account list, per-account settings pages).
//!
//! All settings are bound with [`field`](field::field) and pages only compose row components; pages
//! with manual saving use `UnsavedBar`, and leaving goes through a unified confirmation via
//! [`Nav`].

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

/// Events arriving in batches are coalesced before re-reading.
const COALESCE: Duration = Duration::from_millis(30);

/// App-level pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    General,
    /// Data directory.
    Files,
    /// Palette.
    Palette,
    Accounts,
    Plugins,
    /// Developer mode (debug builds only).
    #[cfg(debug_assertions)]
    Developer,
    About,
}

/// Pages of the account selected under "Zulip accounts".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountPage {
    #[default]
    Server,
    /// Me.
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

/// Where to navigate when the settings window opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTarget {
    #[default]
    General,
    /// Palette.
    Palette,
    /// Zulip accounts -> this account -> "Me".
    AccountProfile(i64),
    #[allow(dead_code)]
    Page(Page),
    /// Zulip accounts -> this account -> the given page.
    #[allow(dead_code)]
    Account(i64, AccountPage),
}

/// The location currently shown in the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Place {
    page: Page,
    /// The account selected under "Zulip accounts"; when none, the account shown in the main
    /// window.
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

/// Navigation: unsaved changes are confirmed in one place before switching pages or accounts.
#[derive(Clone, Copy)]
struct Nav {
    place: Signal<Place>,
    /// The current page has unsaved changes; set by pages with manual saving.
    dirty: Signal<bool>,
}

impl Nav {
    /// Whether the current page can be left: no unsaved changes, or the user chose to discard them.
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

/// Whether the current page has unsaved changes (used by pages with manual saving).
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

/// Context read by settings pages: the selected account's session (absent for app-level pages) and
/// the version number that triggers re-reads.
#[derive(Clone)]
struct Store {
    pub session: Option<Arc<Session>>,
    /// Account ID; an offline account has no session.
    pub account: Option<i64>,
    pub rev: Signal<u64>,
}

impl Store {
    /// Used by account pages, which always have a session.
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

/// Provides the [`Store`]: local setting changes and events of this account both change its version
/// number.
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

/// The window shell is outside the component; close requests are passed to the inner component
/// through this counter.
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

/// The page's scroll area: the secondary navigation scrolls and locates sections based on it.
#[component]
fn Scrolled(children: Element) -> Element {
    rsx! {
        div { id: section_nav::SCROLL_ID, class: "settings-scroll scroll", {children} }
    }
}
