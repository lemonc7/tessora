use std::collections::HashMap;

use tokio::sync::RwLock;

use crate::{
    domain::{DownloadTask, TaskStatus},
    error::{AppError, Result},
};

#[derive(Default)]
pub struct TaskStore {
    state: RwLock<TaskState>,
}

#[derive(Default)]
struct TaskState {
    tasks: HashMap<String, DownloadTask>,
    reserved_outputs: HashMap<(String, String), String>,
}

impl TaskStore {
    pub async fn reserve_and_insert(&self, task: DownloadTask) -> Result<()> {
        let mut state = self.state.write().await;
        let output = (task.directory.clone(), task.file_name.clone());
        if state.reserved_outputs.contains_key(&output) {
            return Err(AppError::Validation(
                "相同目录和文件名的任务已经存在，请先删除原任务".into(),
            ));
        }
        state.reserved_outputs.insert(output, task.id.clone());
        state.tasks.insert(task.id.clone(), task);
        Ok(())
    }

    pub async fn list(&self) -> Vec<DownloadTask> {
        let mut tasks = self
            .state
            .read()
            .await
            .tasks
            .values()
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        tasks
    }

    pub async fn get(&self, id: &str) -> Result<DownloadTask> {
        self.state
            .read()
            .await
            .tasks
            .get(id)
            .cloned()
            .ok_or(AppError::NotFound)
    }

    pub async fn set_started(&self, id: &str, timestamp: i64) -> Result<()> {
        let mut state = self.state.write().await;
        let task = state.tasks.get_mut(id).ok_or(AppError::NotFound)?;
        task.status = TaskStatus::Downloading;
        task.started_at = Some(timestamp);
        task.finished_at = None;
        task.error = None;
        task.progress = None;
        Ok(())
    }

    pub async fn set_finished(
        &self,
        id: &str,
        status: TaskStatus,
        timestamp: i64,
        error: Option<String>,
    ) -> Result<()> {
        let mut state = self.state.write().await;
        if status == TaskStatus::Completed {
            let task = state.tasks.remove(id).ok_or(AppError::NotFound)?;
            let output = (task.directory, task.file_name);
            if state
                .reserved_outputs
                .get(&output)
                .is_some_and(|owner| owner == id)
            {
                state.reserved_outputs.remove(&output);
            }
            return Ok(());
        }

        let task = state.tasks.get_mut(id).ok_or(AppError::NotFound)?;
        task.status = status;
        task.finished_at = Some(timestamp);
        task.error = error;
        task.progress = None;
        Ok(())
    }

    pub async fn reset_for_retry(&self, id: &str) -> Result<DownloadTask> {
        let mut state = self.state.write().await;
        let task = state.tasks.get_mut(id).ok_or(AppError::NotFound)?;
        if !task.status.can_retry() {
            return Err(AppError::InvalidState);
        }
        task.status = TaskStatus::Queued;
        task.error = None;
        task.progress = None;
        task.started_at = None;
        task.finished_at = None;
        Ok(task.clone())
    }

    pub async fn delete_inactive(&self, id: &str) -> Result<DownloadTask> {
        let mut state = self.state.write().await;
        let task = state.tasks.get(id).ok_or(AppError::NotFound)?;
        if task.status.is_active() {
            return Err(AppError::Validation("请先取消正在运行的任务".into()));
        }
        let task = state.tasks.remove(id).ok_or(AppError::NotFound)?;
        let output = (task.directory.clone(), task.file_name.clone());
        if state
            .reserved_outputs
            .get(&output)
            .is_some_and(|owner| owner == id)
        {
            state.reserved_outputs.remove(&output);
        }
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> DownloadTask {
        DownloadTask {
            id: id.into(),
            url: "https://example.com/video.m3u8".into(),
            directory: "shows".into(),
            file_name: "episode.mp4".into(),
            status: TaskStatus::Queued,
            progress: None,
            error: None,
            created_at: 1,
            started_at: None,
            finished_at: None,
        }
    }

    #[tokio::test]
    async fn clears_completed_tasks_and_releases_their_output() {
        let store = TaskStore::default();
        store.reserve_and_insert(task("one")).await.unwrap();
        assert!(store.reserve_and_insert(task("two")).await.is_err());

        store
            .set_finished("one", TaskStatus::Completed, 2, None)
            .await
            .unwrap();
        assert!(store.list().await.is_empty());

        store.reserve_and_insert(task("two")).await.unwrap();
        assert!(store.reserve_and_insert(task("three")).await.is_err());
        store
            .set_finished("two", TaskStatus::Failed, 3, Some("failed".into()))
            .await
            .unwrap();
        store.delete_inactive("two").await.unwrap();
        store.reserve_and_insert(task("three")).await.unwrap();
    }

    #[tokio::test]
    async fn failed_tasks_can_be_retried() {
        let store = TaskStore::default();
        store.reserve_and_insert(task("one")).await.unwrap();
        store
            .set_finished("one", TaskStatus::Failed, 2, Some("failed".into()))
            .await
            .unwrap();
        store.reset_for_retry("one").await.unwrap();

        let task = store.get("one").await.unwrap();
        assert_eq!(task.status, TaskStatus::Queued);
        assert_eq!(task.error, None);
    }

    #[tokio::test]
    async fn retry_rejects_a_task_that_is_not_currently_retryable() {
        let store = TaskStore::default();
        store.reserve_and_insert(task("one")).await.unwrap();

        assert!(matches!(
            store.reset_for_retry("one").await,
            Err(AppError::InvalidState)
        ));
        assert_eq!(store.get("one").await.unwrap().status, TaskStatus::Queued);
    }

    #[tokio::test]
    async fn delete_rejects_active_tasks_without_releasing_the_output() {
        let store = TaskStore::default();
        store.reserve_and_insert(task("one")).await.unwrap();

        assert!(matches!(
            store.delete_inactive("one").await,
            Err(AppError::Validation(_))
        ));
        assert!(store.get("one").await.is_ok());
        assert!(store.reserve_and_insert(task("two")).await.is_err());
    }

    #[tokio::test]
    async fn concurrent_retry_and_delete_cannot_both_succeed() {
        let store = std::sync::Arc::new(TaskStore::default());
        store.reserve_and_insert(task("one")).await.unwrap();
        store
            .set_finished("one", TaskStatus::Failed, 2, Some("failed".into()))
            .await
            .unwrap();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));

        let retry = {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                store.reset_for_retry("one").await
            })
        };
        let delete = {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                store.delete_inactive("one").await
            })
        };
        barrier.wait().await;

        let retry = retry.await.unwrap();
        let delete = delete.await.unwrap();
        assert_ne!(retry.is_ok(), delete.is_ok());
        match (retry, delete) {
            (Ok(task), Err(AppError::Validation(_))) => {
                assert_eq!(task.status, TaskStatus::Queued);
                assert_eq!(store.get("one").await.unwrap().status, TaskStatus::Queued);
            }
            (Err(AppError::NotFound), Ok(task)) => {
                assert_eq!(task.status, TaskStatus::Failed);
                assert!(matches!(store.get("one").await, Err(AppError::NotFound)));
            }
            (retry, delete) => panic!("unexpected results: {retry:?}, {delete:?}"),
        }
    }
}
