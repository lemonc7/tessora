use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::sync::{Mutex, RwLock};

use crate::{
    domain::{MediaFile, ScanReport},
    error::{AppError, Result},
};

const MAX_SCAN_DEPTH: usize = 32;
const MAX_SCAN_FILES: usize = 100_000;

pub struct MediaCatalog {
    root: PathBuf,
    settle_seconds: u64,
    files: RwLock<Vec<MediaFile>>,
    scan_lock: Mutex<()>,
}

impl MediaCatalog {
    pub fn new(root: PathBuf, settle_seconds: u64) -> Self {
        Self {
            root,
            settle_seconds,
            files: RwLock::new(Vec::new()),
            scan_lock: Mutex::new(()),
        }
    }

    pub async fn list(&self) -> Vec<MediaFile> {
        self.files.read().await.clone()
    }

    pub async fn search(&self, query: &str) -> Vec<MediaFile> {
        let query = query.trim().to_lowercase();
        self.files
            .read()
            .await
            .iter()
            .filter(|file| query.is_empty() || file.relative_path().to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    pub async fn scan(&self) -> Result<ScanReport> {
        let _guard = self
            .scan_lock
            .try_lock()
            .map_err(|_| AppError::Validation("媒体库正在扫描，请稍后再试".into()))?;
        let root = self.root.clone();
        let settle_seconds = self.settle_seconds;
        let known_files = self
            .files
            .read()
            .await
            .iter()
            .map(|file| (file.directory.clone(), file.file_name.clone()))
            .collect();
        let (files, skipped) =
            tokio::task::spawn_blocking(move || scan_root(&root, settle_seconds, &known_files))
                .await
                .map_err(|error| AppError::Internal(format!("媒体扫描任务异常结束：{error}")))??;

        let report = ScanReport {
            files: files.len(),
            skipped,
        };
        *self.files.write().await = files;
        Ok(report)
    }

    pub async fn register(&self, file: MediaFile) {
        let mut files = self.files.write().await;
        if let Some(existing) = files.iter_mut().find(|existing| {
            existing.directory == file.directory && existing.file_name == file.file_name
        }) {
            *existing = file;
        } else {
            files.push(file);
        }
        files.sort_by_key(MediaFile::relative_path);
    }

    pub async fn remove(&self, directory: &str, file_name: &str) {
        self.files
            .write()
            .await
            .retain(|file| file.directory != directory || file.file_name != file_name);
    }
}

fn scan_root(
    root: &Path,
    settle_seconds: u64,
    known_files: &HashSet<(String, String)>,
) -> Result<(Vec<MediaFile>, usize)> {
    let mut files = Vec::new();
    let mut skipped = 0;
    scan_directory(
        root,
        root,
        0,
        settle_seconds,
        known_files,
        &mut files,
        &mut skipped,
    )?;
    Ok((files, skipped))
}

fn scan_directory(
    root: &Path,
    current: &Path,
    depth: usize,
    settle_seconds: u64,
    known_files: &HashSet<(String, String)>,
    files: &mut Vec<MediaFile>,
    skipped: &mut usize,
) -> Result<()> {
    if depth >= MAX_SCAN_DEPTH {
        return Err(AppError::Internal(format!(
            "媒体目录层级超过 {MAX_SCAN_DEPTH} 层"
        )));
    }
    if files.len() >= MAX_SCAN_FILES {
        return Err(AppError::Internal(format!(
            "媒体文件数量超过 {MAX_SCAN_FILES} 个"
        )));
    }

    let mut entries = std::fs::read_dir(current)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            *skipped += 1;
            continue;
        };
        if file_type.is_symlink() || name.starts_with('.') || name.ends_with(".importing") {
            *skipped += 1;
            continue;
        }
        if file_type.is_dir() {
            scan_directory(
                root,
                &path,
                depth + 1,
                settle_seconds,
                known_files,
                files,
                skipped,
            )?;
            continue;
        }
        if !file_type.is_file() || !name.to_ascii_lowercase().ends_with(".mp4") {
            *skipped += 1;
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .map_err(|_| AppError::Internal("无法生成媒体文件相对路径".into()))?;
        let Some(file_name) = relative.file_name().and_then(|value| value.to_str()) else {
            *skipped += 1;
            continue;
        };
        let Some(directory) = relative
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .components()
            .map(|part| part.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("/"))
        else {
            *skipped += 1;
            continue;
        };
        let metadata = entry.metadata()?;
        let modified = metadata.modified().ok();
        let stable = SystemTime::now()
            .duration_since(modified.unwrap_or(SystemTime::now()))
            .map(|age| age.as_secs() >= settle_seconds)
            .unwrap_or(false);
        if !stable && !known_files.contains(&(directory.clone(), file_name.to_owned())) {
            *skipped += 1;
            continue;
        }
        let modified_at = modified
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64)
            .unwrap_or_default();
        files.push(MediaFile {
            directory,
            file_name: file_name.to_owned(),
            size_bytes: metadata.len(),
            modified_at,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scans_and_searches_media_files() {
        let root = std::env::temp_dir().join(format!("tessora-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("shows/season-1")).unwrap();
        std::fs::write(root.join("movie.mp4"), b"movie").unwrap();
        std::fs::write(root.join("shows/season-1/01.MP4"), b"episode").unwrap();
        std::fs::write(root.join("notes.txt"), b"notes").unwrap();

        let catalog = MediaCatalog::new(root.clone(), 0);
        let report = catalog.scan().await.unwrap();
        assert_eq!(report.files, 2);
        assert_eq!(catalog.search("SEASON-1").await.len(), 1);

        std::fs::remove_dir_all(root).unwrap();
    }
}
