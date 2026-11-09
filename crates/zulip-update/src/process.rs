use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};
use windows::core::{Owned, PWSTR};

use crate::layout::APP_EXE_NAME;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A process after launch.
pub trait StartedApp {
    fn has_exited(&mut self) -> bool;

    /// Kills the process and its child processes.
    fn kill(&mut self) -> io::Result<()>;
}

/// Update operations on processes; tests use a mock implementation instead.
pub trait ProcessHost {
    /// Waits for the process to exit and forcibly kills it after the timeout. Returns immediately
    /// if the process does not exist.
    fn wait_for_exit(&self, process_id: u32, timeout: Duration);

    /// Whether a main app is running from `app_dir` (other than `except_process_id`).
    fn is_app_running(&self, app_dir: &Path, except_process_id: u32) -> bool;

    fn start(&self, exe: &Path, args: &[&OsStr]) -> io::Result<Box<dyn StartedApp>>;
}

pub struct SystemProcessHost;

impl ProcessHost for SystemProcessHost {
    #[allow(unsafe_code)]
    fn wait_for_exit(&self, process_id: u32, timeout: Duration) {
        // SAFETY: the handle is closed by Owned; the remaining calls only use this valid handle.
        unsafe {
            let Ok(handle) =
                OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, process_id)
            else {
                return;
            };
            let handle = Owned::new(handle);
            let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
            if WaitForSingleObject(*handle, millis) == WAIT_TIMEOUT {
                // The main app has already ended all sessions but failed to exit: kill it,
                // otherwise the files stay in use
                let _ = TerminateProcess(*handle, 1);
                WaitForSingleObject(*handle, 10_000);
            }
        }
    }

    fn is_app_running(&self, app_dir: &Path, except_process_id: u32) -> bool {
        let expected = normalize(&app_dir.join(APP_EXE_NAME));
        process_ids_named(APP_EXE_NAME)
            .into_iter()
            .filter(|&id| id != except_process_id)
            .any(|id| image_path(id).is_some_and(|path| normalize(&path) == expected))
    }

    fn start(&self, exe: &Path, args: &[&OsStr]) -> io::Result<Box<dyn StartedApp>> {
        let mut command = Command::new(exe);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(dir) = exe.parent() {
            command.current_dir(dir);
        }
        Ok(Box::new(StartedChild(command.spawn()?)))
    }
}

struct StartedChild(Child);

impl StartedApp for StartedChild {
    fn has_exited(&mut self) -> bool {
        !matches!(self.0.try_wait(), Ok(None))
    }

    fn kill(&mut self) -> io::Result<()> {
        let tree_killed = Command::new("taskkill")
            .args(["/PID", &self.0.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .is_ok_and(|status| status.success());
        if !tree_killed {
            self.0.kill()?;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.0.try_wait()?.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

fn normalize(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
}

#[allow(unsafe_code)]
fn process_ids_named(name: &str) -> Vec<u32> {
    let mut ids = Vec::new();
    // SAFETY: the snapshot handle is closed by Owned; PROCESSENTRY32W's dwSize is set as required.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return ids;
        };
        let snapshot = Owned::new(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(*snapshot, &mut entry).is_ok();
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(name) {
                ids.push(entry.th32ProcessID);
            }
            more = Process32NextW(*snapshot, &mut entry).is_ok();
        }
    }
    ids
}

// A process belonging to another user or lacking permission cannot be opened; treat it as not
// running from this app directory
#[allow(unsafe_code)]
fn image_path(process_id: u32) -> Option<PathBuf> {
    // SAFETY: the handle is closed by Owned; the buffer length matches the length passed in.
    unsafe {
        let handle: HANDLE =
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;
        let handle = Owned::new(handle);
        let mut buffer = vec![0u16; 4096];
        let mut len = buffer.len() as u32;
        QueryFullProcessImageNameW(
            *handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
        Some(PathBuf::from(OsString::from_wide(&buffer[..len as usize])))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    // Copy the system's ping.exe as yaz.exe, to serve as a long-running "main app"
    fn spawn_fake_app(dir: &Path) -> (StartedChild, u32) {
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let exe = dir.join(APP_EXE_NAME);
        fs::copy(Path::new(&system).join(r"System32\ping.exe"), &exe).unwrap();
        let child = Command::new(&exe)
            .args(["-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let id = child.id();
        (StartedChild(child), id)
    }

    #[test]
    fn detects_app_running_from_directory_and_ignores_excluded_process() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("app");
        fs::create_dir(&dir).unwrap();
        assert!(!SystemProcessHost.is_app_running(&dir, 0));

        let (mut app, id) = spawn_fake_app(&dir);
        assert!(SystemProcessHost.is_app_running(&dir, 0));
        assert!(!SystemProcessHost.is_app_running(&dir, id));
        assert!(!SystemProcessHost.is_app_running(temp.path(), 0));

        app.kill().unwrap();
        assert!(app.has_exited());
        assert!(!SystemProcessHost.is_app_running(&dir, 0));
    }

    #[test]
    fn wait_for_exit_kills_process_after_timeout() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("app");
        fs::create_dir(&dir).unwrap();
        let (mut app, id) = spawn_fake_app(&dir);

        let start = Instant::now();
        SystemProcessHost.wait_for_exit(id, Duration::from_millis(100));

        assert!(start.elapsed() < Duration::from_secs(30));
        assert!(app.has_exited());
    }

    #[test]
    fn wait_for_exit_returns_immediately_for_missing_process() {
        let start = Instant::now();
        SystemProcessHost.wait_for_exit(u32::MAX - 1, Duration::from_secs(30));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn start_reports_missing_executable() {
        let result = SystemProcessHost.start(Path::new(r"C:\no-such-dir\yaz.exe"), &[]);
        assert!(result.is_err());
    }
}
