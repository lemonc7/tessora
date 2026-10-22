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
const MAX_SCAN_ENTRIES: usize = 500_000;
const MAX_SEARCH_RESULTS: usize = 200;

#[derive(Clone, Copy)]
struct ScanLimits {
    depth: usize,
    files: usize,
    entries: usize,
}

const SCAN_LIMITS: ScanLimits = ScanLimits {
    depth: MAX_SCAN_DEPTH,
    files: MAX_SCAN_FILES,
    entries: MAX_SCAN_ENTRIES,
};

#[derive(Default)]
struct ScanState {
    files: Vec<MediaFile>,
    skipped: usize,
    entries: usize,
}

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
        if query.is_empty() {
            return Vec::new();
        }

        self.files
            .read()
            .await
            .iter()
            .filter(|file| file.file_name.to_lowercase().contains(&query))
            .take(MAX_SEARCH_RESULTS)
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
    scan_root_with_limits(root, settle_seconds, known_files, SCAN_LIMITS)
}

fn scan_root_with_limits(
    root: &Path,
    settle_seconds: u64,
    known_files: &HashSet<(String, String)>,
    limits: ScanLimits,
) -> Result<(Vec<MediaFile>, usize)> {
    let mut state = ScanState::default();
    scan_directory(
        root,
        root,
        0,
        settle_seconds,
        known_files,
        limits,
        &mut state,
    )?;
    Ok((state.files, state.skipped))
}

fn scan_directory(
    root: &Path,
    current: &Path,
    depth: usize,
    settle_seconds: u64,
    known_files: &HashSet<(String, String)>,
    limits: ScanLimits,
    state: &mut ScanState,
) -> Result<()> {
    if depth >= limits.depth {
        return Err(AppError::Internal(format!(
            "媒体目录层级超过 {} 层",
            limits.depth
        )));
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(current)? {
        if state.entries >= limits.entries {
            return Err(AppError::Internal(format!(
                "扫描的目录项数量超过 {} 个",
                limits.entries
            )));
        }
        state.entries += 1;
        entries.push(entry?);
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            state.skipped += 1;
            continue;
        };
        if file_type.is_symlink() || name.starts_with('.') || name.ends_with(".importing") {
            state.skipped += 1;
            continue;
        }
        if file_type.is_dir() {
            scan_directory(
                root,
                &path,
                depth + 1,
                settle_seconds,
                known_files,
                limits,
                state,
            )?;
            continue;
        }
        if !file_type.is_file() || !name.to_ascii_lowercase().ends_with(".mp4") {
            state.skipped += 1;
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .map_err(|_| AppError::Internal("无法生成媒体文件相对路径".into()))?;
        let Some(file_name) = relative.file_name().and_then(|value| value.to_str()) else {
            state.skipped += 1;
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
            state.skipped += 1;
            continue;
        };
        let metadata = entry.metadata()?;
        let modified = metadata.modified().ok();
        let stable = SystemTime::now()
            .duration_since(modified.unwrap_or(SystemTime::now()))
            .map(|age| age.as_secs() >= settle_seconds)
            .unwrap_or(false);
        if !stable && !known_files.contains(&(directory.clone(), file_name.to_owned())) {
            state.skipped += 1;
            continue;
        }
        let modified_at = modified
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64)
            .unwrap_or_default();
        if state.files.len() >= limits.files {
            return Err(AppError::Internal(format!(
                "媒体文件数量超过 {} 个",
                limits.files
            )));
        }
        state.files.push(MediaFile {
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

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tessora-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn scans_and_searches_media_files() {
        let root = test_root("scan");
        std::fs::create_dir_all(root.join("shows/season-1")).unwrap();
        std::fs::write(root.join("movie.mp4"), b"movie").unwrap();
        std::fs::write(root.join("shows/season-1/01.MP4"), b"episode").unwrap();
        std::fs::write(root.join("notes.txt"), b"notes").unwrap();

        let catalog = MediaCatalog::new(root.clone(), 0);
        let report = catalog.scan().await.unwrap();
        assert_eq!(report.files, 2);
        assert!(catalog.search("  ").await.is_empty());
        assert!(catalog.search("SEASON-1").await.is_empty());
        assert_eq!(catalog.search("01.mp4").await.len(), 1);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enforces_media_file_limit_in_a_single_directory() {
        let root = test_root("file-limit");
        std::fs::create_dir(&root).unwrap();
        for index in 0..3 {
            std::fs::write(root.join(format!("{index}.mp4")), b"media").unwrap();
        }

        let result = scan_root_with_limits(
            &root,
            0,
            &HashSet::new(),
            ScanLimits {
                depth: MAX_SCAN_DEPTH,
                files: 2,
                entries: 10,
            },
        );

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("媒体文件数量超过 2 个")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enforces_total_entry_limit_for_non_media_files() {
        let root = test_root("entry-limit");
        std::fs::create_dir(&root).unwrap();
        for index in 0..3 {
            std::fs::write(root.join(format!("{index}.txt")), b"not media").unwrap();
        }

        let result = scan_root_with_limits(
            &root,
            0,
            &HashSet::new(),
            ScanLimits {
                depth: MAX_SCAN_DEPTH,
                files: 10,
                entries: 2,
            },
        );

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("扫描的目录项数量超过 2 个")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn limits_search_results() {
        let catalog = MediaCatalog::new(PathBuf::new(), 0);
        let mut files = catalog.files.write().await;
        for index in 0..(MAX_SEARCH_RESULTS + 1) {
            files.push(MediaFile {
                directory: "shows".to_owned(),
                file_name: format!("episode-{index}.mp4"),
                size_bytes: 0,
                modified_at: 0,
            });
        }
        drop(files);

        assert_eq!(catalog.search("episode").await.len(), MAX_SEARCH_RESULTS);
    }
}
