//! 根窗口：数据目录提示 → 搬迁数据目录 → 登录 → 主界面是同一个窗口的四个阶段；托盘、通知、退出流程都在这里。

use crate::pages::composer::{filled_draft, is_composer_open};
use crate::pages::data_move::{DataMovePage, DataProblem, DataProblemPage};
use crate::pages::login::LoginWindowPage;
use crate::pages::main_window::MainWindowPage;
use crate::pages::notification::NotificationClick;
use crate::platform::{self, Tray, TrayEvent, TrayItem, use_tray};
use crate::services::{
    MAIN_BOUNDS_KEY, SettingsBounds, boot, enable_main_bounds, init_or_report, load_theme,
    scheme_of, services, try_services,
};
use crate::shell::{ThemeHub, WindowBoundsStore, WindowSpec, activate, close_all_windows};
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

const LOGIN_SIZE: (f64, f64) = (320.0, 448.0);
const MOVING_SIZE: (f64, f64) = (440.0, 180.0);
const PROBLEM_SIZE: (f64, f64) = (480.0, 240.0);
const MAIN_SIZE: (f64, f64) = (1090.0, 694.0);
const MAIN_MIN_SIZE: (f64, f64) = (800.0, 500.0);
const CLASSIC_SIZE: (f64, f64) = (374.0, 694.0);
const CLASSIC_MIN_SIZE: (f64, f64) = (300.0, 500.0);
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(5);
/// 检查用户输入的间隔：这段时间内有输入就报告给会话。
const INPUT_POLL: Duration = Duration::from_secs(5);

/// 根窗口的阶段。
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// 数据目录有问题，等用户选择。这时还没有打开数据库。
    DataProblem(DataProblem),
    /// 正在把数据目录搬到这个位置。
    Moving(PathBuf),
    Login,
    /// `restore` 为 true 时登录设置为启动时登录的账号。
    Main {
        restore: bool,
    },
}

/// 有启动时登录的账号时直接进主界面，否则显示登录。
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

/// 根窗口的创建参数。
pub fn root_window_spec(stage: &Stage) -> WindowSpec {
    let spec = |size, min_size, resizable| {
        let mut spec = WindowSpec::new(MAIN_BOUNDS_KEY, "YaZ", size)
            .min_size(min_size)
            .hide_on_close();
        spec.resizable = resizable;
        // 主窗口启动时自己决定是否显示（可能最小化到托盘）
        spec.show_when_ready = false;
        spec
    };
    match stage {
        Stage::DataProblem(_) => spec(PROBLEM_SIZE, PROBLEM_SIZE, false),
        Stage::Moving(_) => spec(MOVING_SIZE, MOVING_SIZE, false),
        Stage::Login => spec(LOGIN_SIZE, LOGIN_SIZE, false),
        Stage::Main { .. } => {
            let (size, min) = main_sizes(services().settings.classic_mode());
            spec(size, min, true)
        }
    }
}

/// 把窗口放到屏幕中央。
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

/// 主界面的窗口大小和位置：按模式恢复；经典模式切换时也调用。
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

/// 根组件的阶段切换入口。
#[derive(Clone, Copy)]
pub struct RootStage(pub Signal<Stage>);

impl RootStage {
    /// 搬迁完成：继续正常启动。
    pub fn finish_moving(mut self) {
        self.0.set(stage_after_boot());
    }

    /// 数据目录恢复可用：继续启动序列，有待办的搬迁时先搬迁。
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

    pub fn enter_main(mut self) {
        self.0.set(Stage::Main { restore: false });
    }
}

#[component]
pub fn Root() -> Element {
    let initial = use_context::<Stage>();
    let stage = use_signal(|| initial.clone());
    use_context_provider(|| RootStage(stage));
    let window = use_window();

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
                Stage::Login => fixed_size(&window, LOGIN_SIZE),
                Stage::Main { .. } => apply_main_bounds(&window),
            }
            applied = current;
        }
    });

    use_hook(start_update_service);

    use_app_command({
        let window = window.clone();
        move |command| match command {
            AppCommand::ShowMain => activate(&window),
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
            Stage::Login => rsx! { LoginWindowPage {} },
            Stage::Main { restore } => rsx! {
                MainWindowPage {
                    AppRuntime { restore }
                }
            },
        }
    }
}

/// 窗口完成首次布局后报告启动成功，稍后自动检查更新。
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

/// 关闭按钮的动作（系统菜单、Alt+F4 也到这里）。窗口已被系统隐藏。
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
    // 进程退出时不会析构组件，托盘图标要先隐藏，否则残留在通知区域
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// 退出流程：停止更新检查 → 关闭子窗口（各窗口在 `use_drop` 收尾）→ 结束会话 → 重启或启动更新器。
/// 妥协：框架没有公开关闭事件循环的接口，收尾后直接结束进程；托盘图标先隐藏，否则残留在通知区域。
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

/// 托盘的文字提示：程序名和在线账号的名字。
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

/// 所有账号都没有未读，也没有免打扰会话的提醒。
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

/// 插件的系统通知：不属于任何会话，文字头像用插件名的第一个字。
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

/// 把文字填入没有输入框显示的会话：写进草稿后打开会话，输入框挂载时读到。
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

/// 正在显示的插件对话框：请求 id → 通知它关闭的发送端。对话框结束时移除。
#[derive(Clone, Default)]
struct ShownDialogs(Rc<RefCell<HashMap<i64, oneshot::Sender<()>>>>);

impl ShownDialogs {
    /// 与宿主的待显示表对齐：通知已不在表中的对话框关闭，返回表中还没有显示的请求，并记录为已显示。
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

/// 显示宿主待显示表中还没有显示的对话框，关闭已超时的。事件和全量对齐都走这里，所以订阅前错过、
/// 落后丢失的请求都能补上，同一个请求也不会显示两次。
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

/// 插件事件是广播，这些事件只在这里消费，多个窗口才不会重复。
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

/// 主界面阶段的后台工作：托盘、闪烁、通知、主题跟随账号、插件的提示和对话框。
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

    use_hook({
        let tray = tray.clone();
        move || {
            if let Some(tray) = tray.as_ref() {
                tray.set_visible(services().settings.show_tray_icon());
                TRAY.with_borrow_mut(|slot| *slot = Some(tray.clone()));
            }
            if restore {
                spawn(async {
                    crate::services::load_plugins().await;
                    if let Err(error) = services().hub.restore_startup_accounts().await {
                        log::warn!("恢复账号失败：{error}");
                    }
                });
            }
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
                    // 隐藏托盘后窗口再也找不回来
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

    // 在线状态的“活跃/空闲”：本程序的窗口在前台时有键盘鼠标输入，算作所有已登录账号的输入
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

/// 按请求响铃；正在看这个账号时不再闪烁和弹窗，否则闪烁任务栏和托盘，按请求弹窗。
/// 正在看的会话本身不会产生请求，所以主窗口在前台时其它会话的新消息仍然响铃。
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
        // 事件和全量对齐重复到达时不再显示。
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
