use std::io;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// Update package verification failed: corrupt content, tampering, or wrong structure.
    #[error("{0}")]
    PackageInvalid(String),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[cfg(feature = "service")]
    #[error("{0}")]
    Http(#[from] reqwest::Error),
    #[cfg(feature = "service")]
    #[error("更新清单内容不合法")]
    ManifestInvalid,
    #[cfg(feature = "service")]
    #[error("连接超时")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, UpdateError>;

pub(crate) fn invalid(message: impl Into<String>) -> UpdateError {
    UpdateError::PackageInvalid(message.into())
}
