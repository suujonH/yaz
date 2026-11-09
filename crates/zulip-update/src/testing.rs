use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::layout::{APP_EXE_NAME, FILE_LIST_NAME, UPDATER_EXE_NAME};
#[cfg(feature = "service")]
use crate::manifest::UpdateManifest;

/// Relative path (separated by `\`) to file content.
pub(crate) type Files = BTreeMap<String, String>;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Same format as tools/pack-release.ps1
fn file_list(files: &Files) -> String {
    files
        .iter()
        .map(|(path, content)| format!("{}  {path}\n", sha256_hex(content.as_bytes())))
        .collect()
}

/// Writes files into a directory and generates the file manifest.
pub(crate) fn write_tree(dir: &Path, files: &Files) {
    for (path, content) in files {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(FILE_LIST_NAME), file_list(files)).unwrap();
}

/// The main app, the updater and the files in `extra`, with `marker` in the content to tell
/// versions apart.
pub(crate) fn version_files(marker: &str, extra: &[&str]) -> Files {
    let mut files = Files::new();
    files.insert(APP_EXE_NAME.into(), format!("app {marker}"));
    files.insert(UPDATER_EXE_NAME.into(), format!("updater {marker}"));
    for path in extra {
        files.insert((*path).into(), format!("{path} {marker}"));
    }
    files
}

/// Builds an update package; returns the zip's content and the matching manifest.
#[cfg(feature = "service")]
pub(crate) fn zip_package(version: &str, files: &Files) -> (Vec<u8>, UpdateManifest) {
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for (path, content) in files {
        writer.start_file(path.replace('\\', "/"), options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.start_file(FILE_LIST_NAME, options).unwrap();
    writer.write_all(file_list(files).as_bytes()).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let manifest = UpdateManifest {
        version: version.into(),
        package: format!("YaZ-v{version}-win-x64.zip"),
        sha256: sha256_hex(&bytes),
        size: bytes.len() as u64,
    };
    (bytes, manifest)
}
