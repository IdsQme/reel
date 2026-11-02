//! The built-in "Local Files" addon: browse configured folders of video files.
use crate::addon::{CatalogDef, Manifest, Meta, ResourceDef, Stream};
use crate::config::{Config, expand_home};
use std::fs;
use std::path::{Path, PathBuf};

pub const ID_PREFIX: &str = "local:";
const VIDEO_EXTENSIONS: [&str; 9] = ["mkv", "mp4", "webm", "avi", "mov", "m4v", "ts", "mpg", "wmv"];

fn dirs() -> Vec<String> {
    let config = Config::load().unwrap_or_default();
    config.local.dirs.into_iter().filter(|d| expand_home(d).is_dir()).collect()
}

pub fn manifest() -> Manifest {
    let resource = |name: &str| ResourceDef { name: name.into(), types: vec!["movie".into()], id_prefixes: vec![ID_PREFIX.into()] };
    Manifest {
        id: "reel.local".into(),
        name: "Local Files".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "Video files from the folders in [local] dirs of config.toml.".into(),
        types: vec!["movie".into()],
        id_prefixes: vec![ID_PREFIX.into()],
        resources: vec![resource("catalog"), resource("meta"), resource("stream")],
        catalogs: dirs()
            .into_iter()
            .map(|dir| CatalogDef { kind: "movie".into(), id: dir.clone(), name: dir, extras: vec!["search".into()], ..Default::default() })
            .collect(),
        configurable: false,
    }
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, out);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())) {
            out.push(path);
        }
    }
}

/// "Sherlock.Jr.1924.1080p.mkv" gives ("Sherlock Jr", "1924").
pub fn parse_filename(stem: &str) -> (String, String) {
    let clean = stem.replace(['.', '_'], " ");
    let words: Vec<&str> = clean.split_whitespace().collect();
    let year = words.iter().enumerate().skip(1).find(|(_, w)| {
        let w = w.trim_matches(['(', ')', '[', ']']);
        w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()) && matches!(&w[..2], "18" | "19" | "20")
    });
    match year {
        Some((i, y)) => (words[..i].join(" "), y.trim_matches(['(', ')', '[', ']']).to_string()),
        None => (clean.trim().to_string(), String::new()),
    }
}

fn meta_for(path: &Path) -> Meta {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let (name, year) = parse_filename(stem);
    let description = path.display().to_string();
    Meta {
        id: format!("{ID_PREFIX}{}", path.display()),
        kind: "movie".into(),
        name,
        release_info: year,
        description,
        sources: vec!["Local Files".into()],
        ..Default::default()
    }
}

pub fn catalog(dir: &str, search: Option<&str>) -> Vec<Meta> {
    let mut files = Vec::new();
    walk(&expand_home(dir), 3, &mut files);
    files.sort();
    let needle = search.map(str::to_lowercase);
    files
        .iter()
        .filter(|p| needle.as_ref().is_none_or(|n| p.file_name().and_then(|f| f.to_str()).unwrap_or_default().to_lowercase().replace(['.', '_'], " ").contains(n.as_str())))
        .map(|p| meta_for(p))
        .collect()
}

pub fn meta(id: &str) -> Option<Meta> {
    let path = Path::new(id.strip_prefix(ID_PREFIX)?);
    path.exists().then(|| meta_for(path))
}

pub fn streams(id: &str) -> Vec<Stream> {
    let Some(path) = id.strip_prefix(ID_PREFIX) else { return Vec::new() };
    let path = Path::new(path);
    if !path.exists() {
        return Vec::new();
    }
    let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or_default().to_string();
    vec![Stream {
        url: Some(path.display().to_string()),
        name: "Local Files".into(),
        title: filename.clone(),
        filename,
        size: fs::metadata(path).ok().map(|m| m.len()),
        ..Default::default()
    }]
}

#[cfg(test)]
mod tests {
    use super::parse_filename;

    #[test]
    fn parses_filenames() {
        assert_eq!(parse_filename("Sherlock.Jr.1924.1080p"), ("Sherlock Jr".to_string(), "1924".to_string()));
        assert_eq!(parse_filename("Detour (1945)"), ("Detour".to_string(), "1945".to_string()));
        assert_eq!(parse_filename("home_movie"), ("home movie".to_string(), String::new()));
        assert_eq!(parse_filename("1917.2019"), ("1917".to_string(), "2019".to_string()));
    }
}
