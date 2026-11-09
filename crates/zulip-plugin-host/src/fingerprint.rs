//! Fingerprint of a plugin directory: SHA-256 over the relative paths, lengths and contents of all
//! files in the directory.

use std::fs;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Directory fingerprint (lowercase hex); returns an error if reading fails, and such a plugin
/// cannot be enabled.
pub fn compute(directory: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    collect(directory, "", &mut files)?;
    files.sort();
    let mut hash = Sha256::new();
    for relative in files {
        let content = fs::read(directory.join(&relative))?;
        hash.update(relative.as_bytes());
        hash.update([0u8]);
        hash.update((content.len() as i64).to_le_bytes());
        hash.update(&content);
    }
    Ok(hex::encode(hash.finalize()))
}

fn collect(directory: &Path, prefix: &str, files: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type()?.is_dir() {
            collect(&entry.path(), &relative, files)?;
        } else {
            files.push(relative);
        }
    }
    Ok(())
}

/// Short form for display: 4 groups of 4 hex digits.
pub fn short(fingerprint: &str) -> String {
    let head: Vec<char> = fingerprint.chars().take(16).collect();
    head.chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_with_content_and_names() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.dll"), "x").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("b.json"), "y").unwrap();
        let first = compute(dir.path()).unwrap();
        assert_eq!(first, compute(dir.path()).unwrap());
        assert_eq!(first.len(), 64);

        fs::write(dir.path().join("a.dll"), "z").unwrap();
        let second = compute(dir.path()).unwrap();
        assert_ne!(first, second);

        fs::rename(dir.path().join("a.dll"), dir.path().join("c.dll")).unwrap();
        assert_ne!(second, compute(dir.path()).unwrap());
        assert!(compute(&dir.path().join("没有")).is_err());
    }

    #[test]
    fn short_form() {
        assert_eq!(short("0123456789abcdef0123"), "0123 4567 89ab cdef");
    }
}
