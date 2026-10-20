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
        let output = {
            let task = state.tasks.get_mut(id).ok_or(AppError::NotFound)?;
            task.status = status;
            task.finished_at = Some(timestamp);
            task.error = error;
            task.progress = None;
            (task.directory.clone(), task.file_name.clone())
        };
        if status == TaskStatus::Completed
            && state
                .reserved_outputs
                .get(&output)
                .is_some_and(|owner| owner == id)
        {
            state.reserved_outputs.remove(&output);
        }
        Ok(())
    }

    pub async fn reset_for_retry(&self, id: &str) -> Result<()> {
        let mut state = self.state.write().await;
        let task = state.tasks.get_mut(id).ok_or(AppError::NotFound)?;
        task.status = TaskStatus::Queued;
        task.error = None;
        task.progress = None;
        task.started_at = None;
        task.finished_at = None;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<DownloadTask> {
        let mut state = self.state.write().await;
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
    async fn reserves_output_until_completion_or_deletion() {
        let store = TaskStore::default();
        store.reserve_and_insert(task("one")).await.unwrap();
        assert!(store.reserve_and_insert(task("two")).await.is_err());

        store
            .set_finished("one", TaskStatus::Completed, 2, None)
            .await
            .unwrap();
        store.reserve_and_insert(task("two")).await.unwrap();
        store.delete("one").await.unwrap();
        assert!(store.reserve_and_insert(task("three")).await.is_err());
        store.delete("two").await.unwrap();
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
}
