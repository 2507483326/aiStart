use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("文件操作失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("数据解析失败: {0}")]
    Json(#[from] serde_json::Error),
    #[error("数据库操作失败: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("网络请求失败: {0}")]
    Http(#[from] reqwest::Error),
    #[error("未找到: {0}")]
    NotFound(String),
    #[error("不支持的操作: {0}")]
    #[allow(dead_code)]
    Unsupported(String),
    #[error("配置无效: {0}")]
    InvalidConfig(String),
    #[error("{0}")]
    Message(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

impl From<&str> for AppError {
    fn from(value: &str) -> Self {
        AppError::Message(value.to_string())
    }
}

impl From<String> for AppError {
    fn from(value: String) -> Self {
        AppError::Message(value)
    }
}
