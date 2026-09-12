use std::{collections::VecDeque, path::Path, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::{ChildStderr, ChildStdout, Command},
    sync::oneshot,
    time::Instant,
};

use crate::backend::error::{AppError, Result};

#[derive(Debug)]
pub struct DownloadProgress {
    pub duration_ms: u64,
}

pub struct FfmpegRunner<'a> {
    ffmpeg_binary: &'a str,
    ffprobe_binary: &'a str,
}

impl<'a> FfmpegRunner<'a> {
    pub fn new(ffmpeg_binary: &'a str, ffprobe_binary: &'a str) -> Self {
        Self {
            ffmpeg_binary,
            ffprobe_binary,
        }
    }

    pub async fn probe_duration(&self, url: &str) -> Option<u64> {
        let mut command = Command::new(self.ffprobe_binary);
        command
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "http,https,tcp,tls,crypto",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                url,
            ])
            .kill_on_drop(true);
        let probe = command.output();
        let output = tokio::time::timeout(Duration::from_secs(30), probe)
            .await
            .ok()?
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let seconds = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .ok()?;
        if !seconds.is_finite() || seconds <= 0.0 {
            return None;
        }
        Some((seconds * 1_000.0).round() as u64)
    }

    pub async fn run<F>(
        &self,
        url: &str,
        temporary_output: &Path,
        mut cancellation: oneshot::Receiver<()>,
        mut on_progress: F,
    ) -> Result<RunOutcome>
    where
        F: FnMut(DownloadProgress) -> Result<()>,
    {
        let mut child = Command::new(self.ffmpeg_binary)
            .args([
                "-hide_banner",
                "-nostdin",
                "-y",
                "-protocol_whitelist",
                "http,https,tcp,tls,crypto",
                "-reconnect",
                "1",
                "-reconnect_streamed",
                "1",
                "-reconnect_delay_max",
                "5",
                "-i",
                url,
                "-map",
                "0:v:0?",
                "-map",
                "0:a:0?",
                "-c",
                "copy",
                "-movflags",
                "+faststart",
                "-progress",
                "pipe:1",
                "-nostats",
                "-f",
                "mp4",
            ])
            .arg(temporary_output)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                AppError::Internal(format!("无法启动 FFmpeg，请确认它已经安装：{error}"))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::Internal("无法读取 FFmpeg 进度".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::Internal("无法读取 FFmpeg 错误输出".into()))?;
        let stderr_task = tokio::spawn(collect_stderr(stderr));

        let mut lines = BufReader::new(stdout).lines();
        let mut progress = DownloadProgress { duration_ms: 0 };
        let mut last_saved = Instant::now() - Duration::from_secs(2);

        loop {
            tokio::select! {
                _ = &mut cancellation => {
                    child.kill().await.ok();
                    child.wait().await.ok();
                    return Ok(RunOutcome::Cancelled);
                }
                line = next_line(&mut lines) => {
                    let Some(line) = line? else { break };
                    if let Some((key, value)) = line.split_once('=') {
                        match key {
                            "out_time_us" | "out_time_ms" => {
                                // FFmpeg currently reports this field in microseconds despite its legacy name.
                                progress.duration_ms = value.parse::<u64>().unwrap_or(0) / 1_000;
                            }
                            _ => {}
                        }
                        if last_saved.elapsed() >= Duration::from_secs(1) || value == "end" {
                            on_progress(DownloadProgress {
                                duration_ms: progress.duration_ms,
                            })?;
                            last_saved = Instant::now();
                        }
                    }
                }
            }
        }

        let status = child.wait().await?;
        let stderr = stderr_task
            .await
            .unwrap_or_else(|_| "FFmpeg 日志读取任务异常结束".into());
        if !status.success() {
            let message = if stderr.trim().is_empty() {
                format!("FFmpeg 退出，状态码：{status}")
            } else {
                stderr
            };
            return Err(AppError::Internal(message));
        }
        Ok(RunOutcome::Completed)
    }
}

#[derive(Clone, Copy)]
pub enum RunOutcome {
    Completed,
    Cancelled,
}

async fn next_line(lines: &mut Lines<BufReader<ChildStdout>>) -> std::io::Result<Option<String>> {
    lines.next_line().await
}

async fn collect_stderr(stderr: ChildStderr) -> String {
    let mut lines = BufReader::new(stderr).lines();
    let mut tail = VecDeque::with_capacity(24);
    while let Ok(Some(line)) = lines.next_line().await {
        if tail.len() == 24 {
            tail.pop_front();
        }
        tail.push_back(sanitize_log_line(&line));
    }
    tail.into_iter().collect::<Vec<_>>().join("\n")
}

fn sanitize_log_line(line: &str) -> String {
    const SENSITIVE_HEADERS: &[&str] = &[
        "authorization",
        "proxy-authorization",
        "cookie",
        "set-cookie",
        "x-api-key",
    ];

    let lowercase = line.to_ascii_lowercase();
    for header in SENSITIVE_HEADERS {
        let needle = format!("{header}:");
        if let Some(start) = lowercase.find(&needle)
            && (start == 0
                || line[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|character| character.is_whitespace() || character == ']'))
        {
            let value_start = start + needle.len();
            return format!("{} [已隐藏]", &line[..value_start]);
        }
    }

    sanitize_urls(line)
}

fn sanitize_urls(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut remaining = line;

    while let Some(start) = find_url_start(remaining) {
        result.push_str(&remaining[..start]);
        let candidate = &remaining[start..];
        let end = candidate
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '\'' | '"' | '<' | '>' | '[' | ']')
            })
            .unwrap_or(candidate.len());
        let raw_url = &candidate[..end];
        result.push_str(&sanitize_url(raw_url));
        remaining = &candidate[end..];
    }

    result.push_str(remaining);
    result
}

fn find_url_start(value: &str) -> Option<usize> {
    let lowercase = value.to_ascii_lowercase();
    ["http://", "https://"]
        .into_iter()
        .filter_map(|scheme| lowercase.find(scheme))
        .min()
}

fn sanitize_url(raw_url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(raw_url) else {
        return "[链接已隐藏]".into();
    };
    let had_query = parsed.query().is_some();
    let had_fragment = parsed.fragment().is_some();
    parsed.set_username("").ok();
    parsed.set_password(None).ok();
    parsed.set_query(None);
    parsed.set_fragment(None);

    let mut sanitized = parsed.to_string();
    if had_query {
        sanitized.push_str("?[已隐藏]");
    }
    if had_fragment {
        sanitized.push_str("#[已隐藏]");
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::sanitize_log_line;

    #[test]
    fn hides_url_credentials_query_and_fragment() {
        let sanitized = sanitize_log_line(
            "failed to open 'https://alice:secret@example.com/live/index.m3u8?token=abc#key'",
        );

        assert!(
            sanitized.contains("https://example.com/live/index.m3u8?[已隐藏]#[已隐藏]"),
            "unexpected sanitized log: {sanitized}"
        );
        assert!(!sanitized.contains("alice"));
        assert!(!sanitized.contains("secret"));
        assert!(!sanitized.contains("token"));
        assert!(!sanitized.contains("abc"));
        assert!(!sanitized.contains("key"));
    }

    #[test]
    fn hides_sensitive_header_values_case_insensitively() {
        assert_eq!(
            sanitize_log_line("  Authorization: Bearer very-secret-token"),
            "  Authorization: [已隐藏]"
        );
        assert_eq!(
            sanitize_log_line("cookie: session=very-secret-cookie"),
            "cookie: [已隐藏]"
        );
        assert_eq!(
            sanitize_log_line("[http @ 0x123] X-Api-Key: very-secret-key"),
            "[http @ 0x123] X-Api-Key: [已隐藏]"
        );
    }

    #[test]
    fn keeps_useful_non_sensitive_context() {
        assert_eq!(
            sanitize_log_line("HTTP error 404 at https://example.com/video/segment.ts"),
            "HTTP error 404 at https://example.com/video/segment.ts"
        );
    }
}
