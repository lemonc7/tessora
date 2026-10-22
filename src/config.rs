use std::{env, path::PathBuf};

use crate::error::{AppError, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub download_root: PathBuf,
    pub temporary_root: PathBuf,
    pub ffmpeg_bin: String,
    pub concurrent_fragments: usize,
    pub max_concurrent_downloads: usize,
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
            .unwrap_or_else(|| download_root.join(".tessora-temp"));
        std::fs::create_dir_all(&temporary_root)?;
        let temporary_root = temporary_root.canonicalize()?;

        let max_concurrent_downloads = parse_bounded_usize("MAX_CONCURRENT_DOWNLOADS", 2, 1..=16)?;
        let concurrent_fragments = parse_bounded_usize("CONCURRENT_FRAGMENTS", 8, 1..=32)?;

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
            ffmpeg_bin: env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()),
            concurrent_fragments,
            max_concurrent_downloads,
            media_settle_seconds,
        })
    }
}

fn parse_bounded_usize(
    name: &str,
    default: usize,
    range: std::ops::RangeInclusive<usize>,
) -> Result<usize> {
    let value = env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<usize>()
        .map_err(|_| AppError::Config(format!("{name} must be a number")))?;
    if !range.contains(&value) {
        return Err(AppError::Config(format!(
            "{name} must be between {} and {}",
            range.start(),
            range.end()
        )));
    }
    Ok(value)
}
