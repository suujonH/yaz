//! 窗口内的全局状态：当前账号、会话，以及把 Hub / Session 事件转成信号的 hook。
//! 页面从这里读状态，不直接订阅 Hub。

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
/// 事件成批到达时合并后再重读。
const COALESCE: Duration = Duration::from_millis(30);

/// 打开会话后定位一条消息。每次请求都不相等，重复定位同一条消息也会执行。
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

/// 程序级命令：任何线程、任何窗口都能发，用 [`use_app_command`] 接收。
#[derive(Clone, Debug, PartialEq)]
pub enum AppCommand {
    /// 显示并激活主窗口。
    ShowMain,
    /// 主窗口右侧打开会话（由 [`crate::windows::open_chat`] 判断后发出）。
    ShowChat {
        account: i64,
        key: ConversationKey,
        focus: Option<FocusRequest>,
    },
    /// 主窗口切到联系人页的频道并展开。
    ShowChannel {
        account: i64,
        stream_id: i64,
    },
    /// 主窗口右侧显示系统通知。
    ShowNotices {
        account: i64,
    },
    /// 退出当前账号（走主窗口的确认流程）。
    Logout,
    Exit,
    /// 新版本已准备好：退出并启动更新器。
    ExitForUpdate,
    /// 设置页修改数据目录后发出。
    #[allow(dead_code)]
    Restart,
    /// [`crate::services::write_setting`]、[`crate::services::write_account_setting`] 写入后发出。
    SettingsChanged,
    /// 更新服务的状态变化；读 `boot().update` 的 `status()`。
    UpdateStatusChanged,
    /// 托盘停止闪烁，收起托盘消息列表。
    TrayFlashingStopped,
}

static COMMANDS: LazyLock<broadcast::Sender<AppCommand>> =
    LazyLock::new(|| broadcast::channel(64).0);

pub fn send_command(command: AppCommand) {
    // 没有订阅方时命令没有接收者，属于正常情况
    let _ = COMMANDS.send(command);
}

/// 接收程序级命令。
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

/// 窗口绑定到某个账号（放在窗口根上下文）。没有时窗口跟随当前账号。
/// 绑定的账号结束时窗口自动关闭。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountScope(pub i64);

/// 窗口内的全局状态，[`WindowShell`](crate::shell::WindowShell) 提供。
#[derive(Clone, Copy)]
pub struct AppState {
    /// 这个窗口对应的账号。
    pub account: Signal<Option<i64>>,
    pub session: Signal<Option<Arc<Session>>>,
    /// 全部账号及其状态。
    pub accounts: Signal<Rc<Vec<AccountEntry>>>,
    events: CopyValue<broadcast::Sender<SessionEvent>>,
}

/// 当前窗口的 [`AppState`]。
pub fn use_app_state() -> AppState {
    use_context::<AppState>()
}

/// 这个窗口当前的会话（响应式）。
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

/// [`WindowShell`](crate::shell::WindowShell) 调用：提供 [`AppState`]，把这个窗口账号的事件转发给页面。
/// 切换账号在页面看来是一次 [`SessionEvent::Resynced`]。
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
            // 窗口创建到开始订阅之间可能有变化
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

/// 接收这个窗口账号的会话事件。转发落后、切换账号都收到 [`SessionEvent::Resynced`]。
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

/// 从会话读出的数据，事件到来时重读：`filter` 为真的事件以及 `Resynced`、切换账号都触发；
/// 成批的事件合并成一次；结果不变时不触发渲染。没有会话时是 `T::default()`。
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

/// 本机设置的值，其它窗口经 [`write_setting`](crate::services::write_setting) 修改后更新。
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

/// 接收插件宿主的事件（可能来自任何线程，经协程转到界面）。落后时收到 `PluginsChanged`，这时全量重读。
pub fn use_plugin_events(handler: impl FnMut(PluginEvent) + 'static) {
    use_plugin_events_resync(handler, || {});
}

/// 同 [`use_plugin_events`]，另外在开始订阅之后和每次落后时调用 `resync`：
/// 用来补上订阅之前和落后期间错过的事件。
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

/// 插件的日志有变化：参数是插件 id；落后时为 `None`，这时任何插件的日志都可能变了。
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
