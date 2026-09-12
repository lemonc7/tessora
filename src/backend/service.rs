use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use dioxus::logger::tracing;
use tokio::{
    sync::{Mutex, Semaphore, oneshot},
    task::JoinSet,
};

use crate::{
    backend::{
        config::Config,
        db::TaskRepository,
        downloader::temporary_output,
        error::{AppError, Result},
        events::TaskEvents,
        hls::HlsDownloader,
        media::{FfmpegRunner, RunOutcome},
        task::{
            default_file_name, normalize_file_name, normalize_relative_directory, now_timestamp,
            resolved_directory, validate_url,
        },
    },
    contracts::{
        api::{CreateTaskRequest, DirectoryEntry},
        tasks::{DownloadTask, TaskProgress, TaskStatus},
    },
};

pub struct TaskManager {
    repository: Arc<TaskRepository>,
    config: Arc<Config>,
    semaphore: Arc<Semaphore>,
    cancellations: Mutex<HashMap<String, oneshot::Sender<()>>>,
    progress: RwLock<HashMap<String, TaskProgress>>,
    tasks: Mutex<JoinSet<()>>,
    shutting_down: AtomicBool,
    events: TaskEvents,
}

impl TaskManager {
    pub fn new(repository: Arc<TaskRepository>, config: Arc<Config>) -> Arc<Self> {
        Arc::new(Self {
            repository,
            semaphore: Arc::new(Semaphore::new(config.max_concurrent_downloads)),
            config,
            cancellations: Mutex::new(HashMap::new()),
            progress: RwLock::new(HashMap::new()),
            tasks: Mutex::new(JoinSet::new()),
            shutting_down: AtomicBool::new(false),
            events: TaskEvents::default(),
        })
    }

    pub fn subscribe(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::contracts::tasks::TaskEvent> {
        self.events.subscribe()
    }

    pub fn event_shutdown_token(&self) -> tokio_util::sync::CancellationToken {
        self.events.shutdown_token()
    }

    pub fn close_event_streams(&self) {
        self.events.close_streams();
    }

    pub async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.close_event_streams();

        let cancellations = {
            let mut active = self.cancellations.lock().await;
            std::mem::take(&mut *active)
        };
        tracing::info!(
            active_tasks = cancellations.len(),
            "stopping download tasks"
        );
        for cancellation in cancellations.into_values() {
            cancellation.send(()).ok();
        }

        let mut tasks = self.tasks.lock().await;
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error, "download task ended unexpectedly during shutdown");
            }
        }
        tracing::info!("download tasks stopped");
    }

    pub async fn list_tasks(&self) -> Result<Vec<DownloadTask>> {
        let mut tasks = self.repository.list().await?;
        let progress = self
            .progress
            .read()
            .map_err(|_| AppError::Internal("progress lock is poisoned".into()))?;
        for task in &mut tasks {
            task.progress = progress.get(&task.id).cloned();
        }
        Ok(tasks)
    }

    pub async fn create_task(self: &Arc<Self>, request: CreateTaskRequest) -> Result<DownloadTask> {
        let url = validate_url(&request.url)?;
        let relative_directory = normalize_relative_directory(&request.directory)?;
        let output_directory = resolved_directory(&self.config.download_root, &relative_directory)?;
        let file_name = if request.file_name.trim().is_empty() {
            normalize_file_name(&default_file_name(&request.url))?
        } else {
            normalize_file_name(&request.file_name)?
        };
        let output = output_directory.join(&file_name);
        if output.exists() {
            return Err(AppError::Validation(
                "目标文件已经存在，请更换文件名".into(),
            ));
        }

        let directory = path_for_storage(&relative_directory);
        if self
            .repository
            .output_exists(&directory, &file_name)
            .await?
        {
            return Err(AppError::Validation(
                "相同目录和文件名的任务已经存在，请先删除原任务".into(),
            ));
        }

        let task = DownloadTask {
            id: uuid::Uuid::new_v4().to_string(),
            url,
            directory,
            file_name,
            status: TaskStatus::Queued,
            progress: None,
            error: None,
            created_at: now_timestamp(),
            started_at: None,
            finished_at: None,
        };
        self.repository.insert(&task).await?;
        self.events.changed(&task.id);
        self.enqueue(task.id.clone()).await;
        Ok(task)
    }

    pub async fn cancel_task(&self, id: &str) -> Result<()> {
        let task = self.repository.get(id).await?;
        if !task.status.is_active() {
            return Err(AppError::InvalidState);
        }
        let sender = self.cancellations.lock().await.remove(id);
        let Some(sender) = sender else {
            return Err(AppError::InvalidState);
        };
        sender.send(()).ok();
        Ok(())
    }

    pub async fn retry_task(self: &Arc<Self>, id: &str) -> Result<()> {
        let task = self.repository.get(id).await?;
        if !task.status.can_retry() {
            return Err(AppError::InvalidState);
        }
        let (_, temporary) = self.output_paths(&task)?;
        remove_file_if_exists(&temporary).await?;
        self.repository.reset_for_retry(id).await?;
        self.events.changed(id);
        self.enqueue(id.to_owned()).await;
        Ok(())
    }

    pub async fn delete_task(&self, id: &str, delete_file: bool) -> Result<()> {
        let task = self.repository.get(id).await?;
        if task.status.is_active() {
            return Err(AppError::Validation("请先取消正在运行的任务".into()));
        }
        let (output, temporary) = self.output_paths(&task)?;
        if delete_file {
            remove_file_if_exists(&output).await?;
        }
        remove_file_if_exists(&temporary).await?;
        self.repository.delete(id).await?;
        self.events.changed(id);
        Ok(())
    }

    pub fn list_directories(&self) -> Result<Vec<DirectoryEntry>> {
        let mut directories = vec![DirectoryEntry {
            value: String::new(),
            label: "根目录".into(),
        }];
        collect_directories(
            &self.config.download_root,
            &self.config.download_root,
            0,
            &mut directories,
        )?;
        directories.sort_by(|left, right| left.value.cmp(&right.value));
        Ok(directories)
    }

    async fn enqueue(self: &Arc<Self>, id: String) {
        let (sender, receiver) = oneshot::channel();
        self.cancellations.lock().await.insert(id.clone(), sender);
        let manager = Arc::clone(self);
        let mut tasks = self.tasks.lock().await;
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error, "download task ended unexpectedly");
            }
        }
        tasks.spawn(async move {
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
                self.finish(id, self.cancellation_status(), None).await;
                return;
            }
        };

        let result = self.download(id, cancellation).await;
        drop(permit);
        match result {
            Ok(RunOutcome::Completed) => self.finish(id, TaskStatus::Completed, None).await,
            Ok(RunOutcome::Cancelled) => {
                self.finish(id, self.cancellation_status(), None).await;
            }
            Err(error) => {
                tracing::warn!(task_id = id, error = %error, "download failed");
                self.finish(id, TaskStatus::Failed, Some(error.to_string()))
                    .await;
            }
        }
    }

    async fn download(&self, id: &str, cancellation: oneshot::Receiver<()>) -> Result<RunOutcome> {
        let task = self.repository.get(id).await?;
        let (output, temporary) = self.output_paths(&task)?;
        remove_file_if_exists(&temporary).await?;
        self.repository.set_started(id, now_timestamp()).await?;
        self.events.changed(id);

        self.set_progress(id, 0.0, None)?;

        let work_dir = self
            .config
            .temporary_root
            .join(format!(".{}.work", task.id));
        let raw = self
            .config
            .temporary_root
            .join(format!(".{}.concat", task.id));
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
                if let Err(error) = remove_file_if_exists(&temporary).await {
                    return Err(AppError::Internal(format!(
                        "下载已取消，但临时文件清理失败：{error}"
                    )));
                }
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
                let metadata = match tokio::fs::metadata(&temporary).await {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        return Err(cleanup_after_error(&temporary, error.into()).await);
                    }
                };
                if metadata.len() == 0 {
                    let error = AppError::Internal("FFmpeg 生成了空文件".into());
                    return Err(cleanup_after_error(&temporary, error).await);
                }
                if let Err(error) = move_file(&temporary, &output).await {
                    return Err(cleanup_after_error(&temporary, error.into()).await);
                }
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
        match self.progress.write() {
            Ok(mut progress) => {
                progress.remove(id);
            }
            Err(_) => tracing::error!(task_id = id, "progress lock is poisoned"),
        }
        match self
            .repository
            .set_finished(id, status, now_timestamp(), error.as_deref())
            .await
        {
            Ok(()) => self.events.changed(id),
            Err(update_error) => {
                tracing::error!(task_id = id, error = %update_error, "failed to persist task state");
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

    fn cancellation_status(&self) -> TaskStatus {
        if self.shutting_down.load(Ordering::Acquire) {
            TaskStatus::Interrupted
        } else {
            TaskStatus::Cancelled
        }
    }
}

fn collect_directories(
    root: &Path,
    current: &Path,
    depth: usize,
    entries: &mut Vec<DirectoryEntry>,
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
            .map_err(|_| AppError::Internal("failed to make directory path relative".into()))?;
        let value = path_for_storage(relative);
        entries.push(DirectoryEntry {
            label: value.clone(),
            value,
        });
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

// Renames when possible, otherwise copies across filesystems (e.g. container-internal
// temp dir -> host bind mount) and removes the source, so the whole download is not
// streamed through a slow bind mount.
async fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Ok(()) = tokio::fs::rename(from, to).await {
        return Ok(());
    }
    tokio::fs::copy(from, to).await?;
    tokio::fs::remove_file(from).await?;
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
