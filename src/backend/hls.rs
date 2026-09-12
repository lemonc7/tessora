use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use aes::Aes128;
use cbc::{
    Decryptor,
    cipher::{
        BlockDecryptMut, KeyIvInit,
        block_padding::{NoPadding, Pkcs7},
    },
};
use m3u8_rs::{KeyMethod, MediaPlaylist, MediaSegment, Playlist};
use reqwest::{Client, redirect::Policy};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    sync::{Semaphore, oneshot},
    task::JoinSet,
    time::Instant,
};
use url::Url;

use crate::backend::{
    error::{AppError, Result},
    media::RunOutcome,
};

const USER_AGENT: &str = "Mozilla/5.0 (compatible; Tessora/0.1)";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_MASTER_DEPTH: usize = 3;

#[derive(Clone, Copy)]
struct SegmentKey {
    key: [u8; 16],
    iv: [u8; 16],
}

struct PreparedSegment {
    url: String,
    range: Option<(u64, u64)>,
    key: Option<SegmentKey>,
}

/// Downloads a VOD HLS stream with parallel fragment fetching. Fragments are fetched
/// concurrently (bounded by `concurrent_fragments`) and concatenated into a single raw
/// stream, which the caller then remuxes to MP4 with ffmpeg.
pub struct HlsDownloader {
    client: Client,
    concurrent_fragments: usize,
}

impl HlsDownloader {
    pub fn new(concurrent_fragments: usize) -> Result<Self> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(Policy::limited(10))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| AppError::Internal(format!("无法初始化 HTTP 客户端：{error}")))?;
        Ok(Self {
            client,
            concurrent_fragments,
        })
    }

    pub async fn download<F>(
        &self,
        url: &str,
        raw_output: &Path,
        work_dir: &Path,
        mut cancellation: oneshot::Receiver<()>,
        mut on_progress: F,
    ) -> Result<RunOutcome>
    where
        F: FnMut(f32) -> Result<()>,
    {
        let base = Url::parse(url)
            .map_err(|error| AppError::Internal(format!("无效的下载地址：{error}")))?;
        let playlist = self.resolve_media_playlist(&base).await?;
        let segments = normalize_segments(playlist);

        if segments.is_empty() {
            return Err(AppError::Internal("播放列表中没有分片".into()));
        }

        tokio::fs::create_dir_all(work_dir).await?;

        let segment_keys = self.prepare_keys(&base, &segments).await?;
        let prepared = Arc::new(prepare_segments(&base, &segments, &segment_keys)?);

        let init_bytes = self.fetch_init_map(&base, segments.first()).await?;

        let total = prepared.len();
        let semaphore = Arc::new(Semaphore::new(self.concurrent_fragments));
        let completed = Arc::new(AtomicUsize::new(0));
        let mut tasks = JoinSet::new();

        for index in 0..total {
            let prepared = Arc::clone(&prepared);
            let client = self.client.clone();
            let permit = Arc::clone(&semaphore);
            let completed = Arc::clone(&completed);
            let path = work_dir.join(format!("{index:06}"));

            tasks.spawn(async move {
                let _guard = permit
                    .acquire_owned()
                    .await
                    .map_err(|_| AppError::Internal("分片下载并发控制异常".into()))?;
                let segment = &prepared[index];
                let data = fetch_segment(&client, segment).await?;
                let mut file = File::create(&path).await?;
                file.write_all(&data).await?;
                file.flush().await?;
                completed.fetch_add(1, Ordering::Relaxed);
                Ok::<(), AppError>(())
            });
        }

        let mut last_reported = Instant::now() - Duration::from_secs(2);
        loop {
            tokio::select! {
                _ = &mut cancellation => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    let _ = tokio::fs::remove_dir_all(work_dir).await;
                    return Ok(RunOutcome::Cancelled);
                }
                result = tasks.join_next() => {
                    match result {
                        Some(Ok(Ok(()))) => {
                            let done = completed.load(Ordering::Relaxed);
                            let percentage = done as f32 / total as f32 * 100.0;
                            if done == total || last_reported.elapsed() >= Duration::from_millis(200) {
                                on_progress(percentage)?;
                                last_reported = Instant::now();
                            }
                        }
                        Some(Ok(Err(error))) => {
                            tasks.abort_all();
                            while tasks.join_next().await.is_some() {}
                            let _ = tokio::fs::remove_dir_all(work_dir).await;
                            return Err(error);
                        }
                        Some(Err(join_error)) => {
                            tasks.abort_all();
                            while tasks.join_next().await.is_some() {}
                            let _ = tokio::fs::remove_dir_all(work_dir).await;
                            return Err(AppError::Internal(format!("分片下载任务异常结束：{join_error}")));
                        }
                        None => break,
                    }
                }
            }
        }

        let mut output = File::create(raw_output).await?;
        if let Some(init) = init_bytes {
            output.write_all(&init).await?;
        }
        for index in 0..total {
            let path = work_dir.join(format!("{index:06}"));
            let data = tokio::fs::read(&path).await?;
            output.write_all(&data).await?;
        }
        output.flush().await?;
        let _ = tokio::fs::remove_dir_all(work_dir).await;

        on_progress(100.0)?;
        Ok(RunOutcome::Completed)
    }

    async fn resolve_media_playlist(&self, base: &Url) -> Result<MediaPlaylist> {
        let mut current = base.clone();
        for _ in 0..MAX_MASTER_DEPTH {
            let bytes = self.fetch_bytes(current.as_str()).await?;
            let playlist = m3u8_rs::parse_playlist_res(&bytes)
                .map_err(|_| AppError::Internal("无法解析 m3u8 播放列表".into()))?;
            match playlist {
                Playlist::MediaPlaylist(media) => return Ok(media),
                Playlist::MasterPlaylist(master) => {
                    let variant = master
                        .variants
                        .iter()
                        .filter(|variant| !variant.is_i_frame)
                        .max_by_key(|variant| variant.bandwidth)
                        .ok_or_else(|| {
                            AppError::Internal("master 播放列表中没有可用的视频流".into())
                        })?;
                    current = current
                        .join(&variant.uri)
                        .map_err(|error| AppError::Internal(format!("变体地址无效：{error}")))?;
                }
            }
        }
        Err(AppError::Internal("m3u8 播放列表嵌套层级过深".into()))
    }

    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| AppError::Internal(format!("下载失败：{error}")))?;
        if !response.status().is_success() {
            return Err(AppError::Internal(format!(
                "下载失败：HTTP {}",
                response.status()
            )));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| AppError::Internal(format!("读取响应失败：{error}")))
    }

    async fn prepare_keys(
        &self,
        base: &Url,
        segments: &[MediaSegment],
    ) -> Result<Vec<Option<SegmentKey>>> {
        let mut cache: HashMap<String, [u8; 16]> = HashMap::new();
        let mut keys = Vec::with_capacity(segments.len());

        for (index, segment) in segments.iter().enumerate() {
            let Some(key) = &segment.key else {
                keys.push(None);
                continue;
            };
            if key.method != KeyMethod::AES128 {
                keys.push(None);
                continue;
            }
            let uri = key
                .uri
                .as_ref()
                .ok_or_else(|| AppError::Internal("AES-128 加密分片缺少密钥 URI".into()))?;
            let key_bytes = match cache.get(uri) {
                Some(bytes) => *bytes,
                None => {
                    let key_url = base
                        .join(uri)
                        .map_err(|error| AppError::Internal(format!("密钥地址无效：{error}")))?;
                    let bytes = self.fetch_bytes(key_url.as_str()).await?;
                    if bytes.len() != 16 {
                        return Err(AppError::Internal(format!(
                            "密钥长度异常：应为 16 字节，实际 {} 字节",
                            bytes.len()
                        )));
                    }
                    let mut key = [0u8; 16];
                    key.copy_from_slice(&bytes);
                    cache.insert(uri.clone(), key);
                    key
                }
            };
            let iv = match &key.iv {
                Some(value) => parse_hex_16(value)
                    .ok_or_else(|| AppError::Internal("加密分片 IV 格式无效".into()))?,
                None => default_iv(index as u64),
            };
            keys.push(Some(SegmentKey { key: key_bytes, iv }));
        }
        Ok(keys)
    }

    async fn fetch_init_map(
        &self,
        base: &Url,
        first_segment: Option<&MediaSegment>,
    ) -> Result<Option<Vec<u8>>> {
        let Some(map) = first_segment.and_then(|segment| segment.map.as_ref()) else {
            return Ok(None);
        };
        let map_url = base
            .join(&map.uri)
            .map_err(|error| AppError::Internal(format!("初始化分片地址无效：{error}")))?;
        let bytes = self.fetch_bytes(map_url.as_str()).await?;
        Ok(Some(bytes))
    }
}

/// m3u8-rs attaches `#EXT-X-KEY`/`#EXT-X-MAP` only to the immediately following segment, but
/// the HLS spec says they apply to every following segment until overridden. Carry them forward.
fn normalize_segments(mut playlist: MediaPlaylist) -> Vec<MediaSegment> {
    let mut key = None;
    let mut map = None;
    for segment in &mut playlist.segments {
        if segment.key.is_some() {
            key = segment.key.clone();
        } else {
            segment.key = key.clone();
        }
        if segment.map.is_some() {
            map = segment.map.clone();
        } else {
            segment.map = map.clone();
        }
    }
    playlist.segments
}

fn prepare_segments(
    base: &Url,
    segments: &[MediaSegment],
    keys: &[Option<SegmentKey>],
) -> Result<Vec<PreparedSegment>> {
    let mut last_end: HashMap<String, u64> = HashMap::new();
    let mut prepared = Vec::with_capacity(segments.len());

    for (index, segment) in segments.iter().enumerate() {
        let url = base
            .join(&segment.uri)
            .map_err(|error| AppError::Internal(format!("分片地址无效：{error}")))?;
        let range = segment.byte_range.as_ref().map(|range| {
            let start = range
                .offset
                .unwrap_or_else(|| last_end.get(&segment.uri).copied().unwrap_or(0));
            last_end.insert(segment.uri.clone(), start + range.length);
            (start, start + range.length - 1)
        });
        prepared.push(PreparedSegment {
            url: url.to_string(),
            range,
            key: keys[index],
        });
    }
    Ok(prepared)
}

async fn fetch_segment(client: &Client, segment: &PreparedSegment) -> Result<Vec<u8>> {
    let mut request = client.get(segment.url.as_str());
    if let Some((start, end)) = segment.range {
        request = request.header(reqwest::header::RANGE, format!("bytes={start}-{end}"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::Internal(format!("下载分片失败：{error}")))?;
    if !response.status().is_success() {
        return Err(AppError::Internal(format!(
            "下载分片失败：HTTP {}",
            response.status()
        )));
    }
    let data = response
        .bytes()
        .await
        .map_err(|error| AppError::Internal(format!("读取分片失败：{error}")))?;
    match segment.key {
        Some(key) => decrypt_segment(&key.key, &key.iv, &data),
        None => Ok(data.to_vec()),
    }
}

fn decrypt_segment(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    let decryptor = || {
        Decryptor::<Aes128>::new_from_slices(&key[..], &iv[..])
            .map_err(|error| AppError::Internal(format!("AES 初始化失败：{error}")))
    };
    if let Ok(plain) = decryptor()?.decrypt_padded_vec_mut::<Pkcs7>(data) {
        return Ok(plain);
    }
    // Some packagers emit unpadded ciphertext; fall back to raw block decryption.
    decryptor()?
        .decrypt_padded_vec_mut::<NoPadding>(data)
        .map_err(|error| AppError::Internal(format!("分片解密失败：{error}")))
}

fn parse_hex_16(value: &str) -> Option<[u8; 16]> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if hex.len() != 32 {
        return None;
    }
    let bytes = hex.as_bytes();
    let mut out = [0u8; 16];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        out[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn default_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

#[cfg(test)]
mod tests {
    use super::{default_iv, normalize_segments, parse_hex_16};

    #[test]
    fn parses_hexadecimal_iv() {
        let iv = parse_hex_16("0x00000000000000000000000000000001").unwrap();
        assert_eq!(iv[15], 1);
        assert_eq!(iv[0], 0);
        assert_eq!(parse_hex_16("0xGG000000000000000000000000000001"), None);
        assert_eq!(parse_hex_16("0x1234"), None);
    }

    #[test]
    fn builds_default_iv_from_sequence() {
        let iv = default_iv(1);
        assert_eq!(iv[15], 1);
        assert!(iv[..15].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn carries_encryption_key_across_segments() {
        let playlist = m3u8_rs::parse_media_playlist_res(
            b"#EXTM3U\n\
              #EXT-X-TARGETDURATION:10\n\
              #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
              #EXTINF:10,\nseg0.ts\n\
              #EXTINF:10,\nseg1.ts\n\
              #EXTINF:10,\nseg2.ts\n\
              #EXT-X-ENDLIST\n",
        )
        .unwrap();
        let segments = normalize_segments(playlist);
        assert_eq!(segments.len(), 3);
        for segment in &segments {
            assert_eq!(
                segment.key.as_ref().unwrap().method,
                m3u8_rs::KeyMethod::AES128
            );
        }
    }
}
