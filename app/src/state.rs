//! Global state inside a window: current account, session, and hooks that turn Hub / Session events
//! into signals. Pages read state from here and do not subscribe to the Hub directly.

use crate::services::{plugins, try_services};
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use zulip_core::ConversationKey;
use zulip_plugin_host::PluginEvent;
use zulip_session::{AccountEntry, HubEvent, Session, SessionEvent};
use zulip_store::LocalSettings;

const WINDOW_EVENT_CAPACITY: usize = 256;
/// Events arriving in batches are coalesced before re-reading.
const COALESCE: Duration = Duration::from_millis(30);

/// Locate a message after opening a conversation. Every request compares unequal, so locating the
/// same message again also runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusRequest {
    pub id: i64,
    request: u64,
}

impl FocusRequest {
    pub fn new(id: i64) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self {
            id,
            request: NEXT.fetch_add(1, Ordering::Relaxed),
        }
    }
}

/// App-level commands: can be sent from any thread and any window, and are received with
/// [`use_app_command`].
#[derive(Clone, Debug, PartialEq)]
pub enum AppCommand {
    /// Shows and activates the main window.
    ShowMain,
    /// Opens a conversation on the right side of the main window (sent after
    /// [`crate::windows::open_chat`] decides).
    ShowChat {
        account: i64,
        key: ConversationKey,
        focus: Option<FocusRequest>,
    },
    /// The main window switches to the channel on the contacts page and expands it.
    ShowChannel {
        account: i64,
        stream_id: i64,
    },
    /// Shows system notices on the right side of the main window.
    ShowNotices {
        account: i64,
    },
    /// Shows message reminders on the right side of the main window.
    ShowReminders {
        account: i64,
    },
    /// Logs out of the current account (goes through the main window's confirmation flow).
    Logout,
    Exit,
    /// A new version is ready: quit and launch the updater.
    ExitForUpdate,
    /// Sent after the settings page changes the data directory.
    #[allow(dead_code)]
    Restart,
    /// Sent after a write by [`crate::services::write_setting`] or
    /// [`crate::services::write_account_setting`].
    SettingsChanged,
    /// The update service's state changed; read `status()` of `boot().update`.
    UpdateStatusChanged,
    /// The tray stops flashing; collapse the tray message list.
    TrayFlashingStopped,
    /// The spare window was shown and became an ordinary window: create a new one.
    RefillSpareWindow,
    /// The login window is done (logged in, or "log in later"): the hidden root window enters the
    /// main UI, shows itself and closes the login window.
    EnterMain,
}

static COMMANDS: LazyLock<broadcast::Sender<AppCommand>> =
    LazyLock::new(|| broadcast::channel(64).0);

pub fn send_command(command: AppCommand) {
    // With no subscribers the command has no receiver, which is normal
    let _ = COMMANDS.send(command);
}

/// Receives app-level commands.
pub fn use_app_command(handler: impl FnMut(AppCommand) + 'static) {
    let handler = use_callback(handler);
    use_future(move || {
        let mut rx = COMMANDS.subscribe();
        async move {
            loop {
                match rx.recv().await {
                    Ok(command) => handler.call(command),
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
            }
        }
    });
}

/// A window bound to an account (placed in the window's root context). When absent the window
/// follows the current account. The window closes automatically when the bound account ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountScope(pub i64);

/// Global state inside the window, provided by [`WindowShell`](crate::shell::WindowShell).
#[derive(Clone, Copy)]
pub struct AppState {
    /// The account this window corresponds to.
    pub account: Signal<Option<i64>>,
    pub session: Signal<Option<Arc<Session>>>,
    /// All accounts and their states.
    pub accounts: Signal<Rc<Vec<AccountEntry>>>,
    events: CopyValue<broadcast::Sender<SessionEvent>>,
}

/// The [`AppState`] of the current window.
pub fn use_app_state() -> AppState {
    use_context::<AppState>()
}

/// This window's current session (reactive).
pub fn use_session() -> Option<Arc<Session>> {
    use_app_state().session.read().clone()
}

fn read_accounts() -> Rc<Vec<AccountEntry>> {
    let accounts = try_services().and_then(|s| {
        s.hub
            .accounts()
            .inspect_err(|e| log::warn!("读取账号列表失败：{e}"))
            .ok()
    });
    Rc::new(accounts.unwrap_or_default())
}

fn current_session(scope: Option<AccountScope>) -> (Option<i64>, Option<Arc<Session>>) {
    let Some(services) = try_services() else {
        return (None, None);
    };
    let account = scope.map_or_else(|| services.hub.active_account(), |s| Some(s.0));
    (account, account.and_then(|id| services.hub.session(id)))
}

/// Called by [`WindowShell`](crate::shell::WindowShell): provides [`AppState`] and forwards this
/// window account's events to the page. From the page's view, switching accounts is a single
/// [`SessionEvent::Resynced`].
pub fn use_provide_app_state() {
    let window = use_window();
    let scope = use_hook(try_consume_context::<AccountScope>);
    let state = use_context_provider(|| {
        let (account, session) = current_session(scope);
        AppState {
            account: Signal::new(account),
            session: Signal::new(session),
            accounts: Signal::new(read_accounts()),
            events: CopyValue::new(broadcast::channel(WINDOW_EVENT_CAPACITY).0),
        }
    });
    use_future(move || {
        let mut state = state;
        let window = window.clone();
        async move {
            let Some(services) = try_services() else {
                return;
            };
            let mut rx = services.hub.subscribe();
            let forward = |event: SessionEvent| {
                let _ = state.events.peek().send(event);
            };
            let mut refresh = move || {
                state.accounts.set(read_accounts());
                let (account, session) = current_session(scope);
                let changed = match (&*state.session.peek(), &session) {
                    (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                    (None, None) => false,
                    _ => true,
                };
                state.account.set(account);
                if changed {
                    state.session.set(session.clone());
                }
                if scope.is_some() && session.is_none() {
                    window.close();
                }
                changed
            };
            // Changes may happen between window creation and the start of the subscription
            if refresh() {
                forward(SessionEvent::Resynced);
            }
            loop {
                match rx.recv().await {
                    Ok(HubEvent::AccountsChanged | HubEvent::ActiveChanged(_)) => {
                        if refresh() {
                            forward(SessionEvent::Resynced);
                        }
                    }
                    Ok(HubEvent::Session(id, event)) => {
                        if *state.account.peek() == Some(id) {
                            forward(event);
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        refresh();
                        forward(SessionEvent::Resynced);
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        }
    });
}

/// Receives the session events of this window's account. Both lagging forwarding and switching
/// accounts deliver [`SessionEvent::Resynced`].
pub fn use_session_event(handler: impl FnMut(SessionEvent) + 'static) {
    let state = use_app_state();
    let handler = use_callback(handler);
    use_future(move || {
        let mut rx = state.events.peek().subscribe();
        async move {
            loop {
                match rx.recv().await {
                    Ok(event) => handler.call(event),
                    Err(RecvError::Lagged(_)) => handler.call(SessionEvent::Resynced),
                    Err(RecvError::Closed) => return,
                }
            }
        }
    });
}

/// Data read from the session and re-read when events arrive: triggered by events for which
/// `filter` is true, as well as `Resynced` and account switches; batched events are coalesced into
/// one; no render is triggered when the result is unchanged. Without a session it is
/// `T::default()`.
pub fn use_session_data<T: Default + PartialEq + 'static>(
    filter: fn(&SessionEvent) -> bool,
    read: impl Fn(&Arc<Session>) -> T + 'static,
) -> Signal<T> {
    let state = use_app_state();
    let read = use_hook(|| Rc::new(read));
    let mut value = use_signal({
        let read = read.clone();
        move || {
            state
                .session
                .peek()
                .as_ref()
                .map_or_else(T::default, |s| read(s))
        }
    });
    use_future(move || {
        let read = read.clone();
        let mut rx = state.events.peek().subscribe();
        async move {
            loop {
                match rx.recv().await {
                    Ok(event) if event == SessionEvent::Resynced || filter(&event) => {}
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
                tokio::time::sleep(COALESCE).await;
                while !matches!(
                    rx.try_recv(),
                    Err(broadcast::error::TryRecvError::Empty
                        | broadcast::error::TryRecvError::Closed)
                ) {}
                let next = state
                    .session
                    .peek()
                    .as_ref()
                    .map_or_else(T::default, |s| read(s));
                if *value.peek() != next {
                    value.set(next);
                }
            }
        }
    });
    value
}

/// Value of a local setting, updated after other windows change it through
/// [`write_setting`](crate::services::write_setting).
pub fn use_local_setting<T: PartialEq + 'static>(read: fn(&LocalSettings) -> T) -> Signal<T> {
    let current = move || read(&crate::services::services().settings);
    let mut value = use_signal(current);
    use_app_command(move |command| {
        if command == AppCommand::SettingsChanged {
            let next = current();
            if *value.peek() != next {
                value.set(next);
            }
        }
    });
    value
}

/// Receives events from the plugin host (possibly from any thread, relayed to the UI through a
/// coroutine). A lagging receiver gets `PluginsChanged` and then re-reads everything.
pub fn use_plugin_events(handler: impl FnMut(PluginEvent) + 'static) {
    use_plugin_events_resync(handler, || {});
}

/// Same as [`use_plugin_events`], and additionally calls `resync` after the subscription starts and
/// every time it lags: used to make up for events missed before subscribing and while lagging.
pub fn use_plugin_events_resync(
    handler: impl FnMut(PluginEvent) + 'static,
    mut resync: impl FnMut() + 'static,
) {
    let handler = use_callback(handler);
    let resync = use_callback(move |()| resync());
    use_coroutine(move |_: UnboundedReceiver<()>| async move {
        let mut events = plugins().subscribe();
        resync.call(());
        loop {
            match events.recv().await {
                Ok(event) => handler.call(event),
                Err(RecvError::Lagged(_)) => {
                    handler.call(PluginEvent::PluginsChanged);
                    resync.call(());
                }
                Err(RecvError::Closed) => return,
            }
        }
    });
}

/// Plugin logs changed: the argument is the plugin id; `None` when lagging, meaning any plugin's
/// logs may have changed.
pub fn use_plugin_logs(handler: impl FnMut(Option<String>) + 'static) {
    let handler = use_callback(handler);
    use_coroutine(move |_: UnboundedReceiver<()>| async move {
        let mut logs = plugins().subscribe_logs();
        loop {
            match logs.recv().await {
                Ok(plugin_id) => handler.call(Some(plugin_id)),
                Err(RecvError::Lagged(_)) => handler.call(None),
                Err(RecvError::Closed) => return,
            }
        }
    });
}
