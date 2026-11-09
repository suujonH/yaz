use std::fs::{self, File};
use std::io;
use std::path::Path;

use zip::ZipArchive;
use zip::result::ZipError;

use crate::error::{Result, UpdateError, invalid};
use crate::manifest::UpdateManifest;
use crate::package::hash_file;

/// 校验更新包的大小和 SHA-256。
pub fn verify_archive(zip_path: &Path, manifest: &UpdateManifest) -> Result<()> {
    let length = fs::metadata(zip_path)?.len();
    if length != manifest.size {
        return Err(invalid(format!(
            "更新包大小不符：应为 {}，实际 {length}",
            manifest.size
        )));
    }
    let actual = hash_file(zip_path)?;
    if !actual.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(invalid(format!(
            "更新包 SHA-256 不符：应为 {}，实际 {actual}",
            manifest.sha256
        )));
    }
    Ok(())
}

/// 解压到目录；条目指向目录之外时报错。
pub fn extract_safely(zip_path: &Path, target_dir: &Path) -> Result<()> {
    fs::create_dir_all(target_dir)?;
    let mut archive = ZipArchive::new(File::open(zip_path)?).map_err(zip_error)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_error)?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(invalid(format!("更新包中的路径不合法：{}", entry.name())));
        };
        let path = target_dir.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        io::copy(&mut entry, &mut File::create(&path)?)?;
    }
    Ok(())
}

fn zip_error(error: ZipError) -> UpdateError {
    match error {
        ZipError::Io(error) => UpdateError::Io(error),
        error => invalid(format!("更新包无法解压：{error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::package::verify_tree;
    use crate::testing::{version_files, zip_package};

    fn is_invalid<T>(result: Result<T>) -> bool {
        matches!(result, Err(UpdateError::PackageInvalid(_)))
    }

    #[test]
    fn archive_with_matching_hash_passes_and_any_change_fails() {
        let temp = TempDir::new().unwrap();
        let (mut zip, manifest) = zip_package("1.0.0", &version_files("new", &[]));
        let path = temp.path().join("p.zip");
        fs::write(&path, &zip).unwrap();
        verify_archive(&path, &manifest).unwrap();

        *zip.last_mut().unwrap() ^= 0xFF;
        fs::write(&path, &zip).unwrap();
        assert!(is_invalid(verify_archive(&path, &manifest)));

        zip.pop();
        fs::write(&path, &zip).unwrap();
        assert!(is_invalid(verify_archive(&path, &manifest)));
    }

    #[test]
    fn extract_unpacks_nested_files() {
        let temp = TempDir::new().unwrap();
        let files = version_files("new", &[r"plugins\a\a.dll"]);
        let (zip, _) = zip_package("1.0.0", &files);
        let zip_path = temp.path().join("p.zip");
        fs::write(&zip_path, zip).unwrap();

        let out = temp.path().join("out");
        extract_safely(&zip_path, &out).unwrap();

        assert_eq!(verify_tree(&out).unwrap().len(), 3);
        assert_eq!(
            fs::read_to_string(out.join(r"plugins\a\a.dll")).unwrap(),
            r"plugins\a\a.dll new"
        );
    }

    #[test]
    fn extract_rejects_entries_outside_target() {
        let temp = TempDir::new().unwrap();
        let zip_path = temp.path().join("evil.zip");
        let mut writer = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        writer
            .start_file("../outside.txt", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"x").unwrap();
        writer.finish().unwrap();

        let out = temp.path().join("out");
        assert!(is_invalid(extract_safely(&zip_path, &out)));
        assert!(!temp.path().join("outside.txt").exists());
    }

    #[test]
    fn extract_rejects_non_zip_content() {
        let temp = TempDir::new().unwrap();
        let zip_path = temp.path().join("bad.zip");
        fs::write(&zip_path, "this is not a zip").unwrap();
        assert!(is_invalid(extract_safely(
            &zip_path,
            &temp.path().join("out")
        )));
    }
}
