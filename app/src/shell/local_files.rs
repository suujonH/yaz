//! 让 WebView 读取本机文件：只有显式登记的路径才能访问，消息内容里的地址读不到其它文件。

use dioxus::desktop::wry::RequestAsyncResponder;
use dioxus::desktop::wry::http::{Request, Response};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// 自定义协议名（WebView2 中地址为 `http://zulip-file.localhost/...`）。
pub const SCHEME: &str = "zulip-file";

static ALLOWED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

fn encode(path: &Path) -> String {
    utf8_percent_encode(&path.to_string_lossy(), NON_ALPHANUMERIC).to_string()
}

fn decode(url_path: &str) -> PathBuf {
    let raw = url_path.trim_start_matches('/');
    PathBuf::from(percent_decode_str(raw).decode_utf8_lossy().as_ref())
}

/// 登记文件并返回可在 `img`、CSS 中使用的地址。
pub fn url_for(path: &Path) -> Option<String> {
    ALLOWED.lock().ok()?.insert(path.to_path_buf());
    Some(format!("http://{SCHEME}.localhost/{}", encode(path)))
}

/// 协议处理函数：在后台线程读文件。
pub fn handle(request: Request<Vec<u8>>, responder: RequestAsyncResponder) {
    let path = decode(request.uri().path());
    let allowed = ALLOWED.lock().is_ok_and(|set| set.contains(&path));
    std::thread::spawn(move || {
        let response = match allowed.then(|| std::fs::read(&path)) {
            Some(Ok(bytes)) => Response::builder()
                .header(
                    "Content-Type",
                    mime_guess::from_path(&path)
                        .first_or_octet_stream()
                        .as_ref(),
                )
                .header("Access-Control-Allow-Origin", "*")
                .body(Cow::Owned(bytes)),
            _ => Response::builder()
                .status(404)
                .body(Cow::Borrowed(&b""[..])),
        };
        if let Ok(response) = response {
            responder.respond(response);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_round_trips_paths_with_spaces_and_unicode() {
        let path = Path::new(r"C:\图片 1\a b.png");
        let url = url_for(path).unwrap();
        let tail = url.strip_prefix("http://zulip-file.localhost").unwrap();
        assert_eq!(decode(tail), path);
        assert!(!tail.contains(' '));
    }
}
