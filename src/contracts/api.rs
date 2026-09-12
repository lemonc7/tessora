use dioxus::fullstack::serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
pub struct CreateTaskRequest {
    pub url: String,
    pub directory: String,
    pub file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "dioxus::fullstack::serde")]
pub struct DirectoryEntry {
    pub value: String,
    pub label: String,
}
