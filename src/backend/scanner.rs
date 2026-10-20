use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::backend::error::{AppError, Result};

const MAX_SCAN_DEPTH: usize = 32;
const MAX_SCAN_FILES: usize = 100_000;

#[derive(Debug)]
pub struct ScannedMediaFile {
    pub directory: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub modified_at: i64,
    pub stable: bool,
}

pub struct ScanResult {
    pub files: Vec<ScannedMediaFile>,
    pub skipped: usize,
}

#[derive(Clone)]
pub struct MediaScanner {
    root: PathBuf,
    settle_seconds: u64,
}

impl MediaScanner {
    pub fn new(root: PathBuf, settle_seconds: u64) -> Self {
        Self {
            root,
            settle_seconds,
        }
    }

    pub async fn scan(&self) -> Result<ScanResult> {
        let root = self.root.clone();
        let settle_seconds = self.settle_seconds;
        tokio::task::spawn_blocking(move || scan_root(&root, settle_seconds))
            .await
            .map_err(|error| AppError::Internal(format!("媒体扫描任务异常结束：{error}")))?
    }
}

fn scan_root(root: &Path, settle_seconds: u64) -> Result<ScanResult> {
    let mut result = ScanResult {
        files: Vec::new(),
        skipped: 0,
    };
    scan_directory(root, root, 0, settle_seconds, &mut result)?;
    Ok(result)
}

fn scan_directory(
    root: &Path,
    current: &Path,
    depth: usize,
    settle_seconds: u64,
    result: &mut ScanResult,
) -> Result<()> {
    if depth >= MAX_SCAN_DEPTH {
        return Err(AppError::Internal(format!(
            "媒体目录层级超过 {MAX_SCAN_DEPTH} 层"
        )));
    }
    if result.files.len() >= MAX_SCAN_FILES {
        return Err(AppError::Internal(format!(
            "媒体文件数量超过 {MAX_SCAN_FILES} 个"
        )));
    }

    let mut entries = std::fs::read_dir(current)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            result.skipped += 1;
            continue;
        }
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            result.skipped += 1;
            continue;
        };
        if name.starts_with('.') || name.ends_with(".importing") {
            result.skipped += 1;
            continue;
        }
        if file_type.is_dir() {
            scan_directory(root, &path, depth + 1, settle_seconds, result)?;
            continue;
        }
        if !file_type.is_file() || !name.to_ascii_lowercase().ends_with(".mp4") {
            result.skipped += 1;
            continue;
        }
        if result.files.len() >= MAX_SCAN_FILES {
            return Err(AppError::Internal(format!(
                "媒体文件数量超过 {MAX_SCAN_FILES} 个"
            )));
        }

        let relative = path
            .strip_prefix(root)
            .map_err(|_| AppError::Internal("无法生成媒体文件相对路径".into()))?;
        let Some(file_name) = relative.file_name().and_then(|value| value.to_str()) else {
            result.skipped += 1;
            continue;
        };
        let directory = relative.parent().unwrap_or_else(|| Path::new(""));
        let Some(directory) = path_for_storage(directory) else {
            result.skipped += 1;
            continue;
        };
        let metadata = entry.metadata()?;
        let modified = metadata.modified().ok();
        let modified_at = modified
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64)
            .unwrap_or_default();
        let stable = SystemTime::now()
            .duration_since(modified.unwrap_or(SystemTime::now()))
            .map(|age| age.as_secs() >= settle_seconds)
            .unwrap_or(false);
        result.files.push(ScannedMediaFile {
            directory,
            file_name: file_name.to_owned(),
            size_bytes: metadata.len(),
            modified_at,
            stable,
        });
    }
    Ok(())
}

fn path_for_storage(path: &Path) -> Option<String> {
    path.components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scans_mp4_files_recursively_and_skips_hidden_files() {
        let root = std::env::temp_dir().join(format!("tessora-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("shows/season-1")).unwrap();
        std::fs::write(root.join("movie.mp4"), b"movie").unwrap();
        std::fs::write(root.join("shows/season-1/01.MP4"), b"episode").unwrap();
        std::fs::write(root.join("notes.txt"), b"notes").unwrap();
        std::fs::write(root.join(".partial.mp4"), b"partial").unwrap();

        let result = MediaScanner::new(root.clone(), 0).scan().await.unwrap();
        assert_eq!(result.files.len(), 2);
        assert!(result.files.iter().any(|file| file.directory.is_empty()));
        assert!(
            result
                .files
                .iter()
                .any(|file| file.directory == "shows/season-1")
        );

        std::fs::remove_dir_all(root).unwrap();
    }
}
