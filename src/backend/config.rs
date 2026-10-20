use std::{env, path::PathBuf};

use crate::backend::error::{AppError, Result};

#[derive(Debug)]
pub struct Config {
    pub download_root: PathBuf,
    pub temporary_root: PathBuf,
    pub database_path: PathBuf,
    pub ffmpeg_bin: String,
    pub concurrent_fragments: usize,
    pub max_concurrent_downloads: usize,
    pub media_scan_interval_seconds: u64,
    pub media_settle_seconds: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let download_root = env::var_os("DOWNLOAD_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("downloads"));
        std::fs::create_dir_all(&download_root)?;
        let download_root = download_root.canonicalize()?;

        let temporary_root = env::var_os("TEMP_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| download_root.clone());
        std::fs::create_dir_all(&temporary_root)?;

        let database_path = env::var_os("DATABASE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data/tessora.db"));
        if let Some(parent) = database_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let max_concurrent_downloads = env::var("MAX_CONCURRENT_DOWNLOADS")
            .unwrap_or_else(|_| "2".into())
            .parse::<usize>()
            .map_err(|_| AppError::Config("MAX_CONCURRENT_DOWNLOADS must be a number".into()))?;
        if !(1..=16).contains(&max_concurrent_downloads) {
            return Err(AppError::Config(
                "MAX_CONCURRENT_DOWNLOADS must be between 1 and 16".into(),
            ));
        }

        let concurrent_fragments = env::var("CONCURRENT_FRAGMENTS")
            .unwrap_or_else(|_| "8".into())
            .parse::<usize>()
            .map_err(|_| AppError::Config("CONCURRENT_FRAGMENTS must be a number".into()))?;
        if !(1..=32).contains(&concurrent_fragments) {
            return Err(AppError::Config(
                "CONCURRENT_FRAGMENTS must be between 1 and 32".into(),
            ));
        }

        let media_scan_interval_seconds = env::var("MEDIA_SCAN_INTERVAL_SECONDS")
            .unwrap_or_else(|_| "300".into())
            .parse::<u64>()
            .map_err(|_| AppError::Config("MEDIA_SCAN_INTERVAL_SECONDS must be a number".into()))?;
        if media_scan_interval_seconds > 86_400 {
            return Err(AppError::Config(
                "MEDIA_SCAN_INTERVAL_SECONDS must be between 0 and 86400".into(),
            ));
        }

        let media_settle_seconds = env::var("MEDIA_SETTLE_SECONDS")
            .unwrap_or_else(|_| "60".into())
            .parse::<u64>()
            .map_err(|_| AppError::Config("MEDIA_SETTLE_SECONDS must be a number".into()))?;
        if media_settle_seconds > 3_600 {
            return Err(AppError::Config(
                "MEDIA_SETTLE_SECONDS must be between 0 and 3600".into(),
            ));
        }

        Ok(Self {
            download_root,
            temporary_root,
            database_path,
            ffmpeg_bin: env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()),
            concurrent_fragments,
            max_concurrent_downloads,
            media_scan_interval_seconds,
            media_settle_seconds,
        })
    }
}
