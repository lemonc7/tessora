use std::{env, path::PathBuf};

use crate::backend::error::{AppError, Result};

#[derive(Debug)]
pub struct Config {
    pub download_root: PathBuf,
    pub database_path: PathBuf,
    pub ffmpeg_bin: String,
    pub ffprobe_bin: String,
    pub max_concurrent_downloads: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let download_root = env::var_os("DOWNLOAD_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("downloads"));
        std::fs::create_dir_all(&download_root)?;
        let download_root = download_root.canonicalize()?;

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

        Ok(Self {
            download_root,
            database_path,
            ffmpeg_bin: env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()),
            ffprobe_bin: env::var("FFPROBE_BIN").unwrap_or_else(|_| "ffprobe".into()),
            max_concurrent_downloads,
        })
    }
}
