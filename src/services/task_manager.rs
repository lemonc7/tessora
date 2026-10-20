use std::{
    collections::HashMap,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::UNIX_EPOCH,
};

use tokio::{
    sync::{Mutex, Semaphore, oneshot},
    task::JoinSet,
};

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
    cancellations: Mutex<HashMap<String, oneshot::Sender<()>>>,
    progress: RwLock<HashMap<String, TaskProgress>>,
    workers: Mutex<JoinSet<()>>,
    shutting_down: AtomicBool,
    events: TaskEvents,
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
            workers: Mutex::new(JoinSet::new()),
            shutting_down: AtomicBool::new(false),
            events: TaskEvents::default(),
        })
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
    }

    pub async fn initialize(&self) -> Result<ScanReport> {
        let download_files_removed = cleanup_stale_temporary_files(&self.config.download_root)?;
        let temporary_files_removed = if self.config.temporary_root != self.config.download_root {
            cleanup_stale_temporary_files(&self.config.temporary_root)?
        } else {
            0
        };
        tracing::info!(
            download_files_removed,
            temporary_files_removed,
            "stale temporary files cleaned"
        );
        self.media.scan().await
    }

    pub async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        let cancellations = {
            let mut active = self.cancellations.lock().await;
            std::mem::take(&mut *active)
        };
        for cancellation in cancellations.into_values() {
            cancellation.send(()).ok();
        }

        let mut workers = self.workers.lock().await;
        while let Some(result) = workers.join_next().await {
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
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AppError::InvalidState);
        }
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
        self.enqueue(task.id.clone()).await;
        Ok(task)
    }

    pub async fn cancel_task(&self, id: &str) -> Result<()> {
        let task = self.store.get(id).await?;
        if !task.status.is_active() {
            return Err(AppError::InvalidState);
        }
        let sender = self.cancellations.lock().await.remove(id);
        let Some(sender) = sender else {
            return Err(AppError::InvalidState);
        };
        sender.send(()).ok();
        tracing::info!(task_id = id, "download task cancellation requested");
        Ok(())
    }

    pub async fn retry_task(self: &Arc<Self>, id: &str) -> Result<()> {
        let task = self.store.get(id).await?;
        if !task.status.can_retry() {
            return Err(AppError::InvalidState);
        }
        let (_, temporary) = self.output_paths(&task)?;
        remove_file_if_exists(&temporary).await?;
        self.store.reset_for_retry(id).await?;
        self.events.changed(id);
        tracing::info!(task_id = id, "download task queued for retry");
        self.enqueue(id.to_owned()).await;
        Ok(())
    }

    pub async fn delete_task(&self, id: &str) -> Result<()> {
        let task = self.store.get(id).await?;
        if task.status.is_active() {
            return Err(AppError::Validation("请先取消正在运行的任务".into()));
        }
        let (_, temporary) = self.output_paths(&task)?;
        remove_file_if_exists(&temporary).await?;
        self.store.delete(id).await?;
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

    async fn enqueue(self: &Arc<Self>, id: String) {
        let (sender, receiver) = oneshot::channel();
        self.cancellations.lock().await.insert(id.clone(), sender);
        let manager = Arc::clone(self);
        let mut workers = self.workers.lock().await;
        while let Some(result) = workers.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error, "download task ended unexpectedly");
            }
        }
        workers.spawn(async move {
            manager.run_task(&id, receiver).await;
            manager.cancellations.lock().await.remove(&id);
        });
    }

    async fn run_task(&self, id: &str, mut cancellation: oneshot::Receiver<()>) {
        let permit = tokio::select! {
            result = Arc::clone(&self.semaphore).acquire_owned() => match result {
                Ok(permit) => permit,
                Err(_) => return,
            },
            _ = &mut cancellation => {
                self.finish(id, TaskStatus::Cancelled, None).await;
                return;
            }
        };

        let result = self.download(id, cancellation).await;
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

    async fn download(&self, id: &str, cancellation: oneshot::Receiver<()>) -> Result<RunOutcome> {
        let task = self.store.get(id).await?;
        let (output, temporary) = self.output_paths(&task)?;
        remove_file_if_exists(&temporary).await?;
        self.store.set_started(id, now_timestamp()).await?;
        self.events.changed(id);
        self.set_progress(id, 0.0, None)?;

        let work_dir = self.config.temporary_root.join(format!(".{id}.work"));
        let raw = self.config.temporary_root.join(format!(".{id}.concat"));
        let downloader = HlsDownloader::new(self.config.concurrent_fragments)?;
        let outcome = match downloader
            .download(
                &task.url,
                &raw,
                &work_dir,
                cancellation,
                move |percentage| self.set_progress(id, percentage, None),
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = remove_file_if_exists(&raw).await;
                let _ = tokio::fs::remove_dir_all(&work_dir).await;
                return Err(cleanup_after_error(&temporary, error).await);
            }
        };

        match outcome {
            RunOutcome::Cancelled => {
                let _ = remove_file_if_exists(&raw).await;
                let _ = tokio::fs::remove_dir_all(&work_dir).await;
                remove_file_if_exists(&temporary).await?;
            }
            RunOutcome::Completed => {
                let remux = FfmpegRunner::new(&self.config.ffmpeg_bin)
                    .remux_to_mp4(&raw, &temporary)
                    .await;
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
        if !file_type.is_dir() || file_type.is_symlink() {
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
    if to.exists() {
        return Err(std::io::Error::new(
            ErrorKind::AlreadyExists,
            "目标文件在下载期间被外部程序创建，未覆盖该文件",
        ));
    }

    let stem = to
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let staged = to.with_file_name(format!(".{stem}.{task_id}.part.mp4"));
    let moved = match tokio::fs::rename(from, &staged).await {
        Ok(()) => true,
        Err(_) => {
            let copy_result = async {
                let mut source = tokio::fs::File::open(from).await?;
                let mut target = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&staged)
                    .await?;
                tokio::io::copy(&mut source, &mut target).await?;
                target.sync_all().await
            }
            .await;
            if let Err(error) = copy_result {
                let _ = tokio::fs::remove_file(&staged).await;
                return Err(error);
            }
            false
        }
    };

    if let Err(error) = tokio::fs::hard_link(&staged, to).await {
        if moved {
            let _ = tokio::fs::rename(&staged, from).await;
        } else {
            let _ = tokio::fs::remove_file(&staged).await;
        }
        return Err(if error.kind() == ErrorKind::AlreadyExists {
            std::io::Error::new(
                ErrorKind::AlreadyExists,
                "目标文件在下载期间被外部程序创建，未覆盖该文件",
            )
        } else {
            error
        });
    }
    tokio::fs::remove_file(&staged).await?;
    if !moved {
        tokio::fs::remove_file(from).await?;
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
