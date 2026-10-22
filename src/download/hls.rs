use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use aes::Aes128;
use cbc::{
    Decryptor,
    cipher::{
        BlockModeDecrypt, KeyIvInit,
        block_padding::{NoPadding, Pkcs7},
    },
};
use m3u8_rs::{ByteRange, KeyMethod, MediaPlaylist, MediaSegment, Playlist};
use reqwest::{Client, Response, StatusCode, header::CONTENT_RANGE, redirect::Policy};
use tokio::{fs::File, io::AsyncWriteExt, sync::Semaphore, task::JoinSet};
use url::Url;

use crate::{
    download::RunOutcome,
    error::{AppError, Result},
};

const USER_AGENT: &str = "Mozilla/5.0 (compatible; Tessora/0.1)";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_MASTER_DEPTH: usize = 3;
const MAX_PLAYLIST_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SEGMENTS: usize = 50_000;
const MAX_DOWNLOAD_BYTES: u64 = 100 * 1024 * 1024 * 1024;

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
        mut on_progress: F,
    ) -> Result<RunOutcome>
    where
        F: FnMut(f32) -> Result<()>,
    {
        let base = Url::parse(url)
            .map_err(|error| AppError::Internal(format!("无效的下载地址：{error}")))?;
        let (playlist, media_url) = self.resolve_media_playlist(&base).await?;
        let media_sequence = playlist.media_sequence;
        let segments = normalize_segments(playlist);

        if segments.is_empty() {
            return Err(AppError::Internal("播放列表中没有分片".into()));
        }
        if segments.windows(2).any(|pair| pair[0].map != pair[1].map) {
            return Err(AppError::Internal(
                "暂不支持播放列表中途切换 EXT-X-MAP 初始化分片".into(),
            ));
        }
        if segments.len() > MAX_SEGMENTS {
            return Err(AppError::Internal(format!(
                "播放列表分片数量超过上限 {MAX_SEGMENTS}"
            )));
        }

        tokio::fs::create_dir_all(work_dir).await?;

        let segment_keys = self
            .prepare_keys(&media_url, media_sequence, &segments)
            .await?;
        let prepared = Arc::new(prepare_segments(&media_url, &segments, &segment_keys)?);

        let init_bytes = self.fetch_init_map(&media_url, segments.first()).await?;

        let total = prepared.len();
        let semaphore = Arc::new(Semaphore::new(self.concurrent_fragments));
        let completed = Arc::new(AtomicUsize::new(0));
        let downloaded_bytes = Arc::new(AtomicU64::new(
            init_bytes.as_ref().map_or(0, |bytes| bytes.len() as u64),
        ));
        let mut tasks = JoinSet::new();

        for index in 0..total {
            let prepared = Arc::clone(&prepared);
            let client = self.client.clone();
            let permit = Arc::clone(&semaphore);
            let completed = Arc::clone(&completed);
            let downloaded_bytes = Arc::clone(&downloaded_bytes);
            let path = work_dir.join(format!("{index:06}"));

            tasks.spawn(async move {
                let _guard = permit
                    .acquire_owned()
                    .await
                    .map_err(|_| AppError::Internal("分片下载并发控制异常".into()))?;
                let segment = &prepared[index];
                let data = fetch_segment(&client, segment, &downloaded_bytes).await?;
                let mut file = File::create(&path).await?;
                file.write_all(&data).await?;
                file.flush().await?;
                completed.fetch_add(1, Ordering::Relaxed);
                Ok::<(), AppError>(())
            });
        }

        let mut last_reported = Instant::now() - Duration::from_secs(2);
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {
                    let done = completed.load(Ordering::Relaxed);
                    let percentage = done as f32 / total as f32 * 100.0;
                    if done == total || last_reported.elapsed() >= Duration::from_millis(200) {
                        on_progress(percentage)?;
                        last_reported = Instant::now();
                    }
                }
                Ok(Err(error)) => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    let _ = tokio::fs::remove_dir_all(work_dir).await;
                    return Err(error);
                }
                Err(join_error) => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    let _ = tokio::fs::remove_dir_all(work_dir).await;
                    return Err(AppError::Internal(format!(
                        "分片下载任务异常结束：{join_error}"
                    )));
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

    async fn resolve_media_playlist(&self, base: &Url) -> Result<(MediaPlaylist, Url)> {
        let mut current = base.clone();
        for _ in 0..MAX_MASTER_DEPTH {
            let response = self
                .client
                .get(current.as_str())
                .send()
                .await
                .map_err(|error| AppError::Internal(format!("下载失败：{error}")))?;
            if !response.status().is_success() {
                return Err(AppError::Internal(format!(
                    "下载失败：HTTP {}",
                    response.status()
                )));
            }
            let response_url = response.url().clone();
            let bytes = read_response_limited(response, MAX_PLAYLIST_BYTES, "读取播放列表").await?;
            let playlist = m3u8_rs::parse_playlist_res(&bytes)
                .map_err(|_| AppError::Internal("无法解析 m3u8 播放列表".into()))?;
            match playlist {
                Playlist::MediaPlaylist(media) => return Ok((media, response_url)),
                Playlist::MasterPlaylist(master) => {
                    let variant = master
                        .variants
                        .iter()
                        .filter(|variant| !variant.is_i_frame)
                        .max_by_key(|variant| variant.bandwidth)
                        .ok_or_else(|| {
                            AppError::Internal("master 播放列表中没有可用的视频流".into())
                        })?;
                    if variant.audio.is_some() {
                        return Err(AppError::Internal(
                            "暂不支持带独立音轨的 HLS 播放列表".into(),
                        ));
                    }
                    current = response_url
                        .join(&variant.uri)
                        .map_err(|error| AppError::Internal(format!("变体地址无效：{error}")))?;
                }
            }
        }
        Err(AppError::Internal("m3u8 播放列表嵌套层级过深".into()))
    }

    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        fetch_resource(&self.client, url, None, "下载").await
    }

    async fn prepare_keys(
        &self,
        base: &Url,
        media_sequence: u64,
        segments: &[MediaSegment],
    ) -> Result<Vec<Option<SegmentKey>>> {
        for segment in segments {
            if let Some(key) = &segment.key
                && !matches!(key.method, KeyMethod::None | KeyMethod::AES128)
            {
                return Err(AppError::Internal(format!(
                    "不支持的 HLS 加密方式：{}",
                    key.method
                )));
            }
        }

        let mut cache: HashMap<String, [u8; 16]> = HashMap::new();
        let mut keys = Vec::with_capacity(segments.len());

        for (index, segment) in segments.iter().enumerate() {
            let Some(key) = &segment.key else {
                keys.push(None);
                continue;
            };
            match &key.method {
                KeyMethod::None => {
                    keys.push(None);
                    continue;
                }
                KeyMethod::AES128 => {}
                _ => unreachable!("unsupported key methods were rejected before key fetching"),
            }
            let uri = key
                .uri
                .as_ref()
                .ok_or_else(|| AppError::Internal("AES-128 加密分片缺少密钥 URI".into()))?;
            let key_url = base
                .join(uri)
                .map_err(|error| AppError::Internal(format!("密钥地址无效：{error}")))?;
            let cache_key = key_url.to_string();
            let key_bytes = match cache.get(&cache_key) {
                Some(bytes) => *bytes,
                None => {
                    let bytes = self.fetch_bytes(key_url.as_str()).await?;
                    if bytes.len() != 16 {
                        return Err(AppError::Internal(format!(
                            "密钥长度异常：应为 16 字节，实际 {} 字节",
                            bytes.len()
                        )));
                    }
                    let mut key = [0u8; 16];
                    key.copy_from_slice(&bytes);
                    cache.insert(cache_key, key);
                    key
                }
            };
            let iv = match &key.iv {
                Some(value) => parse_hex_16(value)
                    .ok_or_else(|| AppError::Internal("加密分片 IV 格式无效".into()))?,
                None => implicit_iv(media_sequence, index)?,
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
        let range = map
            .byte_range
            .as_ref()
            .map(|range| prepare_byte_range(range, None))
            .transpose()?;
        let bytes = fetch_resource(&self.client, map_url.as_str(), range, "下载初始化分片").await?;
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
    let mut previous_range: Option<(String, u64)> = None;
    let mut prepared = Vec::with_capacity(segments.len());

    for (index, segment) in segments.iter().enumerate() {
        let url = base
            .join(&segment.uri)
            .map_err(|error| AppError::Internal(format!("分片地址无效：{error}")))?;
        let resolved_url = url.to_string();
        let range = match segment.byte_range.as_ref() {
            Some(range) => {
                let inherited_offset = match &previous_range {
                    Some((previous_url, next_offset)) if previous_url == &resolved_url => {
                        Some(*next_offset)
                    }
                    _ => None,
                };
                let prepared_range = prepare_byte_range(range, inherited_offset)?;
                let next_offset = prepared_range
                    .1
                    .checked_add(1)
                    .ok_or_else(|| AppError::Internal("分片 byte-range 结束位置溢出".into()))?;
                previous_range = Some((resolved_url.clone(), next_offset));
                Some(prepared_range)
            }
            None => {
                previous_range = None;
                None
            }
        };
        prepared.push(PreparedSegment {
            url: resolved_url,
            range,
            key: keys[index],
        });
    }
    Ok(prepared)
}

fn prepare_byte_range(range: &ByteRange, inherited_offset: Option<u64>) -> Result<(u64, u64)> {
    if range.length == 0 {
        return Err(AppError::Internal("byte-range 长度不能为 0".into()));
    }
    let start = match range.offset.or(inherited_offset) {
        Some(start) => start,
        None => {
            return Err(AppError::Internal(
                "隐式 byte-range offset 必须继承紧邻的同 URL range".into(),
            ));
        }
    };
    let end = start
        .checked_add(range.length - 1)
        .ok_or_else(|| AppError::Internal("byte-range 结束位置溢出".into()))?;
    Ok((start, end))
}

async fn fetch_resource(
    client: &Client,
    url: &str,
    range: Option<(u64, u64)>,
    action: &str,
) -> Result<Vec<u8>> {
    let mut request = client.get(url);
    if let Some((start, end)) = range {
        request = request.header(reqwest::header::RANGE, format!("bytes={start}-{end}"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::Internal(format!("{action}失败：{error}")))?;

    if let Some((start, end)) = range {
        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(AppError::Internal(format!(
                "{action}失败：range 请求应返回 HTTP 206，实际 {}",
                response.status()
            )));
        }
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| AppError::Internal(format!("{action}失败：缺少 Content-Range")))?;
        let (actual_start, actual_end) = parse_content_range(content_range)
            .ok_or_else(|| AppError::Internal(format!("{action}失败：Content-Range 无效")))?;
        if (actual_start, actual_end) != (start, end) {
            return Err(AppError::Internal(format!(
                "{action}失败：Content-Range 范围不匹配，期望 {start}-{end}，实际 {actual_start}-{actual_end}"
            )));
        }
    } else if !response.status().is_success() {
        return Err(AppError::Internal(format!(
            "{action}失败：HTTP {}",
            response.status()
        )));
    }

    let data = read_response_limited(response, MAX_RESOURCE_BYTES, action).await?;
    if let Some((start, end)) = range {
        let expected_length = end - start + 1;
        if u64::try_from(data.len()).ok() != Some(expected_length) {
            return Err(AppError::Internal(format!(
                "{action}失败：range 响应长度不匹配，期望 {expected_length} 字节，实际 {} 字节",
                data.len()
            )));
        }
    }
    Ok(data)
}

async fn read_response_limited(
    mut response: Response,
    max_bytes: usize,
    action: &str,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(AppError::Internal(format!(
            "{action}失败：响应大小超过上限 {max_bytes} 字节"
        )));
    }

    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| AppError::Internal(format!("{action}失败：读取响应失败：{error}")))?
    {
        if data.len().saturating_add(chunk.len()) > max_bytes {
            return Err(AppError::Internal(format!(
                "{action}失败：响应大小超过上限 {max_bytes} 字节"
            )));
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

fn parse_content_range(value: &str) -> Option<(u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    if total.is_empty() || (total != "*" && total.parse::<u64>().is_err()) {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    (start <= end).then_some((start, end))
}

async fn fetch_segment(
    client: &Client,
    segment: &PreparedSegment,
    downloaded_bytes: &AtomicU64,
) -> Result<Vec<u8>> {
    let data = fetch_resource(client, segment.url.as_str(), segment.range, "下载分片").await?;
    reserve_download_bytes(downloaded_bytes, data.len())?;
    match segment.key {
        Some(key) => decrypt_segment(&key.key, &key.iv, &data),
        None => Ok(data),
    }
}

fn reserve_download_bytes(downloaded_bytes: &AtomicU64, amount: usize) -> Result<()> {
    let amount =
        u64::try_from(amount).map_err(|_| AppError::Internal("下载分片大小超出支持范围".into()))?;
    downloaded_bytes
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current
                .checked_add(amount)
                .filter(|next| *next <= MAX_DOWNLOAD_BYTES)
        })
        .map(|_| ())
        .map_err(|_| {
            AppError::Internal(format!("单个任务下载量超过上限 {MAX_DOWNLOAD_BYTES} 字节"))
        })
}

fn decrypt_segment(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    let decryptor = || {
        Decryptor::<Aes128>::new_from_slices(&key[..], &iv[..])
            .map_err(|error| AppError::Internal(format!("AES 初始化失败：{error}")))
    };
    if let Ok(plain) = decryptor()?.decrypt_padded_vec::<Pkcs7>(data) {
        return Ok(plain);
    }
    // Some packagers emit unpadded ciphertext; fall back to raw block decryption.
    decryptor()?
        .decrypt_padded_vec::<NoPadding>(data)
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
    for (index, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
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

fn implicit_iv(media_sequence: u64, index: usize) -> Result<[u8; 16]> {
    let index = u64::try_from(index)
        .map_err(|_| AppError::Internal("分片索引超出 HLS 序列号范围".into()))?;
    let sequence = media_sequence
        .checked_add(index)
        .ok_or_else(|| AppError::Internal("HLS media sequence 溢出".into()))?;
    Ok(default_iv(sequence))
}

fn default_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use m3u8_rs::ByteRange;
    use url::Url;

    use super::{
        HlsDownloader, decrypt_segment, default_iv, fetch_resource, implicit_iv,
        normalize_segments, parse_hex_16, prepare_byte_range, prepare_segments,
    };

    #[test]
    fn decrypts_unpadded_aes_128_cbc_segment() {
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let ciphertext = [
            0x76, 0x49, 0xab, 0xac, 0x81, 0x19, 0xb2, 0x46, 0xce, 0xe9, 0x8e, 0x9b, 0x12, 0xe9,
            0x19, 0x7d,
        ];
        let plaintext = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];

        assert_eq!(decrypt_segment(&key, &iv, &ciphertext).unwrap(), plaintext);
    }

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
    fn implicit_iv_uses_media_sequence_and_rejects_overflow() {
        assert_eq!(implicit_iv(41, 1).unwrap(), default_iv(42));
        assert!(implicit_iv(u64::MAX, 1).is_err());
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

    #[test]
    fn validates_byte_ranges_and_only_inherits_from_adjacent_resolved_url() {
        assert!(
            prepare_byte_range(
                &ByteRange {
                    length: 0,
                    offset: Some(0)
                },
                None
            )
            .is_err()
        );
        assert!(
            prepare_byte_range(
                &ByteRange {
                    length: 2,
                    offset: Some(u64::MAX),
                },
                None,
            )
            .is_err()
        );

        let playlist = m3u8_rs::parse_media_playlist_res(
            b"#EXTM3U\n\
              #EXT-X-TARGETDURATION:10\n\
              #EXT-X-BYTERANGE:5@10\n\
              #EXTINF:10,\nparts/../media.bin\n\
              #EXT-X-BYTERANGE:3\n\
              #EXTINF:10,\nmedia.bin\n\
              #EXT-X-ENDLIST\n",
        )
        .unwrap();
        let segments = normalize_segments(playlist);
        let prepared = prepare_segments(
            &Url::parse("https://example.com/base/list.m3u8").unwrap(),
            &segments,
            &[None, None],
        )
        .unwrap();
        assert_eq!(prepared[0].range, Some((10, 14)));
        assert_eq!(prepared[1].range, Some((15, 17)));

        let playlist = m3u8_rs::parse_media_playlist_res(
            b"#EXTM3U\n\
              #EXT-X-TARGETDURATION:10\n\
              #EXT-X-BYTERANGE:5@10\n\
              #EXTINF:10,\nmedia.bin\n\
              #EXTINF:10,\nother.bin\n\
              #EXT-X-BYTERANGE:3\n\
              #EXTINF:10,\nmedia.bin\n\
              #EXT-X-ENDLIST\n",
        )
        .unwrap();
        let segments = normalize_segments(playlist);
        assert!(
            prepare_segments(
                &Url::parse("https://example.com/base/list.m3u8").unwrap(),
                &segments,
                &[None, None, None],
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn resolves_resources_against_final_media_playlist_url() {
        let (origin, server) = serve_requests(5, |path, _request| {
            match path {
            "/top/master.m3u8" => response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\n../variant.m3u8\n",
            ),
            "/variant.m3u8" => response(
                "302 Found",
                &[("Location", "/final/nested/media.m3u8")],
                b"",
            ),
            "/final/nested/media.m3u8" => response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:42\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXT-X-MAP:URI=\"init.bin\"\n#EXTINF:10,\nsegment.ts\n#EXT-X-ENDLIST\n",
            ),
            "/final/nested/key.bin" => response("200 OK", &[], &[7; 16]),
            "/final/nested/init.bin" => response("200 OK", &[], b"init"),
            _ => response("404 Not Found", &[], b""),
        }
        });
        let downloader = HlsDownloader::new(1).unwrap();
        let entry = origin.join("top/master.m3u8").unwrap();
        let (playlist, media_url) = downloader.resolve_media_playlist(&entry).await.unwrap();
        assert_eq!(media_url, origin.join("final/nested/media.m3u8").unwrap());

        let media_sequence = playlist.media_sequence;
        let segments = normalize_segments(playlist);
        let keys = downloader
            .prepare_keys(&media_url, media_sequence, &segments)
            .await
            .unwrap();
        assert_eq!(keys[0].unwrap().iv, default_iv(42));
        let prepared = prepare_segments(&media_url, &segments, &keys).unwrap();
        assert_eq!(
            prepared[0].url,
            origin.join("final/nested/segment.ts").unwrap().as_str()
        );
        assert_eq!(
            downloader
                .fetch_init_map(&media_url, segments.first())
                .await
                .unwrap(),
            Some(b"init".to_vec())
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn rejects_unsupported_encryption_methods() {
        let playlist = m3u8_rs::parse_media_playlist_res(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-KEY:METHOD=AES-128,URI=\"unreachable-key.bin\"\n#EXTINF:10,\nsegment0.ts\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key.bin\"\n#EXTINF:10,\nsegment1.ts\n",
        )
        .unwrap();
        let segments = normalize_segments(playlist);
        let downloader = HlsDownloader::new(1).unwrap();
        assert!(
            downloader
                .prepare_keys(
                    &Url::parse("https://example.com/media.m3u8").unwrap(),
                    0,
                    &segments,
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn validates_range_response_status_content_range_and_length() {
        let cases = [
            (
                response(
                    "206 Partial Content",
                    &[("Content-Range", "bytes 2-4/10")],
                    b"abc",
                ),
                true,
            ),
            (response("200 OK", &[], b"abc"), false),
            (
                response(
                    "206 Partial Content",
                    &[("Content-Range", "bytes 1-3/10")],
                    b"abc",
                ),
                false,
            ),
            (
                response(
                    "206 Partial Content",
                    &[("Content-Range", "bytes 2-4/10")],
                    b"ab",
                ),
                false,
            ),
        ];

        for (server_response, should_succeed) in cases {
            let (origin, server) = serve_requests(1, move |_path, request| {
                assert!(request.contains("range: bytes=2-4"));
                server_response.clone()
            });
            let client = reqwest::Client::new();
            let result = fetch_resource(&client, origin.as_str(), Some((2, 4)), "测试").await;
            assert_eq!(result.is_ok(), should_succeed);
            server.join().unwrap();
        }
    }

    fn serve_requests<F>(request_count: usize, handler: F) -> (Url, thread::JoinHandle<()>)
    where
        F: Fn(&str, &str) -> Vec<u8> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for stream in listener.incoming().take(request_count) {
                let mut stream = stream.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                stream.write_all(&handler(path, &request)).unwrap();
            }
        });
        (Url::parse(&format!("http://{address}/")).unwrap(), handle)
    }

    fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        let mut response = response.into_bytes();
        response.extend_from_slice(body);
        response
    }
}
