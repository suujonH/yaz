use zulip_api::ApiError;
use zulip_store::StoreError;

/// 会话层的错误。`to_string()` 可以直接显示给用户。
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Api(Box<ApiError>),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// 输入无效或操作不允许，内容是给用户看的说明。
    #[error("{0}")]
    Invalid(String),
    #[error("服务器版本过低，需要 Zulip 11.0 及以上")]
    ServerTooOld,
    /// 话题中部分消息超出移动时限。确认后用 `Session::move_topic_from` 只移动允许的部分。
    #[error("话题共 {total} 条消息，只有最近 {allowed} 条可以移动")]
    TopicMoveLimited {
        allowed: i64,
        total: i64,
        first_allowed_id: i64,
    },
}

impl From<ApiError> for SessionError {
    fn from(error: ApiError) -> Self {
        Self::Api(Box::new(error))
    }
}

impl SessionError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub fn api_code(&self) -> Option<&str> {
        match self {
            Self::Api(error) => error.code(),
            _ => None,
        }
    }

    pub fn is_auth_error(&self) -> bool {
        matches!(self, Self::Api(error) if error.is_auth_error())
    }
}

pub type Result<T, E = SessionError> = std::result::Result<T, E>;

/// 登录失败时显示的文字。
pub fn describe_login_error(error: &SessionError) -> String {
    match error {
        SessionError::Api(api) => describe_api_error(api),
        SessionError::Store(StoreError::Crypto(_)) => {
            "保存的登录信息已失效，请重新输入密码".to_owned()
        }
        SessionError::Invalid(_) | SessionError::ServerTooOld => error.to_string(),
        _ => format!("登录失败：{error}"),
    }
}

fn describe_api_error(error: &ApiError) -> String {
    match error.code() {
        Some("AUTHENTICATION_FAILED") => return "邮箱或密码错误".to_owned(),
        Some("PASSWORD_AUTH_DISABLED") => return "该组织未开启密码登录".to_owned(),
        Some("USER_DEACTIVATED") => return "该账号已被停用".to_owned(),
        Some("REALM_DEACTIVATED") => return "该组织已被停用".to_owned(),
        _ => {}
    }
    if error.is_auth_error() {
        return "登录信息已失效，请重新输入密码".to_owned();
    }
    match error {
        ApiError::Api { message, .. } => format!("服务器返回错误：{message}"),
        ApiError::Timeout(_) => "连接服务器超时".to_owned(),
        ApiError::Network(message) if message.to_lowercase().contains("certificate") => {
            "证书无效，若为测试服务器请在高级中允许不受信任的证书".to_owned()
        }
        ApiError::Network(message) => format!("无法连接服务器：{message}"),
        other => format!("登录失败：{other}"),
    }
}
