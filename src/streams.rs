//! Turns raw addon streams into the rows the UI and CLI show, and picks the best one.
use crate::addon::{Stream, Subtitle, lang_code};
use crate::config::Streams;

#[derive(Clone, Debug, Default)]
pub struct StreamRow {
    pub stream: Stream,
    pub quality: String,
    pub release: String,
    pub format: String,
    pub size: String,
    /// `size` in bytes, or 0 when unknown.
    pub bytes: u64,
    pub seeds: Option<u32>,
    pub addon: String,
    pub addon_order: usize,
}

const QUALITY_TOKENS: [(&str, &str); 9] = [
    ("2160p", "2160p"),
    ("4k", "2160p"),
    ("uhd", "2160p"),
    ("1440p", "1440p"),
    ("1080p", "1080p"),
    ("720p", "720p"),
    ("576p", "480p"),
    ("480p", "480p"),
    ("360p", "360p"),
];

/// Release words that always mean a recording or pre-release copy, whatever its resolution.
const LOW_QUALITY_WORDS: [&str; 12] = ["camrip", "hdcam", "hqcam", "telesync", "tsrip", "hdts", "telecine", "hdtc", "dvdscr", "bdscr", "hdscr", "screener"];

pub fn quality_rank(quality: &str) -> u32 {
    match quality {
        "2160p" => 7,
        "1440p" => 6,
        "1080p" => 5,
        "720p" => 4,
        "480p" => 3,
        "360p" => 2,
        "SCR" | "CAM" => 0,
        _ => 1,
    }
}

/// Tokens split on anything non-alphanumeric, so "cam" does not match inside "camera".
fn tokens(text: &str) -> Vec<String> {
    text.to_ascii_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// Camrips often also say 720p or 1080p, so this is checked before the resolution. Bare
/// "cam" or "scr" counts only after the year or resolution: "Cam.2018.1080p" is a film title.
fn is_low_quality(label: &str, release: &str) -> bool {
    let words = tokens(release);
    let is_marker = |w: &String| QUALITY_TOKENS.iter().any(|(t, _)| t == w) || (w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()) && (w.starts_with("19") || w.starts_with("20")));
    let after_marker = words.iter().position(is_marker).map(|i| &words[i + 1..]).unwrap_or(&[]);
    words.iter().any(|w| LOW_QUALITY_WORDS.contains(&w.as_str()))
        || words.windows(2).any(|p| matches!(p[0].as_str(), "hd" | "hq") && matches!(p[1].as_str(), "cam" | "ts" | "tc"))
        || after_marker.iter().any(|w| w == "cam" || w == "scr")
        || tokens(label).iter().any(|w| w == "cam" || w == "scr" || w == "telesync")
}

fn parse_quality(label: &str, release: &str) -> String {
    if is_low_quality(label, release) {
        return "CAM".to_string();
    }
    let words = tokens(&format!("{label}\n{release}"));
    for (token, quality) in QUALITY_TOKENS {
        if words.iter().any(|w| w == token) {
            return quality.to_string();
        }
    }
    "—".to_string()
}

fn parse_size(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for pair in words.windows(2) {
        let unit = pair[1].to_ascii_uppercase();
        let number = pair[0].trim_start_matches(|c: char| !c.is_ascii_digit());
        if matches!(unit.as_str(), "GB" | "MB" | "GIB" | "MIB" | "TB") && number.parse::<f64>().is_ok() {
            return Some(format!("{number} {}", unit.replace("IB", "B")));
        }
    }
    None
}

fn parse_bytes(size: &str) -> u64 {
    let mut parts = size.split_whitespace();
    let number: f64 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
    let scale: f64 = match parts.next() {
        Some("TB") => 1_099_511_627_776.0,
        Some("GB") => 1_073_741_824.0,
        Some("MB") => 1_048_576.0,
        _ => 0.0,
    };
    (number * scale) as u64
}

pub fn human_size(bytes: u64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{:.0} MB", bytes as f64 / 1_048_576.0) }
}

fn parse_seeds(text: &str) -> Option<u32> {
    let rest = &text[text.find('👤')? + '👤'.len_utf8()..];
    rest.split_whitespace().next()?.parse().ok()
}

fn parse_format(filename: &str, text: &str) -> String {
    let lower = filename.to_ascii_lowercase();
    let container = ["mkv", "mp4", "webm", "avi", "mov", "m4v", "ts", "m3u8"]
        .into_iter()
        .find(|ext| lower.ends_with(&format!(".{ext}")));
    let words = tokens(text);
    let has = |names: &[&str]| words.iter().any(|w| names.contains(&w.as_str()));
    let codec = if has(&["hevc", "x265", "h265"]) {
        Some("hevc")
    } else if has(&["av1"]) {
        Some("av1")
    } else if has(&["x264", "h264", "avc"]) {
        Some("h264")
    } else if has(&["vp9"]) {
        Some("vp9")
    } else {
        None
    };
    match (container, codec) {
        (Some(c), Some(v)) => format!("{c} · {v}"),
        (Some(c), None) => c.to_string(),
        (None, Some(v)) => v.to_string(),
        (None, None) => String::new(),
    }
}

pub fn to_row(stream: Stream, addon: &str, addon_order: usize) -> StreamRow {
    let body = if stream.title.is_empty() { stream.description.clone() } else { stream.title.clone() };
    let text = format!("{}\n{body}\n{}", stream.name, stream.filename);
    // The first line of `name` is the addon's own name, not part of the quality label.
    let label = stream.name.split_once('\n').map(|(_, rest)| rest).unwrap_or(&stream.name);
    let release_text = format!("{body}\n{}", stream.filename);
    let mut lines = body.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or(&stream.name).trim().to_string();
    let release = if stream.filename.is_empty() || !first.is_empty() { first } else { stream.filename.clone() };
    let filename = if stream.filename.is_empty() { release.clone() } else { stream.filename.clone() };
    let size = parse_size(&text).or(stream.size.map(human_size)).unwrap_or_default();
    StreamRow {
        quality: parse_quality(label, &release_text),
        bytes: parse_bytes(&size),
        format: parse_format(&filename, &text),
        release,
        size,
        seeds: parse_seeds(&text),
        addon: addon.to_string(),
        addon_order,
        stream,
    }
}

/// Quality first, then swarm health, then addon order ("order is priority").
pub fn sort_rows(rows: &mut [StreamRow]) {
    rows.sort_by_key(|r| (std::cmp::Reverse(quality_rank(&r.quality)), r.addon_order, std::cmp::Reverse(r.seeds.unwrap_or(0))));
}

/// Within one quality, bigger files usually hold more real detail, as long as enough
/// people seed them to stream smoothly. Prefers the largest healthy file under `max_bytes`.
fn best_of_quality(rows: &[StreamRow], group: &[usize], max_bytes: u64) -> Option<usize> {
    let top = group.iter().filter_map(|&i| rows[i].seeds).max().unwrap_or(0);
    let floor = 5.min(top).max(top / 10);
    let healthy: Vec<usize> = group.iter().copied().filter(|&i| rows[i].seeds.is_none_or(|s| s >= floor)).collect();
    let seeds = |i: usize| rows[i].seeds.unwrap_or(u32::MAX);
    healthy
        .iter()
        .copied()
        .filter(|&i| rows[i].bytes <= max_bytes)
        .max_by_key(|&i| (rows[i].bytes, seeds(i)))
        .or_else(|| healthy.iter().copied().min_by_key(|&i| rows[i].bytes))
}

/// Index of the stream to play by default, from the first preferred quality available.
/// Ignores dead torrents and camrips whenever anything else exists.
pub fn best_index(rows: &[StreamRow], config: &Streams) -> Option<usize> {
    let playable: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].stream.external_url.is_none() || rows[i].stream.url.is_some()).collect();
    let alive: Vec<usize> = playable.iter().copied().filter(|&i| rows[i].seeds != Some(0)).collect();
    let pool = if alive.is_empty() { playable } else { alive };
    let real: Vec<usize> = pool.iter().copied().filter(|&i| rows[i].quality != "CAM").collect();
    let pool = if real.is_empty() { pool } else { real };
    let max_bytes = config.max_size_gb.saturating_mul(1_073_741_824);
    let in_quality = |quality: &str| pool.iter().copied().filter(|&i| rows[i].quality == quality).collect::<Vec<_>>();
    for quality in &config.prefer {
        let best = best_of_quality(rows, &in_quality(quality), max_bytes);
        if best.is_some() {
            return best;
        }
    }
    let first = *pool.first()?;
    best_of_quality(rows, &in_quality(&rows[first].quality), max_bytes)
}

pub fn by_quality<'a>(rows: &'a [StreamRow], quality: &str) -> Option<&'a StreamRow> {
    rows.iter().find(|r| r.quality.eq_ignore_ascii_case(quality))
}

/// Unique language codes in first-seen order.
pub fn subtitle_languages(subs: &[Subtitle]) -> Vec<String> {
    let mut langs: Vec<String> = Vec::new();
    for sub in subs {
        let code = lang_code(&sub.lang);
        if !langs.contains(&code) {
            langs.push(code);
        }
    }
    langs
}

/// The URL mpv can open directly, or a magnet link for torrent rows (used by "copy URL").
pub fn display_url(row: &StreamRow) -> String {
    let s = &row.stream;
    if let Some(url) = &s.url {
        url.clone()
    } else if let Some(hash) = &s.info_hash {
        crate::torrent::magnet(hash, &s.trackers)
    } else if let Some(id) = &s.yt_id {
        format!("https://www.youtube.com/watch?v={id}")
    } else {
        s.external_url.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn torrentio_stream() -> Stream {
        Stream {
            info_hash: Some("abc".into()),
            file_idx: Some(0),
            name: "Torrentio\n1080p".into(),
            title: "Nosferatu.1922.1080p.BluRay.x264.mkv\n👤 42 💾 2.1 GB ⚙️ YTS".into(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_torrentio_title() {
        let row = to_row(torrentio_stream(), "Torrentio", 0);
        assert_eq!(row.quality, "1080p");
        assert_eq!(row.seeds, Some(42));
        assert_eq!(row.size, "2.1 GB");
        assert_eq!(row.format, "mkv · h264");
        assert_eq!(row.release, "Nosferatu.1922.1080p.BluRay.x264.mkv");
    }

    #[test]
    fn four_k_label_is_2160p() {
        let row = to_row(Stream { name: "Torrentio\n4k DV | HDR".into(), title: "x\n👤 3".into(), ..Default::default() }, "T", 0);
        assert_eq!(row.quality, "2160p");
    }

    fn config(prefer: &[&str]) -> Streams {
        Streams { prefer: prefer.iter().map(|q| q.to_string()).collect(), ..Default::default() }
    }

    fn sized(quality: &str, seeds: u32, size: &str) -> StreamRow {
        to_row(Stream { name: format!("Torrentio\n{quality}"), title: format!("Release\n👤 {seeds} 💾 {size}"), ..Default::default() }, "T", 0)
    }

    #[test]
    fn recordings_are_labelled_cam_whatever_resolution_they_claim() {
        let quality = |name: &str, title: &str| to_row(Stream { name: name.into(), title: title.into(), ..Default::default() }, "T", 0).quality;
        assert_eq!(quality("Torrentio\n720p", "Whalefall 2026 camrip 720p mp4 FR EN-CinemaCity\n👤 3"), "CAM");
        assert_eq!(quality("Torrentio\n1080p", "Movie.2026.1080p.HDCAM.x264"), "CAM");
        assert_eq!(quality("Torrentio\n1080p", "Movie.2026.HD-TS.1080p"), "CAM");
        assert_eq!(quality("Torrentio\nCAM", "Movie 2026"), "CAM");
        assert_eq!(quality("Torrentio\n720p", "Movie.2026.DVDSCR.720p"), "CAM");
    }

    #[test]
    fn real_releases_are_not_mistaken_for_recordings() {
        let quality = |name: &str, title: &str| to_row(Stream { name: name.into(), title: title.into(), ..Default::default() }, "T", 0).quality;
        assert_eq!(quality("Torrentio\n1080p", "Cam.2018.1080p.WEB.x264"), "1080p");
        assert_eq!(quality("Torrentio\n1080p", "The.Camera.2020.1080p.BluRay.mkv"), "1080p");
        assert_eq!(quality("Torrentio\n720p", "Movie.2020.720p.WEB.ts"), "720p");
        assert_eq!(quality("CamAddon\n1080p", "Movie.2020.1080p"), "1080p");
    }

    #[test]
    fn best_never_picks_a_camrip_when_anything_else_exists() {
        let rows = vec![sized("CAM", 900, "1 GB"), sized("480p", 3, "700 MB")];
        assert_eq!(rows[best_index(&rows, &config(&["1080p", "720p"])).unwrap()].quality, "480p");
        let only_cam = vec![sized("CAM", 900, "1 GB")];
        assert_eq!(best_index(&only_cam, &config(&["1080p"])), Some(0));
    }

    #[test]
    fn best_prefers_the_largest_healthy_file_under_the_limit() {
        let rows = vec![sized("1080p", 252, "1.5 GB"), sized("1080p", 104, "9.8 GB"), sized("1080p", 19, "25 GB"), sized("1080p", 30, "60 GB")];
        // 19 seeds is under a tenth of the top swarm, and 60 GB is over the 25 GB limit.
        assert_eq!(rows[best_index(&rows, &config(&["1080p"])).unwrap()].size, "9.8 GB");
    }

    #[test]
    fn best_takes_the_smallest_when_every_file_is_over_the_limit() {
        let rows = vec![sized("2160p", 50, "60 GB"), sized("2160p", 50, "40 GB")];
        assert_eq!(rows[best_index(&rows, &config(&["2160p"])).unwrap()].size, "40 GB");
    }

    #[test]
    fn best_prefers_config_order_over_highest_quality() {
        let make = |q: &str, seeds: u32| to_row(Stream { name: q.into(), title: format!("t\n👤 {seeds}"), ..Default::default() }, "T", 0);
        let mut rows = vec![make("2160p", 100), make("720p", 5), make("1080p", 2), make("1080p", 50)];
        sort_rows(&mut rows);
        let best = best_index(&rows, &config(&["1080p", "720p"])).unwrap();
        assert_eq!(rows[best].quality, "1080p");
        assert_eq!(rows[best].seeds, Some(50));
    }

    #[test]
    fn best_skips_dead_torrents() {
        let make = |q: &str, seeds: u32| to_row(Stream { name: q.into(), title: format!("t\n👤 {seeds}"), ..Default::default() }, "T", 0);
        let rows = vec![make("1080p", 0), make("720p", 9)];
        let best = best_index(&rows, &config(&["1080p"])).unwrap();
        assert_eq!(rows[best].quality, "720p");
    }
}
