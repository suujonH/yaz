use std::ffi::OsString;
use std::os::windows::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::*;
use crate::layout::BACKUP_DIR_NAME;
use crate::package::verify_files;
use crate::testing::{version_files, write_tree};

#[derive(Default)]
struct FakeApp {
    exited: AtomicBool,
    killed: AtomicBool,
}

struct FakeHandle(Arc<FakeApp>);

impl StartedApp for FakeHandle {
    fn has_exited(&mut self) -> bool {
        self.0.exited.load(Ordering::SeqCst) || self.0.killed.load(Ordering::SeqCst)
    }

    fn kill(&mut self) -> io::Result<()> {
        self.0.killed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

type OnStart = Box<dyn Fn(&FakeApp, &[OsString]) -> io::Result<()>>;

struct FakeHost {
    on_start: Mutex<OnStart>,
    app_running: AtomicBool,
    waited_for: Mutex<Option<u32>>,
    starts: Mutex<Vec<(PathBuf, Vec<OsString>)>>,
    started: Mutex<Vec<Arc<FakeApp>>>,
}

impl ProcessHost for FakeHost {
    fn wait_for_exit(&self, process_id: u32, _timeout: Duration) {
        *self.waited_for.lock().unwrap() = Some(process_id);
    }

    fn is_app_running(&self, _app_dir: &Path, _except_process_id: u32) -> bool {
        self.app_running.load(Ordering::SeqCst)
    }

    fn start(&self, exe: &Path, args: &[&OsStr]) -> io::Result<Box<dyn StartedApp>> {
        let args: Vec<OsString> = args.iter().map(|a| (*a).to_owned()).collect();
        self.starts
            .lock()
            .unwrap()
            .push((exe.to_path_buf(), args.clone()));
        let app = Arc::new(FakeApp::default());
        self.started.lock().unwrap().push(app.clone());
        (self.on_start.lock().unwrap())(&app, &args)?;
        Ok(Box::new(FakeHandle(app)))
    }
}

// The call that starts the new version carries the probe argument
fn is_probe(args: &[OsString]) -> bool {
    args.len() == 2
}

fn confirm_version(args: &[OsString], version: &str) {
    if is_probe(args) {
        fs::write(&args[1], version).unwrap();
    }
}

fn write_ready(layout: &UpdateLayout, version: &str) {
    let manifest = UpdateManifest {
        version: version.into(),
        package: "p.zip".into(),
        sha256: "a".repeat(64),
        size: 1,
    };
    fs::write(layout.ready_path(), manifest.to_json()).unwrap();
}

struct Fixture {
    temp: TempDir,
    layout: UpdateLayout,
    host: FakeHost,
    log: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temp.path().join("update"));
        let app_dir = temp.path().join("app");
        fs::create_dir_all(layout.root()).unwrap();
        write_tree(
            &app_dir,
            &version_files(
                "old",
                &["old-only.dll", r"plugins\builtin\old.dll", "shared.dll"],
            ),
        );
        // Files that do not belong to the app: the user unzipped the zip into a directory with
        // other files, or installed a plugin themselves
        fs::write(app_dir.join("user-notes.txt"), "mine").unwrap();
        fs::create_dir_all(app_dir.join(r"plugins\my-plugin")).unwrap();
        fs::write(app_dir.join(r"plugins\my-plugin\plugin.dll"), "mine").unwrap();

        write_tree(
            &layout.staged_dir(),
            &version_files(
                "new",
                &["new-only.dll", r"plugins\builtin\new.dll", "shared.dll"],
            ),
        );
        write_ready(&layout, "2.0.0");

        let fixture = Self {
            temp,
            layout,
            host: FakeHost {
                on_start: Mutex::new(Box::new(|_, _| Ok(()))),
                app_running: AtomicBool::new(false),
                waited_for: Mutex::new(None),
                starts: Mutex::new(Vec::new()),
                started: Mutex::new(Vec::new()),
            },
            log: Arc::default(),
        };
        fixture.on_start(|_, args| {
            confirm_version(args, "2.0.0");
            Ok(())
        });
        fixture
    }

    fn on_start(&self, f: impl Fn(&FakeApp, &[OsString]) -> io::Result<()> + 'static) {
        *self.host.on_start.lock().unwrap() = Box::new(f);
    }

    fn app_dir(&self) -> PathBuf {
        self.temp.path().join("app")
    }

    fn applier(&self) -> UpdateApplier<'_> {
        let log = self.log.clone();
        let request = UpdateRequest {
            process_id: 1234,
            app_dir: self.app_dir(),
            update_root: self.layout.root().to_path_buf(),
            from_version: "1.0.0".into(),
            to_version: "2.0.0".into(),
        };
        let mut applier = UpdateApplier::new(
            request,
            &self.host,
            Arc::new(move |m: &str| log.lock().unwrap().push(m.to_string())),
        );
        applier.probe_timeout = Duration::from_secs(5);
        applier.stable_time = Duration::ZERO;
        applier.poll_interval = Duration::from_millis(5);
        applier.file_retry_delay = Duration::ZERO;
        applier
    }

    fn app(&self, relative: &str) -> String {
        fs::read_to_string(self.app_dir().join(relative)).unwrap()
    }

    fn app_has(&self, relative: &str) -> bool {
        self.app_dir().join(relative).exists()
    }

    fn state(&self) -> UpdateState {
        UpdateState::load(&self.layout.state_path())
    }

    fn starts(&self) -> Vec<(PathBuf, Vec<OsString>)> {
        self.host.starts.lock().unwrap().clone()
    }

    fn first_started_was_killed(&self) -> bool {
        self.host.started.lock().unwrap()[0]
            .killed
            .load(Ordering::SeqCst)
    }

    fn assert_user_files_intact(&self) {
        assert_eq!(self.app("user-notes.txt"), "mine");
        assert_eq!(self.app(r"plugins\my-plugin\plugin.dll"), "mine");
    }

    fn assert_old_version_intact(&self) {
        assert_eq!(self.app(APP_EXE_NAME), "app old");
        assert_eq!(self.app("old-only.dll"), "old-only.dll old");
        assert_eq!(self.app("shared.dll"), "shared.dll old");
        assert_eq!(
            self.app(r"plugins\builtin\old.dll"),
            r"plugins\builtin\old.dll old"
        );
        assert!(!self.app_has("new-only.dll"));
        assert!(!self.app_has(r"plugins\builtin\new.dll"));
        assert!(!self.app_has(BACKUP_DIR_NAME));
        let list = fs::read_to_string(self.app_dir().join(FILE_LIST_NAME)).unwrap();
        verify_files(&self.app_dir(), &FileEntry::parse_list(&list).unwrap()).unwrap();
        self.assert_user_files_intact();
    }

    // Start the new version with the probe argument first; after it fails, start the old version
    // without arguments
    fn assert_started_old_after_probe(&self) {
        let starts = self.starts();
        assert_eq!(starts.len(), 2);
        assert!(is_probe(&starts[0].1));
        assert!(starts[1].1.is_empty());
    }
}

#[test]
fn success_installs_new_version_and_removes_old_files() {
    let f = Fixture::new();
    assert_eq!(f.applier().run(), ApplyResult::Updated);

    assert_eq!(f.app(APP_EXE_NAME), "app new");
    assert_eq!(f.app("shared.dll"), "shared.dll new");
    assert_eq!(f.app("new-only.dll"), "new-only.dll new");
    assert_eq!(
        f.app(r"plugins\builtin\new.dll"),
        r"plugins\builtin\new.dll new"
    );
    assert!(!f.app_has("old-only.dll"));
    assert!(!f.app_has(r"plugins\builtin\old.dll"));
    assert!(!f.app_has(BACKUP_DIR_NAME));
    f.assert_user_files_intact();

    assert_eq!(*f.host.waited_for.lock().unwrap(), Some(1234));
    let starts = f.starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].0, f.app_dir().join(APP_EXE_NAME));
    assert_eq!(
        starts[0].1,
        [
            OsString::from(PROBE_ARG),
            f.layout.probe_path().into_os_string()
        ]
    );

    let state = f.state();
    assert!(state.allows_auto_attempt("2.0.0"));
    assert!(state.pending_report.unwrap().contains("2.0.0"));
    assert!(!f.layout.staged_dir().exists());
    assert!(!f.layout.ready_path().exists());
}

#[test]
fn old_directory_is_removed_only_when_empty() {
    let f = Fixture::new();
    f.applier().run();
    assert!(f.app_has(r"plugins\my-plugin"));
    assert!(f.app_has(r"plugins\builtin"));

    // When the new version has no builtin directory, the old directory is removed along with the
    // old files
    let f = Fixture::new();
    let staged = f.layout.staged_dir();
    fs::remove_file(staged.join(r"plugins\builtin\new.dll")).unwrap();
    let list = fs::read_to_string(staged.join(FILE_LIST_NAME)).unwrap();
    let list: String = list
        .lines()
        .filter(|line| !line.contains("builtin"))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(staged.join(FILE_LIST_NAME), list).unwrap();

    assert_eq!(f.applier().run(), ApplyResult::Updated);
    assert!(!f.app_has(r"plugins\builtin"));
    assert!(f.app_has(r"plugins\my-plugin"));
}

#[test]
fn read_only_old_files_are_removed() {
    let f = Fixture::new();
    for name in ["old-only.dll", "shared.dll"] {
        let path = f.app_dir().join(name);
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
    }

    assert_eq!(f.applier().run(), ApplyResult::Updated);
    assert!(!f.app_has("old-only.dll"));
    assert!(!f.app_has(BACKUP_DIR_NAME));
}

#[test]
fn new_version_crashing_rolls_back_starts_old_and_blocks_version() {
    let f = Fixture::new();
    f.on_start(|app, args| {
        if is_probe(args) {
            app.exited.store(true, Ordering::SeqCst);
        }
        Ok(())
    });

    assert_eq!(f.applier().run(), ApplyResult::RolledBack);

    f.assert_old_version_intact();
    f.assert_started_old_after_probe();
    let state = f.state();
    assert!(state.blocked);
    assert!(!state.allows_auto_attempt("2.0.0"));
    assert!(state.allows_auto_attempt("2.0.1"));
    assert!(!f.layout.ready_path().exists());
}

#[test]
fn new_version_never_confirming_is_killed_and_rolled_back() {
    let f = Fixture::new();
    f.on_start(|_, _| Ok(()));
    let mut applier = f.applier();
    applier.probe_timeout = Duration::from_millis(50);

    assert_eq!(applier.run(), ApplyResult::RolledBack);

    assert!(f.first_started_was_killed());
    f.assert_old_version_intact();
    f.assert_started_old_after_probe();
}

#[test]
fn new_version_reporting_wrong_version_is_rolled_back() {
    // Otherwise the new app thinks it is still the old version and updates again at every start
    let f = Fixture::new();
    f.on_start(|_, args| {
        confirm_version(args, "1.0.0");
        Ok(())
    });

    assert_eq!(f.applier().run(), ApplyResult::RolledBack);

    assert!(f.first_started_was_killed());
    f.assert_old_version_intact();
    assert!(f.state().blocked);
}

#[test]
fn new_version_exiting_right_after_confirming_is_rolled_back() {
    let f = Fixture::new();
    f.on_start(|app, args| {
        confirm_version(args, "2.0.0");
        app.exited.store(is_probe(args), Ordering::SeqCst);
        Ok(())
    });
    let mut applier = f.applier();
    applier.stable_time = Duration::from_millis(20);

    assert_eq!(applier.run(), ApplyResult::RolledBack);
    f.assert_old_version_intact();
}

#[test]
fn new_version_failing_to_launch_is_rolled_back() {
    let f = Fixture::new();
    f.on_start(|_, args| {
        if is_probe(args) {
            return Err(io::Error::other("无法启动"));
        }
        Ok(())
    });

    assert_eq!(f.applier().run(), ApplyResult::RolledBack);

    f.assert_old_version_intact();
    assert!(f.starts().last().unwrap().1.is_empty());
}

#[test]
fn corrupted_staged_files_fail_before_touching_the_app() {
    let f = Fixture::new();
    fs::write(f.layout.staged_dir().join("new-only.dll"), "tampered").unwrap();

    assert_eq!(f.applier().run(), ApplyResult::RolledBack);

    f.assert_old_version_intact();
    let starts = f.starts();
    assert_eq!(starts.len(), 1);
    assert!(starts[0].1.is_empty());
    let state = f.state();
    assert_eq!(state.failures, 1);
    assert!(!state.blocked);
}

#[test]
fn staged_version_differing_from_request_is_refused() {
    let f = Fixture::new();
    write_ready(&f.layout, "3.0.0");

    assert_eq!(f.applier().run(), ApplyResult::RolledBack);
    f.assert_old_version_intact();
}

#[test]
fn locked_file_during_swap_rolls_back_and_counts_failure() {
    let f = Fixture::new();
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(f.app_dir().join("shared.dll"))
        .unwrap();

    assert_eq!(f.applier().run(), ApplyResult::RolledBack);
    drop(locked);

    f.assert_old_version_intact();
    let starts = f.starts();
    assert_eq!(starts.len(), 1);
    assert!(starts[0].1.is_empty());
    let state = f.state();
    assert_eq!(state.failures, 1);
    assert!(!state.blocked);
}

#[test]
fn another_instance_running_skips_without_changes() {
    let f = Fixture::new();
    f.host.app_running.store(true, Ordering::SeqCst);

    assert_eq!(f.applier().run(), ApplyResult::Skipped);

    assert!(f.starts().is_empty());
    assert_eq!(f.app(APP_EXE_NAME), "app old");
    assert!(f.layout.ready_path().exists());
    assert!(f.state().allows_auto_attempt("2.0.0"));
}

#[test]
fn failure_is_recorded_before_old_version_starts() {
    // The old app must be able to read the failure record when it starts, otherwise it would
    // immediately try again
    let f = Fixture::new();
    let seen = Arc::new(Mutex::new(None));
    let (seen_in_start, state_path) = (seen.clone(), f.layout.state_path());
    f.on_start(move |app, args| {
        if is_probe(args) {
            app.exited.store(true, Ordering::SeqCst);
        } else {
            *seen_in_start.lock().unwrap() = Some(UpdateState::load(&state_path));
        }
        Ok(())
    });

    f.applier().run();

    let seen = seen.lock().unwrap().take().unwrap();
    assert!(!seen.allows_auto_attempt("2.0.0"));
}
