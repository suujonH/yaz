use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::package::FileEntry;
use crate::testing::{version_files, write_tree};

struct Fixture {
    temp: TempDir,
    old: Vec<FileEntry>,
    new: Vec<FileEntry>,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let old = version_files("old", &["old-only.dll", r"lib\old.dll", "shared.dll"]);
        let new = version_files("new", &["new-only.dll", r"bin\new.dll", "shared.dll"]);
        write_tree(&temp.path().join("app"), &old);
        write_tree(&temp.path().join("staged"), &new);
        Self {
            old: entries(&temp.path().join("app")),
            new: entries(&temp.path().join("staged")),
            temp,
        }
    }

    fn app_dir(&self) -> PathBuf {
        self.temp.path().join("app")
    }

    fn swap(&self) -> FileSwap {
        let mut swap = FileSwap::new(&self.app_dir(), Arc::new(|_: &str| {}));
        swap.retry_delay = Duration::ZERO;
        swap.retry_count = 2;
        swap
    }

    fn apply(&self, swap: &mut FileSwap) -> Result<()> {
        swap.apply(&self.old, &self.temp.path().join("staged"), &self.new)
    }

    fn read(&self, relative: &str) -> Option<String> {
        fs::read_to_string(self.app_dir().join(relative)).ok()
    }
}

fn entries(dir: &Path) -> Vec<FileEntry> {
    let list = fs::read_to_string(dir.join(FILE_LIST_NAME)).unwrap();
    FileEntry::parse_list(&list).unwrap()
}

#[test]
fn commit_keeps_new_files_and_drops_old_ones() {
    let f = Fixture::new();
    let mut swap = f.swap();
    f.apply(&mut swap).unwrap();
    swap.commit();

    assert_eq!(f.read("shared.dll").unwrap(), "shared.dll new");
    assert_eq!(f.read(r"bin\new.dll").unwrap(), r"bin\new.dll new");
    assert_eq!(f.read("old-only.dll"), None);
    assert!(!f.app_dir().join("lib").exists());
    assert!(!f.app_dir().join(BACKUP_DIR_NAME).exists());
}

#[test]
fn rollback_restores_old_files_and_removes_new_ones() {
    let f = Fixture::new();
    let mut swap = f.swap();
    f.apply(&mut swap).unwrap();

    assert!(swap.rollback());

    assert_eq!(f.read("shared.dll").unwrap(), "shared.dll old");
    assert_eq!(f.read(r"lib\old.dll").unwrap(), r"lib\old.dll old");
    assert_eq!(f.read("new-only.dll"), None);
    assert!(!f.app_dir().join("bin").exists());
    assert!(!f.app_dir().join(BACKUP_DIR_NAME).exists());
}

#[test]
fn rollback_removes_read_only_new_files() {
    let f = Fixture::new();
    let mut swap = f.swap();
    f.apply(&mut swap).unwrap();
    let path = f.app_dir().join("new-only.dll");
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();

    assert!(swap.rollback());
    assert_eq!(f.read("new-only.dll"), None);
}

#[test]
fn files_outside_both_lists_are_untouched() {
    let f = Fixture::new();
    fs::write(f.app_dir().join("mine.txt"), "mine").unwrap();
    let mut swap = f.swap();
    f.apply(&mut swap).unwrap();
    swap.commit();

    assert_eq!(f.read("mine.txt").unwrap(), "mine");
}

#[test]
fn user_file_with_a_new_files_name_is_backed_up_and_restored() {
    let f = Fixture::new();
    fs::create_dir_all(f.app_dir().join("bin")).unwrap();
    fs::write(f.app_dir().join(r"bin\new.dll"), "user's").unwrap();
    let mut swap = f.swap();
    f.apply(&mut swap).unwrap();
    assert_eq!(f.read(r"bin\new.dll").unwrap(), r"bin\new.dll new");

    assert!(swap.rollback());
    assert_eq!(f.read(r"bin\new.dll").unwrap(), "user's");
}

#[cfg(feature = "service")]
#[test]
fn leftover_backup_is_removed() {
    let f = Fixture::new();
    let backup = f.app_dir().join(BACKUP_DIR_NAME);
    fs::create_dir_all(backup.join("sub")).unwrap();
    fs::write(backup.join("sub").join("a.dll"), "x").unwrap();

    FileSwap::remove_leftover_backup(&f.app_dir()).unwrap();
    assert!(!backup.exists());
    // 没有残留时也不报错
    FileSwap::remove_leftover_backup(&f.app_dir()).unwrap();
}
