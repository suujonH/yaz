use std::fs::{self, File};
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Result, invalid};
use crate::layout::{APP_EXE_NAME, BACKUP_DIR_NAME, FILE_LIST_NAME, UPDATER_EXE_NAME};

/// One entry of the file manifest: a relative path separated by `\` and a SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
}

impl FileEntry {
    /// Parses a manifest in the `sha256 relative path` format; returns an error if the format is
    /// wrong or a path points outside the directory.
    pub fn parse_list(text: &str) -> Result<Vec<FileEntry>> {
        let mut entries = Vec::new();
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let hash_end = line.find(' ').unwrap_or(0);
            if hash_end != 64 || !line[..hash_end].bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid(format!("文件清单格式不对：{line}")));
            }
            let path = line[hash_end + 1..]
                .trim()
                .trim_start_matches('*')
                .replace('/', "\\");
            if !is_safe_relative_path(&path) {
                return Err(invalid(format!("文件清单中的路径不合法：{path}")));
            }
            entries.push(FileEntry {
                path,
                sha256: line[..hash_end].to_string(),
            });
        }
        Ok(entries)
    }
}

// The manifest decides which files are replaced and deleted: paths must not be absolute,
// drive-letter paths or contain .., and must not point into the backup directory
fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.contains(':') {
        return false;
    }
    let first = path.split('\\').next().unwrap_or_default();
    !first.eq_ignore_ascii_case(BACKUP_DIR_NAME)
        && !path.split('\\').any(|part| matches!(part, "" | "." | ".."))
}

pub(crate) fn hash_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verifies the extracted directory: it must have the file manifest, the main app and the updater,
/// and every file in the manifest must exist with a matching SHA-256. Returns the manifest.
pub fn verify_tree(dir: &Path) -> Result<Vec<FileEntry>> {
    let list_path = dir.join(FILE_LIST_NAME);
    if !list_path.is_file() {
        return Err(invalid(format!("更新包缺少文件清单 {FILE_LIST_NAME}")));
    }
    let entries = FileEntry::parse_list(&fs::read_to_string(list_path)?)?;
    for required in [APP_EXE_NAME, UPDATER_EXE_NAME] {
        if !entries
            .iter()
            .any(|e| e.path.eq_ignore_ascii_case(required))
        {
            return Err(invalid(format!("更新包缺少 {required}")));
        }
    }
    verify_files(dir, &entries)?;
    Ok(entries)
}

/// Every file in the manifest exists and has a matching SHA-256.
pub fn verify_files(dir: &Path, entries: &[FileEntry]) -> Result<()> {
    for entry in entries {
        let path = dir.join(&entry.path);
        if !path.is_file() {
            return Err(invalid(format!("缺少文件：{}", entry.path)));
        }
        if !hash_file(&path)?.eq_ignore_ascii_case(&entry.sha256) {
            return Err(invalid(format!("文件校验失败：{}", entry.path)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::error::UpdateError;
    use crate::testing::{version_files, write_tree};

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn is_invalid<T>(result: Result<T>) -> bool {
        matches!(result, Err(UpdateError::PackageInvalid(_)))
    }

    #[test]
    fn tree_requires_list_app_and_updater() {
        let temp = TempDir::new().unwrap();
        assert!(is_invalid(verify_tree(temp.path())));

        let only_updater = [(UPDATER_EXE_NAME.to_string(), "x".to_string())].into();
        write_tree(temp.path(), &only_updater);
        assert!(is_invalid(verify_tree(temp.path())));
    }

    #[test]
    fn tree_detects_missing_and_modified_files() {
        let temp = TempDir::new().unwrap();
        write_tree(temp.path(), &version_files("new", &[r"plugins\a\a.dll"]));
        assert_eq!(verify_tree(temp.path()).unwrap().len(), 3);

        let dll = temp.path().join(r"plugins\a\a.dll");
        fs::write(&dll, "tampered").unwrap();
        assert!(is_invalid(verify_tree(temp.path())));

        fs::remove_file(&dll).unwrap();
        assert!(is_invalid(verify_tree(temp.path())));
    }

    #[test]
    fn file_list_rejects_unsafe_paths() {
        for path in [
            "../evil.dll",
            r"..\evil.dll",
            r"a\..\..\evil.dll",
            r"C:\Windows\evil.dll",
            "C:evil.dll",
            r"\evil.dll",
            r".update-backup\x.dll",
        ] {
            assert!(
                FileEntry::parse_list(&format!("{HASH}  {path}")).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn file_list_rejects_malformed_lines() {
        for line in [
            "short  a.dll",
            "a.dll",
            &format!("{}  a.dll", "g".repeat(64)),
        ] {
            assert!(FileEntry::parse_list(line).is_err(), "{line}");
        }
    }

    #[test]
    fn file_list_normalizes_slashes_and_skips_blank_lines() {
        let text = format!("\n{HASH}  plugins/a/a.dll\n  \n{HASH} *b.dll\n");
        let entries = FileEntry::parse_list(&text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, r"plugins\a\a.dll");
        assert_eq!(entries[1].path, "b.dll");
    }
}
