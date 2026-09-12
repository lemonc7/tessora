#[cfg(feature = "server")]
use dioxus::server::axum::Extension;
use dioxus::{fullstack::ServerEvents, prelude::*};
#[cfg(feature = "server")]
use tokio::sync::broadcast::error::RecvError;

use crate::contracts::{
    api::{CreateTaskRequest, DirectoryEntry},
    tasks::{DownloadTask, TaskEvent},
};

#[cfg(feature = "server")]
fn server_error(error: impl std::fmt::Display) -> ServerFnError {
    ServerFnError::new(error.to_string())
}

#[server(state: Extension<crate::backend::AppState>)]
pub async fn list_tasks() -> ServerFnResult<Vec<DownloadTask>> {
    state.manager.list_tasks().await.map_err(server_error)
}

#[get("/api/tasks/events", state: Extension<crate::backend::AppState>)]
pub async fn task_events() -> ServerFnResult<ServerEvents<TaskEvent>> {
    let mut events = state.manager.subscribe();
    let shutdown = state.manager.event_shutdown_token();

    Ok(ServerEvents::new(move |mut sender| async move {
        loop {
            let received = tokio::select! {
                _ = shutdown.cancelled() => break,
                received = events.recv() => received,
            };
            let event = match received {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) | Err(RecvError::Closed) => break,
            };
            if sender.send(event).await.is_err() {
                break;
            }
        }
    }))
}

#[server(state: Extension<crate::backend::AppState>)]
pub async fn create_task(request: CreateTaskRequest) -> ServerFnResult<DownloadTask> {
    state
        .manager
        .create_task(request)
        .await
        .map_err(server_error)
}

#[server(state: Extension<crate::backend::AppState>)]
pub async fn cancel_task(id: String) -> ServerFnResult<()> {
    state.manager.cancel_task(&id).await.map_err(server_error)
}

#[server(state: Extension<crate::backend::AppState>)]
pub async fn retry_task(id: String) -> ServerFnResult<()> {
    state.manager.retry_task(&id).await.map_err(server_error)
}

#[server(state: Extension<crate::backend::AppState>)]
pub async fn delete_task(id: String, delete_file: bool) -> ServerFnResult<()> {
    state
        .manager
        .delete_task(&id, delete_file)
        .await
        .map_err(server_error)
}

#[server(state: Extension<crate::backend::AppState>)]
pub async fn list_directories() -> ServerFnResult<Vec<DirectoryEntry>> {
    state.manager.list_directories().map_err(server_error)
}
