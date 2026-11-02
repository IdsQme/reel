//! Turns raw addon streams into the rows the UI and CLI show, and picks the best one.
use crate::addon::{Stream, Subtitle, lang_code};

#[derive(Clone, Debug, Default)]
pub struct StreamRow {
    pub stream: Stream,
    pub quality: String,
    pub release: String,
    pub format: String,
    pub size: String,
    pub seeds: Option<u32>,
    pub addon: String,
    pub addon_order: usize,
}

const QUALITY_TOKENS: [(&str, &str); 12] = [
    ("2160p", "2160p"),
    ("4k", "2160p"),
    ("uhd", "2160p"),
    ("1440p", "1440p"),
    ("1080p", "1080p"),
    ("720p", "720p"),
    ("576p", "480p"),
    ("480p", "480p"),
    ("360p", "360p"),
    ("scr", "SCR"),
    ("cam", "CAM"),
    ("telesync", "CAM"),
];

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

fn parse_quality(text: &str) -> String {
    let words = tokens(text);
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
    let mut lines = body.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or(&stream.name).trim().to_string();
    let release = if stream.filename.is_empty() || !first.is_empty() { first } else { stream.filename.clone() };
    let filename = if stream.filename.is_empty() { release.clone() } else { stream.filename.clone() };
    let size = parse_size(&text).or(stream.size.map(human_size)).unwrap_or_default();
    StreamRow {
        quality: parse_quality(&text),
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

/// Index of the stream to play by default: the healthiest stream of the first preferred
/// quality, ignoring torrents nobody is seeding when an alternative exists.
pub fn best_index(rows: &[StreamRow], prefer: &[String]) -> Option<usize> {
    let playable: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].stream.external_url.is_none() || rows[i].stream.url.is_some()).collect();
    let alive: Vec<usize> = playable.iter().copied().filter(|&i| rows[i].seeds != Some(0)).collect();
    let pool = if alive.is_empty() { &playable } else { &alive };
    for quality in prefer {
        let best = pool.iter().copied().filter(|&i| &rows[i].quality == quality).max_by_key(|&i| rows[i].seeds.unwrap_or(u32::MAX));
        if best.is_some() {
            return best;
        }
    }
    pool.first().copied()
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

    #[test]
    fn best_prefers_config_order_over_highest_quality() {
        let make = |q: &str, seeds: u32| to_row(Stream { name: q.into(), title: format!("t\n👤 {seeds}"), ..Default::default() }, "T", 0);
        let mut rows = vec![make("2160p", 100), make("720p", 5), make("1080p", 2), make("1080p", 50)];
        sort_rows(&mut rows);
        let best = best_index(&rows, &["1080p".to_string(), "720p".to_string()]).unwrap();
        assert_eq!(rows[best].quality, "1080p");
        assert_eq!(rows[best].seeds, Some(50));
    }

    #[test]
    fn best_skips_dead_torrents() {
        let make = |q: &str, seeds: u32| to_row(Stream { name: q.into(), title: format!("t\n👤 {seeds}"), ..Default::default() }, "T", 0);
        let rows = vec![make("1080p", 0), make("720p", 9)];
        let best = best_index(&rows, &["1080p".to_string()]).unwrap();
        assert_eq!(rows[best].quality, "720p");
    }
}
