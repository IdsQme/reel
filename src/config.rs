//! config.toml, addons.toml and the small JSON state file, all under ~/.config/reel.
use crate::addon::Meta;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub const LOCAL_ADDON_URL: &str = "local";
pub const DEFAULT_ADDONS: [&str; 4] = [
    "https://v3-cinemeta.strem.io/manifest.json",
    "https://torrentio.strem.fun/manifest.json",
    "https://opensubtitles-v3.strem.io/manifest.json",
    LOCAL_ADDON_URL,
];

pub fn config_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".config").join("reel")
}

pub fn cache_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".cache").join("reel")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub theme: String,
    pub accent: String,
    /// auto|kitty|sixel|blocks|off
    pub posters: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Player {
    pub bin: String,
    pub args: Vec<String>,
    /// embedded|window|fullscreen
    pub output: String,
    pub resume: bool,
    pub volume: u32,
    /// Use mpv's high-quality profile: better scaling and debanding, a little more GPU.
    pub high_quality: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Streams {
    pub prefer: Vec<String>,
    /// best|ask
    pub autoplay: String,
    /// seconds per addon
    pub timeout: u64,
    pub autoplay_next: bool,
    /// "Play best" avoids files larger than this, which need a very fast swarm to stream.
    pub max_size_gb: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Subtitles {
    pub languages: Vec<String>,
    pub auto: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub play: String,
    pub search: String,
    pub addons: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Local {
    pub dirs: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: Ui,
    pub player: Player,
    pub streams: Streams,
    pub subtitles: Subtitles,
    pub keys: Keys,
    pub local: Local,
}

impl Default for Ui {
    fn default() -> Self {
        Self { theme: "catppuccin-mocha".into(), accent: "mauve".into(), posters: "auto".into() }
    }
}
impl Default for Player {
    fn default() -> Self {
        Self { bin: "mpv".into(), args: vec!["--hwdec=auto".into()], output: "embedded".into(), resume: true, volume: 70, high_quality: true }
    }
}
impl Default for Streams {
    fn default() -> Self {
        Self { prefer: vec!["1080p".into(), "720p".into()], autoplay: "best".into(), timeout: 8, autoplay_next: true, max_size_gb: 25 }
    }
}
impl Default for Subtitles {
    fn default() -> Self {
        Self { languages: vec!["en".into()], auto: true }
    }
}
impl Default for Keys {
    fn default() -> Self {
        Self { play: "p".into(), search: "/".into(), addons: "a".into() }
    }
}
impl Default for Local {
    fn default() -> Self {
        Self { dirs: vec!["~/Films".into(), "~/Videos".into()] }
    }
}

pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
            Err(_) => {
                let config = Self::default();
                config.save()?;
                Ok(config)
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(config_dir())?;
        let header = "# ~/.config/reel/config.toml\n# posters: auto|kitty|sixel|blocks|off · output: embedded|window|fullscreen · autoplay: best|ask\n# addons live in addons.toml\n\n";
        fs::write(Self::path(), format!("{header}{}", toml::to_string_pretty(self)?))?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddonEntry {
    pub url: String,
    pub enabled: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AddonsFile {
    #[serde(default)]
    addon: Vec<AddonEntry>,
}

pub fn addons_path() -> PathBuf {
    config_dir().join("addons.toml")
}

pub fn load_addons() -> Result<Vec<AddonEntry>> {
    match fs::read_to_string(addons_path()) {
        Ok(text) => Ok(toml::from_str::<AddonsFile>(&text).context("parsing addons.toml")?.addon),
        Err(_) => {
            let entries: Vec<_> = DEFAULT_ADDONS.iter().map(|u| AddonEntry { url: u.to_string(), enabled: true }).collect();
            save_addons(&entries)?;
            Ok(entries)
        }
    }
}

pub fn save_addons(entries: &[AddonEntry]) -> Result<()> {
    fs::create_dir_all(config_dir())?;
    let file = AddonsFile { addon: entries.to_vec() };
    fs::write(addons_path(), toml::to_string_pretty(&file)?)?;
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub meta: Meta,
    /// Episode id for series, otherwise the same as meta.id.
    pub video_id: String,
    pub position: f64,
    pub duration: f64,
    pub updated: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub library: Vec<Meta>,
    pub progress: HashMap<String, Progress>,
    pub recent: Vec<String>,
}

impl State {
    fn path() -> PathBuf {
        config_dir().join("state.json")
    }

    pub fn load() -> Self {
        fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(config_dir());
        if let Ok(text) = serde_json::to_string(self) {
            let _ = fs::write(Self::path(), text);
        }
    }

    /// In-progress items, most recently watched first.
    pub fn continuing(&self) -> Vec<&Progress> {
        let mut items: Vec<_> = self.progress.values().filter(|p| p.duration > 0.0 && p.position / p.duration < 0.95).collect();
        items.sort_by_key(|p| std::cmp::Reverse(p.updated));
        items
    }

    pub fn in_library(&self, id: &str) -> bool {
        self.library.iter().any(|m| m.id == id)
    }

    pub fn toggle_library(&mut self, meta: &Meta) -> bool {
        if let Some(i) = self.library.iter().position(|m| m.id == meta.id) {
            self.library.remove(i);
            false
        } else {
            self.library.insert(0, meta.clone());
            true
        }
    }

    pub fn push_recent(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.recent.retain(|r| r != query);
        self.recent.insert(0, query.to_string());
        self.recent.truncate(8);
    }
}
