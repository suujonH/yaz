//! 文件：上传与下载附件。

use std::ffi::c_char;
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use zulip_plugin_sdk::abi::{self, Ctx, ProgressCb, StringCb, UserData};

use super::{Call, Fail, invalid, run32, send_string};
use crate::marshal::read_text;
use crate::permission;
use crate::request;

pub(super) unsafe extern "C" fn upload_file(
    ctx: Ctx,
    local_path: *const c_char,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let path = unsafe { read_text(local_path) };
    run32(
        ctx,
        "upload_file",
        Some(permission::UPLOAD_FILE),
        Box::new(move |call| upload(call, path, None, cb, user)),
    )
}

// 进度回调：上传时在其他线程上调用，函数返回后不再调用（user 可能已经失效）。
struct ProgressSink {
    cb: Option<ProgressCb>,
    user: usize,
    open: Mutex<bool>,
}

impl ProgressSink {
    fn new(cb: Option<ProgressCb>, user: UserData) -> Self {
        Self {
            cb,
            user: user as usize,
            open: Mutex::new(true),
        }
    }

    fn report(&self, done: u64, total: u64) {
        let open = self.open.lock();
        if let (true, Some(cb)) = (*open, self.cb) {
            // SAFETY: 回调由插件提供；user 在宿主函数返回前有效，close 之后不再使用。
            unsafe { cb(done as i64, total as i64, self.user as UserData) };
        }
    }

    fn close(&self) {
        *self.open.lock() = false;
    }
}

fn upload(
    call: &Call<'_>,
    path: Option<String>,
    progress: Option<ProgressCb>,
    cb: Option<StringCb>,
    user: UserData,
) -> Result<i64, Fail> {
    let path = path.filter(|p| !p.is_empty());
    let (Some(cb), Some(path)) = (cb, path) else {
        return invalid();
    };
    let path = Path::new(&path);
    if !path.is_file() {
        return Err(Fail::Code(abi::ERR_NOT_FOUND));
    }
    let total = std::fs::metadata(path).map_or(0, |m| m.len());
    let sink = Arc::new(ProgressSink::new(progress, user));
    let reporter = sink.clone();
    let result = call.wait(
        call.session()?
            .upload_file(path, move |done| reporter.report(done, total)),
    );
    sink.close();
    send_string(cb, &result?.url, user);
    Ok(0)
}

pub(super) unsafe extern "C" fn download_attachment(
    ctx: Ctx,
    upload_path: *const c_char,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let path = unsafe { read_text(upload_path) };
    run32(
        ctx,
        "download_attachment",
        Some(permission::READ_ATTACHMENT),
        Box::new(move |call| {
            let (Some(cb), Some(path)) = (cb, path.as_deref().and_then(request::upload_path))
            else {
                return invalid();
            };
            let local = call.wait(call.session()?.download_to_cache(&path, |_, _| {}))?;
            send_string(cb, &local.to_string_lossy(), user);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn download_attachment_ex(
    ctx: Ctx,
    upload_path: *const c_char,
    progress: Option<ProgressCb>,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let path = unsafe { read_text(upload_path) };
    run32(
        ctx,
        "download_attachment_ex",
        Some(permission::READ_ATTACHMENT),
        Box::new(move |call| {
            let (Some(cb), Some(path)) = (cb, path.as_deref().and_then(request::upload_path))
            else {
                return invalid();
            };
            let sink = ProgressSink::new(progress, user);
            let local = call.wait(call.session()?.download_to_cache(&path, |done, total| {
                sink.report(done, total.unwrap_or_default());
            }))?;
            sink.close();
            send_string(cb, &local.to_string_lossy(), user);
            Ok(0)
        }),
    )
}

pub(super) unsafe extern "C" fn upload_file_ex(
    ctx: Ctx,
    local_path: *const c_char,
    progress: Option<ProgressCb>,
    cb: Option<StringCb>,
    user: UserData,
) -> i32 {
    // SAFETY: 插件传入以 0 结尾的字符串。
    let path = unsafe { read_text(local_path) };
    run32(
        ctx,
        "upload_file_ex",
        Some(permission::UPLOAD_FILE),
        Box::new(move |call| upload(call, path, progress, cb, user)),
    )
}
