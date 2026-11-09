#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("数据库已关闭")]
    Closed,

    #[error("数据库错误：{0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("文件读写失败：{0}")]
    Io(#[from] std::io::Error),

    /// DPAPI 加解密失败，例如数据不是当前 Windows 用户加密的。
    #[error("加解密失败：{0}")]
    Crypto(String),

    #[error("配置文件无效：{0}")]
    Config(#[from] serde_json::Error),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;
