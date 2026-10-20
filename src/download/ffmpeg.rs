use std::{path::Path, process::Stdio};

use tokio::process::Command;

use crate::error::{AppError, Result};

pub struct FfmpegRunner<'a> {
    ffmpeg_binary: &'a str,
}

impl<'a> FfmpegRunner<'a> {
    pub fn new(ffmpeg_binary: &'a str) -> Self {
        Self { ffmpeg_binary }
    }

    /// Remuxes a raw stream (e.g. concatenated TS/fMP4 segments) into a standalone MP4.
    pub async fn remux_to_mp4(&self, input: &Path, output: &Path) -> Result<()> {
        let input = input
            .to_str()
            .ok_or_else(|| AppError::Internal("输入路径包含非 UTF-8 字符".into()))?;
        let output = output
            .to_str()
            .ok_or_else(|| AppError::Internal("输出路径包含非 UTF-8 字符".into()))?;

        let result = Command::new(self.ffmpeg_binary)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-i",
                input,
                "-c",
                "copy",
                "-movflags",
                "+faststart",
                "-f",
                "mp4",
                output,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|error| {
                AppError::Internal(format!("无法启动 FFmpeg，请确认它已经安装：{error}"))
            })?;

        if !result.status.success() {
            let message = String::from_utf8_lossy(&result.stderr);
            let message = message.trim();
            return Err(AppError::Internal(if message.is_empty() {
                format!("FFmpeg 转换失败，退出码：{}", result.status)
            } else {
                format!("FFmpeg 转换失败：{message}")
            }));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub enum RunOutcome {
    Completed,
    Cancelled,
}
