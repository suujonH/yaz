//! Root window: data directory prompt -> data directory migration -> login -> main UI are four
//! stages of the same window; tray, notifications and the quit flow are all here. Login is the
//! exception: it has its own window ([`windows::open_login`]) while the root window stays hidden, so
//! the root window never changes from the login size to the main size in front of the user.

use crate::pages::composer::{filled_draft, is_composer_open};
use crate::pages::data_move::{DataMovePage, DataProblem, DataProblemPage};
use crate::pages::main_window::MainWindowPage;
use crate::pages::notification::NotificationClick;
use crate::platform::{self, Tray, TrayEvent, TrayItem, use_tray};
use crate::services::{
    MAIN_BOUNDS_KEY, SettingsBounds, boot, enable_main_bounds, init_or_report, load_theme,
    scheme_of, services, try_services,
};
use crate::shell::{
    ThemeHub, WindowBoundsStore, WindowSpec, activate, close_all_windows, create_spare,
};
use crate::startup::WAIT_PID_ARG;
use crate::state::{AppCommand, send_command, use_app_command, use_plugin_events_resync};
use crate::ui::{Overlays, use_overlays};
use crate::widgets::{ask_plugin_dialog, open_notification_target, plugin_text};
use crate::windows;
use dioxus::desktop::tao::dpi::{LogicalSize, PhysicalPosition};
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::{DesktopService, use_window, use_wry_event_handler};
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::oneshot;
use zulip_core::{ConversationKey, ListEntry};
use zulip_plugin_host::{PendingDialog, PluginEvent};
use zulip_session::{HubEvent, NotificationAvatar, NotificationRequest, SessionEvent};
use zulip_store::{AccountRepo, CloseAction};
use zulip_update::PROBE_ARG;

const MOVING_SIZE: (f64, f64) = (440.0, 180.0);
const PROBLEM_SIZE: (f64, f64) = (480.0, 240.0);
const MAIN_SIZE: (f64, f64) = (1090.0, 694.0);
const MAIN_MIN_SIZE: (f64, f64) = (800.0, 500.0);
const CLASSIC_SIZE: (f64, f64) = (374.0, 694.0);
const CLASSIC_MIN_SIZE: (f64, f64) = (300.0, 500.0);
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(5);
/// Interval for checking user input: input within this period is reported to the session.
const INPUT_POLL: Duration = Duration::from_secs(5);
/// The spare window is created this long after startup, out of the way of loading the main window.
const SPARE_AFTER_START: Duration = Duration::from_millis(1500);
/// A new spare window is created this long after the last one was shown, once it has painted.
const SPARE_AFTER_SHOW: Duration = Duration::from_millis(300);

/// Stages of the root window.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// The data directory has a problem and is waiting for the user's choice. The database is not
    /// open yet.
    DataProblem(DataProblem),
    /// The data directory is being migrated to this location.
    Moving(PathBuf),
    /// The login window is open; the root window is hidden.
    Login,
    /// When `restore` is true, logs in to the accounts set to log in at startup.
    Main { restore: bool },
}

/// Goes straight to the main UI when there are accounts set to log in at startup, otherwise shows
/// login.
fn stage_after_boot() -> Stage {
    let startup = AccountRepo::new(&services().db)
        .get_startup_accounts()
        .unwrap_or_default();
    if startup.is_empty() {
        Stage::Login
    } else {
        enable_main_bounds();
        Stage::Main { restore: true }
    }
}

pub fn initial_stage(moving: Option<PathBuf>) -> Stage {
    moving.map_or_else(stage_after_boot, Stage::Moving)
}

fn main_sizes(classic: bool) -> ((f64, f64), (f64, f64)) {
    if classic {
        (CLASSIC_SIZE, CLASSIC_MIN_SIZE)
    } else {
        (MAIN_SIZE, MAIN_MIN_SIZE)
    }
}

/// Creation parameters of the root window.
pub fn root_window_spec(stage: &Stage) -> WindowSpec {
    let spec = |size, min_size, resizable| {
        let mut spec = WindowSpec::new(MAIN_BOUNDS_KEY, "YaZ", size)
            .min_size(min_size)
            .hide_on_close();
        spec.resizable = resizable;
        // The main window restores its own position (see `apply_main_bounds`) or is centered by
        // `fixed_size`
        spec.centered = false;
        spec.restore_position = true;
        // The main window decides by itself whether to show at startup (it may be minimized to the
        // tray)
        spec.show_when_ready = false;
        spec
    };
    let main = || {
        let (size, min) = main_sizes(services().settings.classic_mode());
        spec(size, min, true)
    };
    match stage {
        Stage::DataProblem(_) => spec(PROBLEM_SIZE, PROBLEM_SIZE, false),
        Stage::Moving(_) => spec(MOVING_SIZE, MOVING_SIZE, false),
        // Created hidden as the main window; shown when login is done (`enter_main_after_login`)
        Stage::Login => {
            let mut spec = main();
            spec.show_when_ready = true;
            spec
        }
        Stage::Main { .. } => main(),
    }
}

/// Centers the window on the screen.
fn center(window: &DesktopService) {
    let Some(monitor) = window.current_monitor() else {
        return;
    };
    let (area, origin) = (monitor.size(), monitor.position());
    let size = window.outer_size();
    window.set_outer_position(PhysicalPosition::new(
        origin.x + (area.width as i32 - size.width as i32) / 2,
        origin.y + (area.height as i32 - size.height as i32) / 2,
    ));
}

fn fixed_size(window: &DesktopService, size: (f64, f64)) {
    window.set_resizable(false);
    window.set_min_inner_size(Some(LogicalSize::new(size.0, size.1)));
    window.set_inner_size(LogicalSize::new(size.0, size.1));
    center(window);
}

/// Window size and position of the main UI: restored per mode; also called when switching classic
/// mode.
pub fn apply_main_bounds(window: &DesktopService) {
    let (size, min) = main_sizes(services().settings.classic_mode());
    window.set_resizable(true);
    window.set_min_inner_size(Some(LogicalSize::new(min.0, min.1)));
    enable_main_bounds();
    match SettingsBounds.load(MAIN_BOUNDS_KEY) {
        Some(bounds) => {
            window.set_inner_size(LogicalSize::new(bounds.width, bounds.height));
            window.set_outer_position(PhysicalPosition::new(bounds.x, bounds.y));
        }
        None => {
            window.set_inner_size(LogicalSize::new(size.0, size.1));
            center(window);
        }
    }
}

/// True from startup until the accounts set to log in at startup are done logging in (successfully
/// or not); the session list shows a loading icon meanwhile.
#[derive(Clone, Copy)]
pub struct StartupLogin(pub Signal<bool>);

/// Entry point for stage transitions of the root component.
#[derive(Clone, Copy)]
pub struct RootStage(pub Signal<Stage>);

impl RootStage {
    /// Migration finished: continue the normal startup.
    pub fn finish_moving(mut self) {
        self.0.set(stage_after_boot());
    }

    /// The data directory is usable again: continue the startup sequence, migrating first if a
    /// migration is pending.
    pub fn data_available(mut self, theme: &ThemeHub) {
        match boot().location.pending_move() {
            Some(target) => self.0.set(Stage::Moving(target)),
            None if init_or_report() => {
                load_theme(theme);
                self.finish_moving();
            }
            None => std::process::exit(1),
        }
    }
}

/// The root window takes the main UI's size and place while still hidden, then shows; the login
/// window closes after it, so there is always a window in front of the user.
fn enter_main_after_login(window: &DesktopService) {
    apply_main_bounds(window);
    activate(window);
    windows::close_login();
}

#[component]
pub fn Root() -> Element {
    let initial = use_context::<Stage>();
    let stage = use_signal(|| initial.clone());
    use_context_provider(|| RootStage(stage));
    let restoring = matches!(initial, Stage::Main { restore: true });
    use_context_provider(|| StartupLogin(Signal::new(restoring)));
    let window = use_window();
    use_hook(move || {
        if *stage.peek() == Stage::Login {
            spawn(async { windows::open_login() });
        }
    });

    use_effect({
        let window = window.clone();
        let mut applied = initial;
        move || {
            let current = stage();
            if current == applied {
                return;
            }
            match &current {
                Stage::DataProblem(_) => fixed_size(&window, PROBLEM_SIZE),
                Stage::Moving(_) => fixed_size(&window, MOVING_SIZE),
                Stage::Login => {
                    window.set_visible(false);
                    windows::open_login();
                }
                // Runs after this render: the main UI is in the page before the window shows
                Stage::Main { .. } if applied == Stage::Login => enter_main_after_login(&window),
                Stage::Main { .. } => apply_main_bounds(&window),
            }
            applied = current;
        }
    });

    use_hook(start_update_service);

    use_app_command({
        let window = window.clone();
        move |command| match command {
            // During login the root window is hidden: the login window stands in for it
            AppCommand::ShowMain if *stage.peek() == Stage::Login => windows::open_login(),
            AppCommand::ShowMain => activate(&window),
            AppCommand::EnterMain if *stage.peek() == Stage::Login => {
                let mut stage = stage;
                stage.set(Stage::Main { restore: false });
            }
            AppCommand::Exit => spawn_exit(&window, ExitMode::Normal),
            AppCommand::ExitForUpdate => spawn_exit(&window, ExitMode::Update),
            AppCommand::Restart => spawn_exit(&window, ExitMode::Restart),
            _ => {}
        }
    });

    {
        let window = window.clone();
        use_wry_event_handler(move |event, _| {
            if let Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } = event
                && *window_id == window.id()
            {
                request_close(&window, &stage.peek());
            }
        });
    }

    let current = stage();
    rsx! {
        match current {
            Stage::DataProblem(problem) => rsx! { DataProblemPage { problem } },
            Stage::Moving(target) => rsx! { DataMovePage { target } },
            Stage::Login => rsx! {},
            Stage::Main { restore } => rsx! {
                MainWindowPage {
                    AppRuntime { restore }
                }
            },
        }
    }
}

/// After the window finishes its first layout, reports a successful startup and checks for updates
/// automatically a little later.
fn start_update_service() {
    let Some(update) = boot().update.clone() else {
        return;
    };
    let args = &boot().args;
    update.on_started(args);
    if !args.iter().any(|a| a == PROBE_ARG) {
        spawn(async move {
            update.check_after_startup().await;
        });
    }
}

/// Action of the close button (the system menu and Alt+F4 also arrive here). The window has already
/// been hidden by the system.
pub fn request_close(window: &DesktopService, stage: &Stage) {
    match stage {
        Stage::Moving(_) => window.set_visible(true),
        Stage::DataProblem(_) | Stage::Login => send_command(AppCommand::Exit),
        Stage::Main { .. } => {
            let settings = &services().settings;
            if settings.show_tray_icon()
                && settings.effective_close_action() == CloseAction::MinimizeToTray
            {
                window.set_visible(false);
            } else {
                send_command(AppCommand::Exit);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitMode {
    Normal,
    Restart,
    Update,
}

fn restart_self() {
    let result = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg(WAIT_PID_ARG)
            .arg(std::process::id().to_string())
            .spawn()
    });
    if let Err(error) = result {
        log::error!("重启失败：{error}");
    }
}

static EXITING: AtomicBool = AtomicBool::new(false);

thread_local! {
    // Components are not dropped when the process exits, so the tray icon must be hidden first or
    // it lingers in the notification area
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// Quit flow: stop update checks -> close child windows (each window finishes up in `use_drop`) ->
/// end sessions -> restart or launch the updater. Compromise: the framework exposes no way to close
/// the event loop, so the process is ended directly afterwards; the tray icon is hidden first or it
/// lingers in the notification area.
fn spawn_exit(window: &DesktopService, mode: ExitMode) {
    if EXITING.swap(true, Ordering::SeqCst) {
        return;
    }
    window.set_visible(false);
    TRAY.with_borrow(|tray| {
        if let Some(tray) = tray {
            tray.set_visible(false);
        }
    });
    spawn_forever(async move {
        let update = boot().update.clone();
        if let Some(update) = &update {
            update.shutdown();
        }
        close_all_windows();
        if let Some(services) = try_services()
            && tokio::time::timeout(SHUTDOWN_LIMIT, services.hub.shutdown())
                .await
                .is_err()
        {
            log::warn!("结束会话超时");
        }
        if let Some(services) = try_services() {
            crate::services::plugins().shutdown();
            services.db.flush();
        }
        match mode {
            ExitMode::Normal => {}
            ExitMode::Restart => restart_self(),
            ExitMode::Update => {
                if !update.is_some_and(|u| u.start_updater()) {
                    restart_self();
                }
            }
        }
        log::info!("YaZ 退出");
        log::logger().flush();
        std::process::exit(0);
    });
}

const TRAY_ITEMS: &[TrayItem] = &[
    TrayItem::Action("show", "打开主面板"),
    TrayItem::Action("logout", "退出账号"),
    TrayItem::Separator,
    TrayItem::Action("exit", "退出"),
];

/// Tray tooltip: the program name and the names of the online accounts.
fn tray_tooltip() -> String {
    let names: Vec<String> = services()
        .hub
        .online_sessions()
        .iter()
        .map(|s| s.display_name(s.self_id()))
        .collect();
    if names.is_empty() {
        "YaZ".to_owned()
    } else {
        format!("YaZ - {}", names.join("、"))
    }
}

/// All accounts have no unread messages and no reminders for muted conversations.
fn nothing_unread() -> bool {
    services()
        .hub
        .online_sessions()
        .iter()
        .all(|s| s.total_unread_for_badge() == 0 && s.unread_muted_alert_keys().is_empty())
}

fn plugin_name(plugin_id: &str) -> String {
    crate::services::plugins()
        .plugin(plugin_id)
        .map_or_else(|| plugin_id.to_owned(), |plugin| plugin.name())
}

/// System notification from a plugin: it belongs to no conversation, and the text avatar uses the
/// first character of the plugin name.
fn plugin_notification(
    self_id: i64,
    plugin_name: &str,
    title: &str,
    body: String,
) -> NotificationRequest {
    let key = ConversationKey::dm([], self_id);
    NotificationRequest {
        slot: ListEntry::Conversation(key.clone()),
        key,
        message_id: None,
        title: plugin_text(plugin_name, title),
        body,
        avatar: NotificationAvatar::Text {
            text: plugin_name.chars().take(1).collect(),
            color: String::new(),
        },
        show_popup: true,
        play_sound: false,
        is_reminder: false,
    }
}

/// Fills text into a conversation that has no composer displayed: written into the draft, then the
/// conversation is opened and the composer reads it when it mounts.
fn fill_closed_conversation(account: i64, key: ConversationKey, text: String, replace: bool) {
    let Some(session) = services().hub.session(account) else {
        return;
    };
    spawn_forever(async move {
        let existing = session.draft(&key).map(|draft| draft.content);
        let draft = filled_draft(existing.as_deref(), &text, replace);
        if let Err(error) = session.save_draft(&key, &draft).await {
            log::warn!("保存草稿失败：{error}");
        }
        windows::open_chat(account, key, None);
    });
}

/// Plugin dialogs currently shown: request id -> sender that tells it to close. Removed when the
/// dialog ends.
#[derive(Clone, Default)]
struct ShownDialogs(Rc<RefCell<HashMap<i64, oneshot::Sender<()>>>>);

impl ShownDialogs {
    /// Aligns with the host's pending-display table: tells dialogs no longer in the table to close,
    /// returns the requests in the table not yet shown, and records them as shown.
    fn reconcile(
        &self,
        pending: Vec<PendingDialog>,
    ) -> Vec<(PendingDialog, oneshot::Receiver<()>)> {
        let mut shown = self.0.borrow_mut();
        let stale: Vec<i64> = shown
            .keys()
            .copied()
            .filter(|id| !pending.iter().any(|dialog| dialog.request_id == *id))
            .collect();
        for id in stale {
            if let Some(close) = shown.remove(&id) {
                let _ = close.send(());
            }
        }
        let mut new_dialogs = Vec::new();
        for dialog in pending {
            if let Entry::Vacant(slot) = shown.entry(dialog.request_id) {
                let (close, closed) = oneshot::channel();
                slot.insert(close);
                new_dialogs.push((dialog, closed));
            }
        }
        new_dialogs
    }

    fn finish(&self, request_id: i64) {
        self.0.borrow_mut().remove(&request_id);
    }
}

/// Shows the dialogs in the host's pending-display table that are not yet shown, and closes those
/// that timed out. Events and full alignment both go through here, so requests missed before
/// subscribing or lost after lagging are all made up, and the same request is never shown twice.
fn sync_plugin_dialogs(overlays: Overlays, window: &DesktopService, shown: &ShownDialogs) {
    let new_dialogs = shown.reconcile(crate::services::plugins().pending_dialogs());
    if !new_dialogs.is_empty()
        && (!window.is_visible() || window.is_minimized() || platform::is_hidden_at_edge(window))
    {
        send_command(AppCommand::ShowMain);
    }
    for (dialog, closed) in new_dialogs {
        let shown = shown.clone();
        spawn_forever(async move {
            let closed = async move {
                let _ = closed.await;
            };
            let name = plugin_name(&dialog.plugin_id);
            let result = ask_plugin_dialog(overlays, &name, dialog.spec, closed).await;
            crate::services::plugins().reply_dialog(dialog.request_id, result);
            shown.finish(dialog.request_id);
        });
    }
}

/// Plugin events are broadcast; these events are consumed only here so that multiple windows do not
/// repeat them.
fn on_plugin_event(
    overlays: Overlays,
    window: &DesktopService,
    shown: &ShownDialogs,
    event: PluginEvent,
) {
    match event {
        PluginEvent::OpenConversation {
            account_id,
            key,
            message_id,
        } => open_notification_target(account_id, Some(key), message_id),
        PluginEvent::Toast { plugin_id, text } => {
            overlays.toast(plugin_text(&plugin_name(&plugin_id), &text));
        }
        PluginEvent::SystemNotification {
            plugin_id,
            title,
            body,
        } => {
            let hub = &services().hub;
            let session = hub
                .active_session()
                .or_else(|| hub.online_sessions().into_iter().next());
            if let Some(session) = session {
                let request =
                    plugin_notification(session.self_id(), &plugin_name(&plugin_id), &title, body);
                windows::show_notification(
                    session.account_id(),
                    request,
                    NotificationClick::ShowMain,
                );
            }
        }
        PluginEvent::ShowDialog { .. } | PluginEvent::DialogClosed { .. } => {
            sync_plugin_dialogs(overlays, window, shown);
        }
        PluginEvent::FillComposer {
            account_id,
            conv: Some(key),
            text,
            replace,
            ..
        } if !is_composer_open(account_id, &key) => {
            fill_closed_conversation(account_id, key, text, replace);
        }
        _ => {}
    }
}

/// Background work of the main UI stage: tray, flashing, notifications, theme following the
/// account, plugin prompts and dialogs.
#[component]
fn AppRuntime(restore: bool) -> Element {
    let overlays = use_overlays();
    let window = use_window();
    let shown_dialogs = use_hook(ShownDialogs::default);
    use_plugin_events_resync(
        {
            let window = window.clone();
            let shown = shown_dialogs.clone();
            move |event| on_plugin_event(overlays, &window, &shown, event)
        },
        {
            let window = window.clone();
            move || sync_plugin_dialogs(overlays, &window, &shown_dialogs)
        },
    );
    use_hook(|| spawn(crate::services::load_plugins()));
    let theme = use_context::<ThemeHub>();
    let tray = use_tray(&tray_tooltip(), TRAY_ITEMS, {
        let window = window.clone();
        move |event| match event {
            TrayEvent::LeftClick => activate(&window),
            TrayEvent::Hover(position) => windows::open_tray_messages(position),
            TrayEvent::FlashingStopped => send_command(AppCommand::TrayFlashingStopped),
            TrayEvent::Menu(id) => match id.as_str() {
                "show" => activate(&window),
                "logout" => {
                    activate(&window);
                    send_command(AppCommand::Logout);
                }
                "exit" => send_command(AppCommand::Exit),
                _ => {}
            },
        }
    });
    let tray = use_hook(|| Rc::new(tray));
    let mut startup_login = use_context::<StartupLogin>().0;

    use_hook({
        let tray = tray.clone();
        move || {
            if let Some(tray) = tray.as_ref() {
                tray.set_visible(services().settings.show_tray_icon());
                TRAY.with_borrow_mut(|slot| *slot = Some(tray.clone()));
            }
            if restore {
                spawn(async move {
                    crate::services::load_plugins().await;
                    if let Err(error) = services().hub.restore_startup_accounts().await {
                        log::warn!("恢复账号失败：{error}");
                    }
                    startup_login.set(false);
                });
            }
        }
    });

    // The hidden spare window that windows are rendered in on hover (`shell::spare`)
    use_hook(|| {
        spawn(async {
            tokio::time::sleep(SPARE_AFTER_START).await;
            create_spare();
        })
    });
    use_app_command(|command| {
        if command == AppCommand::RefillSpareWindow {
            spawn(async {
                tokio::time::sleep(SPARE_AFTER_SHOW).await;
                create_spare();
            });
        }
    });

    use_app_command({
        let tray = tray.clone();
        let window = window.clone();
        let mut shown = services().settings.show_tray_icon();
        move |command| {
            if command == AppCommand::SettingsChanged
                && let Some(tray) = tray.as_ref()
            {
                let show = services().settings.show_tray_icon();
                if show != shown {
                    shown = show;
                    tray.set_visible(show);
                    // Once the tray is hidden the window can never be found again
                    if !show {
                        activate(&window);
                    }
                }
            }
        }
    });

    {
        let tray = tray.clone();
        let window = window.clone();
        use_wry_event_handler(move |event, _| {
            if let Event::WindowEvent {
                event: WindowEvent::Focused(true),
                window_id,
                ..
            } = event
                && *window_id == window.id()
                && let Some(tray) = tray.as_ref()
            {
                tray.stop_flashing();
            }
        });
    }

    // Online presence "active/idle": keyboard and mouse input while this program's window is in the
    // foreground counts as input for all logged-in accounts
    use_future(|| async {
        loop {
            tokio::time::sleep(INPUT_POLL).await;
            if platform::app_is_foreground() && platform::idle_time() < INPUT_POLL {
                for session in services().hub.online_sessions() {
                    session.report_user_input();
                }
            }
        }
    });

    use_future(move || {
        let tray = tray.clone();
        let window = window.clone();
        let theme = theme.clone();
        async move {
            let hub = &services().hub;
            let mut rx = hub.subscribe();
            let apply_theme =
                move || theme.apply_scheme(scheme_of(hub.active_session().as_deref()));
            apply_theme();
            loop {
                match rx.recv().await {
                    Ok(HubEvent::AccountsChanged) => {
                        if let Some(tray) = tray.as_ref() {
                            tray.set_tooltip(&tray_tooltip());
                        }
                    }
                    Ok(HubEvent::ActiveChanged(_)) => apply_theme(),
                    Ok(HubEvent::Session(id, event)) => match event {
                        SessionEvent::SettingsChanged(name)
                            if name == "color_scheme" && hub.active_account() == Some(id) =>
                        {
                            apply_theme();
                        }
                        SessionEvent::UnreadChanged(_) | SessionEvent::ConversationsChanged => {
                            if nothing_unread()
                                && let Some(tray) = tray.as_ref()
                            {
                                tray.stop_flashing();
                            }
                        }
                        SessionEvent::Notification(request) => {
                            on_notification(&window, tray.as_ref().as_ref(), id, *request);
                        }
                        _ => {}
                    },
                    Err(RecvError::Lagged(_)) => apply_theme(),
                    Err(RecvError::Closed) => return,
                }
            }
        }
    });
    rsx! {}
}

/// Rings on request; when the account is being viewed it no longer flashes or pops up, otherwise it
/// flashes the taskbar and tray and pops up on request. The conversation being viewed never
/// produces a request itself, so new messages of other conversations still ring while the main
/// window is in the foreground.
fn on_notification(
    window: &DesktopService,
    tray: Option<&Tray>,
    account: i64,
    request: NotificationRequest,
) {
    if request.play_sound {
        platform::play_notification_sound();
    }
    let watching = platform::is_foreground(window)
        && !platform::is_hidden_at_edge(window)
        && services().hub.active_account() == Some(account);
    if watching {
        return;
    }
    platform::flash_taskbar(window);
    if let Some(tray) = tray {
        tray.start_flashing();
    }
    if request.show_popup {
        windows::show_notification(account, request, NotificationClick::OpenTarget);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zulip_plugin_host::{DialogKind, DialogSpec};

    fn dialog(request_id: i64) -> PendingDialog {
        PendingDialog {
            request_id,
            plugin_id: "p".to_owned(),
            account_id: 1,
            spec: DialogSpec {
                kind: DialogKind::Confirm,
                title: String::new(),
                text: String::new(),
                fields: Vec::new(),
            },
        }
    }

    fn ids(new_dialogs: &[(PendingDialog, oneshot::Receiver<()>)]) -> Vec<i64> {
        new_dialogs.iter().map(|(d, _)| d.request_id).collect()
    }

    #[test]
    fn each_pending_dialog_is_shown_once() {
        let shown = ShownDialogs::default();
        let first = shown.reconcile(vec![dialog(1), dialog(2)]);
        assert_eq!(ids(&first), [1, 2]);
        // Do not show again when events and full alignment arrive in duplicate.
        assert!(shown.reconcile(vec![dialog(1), dialog(2)]).is_empty());
        assert_eq!(
            ids(&shown.reconcile(vec![dialog(1), dialog(2), dialog(3)])),
            [3]
        );
    }

    #[test]
    fn dialog_no_longer_pending_is_told_to_close() {
        let shown = ShownDialogs::default();
        let mut first = shown.reconcile(vec![dialog(1), dialog(2)]);
        let (_, mut keep) = first.remove(1);
        let (_, mut gone) = first.remove(0);
        assert!(shown.reconcile(vec![dialog(2)]).is_empty());
        assert!(gone.try_recv().is_ok());
        assert!(matches!(
            keep.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn finished_dialog_is_forgotten_without_closing() {
        let shown = ShownDialogs::default();
        let mut first = shown.reconcile(vec![dialog(1)]);
        let (_, mut closed) = first.remove(0);
        shown.finish(1);
        assert!(shown.reconcile(Vec::new()).is_empty());
        assert!(matches!(
            closed.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }
}
