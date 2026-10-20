#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTask {
    pub url: String,
    pub directory: String,
    pub file_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Downloading,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn can_retry(self) -> bool {
        matches!(self, Self::Failed | Self::Cancelled)
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Downloading)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadTask {
    pub id: String,
    pub url: String,
    pub directory: String,
    pub file_name: String,
    pub status: TaskStatus,
    pub progress: Option<TaskProgress>,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskProgress {
    pub processed_duration_ms: u64,
    pub total_duration_ms: Option<u64>,
    pub percentage: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskEvent {
    Progress { id: String, progress: TaskProgress },
    Changed { id: String },
    MediaChanged,
}
