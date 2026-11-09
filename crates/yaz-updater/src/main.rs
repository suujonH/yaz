//! 独立更新器。命令行：
//! `--pid 旧进程 --app-dir 程序目录 --update-root 更新工作目录 --from 旧版本 --to 新版本 [--log 日志文件]`。
//! 没有界面；由主程序复制到更新工作目录后启动，流程见 `zulip_update::UpdateApplier`。
#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use zulip_update::{ApplyResult, SystemProcessHost, UpdateApplier, UpdateRequest};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = parse_args(&args);
    let log_path = options.get("--log").map(PathBuf::from);
    let log: zulip_update::Logger =
        Arc::new(move |message| write_log(log_path.as_deref(), message));

    let Some(request) = build_request(&options) else {
        log(&format!("参数不完整：{}", args.join(" ")));
        return ExitCode::from(2);
    };
    match UpdateApplier::new(request, &SystemProcessHost, log).run() {
        ApplyResult::Updated => ExitCode::SUCCESS,
        ApplyResult::RolledBack | ApplyResult::Skipped => ExitCode::from(1),
    }
}

fn parse_args(args: &[String]) -> HashMap<&str, &str> {
    args.as_chunks::<2>()
        .0
        .iter()
        .map(|[name, value]| (name.as_str(), value.as_str()))
        .collect()
}

fn build_request(options: &HashMap<&str, &str>) -> Option<UpdateRequest> {
    Some(UpdateRequest {
        process_id: options.get("--pid")?.parse().ok()?,
        app_dir: options.get("--app-dir")?.into(),
        update_root: options.get("--update-root")?.into(),
        from_version: (*options.get("--from")?).to_string(),
        to_version: (*options.get("--to")?).to_string(),
    })
}

// 日志写不进去不应影响更新
fn write_log(path: Option<&Path>, message: &str) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let _ = writeln!(file, "{time} {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn builds_request_from_complete_arguments() {
        let args = args(&[
            "--pid",
            "1234",
            "--app-dir",
            r"C:\Apps\Zulip",
            "--update-root",
            r"C:\Data\Update",
            "--from",
            "1.0.0",
            "--to",
            "1.1.0",
            "--log",
            r"C:\Data\updater.log",
        ]);
        let options = parse_args(&args);
        let request = build_request(&options).unwrap();

        assert_eq!(request.process_id, 1234);
        assert_eq!(request.app_dir, PathBuf::from(r"C:\Apps\Zulip"));
        assert_eq!(request.update_root, PathBuf::from(r"C:\Data\Update"));
        assert_eq!(request.from_version, "1.0.0");
        assert_eq!(request.to_version, "1.1.0");
        assert_eq!(options["--log"], r"C:\Data\updater.log");
    }

    #[test]
    fn rejects_missing_or_malformed_arguments() {
        let base = [
            "--pid",
            "1",
            "--app-dir",
            "a",
            "--update-root",
            "r",
            "--from",
            "1.0.0",
            "--to",
            "1.1.0",
        ];
        assert!(build_request(&parse_args(&args(&base))).is_some());
        assert!(build_request(&parse_args(&args(&base[2..]))).is_none());
        assert!(build_request(&parse_args(&args(&base[..8]))).is_none());

        let mut bad_pid = args(&base);
        bad_pid[1] = "x".into();
        assert!(build_request(&parse_args(&bad_pid)).is_none());
        bad_pid[1] = "-1".into();
        assert!(build_request(&parse_args(&bad_pid)).is_none());
    }

    #[test]
    fn log_is_appended_with_timestamp_and_skipped_without_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("logs").join("updater.log");

        write_log(Some(&path), "第一行");
        write_log(Some(&path), "第二行");
        write_log(None, "无处可写");

        let text = fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with(" 第一行") && lines[0].len() > "第一行".len() + 20);
        assert!(lines[1].ends_with(" 第二行"));
    }
}
