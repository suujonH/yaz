//! 系统文件选择框。父窗口为当前窗口，只能在组件上下文中调用。

use dioxus::desktop::window;
use rfd::AsyncFileDialog;
use std::path::PathBuf;

/// 文件类型筛选项。
#[derive(Clone, Copy, Debug)]
pub struct FileFilter<'a> {
    pub name: &'a str,
    pub extensions: &'a [&'a str],
}

/// 图片文件的筛选项。
pub const IMAGE_FILTER: FileFilter<'static> = FileFilter {
    name: "图片",
    extensions: &zulip_core::html::IMAGE_EXTENSIONS,
};

fn dialog(title: &str, filters: &[FileFilter<'_>]) -> AsyncFileDialog {
    let current = window();
    let mut dialog = AsyncFileDialog::new()
        .set_title(title)
        .set_parent(&current.window);
    for filter in filters {
        dialog = dialog.add_filter(filter.name, filter.extensions);
    }
    dialog
}

pub async fn pick_file(title: &str, filters: &[FileFilter<'_>]) -> Option<PathBuf> {
    dialog(title, filters)
        .pick_file()
        .await
        .map(|f| f.path().to_path_buf())
}

pub async fn pick_files(title: &str, filters: &[FileFilter<'_>]) -> Vec<PathBuf> {
    dialog(title, filters)
        .pick_files()
        .await
        .map(|files| files.iter().map(|f| f.path().to_path_buf()).collect())
        .unwrap_or_default()
}

pub async fn pick_folder(title: &str) -> Option<PathBuf> {
    dialog(title, &[])
        .pick_folder()
        .await
        .map(|f| f.path().to_path_buf())
}

pub async fn save_file(
    title: &str,
    default_name: &str,
    filters: &[FileFilter<'_>],
) -> Option<PathBuf> {
    dialog(title, filters)
        .set_file_name(default_name)
        .save_file()
        .await
        .map(|f| f.path().to_path_buf())
}
