//! The built-in "YouTube" addon. Listing and playback go through yt-dlp, which only runs
//! while it is needed. Sign-in happens on Google's own page in a dedicated Firefox profile;
//! reel never sees the password and yt-dlp reads the session from that profile.
use crate::addon::{CatalogDef, Manifest, Meta, ResourceDef, Stream};
use crate::config::{Config, cache_dir, config_dir};
use anyhow::{Context, Result, anyhow, bail};
use std::path::PathBuf;
use std::process::Stdio;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::process::Command;

pub const ID_PREFIX: &str = "yt:";
const KIND: &str = "youtube";
const PAGE_SIZE: usize = 60;
const FEED_CACHE_SECONDS: u64 = 600;
const LIST_TIMEOUT: Duration = Duration::from_secs(90);
const SIGN_IN_URL: &str = "https://accounts.google.com/ServiceLogin?service=youtube&continue=https://www.youtube.com/";
/// Google's sign-in cookie, present in the profile only once someone has signed in.
const SESSION_COOKIE: &[u8] = b"SAPISID";
/// The same endpoint youtube.com itself calls. Starting yt-dlp takes seconds; this doesn't.
const SEARCH_URL: &str = "https://www.youtube.com/youtubei/v1/search?prettyPrint=false";
const WEB_CLIENT_VERSION: &str = "2.20261001.00.00";
/// Limits results to videos, without channels or playlists.
const VIDEOS_ONLY: &str = "EgIQAQ==";
const MAX_REMEMBERED_SEARCHES: usize = 16;
const NOT_SIGNED_IN: &str = "This section needs a YouTube account, which is optional. Press L to sign in with Google. The topics above and search work without one.";

/// (catalog id, name, search). YouTube's public pages list nothing to a signed-out client,
/// so browsing without an account is topic searches, listed first so they open by default.
const TOPICS: [(&str, &str, &str); 5] = [("music", "Music", "music"), ("gaming", "Gaming", "gaming"), ("news", "News", "news today"), ("science", "Science", "science documentary"), ("tech", "Tech", "technology")];

/// (catalog id, name, yt-dlp target). All of these need an account.
const FEEDS: [(&str, &str, &str); 5] = [
    ("home", "Home", ":ytrec"),
    ("subscriptions", "Subscriptions", ":ytsubs"),
    ("watchlater", "Watch later", ":ytwatchlater"),
    ("liked", "Liked", "https://www.youtube.com/playlist?list=LL"),
    ("history", "History", ":ythistory"),
];
const SEARCH_CATALOG: &str = "search";

pub fn manifest() -> Manifest {
    let resource = |name: &str| ResourceDef { name: name.into(), types: vec![KIND.into()], id_prefixes: vec![ID_PREFIX.into()] };
    let browsable = |(id, name): (&str, &str)| CatalogDef { kind: KIND.into(), id: id.into(), name: name.into(), extras: vec!["skip".into()], ..Default::default() };
    let mut catalogs: Vec<CatalogDef> = TOPICS.iter().map(|(id, name, _)| browsable((id, name))).chain(FEEDS.iter().map(|(id, name, _)| browsable((id, name)))).collect();
    catalogs.push(CatalogDef { kind: KIND.into(), id: SEARCH_CATALOG.into(), name: "Search".into(), extras: vec!["search".into()], required: vec!["search".into()], ..Default::default() });
    Manifest {
        id: "reel.youtube".into(),
        name: "YouTube".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "YouTube played by mpv without ads. Topics and search need no account; press L on a YouTube catalog to add your own feeds.".into(),
        types: vec![KIND.into()],
        id_prefixes: vec![ID_PREFIX.into()],
        resources: vec![resource("catalog"), resource("stream")],
        catalogs,
        configurable: false,
    }
}

/// winget keeps portable tools in per-package folders, which reach PATH only in terminals
/// opened after the install.
fn winget_tool_dirs() -> Vec<PathBuf> {
    let Some(packages) = dirs::data_local_dir().map(|d| d.join("Microsoft").join("WinGet").join("Packages")) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(packages) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            name.starts_with("yt-dlp.yt-dlp") || name.starts_with("denoland.deno")
        })
        .map(|e| e.path())
        .collect()
}

/// PATH for child processes, so tools installed after this terminal opened are still found.
pub fn child_path() -> &'static std::ffi::OsStr {
    static PATH: OnceLock<std::ffi::OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let mut paths: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        paths.extend(winget_tool_dirs());
        paths.extend(dirs::home_dir().map(|h| h.join(".deno").join("bin")));
        std::env::join_paths(paths).unwrap_or_default()
    })
}

fn find_tool(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::split_paths(child_path()).map(|dir| dir.join(&exe)).find(|path| path.exists())
}

fn ytdlp() -> Option<&'static PathBuf> {
    static TOOL: OnceLock<Option<PathBuf>> = OnceLock::new();
    TOOL.get_or_init(|| find_tool("yt-dlp")).as_ref()
}

fn firefox() -> Option<PathBuf> {
    [r"C:\Program Files\Mozilla Firefox\firefox.exe", r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe"].iter().map(PathBuf::from).find(|p| p.exists()).or_else(|| find_tool("firefox"))
}

fn profile_dir() -> PathBuf {
    config_dir().join("youtube-profile")
}

pub fn signed_in() -> bool {
    let profile = profile_dir();
    ["cookies.sqlite", "cookies.sqlite-wal"].iter().any(|file| {
        std::fs::read(profile.join(file)).is_ok_and(|bytes| bytes.windows(SESSION_COOKIE.len()).any(|w| w == SESSION_COOKIE))
    })
}

/// Opens Google's sign-in page and waits for the window to close.
pub async fn sign_in() -> Result<bool> {
    let browser = firefox().context("YouTube sign-in needs Firefox, which was not found")?;
    let profile = profile_dir();
    tokio::fs::create_dir_all(&profile).await?;
    let status = Command::new(browser).arg("-no-remote").arg("-profile").arg(&profile).arg(SIGN_IN_URL).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().await?;
    if !status.success() {
        bail!("Firefox exited with {status}");
    }
    forget_cached_feeds();
    Ok(signed_in())
}

pub fn sign_out() -> Result<()> {
    if profile_dir().exists() {
        std::fs::remove_dir_all(profile_dir())?;
    }
    forget_cached_feeds();
    Ok(())
}

fn cookie_source() -> Option<String> {
    signed_in().then(|| format!("firefox:{}", profile_dir().display()))
}

fn feed_cache_path(id: &str) -> PathBuf {
    cache_dir().join("youtube").join(format!("{id}.json"))
}

fn forget_cached_feeds() {
    let _ = std::fs::remove_dir_all(cache_dir().join("youtube"));
}

fn read_cached_feed(id: &str) -> Option<Vec<Meta>> {
    let path = feed_cache_path(id);
    let age = SystemTime::now().duration_since(std::fs::metadata(&path).ok()?.modified().ok()?).ok()?;
    if age.as_secs() > FEED_CACHE_SECONDS {
        return None;
    }
    saved_feed(id)
}

/// The last list saved for a catalog, however old, to show while a fresh one loads.
pub fn saved_feed(id: &str) -> Option<Vec<Meta>> {
    serde_json::from_slice(&std::fs::read(feed_cache_path(id)).ok()?).ok()
}

fn thumbnail(id: &str) -> String {
    format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")
}

/// "12:34" or "1:02:03" as whole minutes, in the form the rest of reel displays.
fn runtime_from_clock(clock: &str) -> String {
    let seconds = clock.split(':').filter_map(|part| part.trim().parse::<u32>().ok()).fold(0, |total, part| total * 60 + part);
    if seconds == 0 { String::new() } else { format!("{} min", seconds.div_ceil(60)) }
}

/// "3 years ago" as a year; anything more recent counts as this year.
fn year_from_age(age: &str, this_year: i32) -> String {
    let mut words = age.split_whitespace();
    let count = words.next().and_then(|n| n.parse::<i32>().ok());
    match (count, words.next()) {
        (Some(years), Some(unit)) if unit.starts_with("year") => (this_year - years).to_string(),
        (Some(_), Some(_)) => this_year.to_string(),
        _ => String::new(),
    }
}

fn text_of(value: Option<&Value>) -> String {
    let Some(value) = value else { return String::new() };
    if let Some(simple) = value.get("simpleText").and_then(Value::as_str) {
        return simple.to_string();
    }
    value.get("runs").and_then(Value::as_array).map(|runs| runs.iter().filter_map(|r| r.get("text").and_then(Value::as_str)).collect()).unwrap_or_default()
}

fn meta_from_renderer(video: &Value, this_year: i32) -> Option<Meta> {
    let id = video.get("videoId")?.as_str()?;
    let channel = text_of(video.get("ownerText").or_else(|| video.get("longBylineText")));
    Some(Meta {
        id: format!("{ID_PREFIX}{id}"),
        kind: KIND.into(),
        name: text_of(video.get("title")),
        poster: thumbnail(id),
        description: channel.clone(),
        release_info: year_from_age(&text_of(video.get("publishedTimeText")), this_year),
        runtime: runtime_from_clock(&text_of(video.get("lengthText"))),
        genres: if channel.is_empty() { Vec::new() } else { vec![channel] },
        sources: vec!["YouTube".into()],
        ..Default::default()
    })
}

/// Finds videos and the next-page token wherever YouTube nests them in its response.
fn collect_results(value: &Value, this_year: i32, items: &mut Vec<Meta>, next_page: &mut Option<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                match key.as_str() {
                    "videoRenderer" => items.extend(meta_from_renderer(child, this_year)),
                    "continuationCommand" => {
                        if let Some(token) = child.get("token").and_then(Value::as_str) {
                            *next_page = Some(token.to_string());
                        }
                    }
                    _ => collect_results(child, this_year, items, next_page),
                }
            }
        }
        Value::Array(values) => values.iter().for_each(|v| collect_results(v, this_year, items, next_page)),
        _ => {}
    }
}

/// Next-page tokens by query, with how many results came before them.
fn next_pages() -> &'static Mutex<HashMap<String, (usize, String)>> {
    static PAGES: OnceLock<Mutex<HashMap<String, (usize, String)>>> = OnceLock::new();
    PAGES.get_or_init(Mutex::default)
}

async fn search_directly(http: &reqwest::Client, query: &str, skip: usize) -> Result<Vec<Meta>> {
    let client = json!({ "client": { "clientName": "WEB", "clientVersion": WEB_CLIENT_VERSION, "hl": "en", "gl": "US" } });
    let body = if skip == 0 {
        json!({ "context": client, "query": query, "params": VIDEOS_ONLY })
    } else {
        let token = next_pages().lock().unwrap().get(query).filter(|(at, _)| *at == skip).map(|(_, token)| token.clone()).context("no next page")?;
        json!({ "context": client, "continuation": token })
    };
    let response: Value = http.post(SEARCH_URL).json(&body).send().await?.error_for_status()?.json().await?;
    let this_year = chrono::Datelike::year(&chrono::Local::now());
    let (mut items, mut next_page) = (Vec::new(), None);
    collect_results(&response, this_year, &mut items, &mut next_page);
    if items.is_empty() && skip == 0 {
        bail!("no videos in the response");
    }
    let mut pages = next_pages().lock().unwrap();
    if pages.len() >= MAX_REMEMBERED_SEARCHES {
        pages.clear();
    }
    match next_page {
        Some(token) => pages.insert(query.to_string(), (skip + items.len(), token)),
        None => pages.remove(query),
    };
    Ok(items)
}

/// Searches directly, falling back to yt-dlp if YouTube ever changes its response.
async fn search(http: &reqwest::Client, query: &str, skip: usize) -> Result<Vec<Meta>> {
    match search_directly(http, query, skip).await {
        Ok(items) => Ok(items),
        Err(_) => list(&format!("ytsearch{}:{query}", skip + PAGE_SIZE), skip, false).await,
    }
}

/// One line of the yt-dlp output below: id, title, channel, seconds, upload date, tab-separated.
fn parse_line(line: &str) -> Option<Meta> {
    let mut fields = line.split('\t');
    let id = fields.next()?.trim();
    // Feeds can also list channels and playlists; only videos have 11-character ids.
    if id.len() != 11 {
        return None;
    }
    let title = fields.next().unwrap_or_default().trim().to_string();
    let channel = fields.next().filter(|c| *c != "NA").unwrap_or_default().trim().to_string();
    let runtime = fields.next().and_then(|d| d.trim().parse::<f64>().ok()).map(|seconds| format!("{} min", (seconds / 60.0).ceil() as u32)).unwrap_or_default();
    let date = fields.next().filter(|d| *d != "NA").unwrap_or_default().trim();
    Some(Meta {
        id: format!("{ID_PREFIX}{id}"),
        kind: KIND.into(),
        name: title,
        poster: thumbnail(id),
        description: channel.clone(),
        release_info: date.get(..4).unwrap_or_default().to_string(),
        runtime,
        genres: if channel.is_empty() { Vec::new() } else { vec![channel] },
        sources: vec!["YouTube".into()],
        ..Default::default()
    })
}

/// Only the account's own feeds send its session; searches stay anonymous even when signed in.
async fn list(target: &str, skip: usize, use_account: bool) -> Result<Vec<Meta>> {
    let tool = ytdlp().ok_or_else(|| anyhow!("YouTube needs yt-dlp: winget install yt-dlp.yt-dlp"))?;
    let mut command = Command::new(tool);
    command
        .args(["--flat-playlist", "--no-warnings", "--ignore-errors", "--encoding", "utf-8", "--print", "%(id)s\t%(title)s\t%(channel,uploader)s\t%(duration)s\t%(upload_date)s"])
        .args(["--playlist-start", &(skip + 1).to_string(), "--playlist-end", &(skip + PAGE_SIZE).to_string()])
        .env("PATH", child_path())
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    if let Some(cookies) = cookie_source().filter(|_| use_account) {
        command.args(["--cookies-from-browser", &cookies]);
    }
    command.arg(target);
    let output = tokio::time::timeout(LIST_TIMEOUT, command.output()).await.map_err(|_| anyhow!("YouTube took too long to answer"))??;
    let text = String::from_utf8_lossy(&output.stdout);
    let items: Vec<Meta> = text.lines().filter_map(parse_line).collect();
    if items.is_empty() && !output.status.success() {
        let errors = String::from_utf8_lossy(&output.stderr);
        bail!("{}", errors.lines().rfind(|l| l.contains("ERROR")).unwrap_or("yt-dlp failed").trim().to_string());
    }
    Ok(items)
}

pub async fn catalog(http: &reqwest::Client, id: &str, query: Option<&str>, skip: usize) -> Result<Vec<Meta>> {
    if id == SEARCH_CATALOG {
        return search(http, query.unwrap_or_default(), 0).await;
    }
    let topic = TOPICS.iter().find(|(topic, ..)| *topic == id);
    if topic.is_none() && !signed_in() {
        bail!("{NOT_SIGNED_IN}");
    }
    if skip == 0
        && let Some(cached) = read_cached_feed(id)
    {
        return Ok(cached);
    }
    let items = match topic {
        Some((_, _, topic_query)) => search(http, topic_query, skip).await?,
        None => {
            let (_, _, target) = FEEDS.iter().find(|(feed, ..)| *feed == id).context("unknown YouTube catalog")?;
            list(target, skip, true).await?
        }
    };
    if skip == 0 {
        let path = feed_cache_path(id);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec(&items) {
            let _ = std::fs::write(path, bytes);
        }
    }
    Ok(items)
}

fn max_height() -> u32 {
    static HEIGHT: OnceLock<u32> = OnceLock::new();
    *HEIGHT.get_or_init(|| Config::load().unwrap_or_default().youtube.max_height)
}

pub fn streams(id: &str) -> Vec<Stream> {
    let Some(video) = id.strip_prefix(ID_PREFIX) else { return Vec::new() };
    let height = max_height();
    vec![Stream { url: Some(format!("https://www.youtube.com/watch?v={video}")), name: format!("YouTube\n{height}p"), title: format!("Best available up to {height}p, no ads"), ..Default::default() }]
}

pub fn is_youtube_url(target: &str) -> bool {
    target.contains("youtube.com/") || target.contains("youtu.be/")
}

/// mpv options for YouTube playback. Ads are never part of the stream yt-dlp resolves.
pub fn mpv_args() -> Vec<String> {
    let height = max_height();
    let mut args = vec![format!("--ytdl-format=bv*[height<={height}]+ba/b[height<={height}]/b")];
    if let Some(tool) = ytdlp() {
        args.push(format!("--script-opts-append=ytdl_hook-ytdl_path={}", tool.display()));
    }
    if let Some(cookies) = cookie_source() {
        args.push(format!("--ytdl-raw-options-append=cookies-from-browser={cookies}"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listing_lines() {
        let meta = parse_line("aqz-KE-bpKQ\tBig Buck Bunny\tBlender\t635.0\t20140410").unwrap();
        assert_eq!(meta.id, "yt:aqz-KE-bpKQ");
        assert_eq!(meta.name, "Big Buck Bunny");
        assert_eq!(meta.genres, vec!["Blender".to_string()]);
        assert_eq!(meta.runtime, "11 min");
        assert_eq!(meta.release_info, "2014");
        assert!(meta.poster.contains("aqz-KE-bpKQ"));
    }

    #[test]
    fn tolerates_missing_fields_and_skips_non_videos() {
        let meta = parse_line("aqz-KE-bpKQ\tTitle\tNA\tNA\tNA").unwrap();
        assert!(meta.genres.is_empty());
        assert_eq!(meta.runtime, "");
        assert_eq!(meta.release_info, "");
        assert!(parse_line("UCxxxxxxxxxxxxxxxxxxxxxx\tA channel").is_none());
        assert!(parse_line("").is_none());
    }

    /// Talks to YouTube, so it runs only on request: cargo test -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn direct_search_is_fast_and_pages() {
        let http = reqwest::Client::builder().user_agent("reel").build().unwrap();
        let started = std::time::Instant::now();
        let items = search_directly(&http, "big buck bunny", 0).await.unwrap();
        println!("first page: {} videos in {} ms", items.len(), started.elapsed().as_millis());
        assert!(items.len() >= 10, "got {} videos", items.len());
        let first = &items[0];
        println!("sample: {} | {} | {} | {} | {}", first.id, first.name, first.genres.join(","), first.runtime, first.release_info);
        assert!(first.id.starts_with(ID_PREFIX) && !first.name.is_empty());
        assert!(items.iter().any(|m| !m.runtime.is_empty()), "durations should be present");
        let started = std::time::Instant::now();
        let next = search_directly(&http, "big buck bunny", items.len()).await.unwrap();
        println!("next page: {} videos in {} ms", next.len(), started.elapsed().as_millis());
        assert!(!next.is_empty());
        assert!(!items.iter().any(|m| m.id == next[0].id), "the next page has new videos");

        let started = std::time::Instant::now();
        let bytes = http.get(thumbnail("aqz-KE-bpKQ")).send().await.unwrap().bytes().await.unwrap();
        println!("one thumbnail: {} KB in {} ms", bytes.len() / 1024, started.elapsed().as_millis());
    }

    #[test]
    fn converts_durations_and_ages() {
        assert_eq!(runtime_from_clock("12:34"), "13 min");
        assert_eq!(runtime_from_clock("1:02:03"), "63 min");
        assert_eq!(runtime_from_clock(""), "");
        assert_eq!(year_from_age("3 years ago", 2026), "2023");
        assert_eq!(year_from_age("5 months ago", 2026), "2026");
        assert_eq!(year_from_age("Streamed", 2026), "");
    }

    #[test]
    fn finds_videos_however_they_are_nested() {
        let response: Value = serde_json::from_str(
            r#"{"contents":{"a":[{"b":{"videoRenderer":{"videoId":"aqz-KE-bpKQ","title":{"runs":[{"text":"Big "},{"text":"Buck Bunny"}]},
               "ownerText":{"runs":[{"text":"Blender"}]},"lengthText":{"simpleText":"10:35"},"publishedTimeText":{"simpleText":"11 years ago"}}}},
               {"continuationItemRenderer":{"continuationEndpoint":{"continuationCommand":{"token":"NEXT"}}}}]}}"#,
        )
        .unwrap();
        let (mut items, mut next) = (Vec::new(), None);
        collect_results(&response, 2026, &mut items, &mut next);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Big Buck Bunny");
        assert_eq!(items[0].genres, vec!["Blender".to_string()]);
        assert_eq!(items[0].runtime, "11 min");
        assert_eq!(items[0].release_info, "2015");
        assert_eq!(next.as_deref(), Some("NEXT"));
    }

    #[test]
    fn only_youtube_ids_have_streams() {
        assert_eq!(streams("yt:aqz-KE-bpKQ")[0].url.as_deref(), Some("https://www.youtube.com/watch?v=aqz-KE-bpKQ"));
        assert!(streams("tt0013442").is_empty());
    }

    #[test]
    fn manifest_lists_feeds_and_hides_search_from_browsing() {
        let m = manifest();
        assert!(m.supports("stream", "youtube", "yt:abc"));
        assert!(!m.supports("stream", "movie", "tt0013442"));
        assert!(m.catalogs.iter().filter(|c| c.browsable()).count() == TOPICS.len() + FEEDS.len());
        assert_eq!(m.catalogs[0].id, "music", "a section that needs no account opens first");
        assert!(m.catalogs.iter().any(|c| c.supports_search()));
    }
}
