//! 按扩展名判断文件分类和图标；为下载生成安全、不重名的文件名。

use crate::html;
use crate::util::{eq_ignore_case, file_extension};

const DOCUMENT_EXTENSIONS: [&str; 14] = [
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".csv", ".ppt", ".pptx", ".txt", ".md", ".rtf",
    ".odt", ".ods", ".odp",
];
const ARCHIVE_EXTENSIONS: [&str; 8] = [".zip", ".rar", ".7z", ".tar", ".gz", ".tgz", ".bz2", ".xz"];

const MAX_ATTEMPTS: u32 = 9999;
const MAX_DIRECTORY_NAME_CHARS: usize = 60;

const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 文件列表“筛选”的分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Image,
    Video,
    Document,
    Archive,
    Other,
}

impl FileKind {
    pub fn of(path: &str) -> Self {
        let ext = file_extension(path);
        let contains = |list: &[&str]| list.iter().any(|e| eq_ignore_case(e, ext));
        if html::is_image_path(path) {
            Self::Image
        } else if html::is_video_path(path) {
            Self::Video
        } else if contains(&DOCUMENT_EXTENSIONS) {
            Self::Document
        } else if contains(&ARCHIVE_EXTENSIONS) {
            Self::Archive
        } else {
            Self::Other
        }
    }
}

/// 文件图标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileIcon {
    Pdf,
    Word,
    Excel,
    Powerpoint,
    Image,
    Video,
    Zip,
    File,
    Unknown,
}

impl FileIcon {
    pub fn of(path: &str) -> Self {
        match file_extension(path).to_lowercase().as_str() {
            ".pdf" => Self::Pdf,
            ".doc" | ".docx" | ".odt" | ".rtf" => Self::Word,
            ".xls" | ".xlsx" | ".csv" | ".ods" => Self::Excel,
            ".ppt" | ".pptx" | ".odp" => Self::Powerpoint,
            _ => match FileKind::of(path) {
                FileKind::Image => Self::Image,
                FileKind::Video => Self::Video,
                FileKind::Archive => Self::Zip,
                FileKind::Document => Self::File,
                FileKind::Other => Self::Unknown,
            },
        }
    }
}

/// 可用的文件名：原名可用就用原名，否则依次尝试 name(1).ext、name(2).ext……
/// `is_taken` 由调用方判断某个名字是否已被占用（已存在的文件或目录、正在进行的下载）；尝试 9999 次仍被占用时为 `None`。
pub fn next_free_name(file_name: &str, mut is_taken: impl FnMut(&str) -> bool) -> Option<String> {
    let cleaned = replace_invalid(file_name);
    let trimmed = cleaned.trim_end_matches([' ', '.']);
    let safe = if trimmed.is_empty() {
        "_".to_owned()
    } else {
        escape_reserved_name(trimmed)
    };
    if !is_taken(&safe) {
        return Some(safe);
    }
    let (name, ext) = safe.split_at(safe.rfind('.').unwrap_or(safe.len()));
    (1..=MAX_ATTEMPTS)
        .map(|i| format!("{name}({i}){ext}"))
        .find(|candidate| !is_taken(candidate))
}

/// 把任意文字转为可作为目录名的形式：替换非法字符、去掉首尾空格和点、截断到 60 个字符。
pub fn sanitize_directory_name(s: &str) -> String {
    let cleaned = replace_invalid(s);
    let truncated: String = cleaned
        .trim_matches([' ', '.'])
        .chars()
        .take(MAX_DIRECTORY_NAME_CHARS)
        .collect();
    let result = truncated.trim_end_matches([' ', '.']);
    if result.is_empty() {
        "_".to_owned()
    } else {
        escape_reserved_name(result)
    }
}

// Windows 保留的设备名（CON、AUX.zip 等，不区分大小写和扩展名）前加 “_”
fn escape_reserved_name(name: &str) -> String {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    if RESERVED_NAMES.iter().any(|r| eq_ignore_case(r, stem)) {
        format!("_{name}")
    } else {
        name.to_owned()
    }
}

// 文件可能同步到 Windows，所有平台都按 Windows 的规则替换
fn replace_invalid(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() || "\"<>|:*?\\/".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        for (path, expected) in [
            ("/user_uploads/2/ab/photo.PNG", FileKind::Image),
            ("/user_uploads/2/ab/clip.mp4", FileKind::Video),
            ("/user_uploads/2/ab/report.pdf", FileKind::Document),
            ("/user_uploads/2/ab/notes.txt", FileKind::Document),
            ("/user_uploads/2/ab/pack.7z", FileKind::Archive),
            ("/user_uploads/2/ab/setup.exe", FileKind::Other),
            ("/user_uploads/2/ab/noext", FileKind::Other),
        ] {
            assert_eq!(FileKind::of(path), expected, "{path}");
        }
    }

    #[test]
    fn icons() {
        for (path, expected) in [
            ("a.PDF", FileIcon::Pdf),
            ("a.docx", FileIcon::Word),
            ("a.csv", FileIcon::Excel),
            ("a.pptx", FileIcon::Powerpoint),
            ("a.zip", FileIcon::Zip),
            ("a.txt", FileIcon::File),
            ("a.png", FileIcon::Image),
            ("a.webm", FileIcon::Video),
            ("a.bin", FileIcon::Unknown),
        ] {
            assert_eq!(FileIcon::of(path), expected, "{path}");
        }
    }

    fn free(name: &str, taken: &[&str]) -> Option<String> {
        next_free_name(name, |candidate| taken.contains(&candidate))
    }

    #[test]
    fn free_names() {
        for (name, taken, expected) in [
            ("资料.zip", &[][..], "资料.zip"),
            ("a.txt", &["a.txt", "a(1).txt"][..], "a(2).txt"),
            ("b.png", &["b.png"][..], "b(1).png"),
            ("a/b:c.txt", &[][..], "a_b_c.txt"),
            ("README", &["README"][..], "README(1)"),
            ("CON.txt", &[][..], "_CON.txt"),
            ("aux.zip", &[][..], "_aux.zip"),
            ("com1", &[][..], "_com1"),
            ("console.txt", &[][..], "console.txt"),
        ] {
            assert_eq!(free(name, taken).as_deref(), Some(expected), "{name}");
        }
    }

    #[test]
    fn gives_up_after_all_attempts() {
        assert_eq!(next_free_name("a.txt", |_| true), None);
    }

    #[test]
    fn directory_names() {
        for (input, expected) in [
            ("开发", "开发"),
            (" .a:b. ", "a_b"),
            ("...", "_"),
            ("", "_"),
            ("localhost.localdomain:8443", "localhost.localdomain_8443"),
            ("NUL", "_NUL"),
        ] {
            assert_eq!(sanitize_directory_name(input), expected, "{input}");
        }
        assert_eq!(
            sanitize_directory_name(&"x".repeat(100)).chars().count(),
            60
        );
    }
}
