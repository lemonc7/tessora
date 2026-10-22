use std::{
    collections::HashMap,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::UNIX_EPOCH,
};

use tokio::{
    sync::{Mutex, Semaphore},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    domain::{
        CreateTask, DownloadTask, MediaFile, ScanReport, TaskEvent, TaskProgress, TaskStatus,
    },
    download::{
        FfmpegRunner, HlsDownloader, RunOutcome, cleanup_stale_temporary_files, default_file_name,
        normalize_file_name, normalize_relative_directory, now_timestamp, resolved_directory,
        temporary_output, validate_url,
    },
    error::{AppError, Result},
    services::{MediaCatalog, TaskEvents, TaskStore},
};

pub struct TaskManager {
    store: Arc<TaskStore>,
    media: Arc<MediaCatalog>,
    config: Arc<Config>,
    semaphore: Arc<Semaphore>,
    cancellations: Mutex<HashMap<String, Cancellation>>,
    progress: RwLock<HashMap<String, TaskProgress>>,
    workers: Mutex<WorkerState>,
    events: TaskEvents,
}

struct Cancellation {
    generation: u64,
    token: CancellationToken,
}

#[derive(Default)]
struct WorkerState {
    shutting_down: bool,
    next_generation: u64,
    workers: JoinSet<()>,
}

impl TaskManager {
    pub fn new(config: Config) -> Arc<Self> {
        let config = Arc::new(config);
        Arc::new(Self {
            store: Arc::new(TaskStore::default()),
            media: Arc::new(MediaCatalog::new(
                config.download_root.clone(),
                config.media_settle_seconds,
            )),
            semaphore: Arc::new(Semaphore::new(config.max_concurrent_downloads)),
            config,
            cancellations: Mutex::new(HashMap::new()),
            progress: RwLock::new(HashMap::new()),
            workers: Mutex::new(WorkerState::default()),
            events: TaskEvents::default(),
        })
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
    }

    pub async fn initialize(&self) -> Result<ScanReport> {
        let temporary_files_removed = cleanup_stale_temporary_files(&self.config.temporary_root)?;
        tracing::info!(temporary_files_removed, "stale temporary files cleaned");
        self.media.scan().await
    }

    pub async fn shutdown(&self) {
        let mut state = self.workers.lock().await;
        if state.shutting_down {
            return;
        }
        state.shutting_down = true;

        let cancellations = {
            let mut active = self.cancellations.lock().await;
            std::mem::take(&mut *active)
        };
        for cancellation in cancellations.into_values() {
            cancellation.token.cancel();
        }

        while let Some(result) = state.workers.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error, "download task ended unexpectedly during shutdown");
            }
        }
    }

    pub async fn list_tasks(&self) -> Result<Vec<DownloadTask>> {
        let mut tasks = self.store.list().await;
        let progress = self
            .progress
            .read()
            .map_err(|_| AppError::Internal("progress lock is poisoned".into()))?;
        for task in &mut tasks {
            task.progress = progress.get(&task.id).cloned();
        }
        Ok(tasks)
    }

    pub async fn list_media_files(&self) -> Vec<MediaFile> {
        self.media.list().await
    }

    pub async fn search_media_files(&self, query: &str) -> Vec<MediaFile> {
        self.media.search(query).await
    }

    pub async fn scan_media_files(&self) -> Result<ScanReport> {
        let report = self.media.scan().await?;
        self.events.media_changed();
        tracing::info!(
            media_files = report.files,
            skipped_entries = report.skipped,
            "media catalog scanned"
        );
        Ok(report)
    }

    pub async fn create_task(self: &Arc<Self>, request: CreateTask) -> Result<DownloadTask> {
        let url = validate_url(&request.url)?;
        let relative_directory = normalize_relative_directory(&request.directory)?;
        let output_directory = resolved_directory(&self.config.download_root, &relative_directory)?;
        let file_name = if request.file_name.trim().is_empty() {
            normalize_file_name(&default_file_name(&request.url))?
        } else {
            normalize_file_name(&request.file_name)?
        };
        if output_directory.join(&file_name).exists() {
            return Err(AppError::Validation(
                "目标文件已经存在，请更换文件名".into(),
            ));
        }

        let mut workers = self.workers.lock().await;
        if workers.shutting_down {
            return Err(AppError::InvalidState);
        }

        let task = DownloadTask {
            id: uuid::Uuid::new_v4().to_string(),
            url,
            directory: path_for_storage(&relative_directory),
            file_name,
            status: TaskStatus::Queued,
            progress: None,
            error: None,
            created_at: now_timestamp(),
            started_at: None,
            finished_at: None,
        };
        self.store.reserve_and_insert(task.clone()).await?;
        self.events.changed(&task.id);
        tracing::info!(
            task_id = %task.id,
            directory = %task.directory,
            file_name = %task.file_name,
            "download task created"
        );
        self.enqueue_locked(task.id.clone(), &mut workers).await;
        Ok(task)
    }

    pub async fn cancel_task(&self, id: &str) -> Result<()> {
        let task = self.store.get(id).await?;
        if !task.status.is_active() {
            return Err(AppError::InvalidState);
        }
        let cancellation = self.cancellations.lock().await.remove(id);
        let Some(cancellation) = cancellation else {
            return Err(AppError::InvalidState);
        };
        cancellation.token.cancel();
        tracing::info!(task_id = id, "download task cancellation requested");
        Ok(())
    }

    pub async fn retry_task(self: &Arc<Self>, id: &str) -> Result<()> {
        let mut workers = self.workers.lock().await;
        if workers.shutting_down {
            return Err(AppError::InvalidState);
        }

        let task = self.store.reset_for_retry(id).await?;
        let preparation = async {
            let (_, temporary) = self.output_paths(&task)?;
            remove_file_if_exists(&temporary).await
        }
        .await;
        if let Err(error) = preparation {
            let message = error.to_string();
            if let Err(update_error) = self
                .store
                .set_finished(id, TaskStatus::Failed, now_timestamp(), Some(message))
                .await
            {
                tracing::error!(task_id = id, error = %update_error, "failed to roll back retry state");
            }
            self.events.changed(id);
            return Err(error);
        }

        self.events.changed(id);
        tracing::info!(task_id = id, "download task queued for retry");
        self.enqueue_locked(id.to_owned(), &mut workers).await;
        Ok(())
    }

    pub async fn delete_task(&self, id: &str) -> Result<()> {
        let task = self.store.delete_inactive(id).await?;
        let (_, temporary) = self.output_paths(&task)?;
        remove_file_if_exists(&temporary).await?;
        self.events.changed(id);
        tracing::info!(task_id = id, "download task deleted");
        Ok(())
    }

    pub async fn delete_media_file(&self, directory: &str, file_name: &str) -> Result<()> {
        let relative = normalize_relative_directory(directory)?;
        let directory_path = resolved_directory(&self.config.download_root, &relative)?;

        let file_name = file_name.trim();
        normalize_file_name(file_name)?;

        if !file_name.to_ascii_lowercase().ends_with(".mp4") {
            return Err(AppError::Validation("只能删除 MP4 媒体文件".into()));
        }

        let output = directory_path.join(file_name);
        remove_file_if_exists(&output).await?;
        self.media.remove(directory, file_name).await;
        self.events.media_changed();
        tracing::info!(directory, file_name, "media file deleted");

        Ok(())
    }

    pub fn list_directories(&self) -> Result<Vec<String>> {
        let mut directories = vec![String::new()];
        collect_directories(
            &self.config.download_root,
            &self.config.download_root,
            0,
            &mut directories,
        )?;
        directories.sort();
        Ok(directories)
    }

    #[cfg(test)]
    async fn enqueue(self: &Arc<Self>, id: String) -> Result<()> {
        let mut workers = self.workers.lock().await;
        if workers.shutting_down {
            return Err(AppError::InvalidState);
        }
        self.enqueue_locked(id, &mut workers).await;
        Ok(())
    }

    async fn enqueue_locked(self: &Arc<Self>, id: String, state: &mut WorkerState) {
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        let cancellation = CancellationToken::new();
        self.cancellations.lock().await.insert(
            id.clone(),
            Cancellation {
                generation,
                token: cancellation.clone(),
            },
        );
        let manager = Arc::clone(self);
        while let Some(result) = state.workers.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error, "download task ended unexpectedly");
            }
        }
        state.workers.spawn(async move {
            manager.run_task(&id, cancellation).await;
            manager.remove_cancellation(&id, generation).await;
        });
    }

    async fn remove_cancellation(&self, id: &str, generation: u64) {
        let mut cancellations = self.cancellations.lock().await;
        if cancellations
            .get(id)
            .is_some_and(|entry| entry.generation == generation)
        {
            cancellations.remove(id);
        }
    }

    async fn run_task(&self, id: &str, cancellation: CancellationToken) {
        let permit = tokio::select! {
            result = Arc::clone(&self.semaphore).acquire_owned() => match result {
                Ok(permit) => permit,
                Err(_) => return,
            },
            _ = cancellation.cancelled() => {
                self.finish(id, TaskStatus::Cancelled, None).await;
                return;
            }
        };

        let result = self.download(id, &cancellation).await;
        drop(permit);
        match result {
            Ok(RunOutcome::Completed) => self.finish(id, TaskStatus::Completed, None).await,
            Ok(RunOutcome::Cancelled) => self.finish(id, TaskStatus::Cancelled, None).await,
            Err(error) => {
                tracing::warn!(task_id = id, error = %error, "download failed");
                self.finish(id, TaskStatus::Failed, Some(error.to_string()))
                    .await;
            }
        }
    }

    async fn download(&self, id: &str, cancellation: &CancellationToken) -> Result<RunOutcome> {
        let task = self.store.get(id).await?;
        let (output, temporary) = self.output_paths(&task)?;
        remove_file_if_exists(&temporary).await?;
        self.store.set_started(id, now_timestamp()).await?;
        self.events.changed(id);
        self.set_progress(id, 0.0, None)?;

        let work_dir = self.config.temporary_root.join(format!(".{id}.work"));
        let raw = self.config.temporary_root.join(format!(".{id}.concat"));
        let downloader = HlsDownloader::new(self.config.concurrent_fragments)?;
        let download = downloader.download(&task.url, &raw, &work_dir, move |percentage| {
            self.set_progress(id, percentage, None)
        });
        let outcome = tokio::select! {
            _ = cancellation.cancelled() => RunOutcome::Cancelled,
            result = download => match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    let _ = remove_file_if_exists(&raw).await;
                    let _ = tokio::fs::remove_dir_all(&work_dir).await;
                    return Err(cleanup_after_error(&temporary, error).await);
                }
            }
        };

        match outcome {
            RunOutcome::Cancelled => {
                let _ = remove_file_if_exists(&raw).await;
                let _ = tokio::fs::remove_dir_all(&work_dir).await;
                remove_file_if_exists(&temporary).await?;
            }
            RunOutcome::Completed => {
                let ffmpeg = FfmpegRunner::new(&self.config.ffmpeg_bin);
                let remux = tokio::select! {
                    _ = cancellation.cancelled() => {
                        let _ = remove_file_if_exists(&raw).await;
                        let _ = tokio::fs::remove_dir_all(&work_dir).await;
                        remove_file_if_exists(&temporary).await?;
                        return Ok(RunOutcome::Cancelled);
                    }
                    result = ffmpeg.remux_to_mp4(&raw, &temporary) => result,
                };
                let _ = remove_file_if_exists(&raw).await;
                let _ = tokio::fs::remove_dir_all(&work_dir).await;
                if let Err(error) = remux {
                    return Err(cleanup_after_error(&temporary, error).await);
                }
                let metadata = tokio::fs::metadata(&temporary)
                    .await
                    .map_err(AppError::from)?;
                if metadata.len() == 0 {
                    return Err(cleanup_after_error(
                        &temporary,
                        AppError::Internal("FFmpeg 生成了空文件".into()),
                    )
                    .await);
                }
                if let Err(error) = move_file_without_overwrite(&temporary, &output, id).await {
                    return Err(cleanup_after_error(&temporary, error.into()).await);
                }
                let metadata = tokio::fs::metadata(&output).await?;
                let modified_at = metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|value| value.as_secs() as i64)
                    .unwrap_or_else(now_timestamp);
                self.media
                    .register(MediaFile {
                        directory: task.directory.clone(),
                        file_name: task.file_name.clone(),
                        size_bytes: metadata.len(),
                        modified_at,
                    })
                    .await;
                self.events.media_changed();
            }
        }
        Ok(outcome)
    }

    fn output_paths(&self, task: &DownloadTask) -> Result<(PathBuf, PathBuf)> {
        let relative = normalize_relative_directory(&task.directory)?;
        let directory = resolved_directory(&self.config.download_root, &relative)?;
        let output = directory.join(&task.file_name);
        if !output.starts_with(&self.config.download_root) {
            return Err(AppError::Validation("输出路径超出下载根目录".into()));
        }
        let temporary = temporary_output(&self.config.temporary_root, &output, &task.id);
        Ok((output, temporary))
    }

    async fn finish(&self, id: &str, status: TaskStatus, error: Option<String>) {
        if let Ok(mut progress) = self.progress.write() {
            progress.remove(id);
        }
        match self
            .store
            .set_finished(id, status, now_timestamp(), error)
            .await
        {
            Ok(()) => {
                self.events.changed(id);
                tracing::info!(task_id = id, status = ?status, "download task finished");
            }
            Err(update_error) => {
                tracing::error!(task_id = id, error = %update_error, "failed to update task state");
            }
        }
    }

    fn set_progress(
        &self,
        id: &str,
        percentage: f32,
        total_duration_ms: Option<u64>,
    ) -> Result<()> {
        let processed_duration_ms = total_duration_ms
            .filter(|total| *total > 0)
            .map(|total| (percentage as f64 / 100.0 * total as f64) as u64)
            .unwrap_or(0);
        let progress = TaskProgress {
            processed_duration_ms,
            total_duration_ms,
            percentage: Some(percentage),
        };
        self.progress
            .write()
            .map_err(|_| AppError::Internal("progress lock is poisoned".into()))?
            .insert(id.to_owned(), progress.clone());
        self.events.progress(id, progress);
        Ok(())
    }
}

fn collect_directories(
    root: &Path,
    current: &Path,
    depth: usize,
    entries: &mut Vec<String>,
) -> Result<()> {
    if depth >= 8 || entries.len() >= 1_000 {
        return Ok(());
    }
    let mut children = std::fs::read_dir(current)?.collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let file_type = child.file_type()?;
        if !file_type.is_dir()
            || file_type.is_symlink()
            || child.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        let path = child.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| AppError::Internal("无法生成相对目录路径".into()))?;
        entries.push(path_for_storage(relative));
        collect_directories(root, &path, depth + 1, entries)?;
    }
    Ok(())
}

fn path_for_storage(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn move_file_without_overwrite(from: &Path, to: &Path, task_id: &str) -> std::io::Result<()> {
    match tokio::fs::hard_link(from, to).await {
        Ok(()) => {
            if let Err(error) = tokio::fs::remove_file(from).await {
                tracing::warn!(path = %from.display(), %error, "published file but failed to remove temporary source");
            }
            return Ok(());
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return Err(std::io::Error::new(
                ErrorKind::AlreadyExists,
                "目标文件在下载期间被外部程序创建，未覆盖该文件",
            ));
        }
        Err(_) => {}
    }

    let stem = to
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let staged = to.with_file_name(format!(".{stem}.{task_id}.part.mp4"));
    let mut target = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .await?;
    let copy_result = async {
        let mut source = tokio::fs::File::open(from).await?;
        tokio::io::copy(&mut source, &mut target).await?;
        target.sync_all().await
    }
    .await;
    if let Err(error) = copy_result {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(error);
    }

    if let Err(error) = tokio::fs::hard_link(&staged, to).await {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(if error.kind() == ErrorKind::AlreadyExists {
            std::io::Error::new(
                ErrorKind::AlreadyExists,
                "目标文件在下载期间被外部程序创建，未覆盖该文件",
            )
        } else {
            error
        });
    }

    if let Err(error) = tokio::fs::remove_file(&staged).await {
        tracing::warn!(path = %staged.display(), %error, "published file but failed to remove staged file");
    }
    if let Err(error) = tokio::fs::remove_file(from).await {
        tracing::warn!(path = %from.display(), %error, "published file but failed to remove temporary source");
    }
    Ok(())
}

async fn cleanup_after_error(path: &Path, error: AppError) -> AppError {
    match remove_file_if_exists(path).await {
        Ok(()) => error,
        Err(cleanup_error) => AppError::Internal(format!(
            "{error}；临时文件清理失败，可能需要手动删除：{cleanup_error}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> Arc<TaskManager> {
        let root = std::env::temp_dir().join(format!("tessora-manager-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        TaskManager::new(Config {
            download_root: root.clone(),
            temporary_root: root,
            ffmpeg_bin: "ffmpeg".into(),
            concurrent_fragments: 1,
            max_concurrent_downloads: 1,
            media_settle_seconds: 0,
        })
    }

    fn task(id: &str, status: TaskStatus) -> DownloadTask {
        DownloadTask {
            id: id.into(),
            url: "https://example.com/video.m3u8".into(),
            directory: String::new(),
            file_name: format!("{id}.mp4"),
            status,
            progress: None,
            error: None,
            created_at: 1,
            started_at: None,
            finished_at: None,
        }
    }

    #[tokio::test]
    async fn old_worker_does_not_remove_new_generation_cancellation() {
        let manager = manager();
        manager.cancellations.lock().await.insert(
            "one".into(),
            Cancellation {
                generation: 2,
                token: CancellationToken::new(),
            },
        );

        manager.remove_cancellation("one", 1).await;
        assert_eq!(
            manager
                .cancellations
                .lock()
                .await
                .get("one")
                .map(|entry| entry.generation),
            Some(2)
        );

        manager.remove_cancellation("one", 2).await;
        assert!(!manager.cancellations.lock().await.contains_key("one"));
    }

    #[tokio::test]
    async fn shutdown_rejects_retry_and_new_workers() {
        let manager = manager();
        manager
            .store
            .reserve_and_insert(task("one", TaskStatus::Failed))
            .await
            .unwrap();
        manager.shutdown().await;

        assert!(matches!(
            manager.retry_task("one").await,
            Err(AppError::InvalidState)
        ));
        assert!(matches!(
            manager.enqueue("missing".into()).await,
            Err(AppError::InvalidState)
        ));
        assert_eq!(
            manager.store.get("one").await.unwrap().status,
            TaskStatus::Failed
        );
        let workers = manager.workers.lock().await;
        assert!(workers.workers.is_empty());
        assert_eq!(workers.next_generation, 0);
    }

    #[tokio::test]
    async fn cancel_marks_the_current_run_as_cancelled() {
        let manager = manager();
        manager
            .store
            .reserve_and_insert(task("one", TaskStatus::Queued))
            .await
            .unwrap();
        let token = CancellationToken::new();
        manager.cancellations.lock().await.insert(
            "one".into(),
            Cancellation {
                generation: 1,
                token: token.clone(),
            },
        );

        manager.cancel_task("one").await.unwrap();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn publishes_without_overwriting_an_existing_file() {
        let root = std::env::temp_dir().join(format!("tessora-publish-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.mp4");
        let destination = root.join("destination.mp4");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&destination, b"existing").unwrap();

        let result = move_file_without_overwrite(
            &source,
            &destination,
            "550e8400-e29b-41d4-a716-446655440000",
        )
        .await;

        assert_eq!(result.unwrap_err().kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&destination).unwrap(), b"existing");
        assert_eq!(std::fs::read(&source).unwrap(), b"new");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn publishes_a_completed_file() {
        let root = std::env::temp_dir().join(format!("tessora-publish-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.mp4");
        let destination = root.join("destination.mp4");
        std::fs::write(&source, b"completed").unwrap();

        move_file_without_overwrite(
            &source,
            &destination,
            "550e8400-e29b-41d4-a716-446655440000",
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"completed");
        assert!(!source.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
