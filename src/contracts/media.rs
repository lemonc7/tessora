use dioxus::fullstack::serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
#[serde(rename_all = "snake_case")]
pub enum MediaOrigin {
    Tessora,
    External,
}

impl MediaOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tessora => "Tessora",
            Self::External => "外部添加",
        }
    }
}

impl std::fmt::Display for MediaOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Tessora => "tessora",
            Self::External => "external",
        })
    }
}

impl std::str::FromStr for MediaOrigin {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "tessora" => Ok(Self::Tessora),
            "external" => Ok(Self::External),
            _ => Err(format!("unknown media origin: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
#[serde(rename_all = "snake_case")]
pub enum MediaState {
    Present,
    Missing,
}

impl std::fmt::Display for MediaState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Present => "present",
            Self::Missing => "missing",
        })
    }
}

impl std::str::FromStr for MediaState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "present" => Ok(Self::Present),
            "missing" => Ok(Self::Missing),
            _ => Err(format!("unknown media state: {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
pub struct MediaFile {
    pub id: String,
    pub directory: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub modified_at: i64,
    pub origin: MediaOrigin,
    pub state: MediaState,
    pub source_task_id: Option<String>,
    pub content_hash: Option<String>,
    pub discovered_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
pub struct ScanReport {
    pub discovered: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub missing: usize,
    pub skipped: usize,
}
