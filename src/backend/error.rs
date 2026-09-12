use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("参数错误：{0}")]
    Validation(String),
    #[error("任务不存在")]
    NotFound,
    #[error("任务当前状态不允许此操作")]
    InvalidState,
    #[error("配置错误：{0}")]
    Config(String),
    #[error("数据库错误：{0}")]
    Database(#[from] sqlx::Error),
    #[error("数据库迁移错误：{0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("文件系统错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Internal(String),
}
