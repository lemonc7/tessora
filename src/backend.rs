mod config;
mod db;
mod downloader;
mod error;
mod events;
mod media;
mod service;
mod task;

use std::sync::Arc;

use config::Config;
use db::TaskRepository;
use dioxus::logger::tracing;
use downloader::cleanup_stale_temporary_files;
use error::Result;
use service::TaskManager;

#[derive(Clone)]
pub(crate) struct AppState {
    pub manager: Arc<TaskManager>,
}

pub async fn run() -> Result<()> {
    let config = Arc::new(Config::from_env()?);
    let removed_files = cleanup_stale_temporary_files(&config.download_root)?;
    if removed_files > 0 {
        tracing::info!(removed_files, "removed stale temporary downloads");
    }
    let repository = Arc::new(TaskRepository::open(&config.database_path).await?);
    repository.mark_active_tasks_interrupted().await?;
    let manager = TaskManager::new(Arc::clone(&repository), config);
    let state = AppState {
        manager: Arc::clone(&manager),
    };

    let result = serve(state).await;
    manager.shutdown().await;
    tracing::info!("closing database pool");
    repository.close().await;
    tracing::info!("database pool closed");
    result
}

async fn serve(state: AppState) -> Result<()> {
    let address = dioxus::cli_config::fullstack_address_or_localhost();
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "HTTP server started");

    let shutdown_manager = Arc::clone(&state.manager);
    let router =
        dioxus::server::router(crate::app::App).layer(dioxus::server::axum::Extension(state));
    dioxus::server::axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal(shutdown_manager))
        .await?;

    tracing::info!("HTTP server stopped");
    Ok(())
}

async fn shutdown_signal(manager: Arc<TaskManager>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    tracing::error!(%error, "failed to listen for Ctrl+C");
                }
            }
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to listen for Ctrl+C");
    }

    manager.close_event_streams();
    tracing::info!("shutdown signal received; stopping new requests");
}
