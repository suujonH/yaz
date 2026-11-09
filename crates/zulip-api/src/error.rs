use serde_json::Value;

/// 调用 Zulip 接口失败。
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("网络错误：{0}")]
    Network(String),

    #[error("请求超时：{0}")]
    Timeout(String),

    /// 服务器返回错误：HTTP 状态码非 2xx、`result` 为 `error`，或响应不是 JSON。
    #[error("{message}")]
    Api {
        status: u16,
        /// Zulip 的错误码；响应中没有时为 `BAD_REQUEST`，非 JSON 响应为 `HTTP_状态码`。
        code: String,
        message: String,
        /// 完整的错误响应。很少用到，装箱以免错误类型过大。
        body: Option<Box<Value>>,
    },

    #[error("无法解析服务器响应：{0}")]
    Decode(String),

    #[error("请求无效：{0}")]
    InvalidInput(String),

    /// 读写本地文件失败（上传的来源、下载的目标）。
    #[error("文件读写失败：{0}")]
    Io(#[from] std::io::Error),

    /// 连接被 [`ZulipClient::close`](crate::ZulipClient::close) 或 `with_cancel` 的令牌取消。
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

    /// 服务器返回的错误码。
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
