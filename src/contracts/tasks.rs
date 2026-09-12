use dioxus::fullstack::serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Downloading,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "等待中",
            Self::Downloading => "下载中",
            Self::Completed => "已完成",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
            Self::Interrupted => "已中断",
        }
    }

    pub fn can_retry(self) -> bool {
        matches!(self, Self::Failed | Self::Cancelled | Self::Interrupted)
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Downloading)
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        };
        formatter.write_str(value)
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "downloading" => Ok(Self::Downloading),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(format!("unknown task status: {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
pub struct TaskProgress {
    pub processed_duration_ms: u64,
    pub total_duration_ms: Option<u64>,
    pub percentage: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
#[serde(rename_all = "snake_case")]
pub enum TaskEvent {
    Progress { id: String, progress: TaskProgress },
    Changed { id: String },
}
