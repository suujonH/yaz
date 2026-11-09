//! 插件线程：所有插件的导出函数都在这一个线程上依次执行，一个卡住的插件不影响程序其余部分。

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::thread::ThreadId;

use parking_lot::Mutex;
use tokio::sync::oneshot;
use zulip_session::Session;

use crate::plugin::NativePlugin;

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct CallState {
    // 当前回调所属的会话：宿主函数只用它，已退出账号的回调不会用其他账号发送消息。
    session: Option<Arc<Session>>,
    in_outgoing: bool,
    outgoing: Option<String>,
    in_panel: bool,
    panel: Option<String>,
}

// 同一个线程可能被多个插件使用（插件线程），所以按插件区分。
#[derive(Default)]
struct PluginThreadState {
    bound_account: Option<i64>,
    last_error: Option<String>,
}

thread_local! {
    static CALL: RefCell<CallState> = RefCell::default();
    static PER_PLUGIN: RefCell<HashMap<usize, PluginThreadState>> = RefCell::default();
}

fn with_plugin_state<R>(plugin: &NativePlugin, f: impl FnOnce(&mut PluginThreadState) -> R) -> R {
    let key = std::ptr::from_ref(plugin) as usize;
    PER_PLUGIN.with_borrow_mut(|states| f(states.entry(key).or_default()))
}

/// 绑定本线程上这个插件使用的账号；`None` 解除。
pub(crate) fn bind_account(plugin: &NativePlugin, account_id: Option<i64>) {
    with_plugin_state(plugin, |state| state.bound_account = account_id);
}

pub(crate) fn bound_account(plugin: &NativePlugin) -> Option<i64> {
    with_plugin_state(plugin, |state| state.bound_account)
}

pub(crate) fn set_last_error(plugin: &NativePlugin, text: String) {
    with_plugin_state(plugin, |state| state.last_error = Some(text));
}

pub(crate) fn last_error(plugin: &NativePlugin) -> Option<String> {
    with_plugin_state(plugin, |state| state.last_error.clone())
}

pub(crate) fn call_session() -> Option<Arc<Session>> {
    CALL.with_borrow(|call| call.session.clone())
}

/// 回调中切换账号，只在本次回调内有效。回调之外（没有所属会话）不改变。
pub(crate) fn replace_call_session(session: &Arc<Session>) {
    CALL.with_borrow_mut(|call| {
        if call.session.is_some() {
            call.session = Some(session.clone());
        }
    });
}

/// 在 filter_outgoing 期间执行；返回插件用 set_outgoing_content 设置的新内容。
pub(crate) fn in_outgoing_filter<R>(f: impl FnOnce() -> R) -> (R, Option<String>) {
    CALL.with_borrow_mut(|call| {
        call.in_outgoing = true;
        call.outgoing = None;
    });
    let result = f();
    let changed = CALL.with_borrow_mut(|call| {
        call.in_outgoing = false;
        call.outgoing.take()
    });
    (result, changed)
}

pub(crate) fn try_set_outgoing(content: String) -> bool {
    CALL.with_borrow_mut(|call| {
        if call.in_outgoing {
            call.outgoing = Some(content);
        }
        call.in_outgoing
    })
}

/// 在 on_panel 期间执行；返回插件用 reply_panel 回复的内容。
pub(crate) fn in_panel(f: impl FnOnce()) -> Option<String> {
    CALL.with_borrow_mut(|call| {
        call.in_panel = true;
        call.panel = None;
    });
    f();
    CALL.with_borrow_mut(|call| {
        call.in_panel = false;
        call.panel.take()
    })
}

pub(crate) fn try_reply_panel(json: String) -> bool {
    CALL.with_borrow_mut(|call| {
        if call.in_panel {
            call.panel = Some(json);
        }
        call.in_panel
    })
}

pub(crate) struct PluginThread {
    sender: Mutex<Option<Sender<Job>>>,
    id: ThreadId,
}

impl PluginThread {
    pub fn start() -> Self {
        let (sender, receiver) = channel::<Job>();
        let handle = std::thread::Builder::new()
            .name("plugins".to_owned())
            .spawn(move || {
                for job in receiver {
                    job();
                }
            })
            .expect("无法创建插件线程");
        Self {
            sender: Mutex::new(Some(sender)),
            id: handle.thread().id(),
        }
    }

    pub fn is_current(&self) -> bool {
        std::thread::current().id() == self.id
    }

    /// 投递到插件线程，执行期间记录所属的会话。线程已停止或 `f` panic 时接收端报错。
    pub fn run<T: Send + 'static>(
        &self,
        session: Option<Arc<Session>>,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> oneshot::Receiver<T> {
        let (tx, rx) = oneshot::channel();
        let job = Box::new(move || {
            CALL.with_borrow_mut(|call| call.session = session);
            let result = catch_unwind(AssertUnwindSafe(f));
            CALL.with_borrow_mut(|call| *call = CallState::default());
            match result {
                Ok(value) => {
                    let _ = tx.send(value);
                }
                Err(_) => log::warn!("插件线程上的调用发生 panic"),
            }
        });
        if let Some(sender) = self.sender.lock().as_ref() {
            let _ = sender.send(job);
        }
        rx
    }

    /// 不再接受新的调用（不等待，进程退出时线程随之结束）。
    pub fn stop(&self) {
        self.sender.lock().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_in_order_and_survives_panic() {
        let thread = PluginThread::start();
        assert!(!thread.is_current());
        let bad = thread.run(None, || -> i32 { panic!("测试") });
        assert!(bad.await.is_err());
        let good = thread.run(None, || 1);
        assert_eq!(good.await.unwrap(), 1);
        thread.stop();
        assert!(thread.run(None, || 2).await.is_err());
    }

    #[test]
    fn context_only_inside_callback() {
        assert!(!try_set_outgoing("x".into()));
        let (_, changed) = in_outgoing_filter(|| {
            assert!(try_set_outgoing("a".into()));
            assert!(try_set_outgoing("b".into()));
        });
        assert_eq!(changed.as_deref(), Some("b"));
        assert!(!try_set_outgoing("x".into()));

        assert!(!try_reply_panel("x".into()));
        let reply = in_panel(|| {
            assert!(try_reply_panel("r".into()));
        });
        assert_eq!(reply.as_deref(), Some("r"));
    }
}
