use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

use crate::{
    backend::error::{AppError, Result},
    contracts::tasks::{DownloadTask, TaskStatus},
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
}
