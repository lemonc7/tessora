use std::{
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::backend::error::{AppError, Result};

pub fn now_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn validate_url(value: &str) -> Result<String> {
    let value = value.trim();
    if value.len() > 4096 {
        return Err(AppError::Validation("下载链接过长".into()));
    }
    let parsed =
        url::Url::parse(value).map_err(|_| AppError::Validation("请输入有效的 M3U8 URL".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Validation("只支持 HTTP 和 HTTPS 链接".into()));
    }
    if parsed.host_str().is_none() {
        return Err(AppError::Validation("下载链接缺少主机名".into()));
    }
    Ok(value.to_owned())
}

pub fn normalize_file_name(value: &str) -> Result<String> {
    let mut value = value.trim().to_owned();
    if value.to_ascii_lowercase().ends_with(".mp4") {
        value.truncate(value.len() - 4);
        value = value.trim().to_owned();
    }
    if value.is_empty() {
        return Err(AppError::Validation("文件名不能为空".into()));
    }
    if value.chars().count() > 180 {
        return Err(AppError::Validation("文件名不能超过 180 个字符".into()));
    }
    if value == "."
        || value == ".."
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(AppError::Validation("文件名包含不允许的字符".into()));
    }
    Ok(format!("{value}.mp4"))
}

pub fn normalize_relative_directory(value: &str) -> Result<PathBuf> {
    let value = value.trim().replace('\\', "/");
    let path = Path::new(&value);
    if path.is_absolute() {
        return Err(AppError::Validation("下载目录必须是相对路径".into()));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if part.to_string_lossy().starts_with('.') {
                    return Err(AppError::Validation("目录名不能以点开头".into()));
                }
                normalized.push(part);
            }
            Component::CurDir => {}
            _ => return Err(AppError::Validation("下载目录不合法".into())),
        }
    }
    Ok(normalized)
}

pub fn resolved_directory(root: &Path, relative: &Path) -> Result<PathBuf> {
    let candidate = root.join(relative);
    std::fs::create_dir_all(&candidate)?;
    let canonical = candidate.canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(AppError::Validation("下载目录超出允许范围".into()));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_directory_traversal() {
        assert!(normalize_relative_directory("../private").is_err());
        assert!(normalize_relative_directory("videos/../../private").is_err());
        assert!(normalize_relative_directory("/private").is_err());
    }

    #[test]
    fn normalizes_mp4_suffix() {
        assert_eq!(normalize_file_name(" demo.MP4 ").unwrap(), "demo.mp4");
        assert!(normalize_file_name("../demo").is_err());
    }

    #[test]
    fn accepts_http_urls_only() {
        assert!(validate_url("https://example.com/video.m3u8").is_ok());
        assert!(validate_url("file:///tmp/video.m3u8").is_err());
    }
}
