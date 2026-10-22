mod app;
pub mod components;
pub mod config;
pub mod domain;
pub mod download;
pub mod error;
pub mod logging;
pub mod services;

use std::sync::Arc;

use crate::{config::Config, services::TaskManager};

async fn run() -> topcoat::Result<()> {
    // 加载配置
    let config = Config::from_env()?;
    tracing::info!(
        download_root = %config.download_root.display(),
        temporary_root = %config.temporary_root.display(),
        max_concurrent_downloads = config.max_concurrent_downloads,
        concurrent_fragments = config.concurrent_fragments,
        "starting tessora"
    );
    // 任务管理器
    let manager = TaskManager::new(config);

    // 初始化扫描目录
    let scan = manager.initialize().await?;
    tracing::info!(
        media_files = scan.files,
        skipped_entries = scan.skipped,
        "media catalog initialized"
    );

    // 启动服务器
    let result = topcoat::start(app::router(app::AppState {
        manager: Arc::clone(&manager),
    }))
    .await;

    // 关闭管理器
    manager.shutdown().await;
    tracing::info!("tessora stopped");
    Ok(result?)
}

#[tokio::main]
async fn main() {
    if let Err(error) = logging::init() {
        tracing::error!(error = %error, "logging configuration is invalid");
        std::process::exit(1);
    }
    if let Err(error) = run().await {
        tracing::error!(error = %error, "tessora exited with an error");
        std::process::exit(1);
    }
}
