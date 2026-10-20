use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

use crate::{
    backend::error::{AppError, Result},
    backend::scanner::ScannedMediaFile,
    contracts::{
        media::{MediaFile, MediaOrigin, MediaState, ScanReport},
        tasks::{DownloadTask, TaskStatus},
    },
};

pub struct TaskRepository {
    pool: SqlitePool,
}

impl TaskRepository {
    pub async fn open(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub fn media_repository(&self) -> MediaRepository {
        MediaRepository {
            pool: self.pool.clone(),
        }
    }

    pub async fn insert(&self, task: &DownloadTask) -> Result<()> {
        sqlx::query(
            "INSERT INTO download_tasks (
                id, url, directory, file_name, status, error_message,
                created_at, started_at, finished_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&task.id)
        .bind(&task.url)
        .bind(&task.directory)
        .bind(&task.file_name)
        .bind(task.status.to_string())
        .bind(&task.error)
        .bind(task.created_at)
        .bind(task.started_at)
        .bind(task.finished_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<DownloadTask>> {
        let rows = sqlx::query_as::<_, TaskRow>(
            "SELECT id, url, directory, file_name, status, error_message,
                    created_at, started_at, finished_at
             FROM download_tasks ORDER BY created_at DESC, rowid DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TaskRow::into_task).collect()
    }

    pub async fn get(&self, id: &str) -> Result<DownloadTask> {
        let row = sqlx::query_as::<_, TaskRow>(
            "SELECT id, url, directory, file_name, status, error_message,
                    created_at, started_at, finished_at
             FROM download_tasks WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AppError::NotFound)?;
        row.into_task()
    }

    pub async fn output_exists(&self, directory: &str, file_name: &str) -> Result<bool> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
                SELECT 1 FROM download_tasks WHERE directory = ? AND file_name = ?
             )",
        )
        .bind(directory)
        .bind(file_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn set_started(&self, id: &str, timestamp: i64) -> Result<()> {
        let result = sqlx::query(
            "UPDATE download_tasks
             SET status = 'downloading', started_at = ?, finished_at = NULL,
                 error_message = NULL
             WHERE id = ?",
        )
        .bind(timestamp)
        .bind(id)
        .execute(&self.pool)
        .await?;
        require_changed(result.rows_affected())
    }

    pub async fn set_finished(
        &self,
        id: &str,
        status: TaskStatus,
        timestamp: i64,
        error: Option<&str>,
    ) -> Result<()> {
        let result = sqlx::query(
            "UPDATE download_tasks
             SET status = ?, finished_at = ?, error_message = ?
             WHERE id = ?",
        )
        .bind(status.to_string())
        .bind(timestamp)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        require_changed(result.rows_affected())
    }

    pub async fn reset_for_retry(&self, id: &str) -> Result<()> {
        let result = sqlx::query(
            "UPDATE download_tasks
             SET status = 'queued', error_message = NULL,
                 started_at = NULL, finished_at = NULL
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        require_changed(result.rows_affected())
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let result = sqlx::query("DELETE FROM download_tasks WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        require_changed(result.rows_affected())
    }

    pub async fn mark_active_tasks_interrupted(&self) -> Result<()> {
        let now = crate::backend::task::now_timestamp();
        sqlx::query(
            "UPDATE download_tasks
             SET status = 'interrupted', finished_at = ?,
                 error_message = '服务重启，下载已中断'
             WHERE status IN ('queued', 'downloading')",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

pub struct MediaRepository {
    pool: SqlitePool,
}

impl MediaRepository {
    pub async fn list(&self, include_missing: bool) -> Result<Vec<MediaFile>> {
        let rows = sqlx::query_as::<_, MediaRow>(
            "SELECT id, directory, file_name, size_bytes, modified_at, origin,
                    source_task_id, content_hash, state, discovered_at, last_seen_at
             FROM media_files
             WHERE state = 'present' OR ?
             ORDER BY directory, file_name",
        )
        .bind(include_missing)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(MediaRow::into_media_file).collect()
    }

    pub async fn present_output_exists(&self, directory: &str, file_name: &str) -> Result<bool> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
                SELECT 1 FROM media_files
                WHERE directory = ? AND file_name = ? AND state = 'present'
             )",
        )
        .bind(directory)
        .bind(file_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn register_download(
        &self,
        task: &DownloadTask,
        size_bytes: u64,
        modified_at: i64,
        now: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO media_files (
                id, directory, file_name, size_bytes, modified_at, origin,
                source_task_id, content_hash, state, discovered_at, last_seen_at,
                last_seen_scan_id
             ) VALUES (?, ?, ?, ?, ?, 'tessora', ?, NULL, 'present', ?, ?, NULL)
             ON CONFLICT(directory, file_name) DO UPDATE SET
                size_bytes = excluded.size_bytes,
                modified_at = excluded.modified_at,
                origin = 'tessora',
                source_task_id = excluded.source_task_id,
                state = 'present',
                last_seen_at = excluded.last_seen_at",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&task.directory)
        .bind(&task.file_name)
        .bind(size_bytes as i64)
        .bind(modified_at)
        .bind(&task.id)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_missing(&self, directory: &str, file_name: &str) -> Result<()> {
        sqlx::query(
            "UPDATE media_files SET state = 'missing'
             WHERE directory = ? AND file_name = ?",
        )
        .bind(directory)
        .bind(file_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn synchronize(
        &self,
        files: &[ScannedMediaFile],
        scan_id: &str,
        started_at: i64,
        initially_skipped: usize,
    ) -> Result<ScanReport> {
        let mut transaction = self.pool.begin().await?;
        let mut report = ScanReport {
            discovered: 0,
            updated: 0,
            unchanged: 0,
            missing: 0,
            skipped: initially_skipped,
        };

        for file in files {
            let existing = sqlx::query_as::<_, ExistingMediaRow>(
                "SELECT size_bytes, modified_at, state
                 FROM media_files WHERE directory = ? AND file_name = ?",
            )
            .bind(&file.directory)
            .bind(&file.file_name)
            .fetch_optional(&mut *transaction)
            .await?;

            if !file.stable && existing.is_none() {
                report.skipped += 1;
                continue;
            }

            if !file.stable {
                sqlx::query(
                    "UPDATE media_files
                     SET state = 'present', last_seen_at = ?, last_seen_scan_id = ?
                     WHERE directory = ? AND file_name = ?",
                )
                .bind(started_at)
                .bind(scan_id)
                .bind(&file.directory)
                .bind(&file.file_name)
                .execute(&mut *transaction)
                .await?;
                report.unchanged += 1;
                continue;
            }

            match &existing {
                None => report.discovered += 1,
                Some(row)
                    if row.size_bytes != file.size_bytes as i64
                        || row.modified_at != file.modified_at
                        || row.state != "present" =>
                {
                    report.updated += 1;
                }
                Some(_) => report.unchanged += 1,
            }

            let source_task_id = sqlx::query_scalar::<_, String>(
                "SELECT id FROM download_tasks
                 WHERE directory = ? AND file_name = ? AND status = 'completed'
                 LIMIT 1",
            )
            .bind(&file.directory)
            .bind(&file.file_name)
            .fetch_optional(&mut *transaction)
            .await?;
            let origin = if source_task_id.is_some() {
                "tessora"
            } else {
                "external"
            };

            sqlx::query(
                "INSERT INTO media_files (
                    id, directory, file_name, size_bytes, modified_at, origin,
                    source_task_id, content_hash, state, discovered_at, last_seen_at,
                    last_seen_scan_id
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, 'present', ?, ?, ?)
                 ON CONFLICT(directory, file_name) DO UPDATE SET
                    size_bytes = excluded.size_bytes,
                    modified_at = excluded.modified_at,
                    state = 'present',
                    last_seen_at = excluded.last_seen_at,
                    last_seen_scan_id = excluded.last_seen_scan_id",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&file.directory)
            .bind(&file.file_name)
            .bind(file.size_bytes as i64)
            .bind(file.modified_at)
            .bind(origin)
            .bind(source_task_id)
            .bind(started_at)
            .bind(started_at)
            .bind(scan_id)
            .execute(&mut *transaction)
            .await?;
        }

        report.missing = sqlx::query(
            "UPDATE media_files SET state = 'missing'
             WHERE state = 'present'
               AND COALESCE(last_seen_scan_id, '') != ?
               AND last_seen_at < ?",
        )
        .bind(scan_id)
        .bind(started_at)
        .execute(&mut *transaction)
        .await?
        .rows_affected() as usize;

        transaction.commit().await?;
        Ok(report)
    }
}

#[derive(FromRow)]
struct ExistingMediaRow {
    size_bytes: i64,
    modified_at: i64,
    state: String,
}

#[derive(FromRow)]
struct MediaRow {
    id: String,
    directory: String,
    file_name: String,
    size_bytes: i64,
    modified_at: i64,
    origin: String,
    source_task_id: Option<String>,
    content_hash: Option<String>,
    state: String,
    discovered_at: i64,
    last_seen_at: i64,
}

impl MediaRow {
    fn into_media_file(self) -> Result<MediaFile> {
        Ok(MediaFile {
            id: self.id,
            directory: self.directory,
            file_name: self.file_name,
            size_bytes: self.size_bytes.max(0) as u64,
            modified_at: self.modified_at,
            origin: MediaOrigin::from_str(&self.origin).map_err(AppError::Internal)?,
            state: MediaState::from_str(&self.state).map_err(AppError::Internal)?,
            source_task_id: self.source_task_id,
            content_hash: self.content_hash,
            discovered_at: self.discovered_at,
            last_seen_at: self.last_seen_at,
        })
    }
}

#[derive(FromRow)]
struct TaskRow {
    id: String,
    url: String,
    directory: String,
    file_name: String,
    status: String,
    error_message: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
}

impl TaskRow {
    fn into_task(self) -> Result<DownloadTask> {
        let status = TaskStatus::from_str(&self.status).map_err(AppError::Internal)?;
        Ok(DownloadTask {
            id: self.id,
            url: self.url,
            directory: self.directory,
            file_name: self.file_name,
            status,
            progress: None,
            error: self.error_message,
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
        })
    }
}

fn require_changed(rows_affected: u64) -> Result<()> {
    if rows_affected == 0 {
        Err(AppError::NotFound)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stores_and_updates_tasks() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let repository = TaskRepository { pool };
        let task = DownloadTask {
            id: "task-1".into(),
            url: "https://example.com/video.m3u8".into(),
            directory: "shows".into(),
            file_name: "episode.mp4".into(),
            status: TaskStatus::Queued,
            progress: None,
            error: None,
            created_at: 1,
            started_at: None,
            finished_at: None,
        };

        repository.insert(&task).await.unwrap();
        repository.set_started(&task.id, 2).await.unwrap();

        let stored = repository.get(&task.id).await.unwrap();
        assert_eq!(stored.status, TaskStatus::Downloading);
        assert_eq!(stored.progress, None);
    }

    #[tokio::test]
    async fn synchronizes_external_media_and_marks_missing_files() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let repository = MediaRepository { pool };
        let files = vec![ScannedMediaFile {
            directory: "movies".into(),
            file_name: "example.mp4".into(),
            size_bytes: 42,
            modified_at: 10,
            stable: true,
        }];

        let first = repository
            .synchronize(&files, "scan-1", 100, 0)
            .await
            .unwrap();
        assert_eq!(first.discovered, 1);
        let stored = repository.list(false).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].origin, MediaOrigin::External);

        let second = repository.synchronize(&[], "scan-2", 101, 0).await.unwrap();
        assert_eq!(second.missing, 1);
        assert!(repository.list(false).await.unwrap().is_empty());
        assert_eq!(
            repository.list(true).await.unwrap()[0].state,
            MediaState::Missing
        );
    }
}
