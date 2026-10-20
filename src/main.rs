mod app;
pub mod components;

use std::sync::Arc;

use tessora::{config::Config, logging, services::TaskManager};

async fn run() -> topcoat::Result<()> {
    let config = Config::from_env()?;
    tracing::info!(
        download_root = %config.download_root.display(),
        temporary_root = %config.temporary_root.display(),
        max_concurrent_downloads = config.max_concurrent_downloads,
        concurrent_fragments = config.concurrent_fragments,
        "starting tessora"
    );
    let manager = TaskManager::new(config);

    let scan = manager.initialize().await?;
    tracing::info!(
        media_files = scan.files,
        skipped_entries = scan.skipped,
        "media catalog initialized"
    );

    let result = topcoat::start(app::router(app::AppState {
        manager: Arc::clone(&manager),
    }))
    .await;

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
