//! 进程启动：等待旧实例退出、单实例。

use crate::state::{AppCommand, send_command};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent,
    WaitForSingleObject,
};
use windows::core::HSTRING;
use zulip_store::DataLocation;
use zulip_update::UpdateService;

/// 重启时传给新进程：等这个进程退出后再启动。
pub const WAIT_PID_ARG: &str = "--wait-pid";
const WAIT_LIMIT: Duration = Duration::from_secs(30);

/// 打开数据库之前就确定的内容。
pub struct Boot {
    pub args: Vec<String>,
    pub location: DataLocation,
    pub update: Option<Arc<UpdateService>>,
}

/// 重启后新进程要等旧进程结束，否则数据库和单实例锁还被占着。
pub fn wait_for_previous_instance(args: &[String]) {
    let Some(pid) = args
        .iter()
        .position(|a| a == WAIT_PID_ARG)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<u32>().ok())
    else {
        return;
    };
    // SAFETY: 句柄只在本函数内使用，用后关闭
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return;
        };
        let _ = WaitForSingleObject(process, WAIT_LIMIT.as_millis() as u32);
        let _ = CloseHandle(process);
    }
}

/// 取得单实例锁，返回 false 表示已有实例在运行（它已被激活），调用方应退出。
/// 锁的名称含数据目录，指向其它目录的测试实例互不影响。句柄存活到进程退出，由系统回收。
pub fn acquire_instance(default_root: &Path) -> bool {
    let mut hasher = DefaultHasher::new();
    default_root
        .to_string_lossy()
        .to_lowercase()
        .replace('/', "\\")
        .hash(&mut hasher);
    let id = hasher.finish();
    let mutex_name = HSTRING::from(format!("Local\\YaZ-Instance-{id:x}"));
    let event_name = HSTRING::from(format!("Local\\YaZ-Activate-{id:x}"));
    // SAFETY: 名称在调用期间有效；句柄不关闭
    unsafe {
        if let Err(error) = CreateMutexW(None, true, &mutex_name) {
            log::warn!("创建单实例锁失败：{error}");
            return true;
        }
        let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
        let event = match CreateEventW(None, false, false, &event_name) {
            Ok(event) => event,
            Err(error) => {
                log::warn!("创建激活事件失败：{error}");
                return !already_running;
            }
        };
        if already_running {
            let _ = SetEvent(event);
            return false;
        }
        let raw_event = event.0 as isize;
        std::thread::spawn(move || {
            let event = HANDLE(raw_event as *mut _);
            while WaitForSingleObject(event, INFINITE) == WAIT_OBJECT_0 {
                send_command(AppCommand::ShowMain);
            }
        });
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_for_previous_ignores_bad_arguments() {
        wait_for_previous_instance(&[]);
        wait_for_previous_instance(&[WAIT_PID_ARG.to_owned()]);
        wait_for_previous_instance(&[WAIT_PID_ARG.to_owned(), "x".to_owned()]);
    }
}
