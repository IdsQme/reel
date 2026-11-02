//! Built-in BitTorrent engine for infoHash streams (Torrentio without debrid).
//! Each file is exposed to mpv through a small local HTTP server with Range support.
use crate::config::cache_dir;
use anyhow::{Context, Result, anyhow};
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent, Session, SessionOptions, api::TorrentIdOrHash};
use std::collections::HashMap;
use std::fs::File;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DEFAULT_TRACKERS: [&str; 6] = [
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
];
const VIDEO_EXTENSIONS: [&str; 8] = ["mkv", "mp4", "webm", "avi", "mov", "m4v", "ts", "wmv"];
const METADATA_TIMEOUT: Duration = Duration::from_secs(90);
/// Seconds mpv must have buffered before the download is paced, and the level that lifts it.
const PACE_ABOVE_SECONDS: f64 = 30.0;
const UNPACE_BELOW_SECONDS: f64 = 10.0;
/// Paced speed as a multiple of the film's average bitrate, with a floor for swarm hiccups.
const PACE_MULTIPLE: f64 = 2.0;
const PACE_FLOOR_BPS: f64 = 1_048_576.0;
const LOCK_FILE: &str = ".lock";

fn torrents_root() -> PathBuf {
    cache_dir().join("torrents")
}

/// Deletes streams left behind by a crash. A running reel holds its folder's lock, so
/// its data is skipped.
pub fn sweep_stale() {
    let Ok(entries) = std::fs::read_dir(torrents_root()) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let Ok(lock) = File::options().create(true).truncate(false).write(true).open(path.join(LOCK_FILE)) else { continue };
        if lock.try_lock().is_ok() {
            drop(lock);
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// The paced rate for a film of this size and length, in bytes per second.
fn paced_bps(total_bytes: u64, duration: f64) -> u32 {
    (total_bytes as f64 / duration * PACE_MULTIPLE).max(PACE_FLOOR_BPS).min(u32::MAX as f64) as u32
}

/// Whether to pace now. Between the two thresholds the current state holds, so it doesn't flap.
fn should_pace(currently: bool, cache_seconds: f64, buffering: bool) -> bool {
    if buffering || cache_seconds < UNPACE_BELOW_SECONDS {
        false
    } else if cache_seconds >= PACE_ABOVE_SECONDS {
        true
    } else {
        currently
    }
}

pub fn magnet(hash: &str, trackers: &[String]) -> String {
    let mut link = format!("magnet:?xt=urn:btih:{hash}");
    for tracker in trackers.iter().map(String::as_str).chain(DEFAULT_TRACKERS) {
        link.push_str("&tr=");
        link.push_str(&tracker.replace(':', "%3A").replace('/', "%2F"));
    }
    link
}

#[derive(Clone, Debug, Default)]
pub struct TorrentStatus {
    pub peers: usize,
    pub speed_mbps: f64,
    pub progress: f64,
}

pub struct OpenedStream {
    pub url: String,
    pub filename: String,
}

type Handle = Arc<ManagedTorrent>;

pub struct Engine {
    session: Arc<Session>,
    addr: SocketAddr,
    torrents: Arc<Mutex<HashMap<String, Handle>>>,
    /// Held for the engine's lifetime so other reels leave this folder alone.
    _lock: File,
}

fn is_video(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    VIDEO_EXTENSIONS.iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

impl Engine {
    pub async fn start() -> Result<Arc<Self>> {
        let folder = torrents_root().join(std::process::id().to_string());
        tokio::fs::create_dir_all(&folder).await?;
        let lock = File::options().create(true).truncate(false).write(true).open(folder.join(LOCK_FILE))?;
        lock.try_lock().context("locking torrent cache")?;
        let _ = tokio::task::spawn_blocking(sweep_stale).await;
        let session = Session::new_with_opts(folder, SessionOptions { ..Default::default() }).await.context("starting torrent engine")?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let torrents: Arc<Mutex<HashMap<String, Handle>>> = Arc::default();
        let served = torrents.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let served = served.clone();
                tokio::spawn(async move {
                    let _ = serve(socket, served).await;
                });
            }
        });
        Ok(Arc::new(Self { session, addr, torrents, _lock: lock }))
    }

    /// Adds the torrent, waits for its metadata and returns a URL mpv can stream from.
    pub async fn open(&self, hash: &str, file_idx: Option<usize>, trackers: &[String]) -> Result<OpenedStream> {
        let options = AddTorrentOptions {
            overwrite: true,
            only_files: file_idx.map(|i| vec![i]),
            sub_folder: Some(hash.to_string()),
            ..Default::default()
        };
        let add = self.session.add_torrent(AddTorrent::from_url(magnet(hash, trackers)), Some(options));
        let response = tokio::time::timeout(METADATA_TIMEOUT, add)
            .await
            .map_err(|_| anyhow!("no peers sent torrent metadata within {}s", METADATA_TIMEOUT.as_secs()))??;
        let handle = response.into_handle().context("torrent was not started")?;
        handle.wait_until_initialized().await?;

        let files: Vec<(String, u64)> = handle.with_metadata(|m| {
            m.file_infos.iter().map(|f| (f.relative_filename.to_string_lossy().to_string(), f.len)).collect()
        })?;
        let idx = match file_idx {
            Some(i) if i < files.len() => i,
            _ => files
                .iter()
                .enumerate()
                .filter(|(_, (name, _))| is_video(name))
                .max_by_key(|(_, (_, len))| *len)
                .map(|(i, _)| i)
                .context("torrent has no video file")?,
        };
        if file_idx != Some(idx) {
            self.session.update_only_files(&handle, &std::iter::once(idx).collect()).await?;
        }
        self.torrents.lock().unwrap().insert(hash.to_string(), handle);
        let filename = files[idx].0.clone();
        Ok(OpenedStream { url: format!("http://{}/{hash}/{idx}", self.addr), filename })
    }

    pub fn status(&self, hash: &str) -> Option<TorrentStatus> {
        let handle = self.torrents.lock().unwrap().get(hash).cloned()?;
        let stats = handle.stats();
        let progress = if stats.total_bytes > 0 { stats.progress_bytes as f64 / stats.total_bytes as f64 } else { 0.0 };
        let live = stats.live.as_ref();
        Some(TorrentStatus {
            peers: live.map(|l| l.snapshot.peer_stats.live as usize).unwrap_or(0),
            speed_mbps: live.map(|l| l.download_speed.mbps).unwrap_or(0.0),
            progress,
        })
    }

    /// Slows the download once mpv has plenty buffered, so a film isn't pulled to disk far
    /// ahead of where you are. Seeks and low buffers lift the limit at once.
    pub fn pace(&self, hash: &str, duration: f64, cache_seconds: f64, buffering: bool) {
        let handle = self.torrents.lock().unwrap().get(hash).cloned();
        let limits = &self.session.ratelimits;
        let current = limits.get_download_bps();
        let wanted = match handle {
            Some(handle) if duration > 0.0 && should_pace(current.is_some(), cache_seconds, buffering) => NonZeroU32::new(paced_bps(handle.stats().total_bytes, duration)),
            _ => None,
        };
        if wanted != current {
            limits.set_download_bps(wanted);
        }
    }

    /// Stops the torrent and removes its downloaded data.
    pub async fn close(&self, hash: &str) {
        self.session.ratelimits.set_download_bps(None);
        let handle = self.torrents.lock().unwrap().remove(hash);
        if let Some(handle) = handle {
            let _ = self.session.delete(TorrentIdOrHash::Id(handle.id()), true).await;
        }
    }
}

fn content_type(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    match lower.rsplit('.').next().unwrap_or_default() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        _ => "video/x-matroska",
    }
}

fn parse_range(headers: &str, len: u64) -> Option<(u64, u64)> {
    let value = headers.lines().find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.eq_ignore_ascii_case("range").then_some(value.trim())
    })?;
    let (start, end) = value.strip_prefix("bytes=")?.split(',').next()?.split_once('-')?;
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        return Some((len.saturating_sub(suffix), len.checked_sub(1)?));
    }
    let start: u64 = start.parse().ok()?;
    let end = end.parse().unwrap_or(len.saturating_sub(1)).min(len.saturating_sub(1));
    (start <= end).then_some((start, end))
}

async fn serve(mut socket: TcpStream, torrents: Arc<Mutex<HashMap<String, Handle>>>) -> Result<()> {
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = socket.read(&mut chunk).await?;
        if n == 0 || request.len() > 16 * 1024 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&request);
    let mut first = text.lines().next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default();
    let mut path = first.next().unwrap_or_default().trim_start_matches('/').split('/');
    let (hash, idx) = (path.next().unwrap_or_default(), path.next().and_then(|i| i.parse::<usize>().ok()));
    let handle = torrents.lock().unwrap().get(hash).cloned();
    let (Some(handle), Some(idx)) = (handle, idx) else {
        socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        return Ok(());
    };
    let name = handle.with_metadata(|m| m.file_infos.get(idx).map(|f| f.relative_filename.to_string_lossy().to_string()))?.unwrap_or_default();
    let mut stream = handle.stream(idx).await?;
    let len = stream.len();
    let kind = content_type(&name);
    let range = parse_range(&text, len);
    let (start, end) = range.unwrap_or((0, len.saturating_sub(1)));
    let header = match range {
        Some(_) => format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: {kind}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{len}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            end - start + 1
        ),
        None => format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nAccept-Ranges: bytes\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"),
    };
    socket.set_nodelay(true)?;
    socket.write_all(header.as_bytes()).await?;
    if method == "HEAD" {
        return Ok(());
    }
    stream.seek(std::io::SeekFrom::Start(start)).await?;
    let mut limited = stream.take(end - start + 1);
    tokio::io::copy(&mut limited, &mut socket).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_engages_with_a_full_buffer_and_lifts_on_low_buffer_or_stall() {
        assert!(should_pace(false, 30.0, false));
        assert!(!should_pace(false, 20.0, false));
        assert!(should_pace(true, 20.0, false));
        assert!(!should_pace(true, 5.0, false));
        assert!(!should_pace(true, 120.0, true));
    }

    #[test]
    fn paced_rate_is_twice_the_bitrate_with_a_floor() {
        // 6 GB over 8000 s is 750 kB/s, so twice that is 1.5 MB/s.
        assert_eq!(paced_bps(6_000_000_000, 8000.0), 1_500_000);
        assert_eq!(paced_bps(100_000_000, 8000.0), 1_048_576);
    }

    #[test]
    fn parses_ranges() {
        assert_eq!(parse_range("GET / HTTP/1.1\r\nRange: bytes=0-\r\n", 100), Some((0, 99)));
        assert_eq!(parse_range("range: bytes=10-19\r\n", 100), Some((10, 19)));
        assert_eq!(parse_range("Range: bytes=-10\r\n", 100), Some((90, 99)));
        assert_eq!(parse_range("Range: bytes=200-\r\n", 100), None);
        assert_eq!(parse_range("Accept: */*\r\n", 100), None);
    }

    #[test]
    fn magnet_includes_hash_and_trackers() {
        let link = magnet("abc", &["udp://x:1/announce".to_string()]);
        assert!(link.starts_with("magnet:?xt=urn:btih:abc&tr=udp%3A%2F%2Fx%3A1%2Fannounce"));
    }
}
