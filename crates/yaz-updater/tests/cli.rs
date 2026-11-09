use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use zulip_update::{UpdateLayout, UpdateState};

fn updater() -> Command {
    Command::new(env!("CARGO_BIN_EXE_yaz-updater"))
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn incomplete_arguments_exit_with_code_2_and_log_the_reason() {
    let temp = TempDir::new().unwrap();
    let log = temp.path().join("logs").join("updater.log");

    let status = updater()
        .args(["--pid", "1", "--log"])
        .arg(&log)
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(2));
    assert!(read_log(&log).contains("参数不完整"));
}

#[test]
fn missing_staged_update_records_failure_and_exits_with_code_1() {
    let temp = TempDir::new().unwrap();
    let app_dir = temp.path().join("app");
    let root = temp.path().join("update");
    let log = temp.path().join("updater.log");
    fs::create_dir_all(&app_dir).unwrap();

    let status = updater()
        .args(["--pid", "4000000000", "--from", "1.0.0", "--to", "1.1.0"])
        .arg("--app-dir")
        .arg(&app_dir)
        .arg("--update-root")
        .arg(&root)
        .arg("--log")
        .arg(&log)
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(1));
    let state = UpdateState::load(&UpdateLayout::new(&root).state_path());
    assert_eq!(state.failures, 1);
    assert!(!state.blocked);
    assert!(state.last_error.unwrap().contains("没有准备好的新版本"));
    let log = read_log(&log);
    assert!(log.contains("开始更新：1.0.0 → 1.1.0"));
    assert!(log.contains("更新失败"));
}
