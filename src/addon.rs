//! Client for the Stremio addon protocol: manifest, catalog, meta, stream, subtitles.
//! Responses are read leniently from JSON because community addons vary a lot.
use crate::config::{AddonEntry, LOCAL_ADDON_URL, cache_dir};
use crate::local;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResourceDef {
    pub name: String,
    pub types: Vec<String>,
    pub id_prefixes: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CatalogDef {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub genres: Vec<String>,
    pub extras: Vec<String>,
    pub required: Vec<String>,
}

impl CatalogDef {
    pub fn supports_search(&self) -> bool {
        self.extras.iter().any(|e| e == "search")
    }

    /// Browsable without a search term. A required genre is fine: we pick its first option.
    pub fn browsable(&self) -> bool {
        self.required.iter().all(|r| r == "genre" && !self.genres.is_empty())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub types: Vec<String>,
    pub id_prefixes: Vec<String>,
    pub resources: Vec<ResourceDef>,
    pub catalogs: Vec<CatalogDef>,
    pub configurable: bool,
}

fn str_of(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Decodes the HTML entities some metadata sources leave in names and plots.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&apos;", "'").replace("&#39;", "'").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn list_of(v: &Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

impl Manifest {
    pub fn from_value(v: &Value) -> Result<Self> {
        let name = str_of(v, "name");
        if name.is_empty() {
            bail!("not a Stremio addon manifest");
        }
        let types = list_of(v, "types");
        let id_prefixes = list_of(v, "idPrefixes");
        let resources = v
            .get("resources")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|r| match r {
                        Value::String(s) => ResourceDef { name: s.clone(), types: types.clone(), id_prefixes: id_prefixes.clone() },
                        other => ResourceDef {
                            name: str_of(other, "name"),
                            types: if other.get("types").is_some() { list_of(other, "types") } else { types.clone() },
                            id_prefixes: if other.get("idPrefixes").is_some() { list_of(other, "idPrefixes") } else { id_prefixes.clone() },
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();
        let catalogs = v
            .get("catalogs")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(catalog_from_value).collect())
            .unwrap_or_default();
        let hints = v.get("behaviorHints").cloned().unwrap_or(Value::Null);
        Ok(Self {
            id: str_of(v, "id"),
            name,
            version: str_of(v, "version"),
            description: str_of(v, "description"),
            types,
            id_prefixes,
            resources,
            catalogs,
            configurable: hints.get("configurable").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    pub fn has_resource(&self, name: &str) -> bool {
        self.resources.iter().any(|r| r.name == name)
    }

    pub fn resource_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for wanted in ["catalog", "meta", "stream", "subtitles"] {
            if wanted == "catalog" && !self.catalogs.is_empty() || self.has_resource(wanted) && wanted != "catalog" {
                names.push(wanted.to_string());
            }
        }
        names
    }

    /// Whether this addon answers `resource` for this content type and id.
    pub fn supports(&self, resource: &str, kind: &str, id: &str) -> bool {
        self.resources.iter().any(|r| {
            r.name == resource
                && (r.types.is_empty() || r.types.iter().any(|t| t == kind))
                && (r.id_prefixes.is_empty() || r.id_prefixes.iter().any(|p| id.starts_with(p.as_str())))
        })
    }
}

fn catalog_from_value(c: &Value) -> CatalogDef {
    let mut genres = list_of(c, "genres");
    let mut extras = list_of(c, "extraSupported");
    let mut required = list_of(c, "extraRequired");
    if let Some(items) = c.get("extra").and_then(Value::as_array) {
        for extra in items {
            let name = str_of(extra, "name");
            if name == "genre" && genres.is_empty() {
                genres = list_of(extra, "options");
            }
            if extra.get("isRequired").and_then(Value::as_bool).unwrap_or(false) {
                required.push(name.clone());
            }
            extras.push(name);
        }
    }
    CatalogDef { kind: str_of(c, "type"), id: str_of(c, "id"), name: str_of(c, "name"), genres, extras, required }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub season: u32,
    pub episode: u32,
    pub released: String,
    pub overview: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub poster: String,
    pub description: String,
    pub release_info: String,
    pub runtime: String,
    pub genres: Vec<String>,
    pub director: Vec<String>,
    pub cast: Vec<String>,
    pub rating: String,
    pub videos: Vec<Video>,
    /// Which addons listed this item, in addon order. Filled when results are merged.
    #[serde(default)]
    pub sources: Vec<String>,
}

impl Meta {
    pub fn from_value(v: &Value) -> Self {
        let mut id = str_of(v, "id");
        if id.is_empty() {
            id = str_of(v, "imdb_id");
        }
        let mut release_info = str_of(v, "releaseInfo");
        if release_info.is_empty() {
            release_info = str_of(v, "year");
        }
        let mut genres = list_of(v, "genres");
        if genres.is_empty() {
            genres = list_of(v, "genre");
        }
        let videos = v
            .get("videos")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|e| Video {
                        id: str_of(e, "id"),
                        title: {
                            let t = str_of(e, "name");
                            if t.is_empty() { str_of(e, "title") } else { t }
                        },
                        season: e.get("season").and_then(Value::as_u64).unwrap_or(0) as u32,
                        episode: e.get("episode").or_else(|| e.get("number")).and_then(Value::as_u64).unwrap_or(0) as u32,
                        released: str_of(e, "released"),
                        overview: str_of(e, "overview"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            id,
            kind: str_of(v, "type"),
            name: decode_entities(&str_of(v, "name")),
            poster: str_of(v, "poster"),
            description: decode_entities(&str_of(v, "description")),
            release_info,
            runtime: str_of(v, "runtime"),
            genres,
            director: list_of(v, "director").iter().map(|n| decode_entities(n)).collect(),
            cast: list_of(v, "cast").iter().map(|n| decode_entities(n)).collect(),
            rating: str_of(v, "imdbRating"),
            videos,
            sources: Vec::new(),
        }
    }

    pub fn year(&self) -> &str {
        self.release_info.get(..4).unwrap_or(&self.release_info)
    }

    pub fn is_series(&self) -> bool {
        self.kind == "series" || self.kind == "anime" || !self.videos.is_empty() && self.videos.iter().any(|v| v.season > 0)
    }

    /// Fill gaps in this item from a fuller copy of the same title.
    pub fn merge_detail(&mut self, detail: Meta) {
        let sources = std::mem::take(&mut self.sources);
        let keep_poster = std::mem::take(&mut self.poster);
        *self = Meta { id: self.id.clone(), kind: self.kind.clone(), sources, ..detail };
        if self.poster.is_empty() {
            self.poster = keep_poster;
        }
    }

    /// The largest poster the source offers, falling back to Metahub for IMDb ids.
    pub fn poster_url(&self) -> Option<String> {
        if !self.poster.is_empty() {
            return Some(self.poster.replace("/poster/small/", "/poster/medium/"));
        }
        self.id.starts_with("tt").then(|| format!("https://images.metahub.space/poster/medium/{}/img", self.id.split(':').next().unwrap_or(&self.id)))
    }
}

/// "94 min" becomes "1h 34m"; anything else passes through.
pub fn format_runtime(runtime: &str) -> String {
    let digits: String = runtime.chars().take_while(char::is_ascii_digit).collect();
    match digits.parse::<u32>() {
        Ok(mins) if runtime.to_ascii_lowercase().contains("min") => {
            if mins >= 60 { format!("{}h {:02}m", mins / 60, mins % 60) } else { format!("{mins}m") }
        }
        _ => runtime.to_string(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stream {
    pub url: Option<String>,
    pub info_hash: Option<String>,
    pub file_idx: Option<usize>,
    pub yt_id: Option<String>,
    pub external_url: Option<String>,
    pub name: String,
    pub title: String,
    pub description: String,
    pub filename: String,
    pub size: Option<u64>,
    pub trackers: Vec<String>,
}

impl Stream {
    fn from_value(v: &Value) -> Self {
        let opt = |key: &str| Some(str_of(v, key)).filter(|s| !s.is_empty());
        let hints = v.get("behaviorHints").cloned().unwrap_or(Value::Null);
        let trackers = list_of(v, "sources").into_iter().filter_map(|s| s.strip_prefix("tracker:").map(str::to_string)).collect();
        Self {
            url: opt("url"),
            info_hash: opt("infoHash").map(|h| h.to_ascii_lowercase()),
            file_idx: v.get("fileIdx").and_then(Value::as_u64).map(|i| i as usize),
            yt_id: opt("ytId"),
            external_url: opt("externalUrl"),
            name: str_of(v, "name"),
            title: str_of(v, "title"),
            description: str_of(v, "description"),
            filename: str_of(&hints, "filename"),
            size: hints.get("videoSize").and_then(Value::as_u64),
            trackers,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Subtitle {
    pub url: String,
    pub lang: String,
    pub addon: String,
}

#[derive(Clone, Debug)]
pub struct Addon {
    pub url: String,
    pub enabled: bool,
    pub manifest: Manifest,
    /// Last measured response time in ms; None until the addon answers, Some(0) for local.
    pub latency: Option<u64>,
    pub error: Option<String>,
}

impl Addon {
    pub fn is_local(&self) -> bool {
        self.url == LOCAL_ADDON_URL
    }

    fn base(&self) -> &str {
        self.url.strip_suffix("/manifest.json").unwrap_or(&self.url)
    }

    pub fn configure_url(&self) -> Option<String> {
        (self.manifest.configurable && !self.is_local()).then(|| format!("{}/configure", self.base()))
    }
}

/// Accepts a manifest URL, a stremio:// link or an addon's /configure page.
pub fn normalize_url(input: &str) -> Result<String> {
    let input = input.trim();
    if input == LOCAL_ADDON_URL {
        return Ok(input.to_string());
    }
    let url = input.strip_prefix("stremio://").map(|rest| format!("https://{rest}")).unwrap_or_else(|| input.to_string());
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("addon URL must start with http://, https:// or stremio://");
    }
    let url = url.split(['?', '#']).next().unwrap_or(&url).trim_end_matches('/').to_string();
    if url.ends_with("/manifest.json") {
        return Ok(url);
    }
    let base = url.strip_suffix("/configure").unwrap_or(&url);
    Ok(format!("{base}/manifest.json"))
}

fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[derive(Clone)]
pub struct Client {
    pub http: reqwest::Client,
}

impl Client {
    pub fn new(timeout_secs: u64) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("reel/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(timeout_secs.max(2)))
            .build()?;
        Ok(Self { http })
    }

    /// Addons are often small free servers, so one failed request gets one quick retry.
    async fn get_json(&self, url: &str) -> Result<Value> {
        match self.get_json_once(url).await {
            Ok(value) => Ok(value),
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                self.get_json_once(url).await
            }
        }
    }

    async fn get_json_once(&self, url: &str) -> Result<Value> {
        let response = self.http.get(url).send().await.with_context(|| format!("requesting {url}"))?;
        let status = response.status();
        if !status.is_success() {
            bail!("{status} from {url}");
        }
        response.json().await.with_context(|| format!("reading {url}"))
    }

    pub async fn fetch_addon(&self, entry: &AddonEntry) -> Addon {
        if entry.url == LOCAL_ADDON_URL {
            return Addon { url: entry.url.clone(), enabled: entry.enabled, manifest: local::manifest(), latency: Some(0), error: None };
        }
        let started = Instant::now();
        let result = async {
            let value = self.get_json(&entry.url).await?;
            Manifest::from_value(&value)
        }
        .await;
        let cached = ManifestCache::load();
        match result {
            Ok(manifest) => {
                let mut cache = cached;
                cache.0.insert(entry.url.clone(), manifest.clone());
                cache.save();
                Addon { url: entry.url.clone(), enabled: entry.enabled, manifest, latency: Some(started.elapsed().as_millis() as u64), error: None }
            }
            Err(e) => Addon {
                url: entry.url.clone(),
                enabled: entry.enabled,
                manifest: cached.0.get(&entry.url).cloned().unwrap_or_else(|| Manifest { name: entry.url.clone(), ..Default::default() }),
                latency: None,
                error: Some(format!("{e:#}")),
            },
        }
    }

    pub async fn catalog(&self, addon: &Addon, catalog: &CatalogDef, extra: &[(&str, String)]) -> Result<Vec<Meta>> {
        if addon.is_local() {
            let search = extra.iter().find(|(k, _)| *k == "search").map(|(_, v)| v.as_str());
            return Ok(local::catalog(&catalog.id, search));
        }
        let mut url = format!("{}/catalog/{}/{}", addon.base(), catalog.kind, catalog.id);
        if !extra.is_empty() {
            let pairs: Vec<String> = extra.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect();
            url.push('/');
            url.push_str(&pairs.join("&"));
        }
        url.push_str(".json");
        let value = self.get_json(&url).await?;
        let mut items: Vec<Meta> = value
            .get("metas")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(Meta::from_value).filter(|m| !m.id.is_empty()).collect())
            .unwrap_or_default();
        for item in &mut items {
            if item.kind.is_empty() {
                item.kind = catalog.kind.clone();
            }
            item.sources = vec![addon.manifest.name.clone()];
        }
        Ok(items)
    }

    pub async fn meta(&self, addon: &Addon, kind: &str, id: &str) -> Result<Meta> {
        if addon.is_local() {
            return local::meta(id).context("file no longer exists");
        }
        let value = self.get_json(&format!("{}/meta/{kind}/{}.json", addon.base(), encode_id(id))).await?;
        value.get("meta").filter(|m| !m.is_null()).map(Meta::from_value).context("addon returned no meta")
    }

    pub async fn streams(&self, addon: &Addon, kind: &str, id: &str) -> Result<Vec<Stream>> {
        if addon.is_local() {
            return Ok(local::streams(id));
        }
        let value = self.get_json(&format!("{}/stream/{kind}/{}.json", addon.base(), encode_id(id))).await?;
        Ok(value.get("streams").and_then(Value::as_array).map(|s| s.iter().map(Stream::from_value).collect()).unwrap_or_default())
    }

    pub async fn subtitles(&self, addon: &Addon, kind: &str, id: &str) -> Result<Vec<Subtitle>> {
        let value = self.get_json(&format!("{}/subtitles/{kind}/{}.json", addon.base(), encode_id(id))).await?;
        Ok(value
            .get("subtitles")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|s| Subtitle { url: str_of(s, "url"), lang: str_of(s, "lang"), addon: addon.manifest.name.clone() })
                    .filter(|s| !s.url.is_empty())
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Episode ids keep their colons (tt123:1:2), which addons expect unescaped.
fn encode_id(id: &str) -> String {
    id.split(':').map(encode).collect::<Vec<_>>().join(":")
}

#[derive(Default, Serialize, Deserialize)]
pub struct ManifestCache(pub HashMap<String, Manifest>);

impl ManifestCache {
    fn path() -> std::path::PathBuf {
        cache_dir().join("manifests.json")
    }

    pub fn load() -> Self {
        fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(cache_dir());
        if let Ok(text) = serde_json::to_string(self) {
            let _ = fs::write(Self::path(), text);
        }
    }
}

/// Addons from disk, using cached manifests so the UI can draw before the network answers.
pub fn addons_from_cache(entries: &[AddonEntry]) -> Vec<Addon> {
    let cache = ManifestCache::load();
    entries
        .iter()
        .map(|e| {
            let manifest = if e.url == LOCAL_ADDON_URL {
                local::manifest()
            } else {
                cache.0.get(&e.url).cloned().unwrap_or_else(|| Manifest { name: e.url.clone(), ..Default::default() })
            };
            Addon { url: e.url.clone(), enabled: e.enabled, manifest, latency: (e.url == LOCAL_ADDON_URL).then_some(0), error: None }
        })
        .collect()
}

/// Two-letter display code for a subtitle language such as "eng" or "en".
pub fn lang_code(lang: &str) -> String {
    let lower = lang.to_ascii_lowercase();
    let short = match lower.as_str() {
        "eng" => "en",
        "ger" | "deu" => "de",
        "hun" => "hu",
        "fre" | "fra" => "fr",
        "spa" | "spl" => "es",
        "ita" => "it",
        "por" | "pob" => "pt",
        "rus" => "ru",
        "jpn" => "ja",
        "kor" => "ko",
        "chi" | "zho" => "zh",
        "ara" => "ar",
        "dut" | "nld" => "nl",
        "pol" => "pl",
        "tur" => "tr",
        "swe" => "sv",
        "nor" => "no",
        "dan" => "da",
        "fin" => "fi",
        "cze" | "ces" => "cs",
        "gre" | "ell" => "el",
        "heb" => "he",
        "hin" => "hi",
        "rum" | "ron" => "ro",
        "ukr" => "uk",
        other => other,
    };
    short.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_addon_urls() {
        let expected = "https://torrentio.strem.fun/manifest.json";
        assert_eq!(normalize_url("https://torrentio.strem.fun/configure").unwrap(), expected);
        assert_eq!(normalize_url("https://torrentio.strem.fun/configure/").unwrap(), expected);
        assert_eq!(normalize_url("stremio://torrentio.strem.fun/manifest.json").unwrap(), expected);
        assert_eq!(normalize_url("https://torrentio.strem.fun").unwrap(), expected);
        assert_eq!(
            normalize_url("stremio://torrentio.strem.fun/sort=qualitysize|qualityfilter=cam/manifest.json").unwrap(),
            "https://torrentio.strem.fun/sort=qualitysize|qualityfilter=cam/manifest.json"
        );
        assert!(normalize_url("ftp://nope").is_err());
    }

    #[test]
    fn reads_torrentio_style_manifest() {
        let value: Value = serde_json::from_str(
            r#"{"id":"x","name":"Torrentio","version":"0.0.15","types":["movie","series"],"catalogs":[],
                "resources":[{"name":"stream","types":["movie","series","anime"],"idPrefixes":["tt","kitsu"]}],
                "behaviorHints":{"configurable":true}}"#,
        )
        .unwrap();
        let m = Manifest::from_value(&value).unwrap();
        assert!(m.configurable);
        assert!(m.supports("stream", "movie", "tt0013442"));
        assert!(m.supports("stream", "series", "tt0944947:1:2"));
        assert!(!m.supports("stream", "movie", "local:foo"));
        assert!(!m.supports("meta", "movie", "tt0013442"));
    }

    #[test]
    fn decodes_entities_in_meta() {
        let value: Value = serde_json::from_str(r#"{"id":"tt1","name":"Tom &amp; Jerry","cast":["Judith O&apos;Dea"]}"#).unwrap();
        let meta = Meta::from_value(&value);
        assert_eq!(meta.name, "Tom & Jerry");
        assert_eq!(meta.cast[0], "Judith O'Dea");
    }

    #[test]
    fn formats_runtime() {
        assert_eq!(format_runtime("94 min"), "1h 34m");
        assert_eq!(format_runtime("45 min"), "45m");
        assert_eq!(format_runtime("2h"), "2h");
    }
}
