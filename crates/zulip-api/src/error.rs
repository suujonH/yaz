use serde_json::Value;

/// Calling a Zulip endpoint failed.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("网络错误：{0}")]
    Network(String),

    #[error("请求超时：{0}")]
    Timeout(String),

    /// The server returned an error: HTTP status non-2xx, `result` is `error`, or the response is
    /// not JSON.
    #[error("{message}")]
    Api {
        status: u16,
        /// Zulip error code; `BAD_REQUEST` when absent from the response, `HTTP_<status code>` for
        /// non-JSON responses.
        code: String,
        message: String,
        /// The full error response. Rarely used; boxed to keep the error type small.
        body: Option<Box<Value>>,
    },

    #[error("无法解析服务器响应：{0}")]
    Decode(String),

    #[error("请求无效：{0}")]
    InvalidInput(String),

    /// Reading or writing a local file failed (source of an upload, target of a download).
    #[error("文件读写失败：{0}")]
    Io(#[from] std::io::Error),

    /// The connection was cancelled by [`ZulipClient::close`](crate::ZulipClient::close) or the
    /// `with_cancel` token.
    #[error("已取消")]
    Cancelled,
}

impl ApiError {
    pub(crate) fn api(
        status: u16,
        code: &str,
        message: impl Into<String>,
        body: Option<Value>,
    ) -> Self {
        let code = if code.is_empty() { "BAD_REQUEST" } else { code };
        Self::Api {
            status,
            code: code.to_owned(),
            message: message.into(),
            body: body.map(Box::new),
        }
    }

    pub(crate) fn from_reqwest(error: reqwest::Error, operation: &str) -> Self {
        if error.is_timeout() {
            return Self::Timeout(operation.to_owned());
        }
        let mut text = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            text.push('：');
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        Self::Network(text)
    }

    /// Error code returned by the server.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Api { code, .. } => Some(code),
            _ => None,
        }
    }

    pub fn is_bad_event_queue(&self) -> bool {
        self.code() == Some("BAD_EVENT_QUEUE_ID")
    }

    pub fn is_auth_error(&self) -> bool {
        matches!(self, Self::Api { status: 401, .. })
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}
