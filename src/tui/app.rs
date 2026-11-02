//! Application state and all behaviour. Drawing lives in draw.rs.
use super::poster::{PosterKey, Posters};
use crate::addon::{self, Addon, CatalogDef, Client, Meta, Subtitle, lang_code};
use crate::cli;
use crate::config::{self, AddonEntry, Config, Progress, State};
use crate::embed::VideoHost;
use crate::mpv::{self, Mpv, MpvControl, MpvEvent, PlayRequest, PlayerState};
use crate::streams::{self, StreamRow};
use crate::torrent::{Engine, TorrentStatus};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use image::DynamicImage;
use ratatui::layout::Rect;
use ratatui_image::protocol::Protocol;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{OnceCell, mpsc};

pub enum Ev {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    Tick,
    AddonReady(String, Box<Addon>),
    Catalog { generation: u64, items: Vec<Meta>, append: bool, ms: u128, error: Option<String> },
    Meta(String, Box<Meta>),
    Streams { video_id: String, rows: Vec<StreamRow>, ms: u128, error: Option<String> },
    Subs(String, Vec<Subtitle>),
    PreviewDue(u64),
    SearchDue(u64),
    Search { generation: u64, items: Vec<Meta>, ms: u128 },
    PosterRaw(String, Option<DynamicImage>),
    PosterEncoded(PosterKey, Option<Protocol>),
    Installed(Result<Box<Addon>, String>),
    PlayResolved { session: u64, hash: Option<String>, filename: String },
    PlayFailed { session: u64, error: String },
    MpvStarted { session: u64, control: MpvControl },
    Mpv(u64, MpvEvent),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Discover,
    Search,
    Library,
    Addons,
    Settings,
    Details,
    Playing,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiscoverFocus {
    Catalogs,
    Grid,
    Continue,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetailFocus {
    Episodes,
    Streams,
}

/// Where a play request should put the video.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlayMode {
    Configured,
    Embedded,
    Window,
    Fullscreen,
    AudioOnly,
}

/// What a mouse click on part of the last frame does. Draw code registers these.
#[derive(Clone, PartialEq, Debug)]
pub enum Hit {
    Key(KeyCode),
    Go(Screen),
    Grid(usize),
    LibraryItem(usize),
    Catalog(usize),
    ContinueItem(usize),
    SearchRow(usize),
    SearchKind(usize),
    Stream(usize),
    Episode(usize),
    Subtitle(Option<String>),
    Addon(usize),
    AddonToggle(usize),
    Setting(usize),
    PlayWith(usize),
    UpNext(usize),
    /// Audio or subtitle track; id 0 turns subtitles off.
    Track(&'static str, i64),
    Seek,
    Volume,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    None,
    Help,
    PlayWith(usize),
}

pub struct CatalogRow {
    pub addon: usize,
    pub def: CatalogDef,
}

pub const SORTS: [&str; 4] = ["Popular", "Year", "Title", "Rating"];

pub struct Discover {
    pub focus: DiscoverFocus,
    pub catalogs: Vec<CatalogRow>,
    pub selected: usize,
    pub items: Vec<Meta>,
    pub cursor: usize,
    pub continue_cursor: usize,
    pub sort: usize,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub has_more: bool,
    pub loading: bool,
    pub generation: u64,
    pub ms: Option<u128>,
    pub error: Option<String>,
}

pub struct Search {
    pub query: String,
    pub kind: usize,
    pub results: Vec<Meta>,
    pub cursor: usize,
    pub generation: u64,
    pub searching: bool,
    pub ms: Option<u128>,
    pub recent_cursor: usize,
}

pub const SEARCH_KINDS: [&str; 3] = ["all", "movie", "series"];

pub struct Details {
    pub meta: Meta,
    pub back: Screen,
    pub focus: DetailFocus,
    pub season: u32,
    pub episode_cursor: usize,
    pub video_id: String,
    pub stream_cursor: usize,
    pub cursor_touched: bool,
    pub sub_choice: Option<String>,
    pub sub_touched: bool,
}

#[derive(Default, Clone)]
pub struct StreamSet {
    pub rows: Vec<StreamRow>,
    pub pending: usize,
    pub total: usize,
    pub ms: u128,
    /// Addons that failed to answer. Such a set is fetched again next time it is opened.
    pub errors: Vec<String>,
}

impl StreamSet {
    pub fn addons_with_results(&self) -> usize {
        let mut names: Vec<&str> = self.rows.iter().map(|r| r.addon.as_str()).collect();
        names.dedup();
        names.sort();
        names.dedup();
        names.len()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Resolving,
    Loading,
    Playing,
}

pub struct UpNext {
    pub meta: Meta,
    pub video_id: String,
    pub label: String,
    pub detail: String,
}

pub struct Playing {
    pub session: u64,
    pub meta: Meta,
    pub video_id: String,
    pub title: String,
    pub quality: String,
    pub release: String,
    pub addon: String,
    pub hash: Option<String>,
    pub phase: Phase,
    pub state: PlayerState,
    pub control: Option<MpvControl>,
    pub log: VecDeque<(String, String, String)>,
    pub torrent: Option<TorrentStatus>,
    pub last_saved: Instant,
    pub eof: bool,
    pub up_next: Vec<UpNext>,
    pub fullscreen: bool,
    /// The video is drawn inside reel rather than in its own window.
    pub embedded: bool,
    /// Embedded video fills the screen, with only the transport row below it.
    pub theater: bool,
    /// Embedded video covers the whole monitor.
    pub video_fullscreen: bool,
    /// Last value of mpv's fullscreen property: its own controls toggle `video_fullscreen`.
    pub mpv_fullscreen: Option<bool>,
}

pub struct AddonsView {
    pub cursor: usize,
    pub input: Option<String>,
    pub status: Option<(bool, String)>,
    pub confirm_remove: bool,
}

pub const SETTINGS: [&str; 12] =
    ["Accent", "Posters", "Output", "Resume", "Volume", "Prefer quality", "Autoplay", "Autoplay next", "Addon timeout", "Subtitles auto", "Video quality", "Best max size"];

pub struct App {
    pub config: Config,
    pub state: State,
    pub client: Client,
    pub addons: Vec<Addon>,
    pub posters: Posters,
    pub tx: mpsc::UnboundedSender<Ev>,
    pub screen: Screen,
    pub overlay: Overlay,
    pub discover: Discover,
    pub search: Search,
    pub library_cursor: usize,
    pub details: Option<Details>,
    pub addons_view: AddonsView,
    pub settings_cursor: usize,
    pub meta_cache: HashMap<String, Meta>,
    pub stream_cache: HashMap<String, StreamSet>,
    pub sub_cache: HashMap<String, Vec<Subtitle>>,
    pub playing: Option<Playing>,
    pub session: u64,
    pub pending_play: Option<(Meta, String)>,
    pub queue: Vec<Meta>,
    pub toast: Option<(bool, String, Instant)>,
    pub preview_generation: u64,
    pub grid_shape: Cell<(usize, usize)>,
    pub mpv_bin: Option<std::path::PathBuf>,
    pub engine: Arc<OnceCell<Arc<Engine>>>,
    pub should_quit: bool,
    pub needs_clear: bool,
    /// Cells the embedded video should cover this frame, set while drawing.
    pub video_cells: Cell<Option<Rect>>,
    pub frame_size: Cell<(u16, u16)>,
    pub hits: RefCell<Vec<(Rect, Hit)>>,
    video_host: Option<Arc<VideoHost>>,
    posters_http: reqwest::Client,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn copy_to_clipboard(text: &str) {
    #[cfg(windows)]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("clip.exe").stdin(std::process::Stdio::piped()).spawn() {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        let encoded = base64_encode(text.as_bytes());
        let _ = write!(std::io::stdout(), "\x1b]52;c;{encoded}\x07");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(not(windows))]
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

fn open_external(target: &str) {
    #[cfg(windows)]
    let result = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", target]).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(target).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(target).spawn();
    let _ = result;
}

impl App {
    pub fn new(config: Config, state: State, client: Client, addons: Vec<Addon>, _entries: Vec<AddonEntry>, posters: Posters, tx: mpsc::UnboundedSender<Ev>) -> Self {
        let mpv_bin = mpv::find_bin(&config.player.bin);
        let posters_http = reqwest::Client::builder()
            .user_agent(concat!("reel/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        let mut app = Self {
            config,
            state,
            client,
            addons,
            posters,
            tx,
            screen: Screen::Discover,
            overlay: Overlay::None,
            discover: Discover {
                focus: DiscoverFocus::Grid,
                catalogs: Vec::new(),
                selected: 0,
                items: Vec::new(),
                cursor: 0,
                continue_cursor: 0,
                sort: 0,
                genre: None,
                year: None,
                has_more: false,
                loading: false,
                generation: 0,
                ms: None,
                error: None,
            },
            search: Search { query: String::new(), kind: 0, results: Vec::new(), cursor: 0, generation: 0, searching: false, ms: None, recent_cursor: 0 },
            library_cursor: 0,
            details: None,
            addons_view: AddonsView { cursor: 0, input: None, status: None, confirm_remove: false },
            settings_cursor: 0,
            meta_cache: HashMap::new(),
            stream_cache: HashMap::new(),
            sub_cache: HashMap::new(),
            playing: None,
            session: 0,
            pending_play: None,
            queue: Vec::new(),
            toast: None,
            preview_generation: 0,
            grid_shape: Cell::new((5, 2)),
            mpv_bin,
            engine: Arc::new(OnceCell::new()),
            should_quit: false,
            needs_clear: false,
            video_cells: Cell::new(None),
            frame_size: Cell::new((0, 0)),
            hits: RefCell::new(Vec::new()),
            video_host: None,
            posters_http,
        };
        app.rebuild_catalogs();
        app
    }

    /// Refreshes every manifest in the background and loads the first catalog.
    pub fn start(&mut self) {
        tokio::task::spawn_blocking(crate::torrent::sweep_stale);
        for addon in &self.addons {
            let (client, tx, entry) = (self.client.clone(), self.tx.clone(), AddonEntry { url: addon.url.clone(), enabled: addon.enabled });
            tokio::spawn(async move {
                let fetched = client.fetch_addon(&entry).await;
                let _ = tx.send(Ev::AddonReady(entry.url, Box::new(fetched)));
            });
        }
        self.load_catalog(false);
    }

    pub fn after_draw(&mut self) {
        self.posters.process(&self.posters_http, &self.tx);
        if let Some(host) = &self.video_host {
            let (cols, rows) = self.frame_size.get();
            let fullscreen = self.screen == Screen::Playing && self.overlay == Overlay::None && self.playing.as_ref().is_some_and(|p| p.video_fullscreen);
            host.set_fullscreen(fullscreen);
            host.place(self.video_cells.get(), cols, rows);
        }
    }

    pub async fn shutdown(&mut self) {
        self.save_progress();
        self.state.save();
        if let Some(control) = self.playing.as_ref().and_then(|p| p.control.as_ref()) {
            control.command(json!(["quit"]));
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        if let (Some(hash), Some(engine)) = (self.playing.as_ref().and_then(|p| p.hash.clone()), self.engine.get()) {
            let _ = tokio::time::timeout(Duration::from_secs(2), engine.close(&hash)).await;
        }
    }

    pub fn accent(&self) -> ratatui::style::Color {
        crate::theme::accent(&self.config.ui.accent)
    }

    pub fn toast(&mut self, ok: bool, message: impl Into<String>) {
        self.toast = Some((ok, message.into(), Instant::now()));
    }

    pub fn enabled_addons(&self) -> usize {
        self.addons.iter().filter(|a| a.enabled).count()
    }

    fn save_addons(&self) {
        let entries: Vec<AddonEntry> = self.addons.iter().map(|a| AddonEntry { url: a.url.clone(), enabled: a.enabled }).collect();
        let _ = config::save_addons(&entries);
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.toast(false, format!("could not save config: {e}"));
        }
    }

    pub fn rebuild_catalogs(&mut self) {
        let previous = self.discover.catalogs.get(self.discover.selected).map(|c| (self.addons.get(c.addon).map(|a| a.url.clone()), c.def.id.clone(), c.def.kind.clone()));
        let mut rows = Vec::new();
        for (i, addon) in self.addons.iter().enumerate().filter(|(_, a)| a.enabled) {
            for def in addon.manifest.catalogs.iter().filter(|c| c.browsable()) {
                rows.push(CatalogRow { addon: i, def: def.clone() });
            }
        }
        self.discover.catalogs = rows;
        self.discover.selected = previous
            .and_then(|(url, id, kind)| {
                self.discover.catalogs.iter().position(|c| self.addons[c.addon].url == url.clone().unwrap_or_default() && c.def.id == id && c.def.kind == kind)
            })
            .unwrap_or(0);
    }

    pub fn current_catalog(&self) -> Option<&CatalogRow> {
        self.discover.catalogs.get(self.discover.selected)
    }

    pub fn load_catalog(&mut self, append: bool) {
        let Some(row) = self.current_catalog() else {
            self.discover.loading = false;
            return;
        };
        let addon = self.addons[row.addon].clone();
        let def = row.def.clone();
        let mut extra: Vec<(&'static str, String)> = Vec::new();
        let genre = self.discover.genre.clone().or_else(|| def.required.iter().any(|r| r == "genre").then(|| def.genres.first().cloned()).flatten());
        if let Some(genre) = genre {
            extra.push(("genre", genre));
        }
        if append {
            extra.push(("skip", self.discover.items.len().to_string()));
        } else {
            self.discover.items.clear();
            self.discover.cursor = 0;
        }
        self.discover.generation += 1;
        self.discover.loading = true;
        self.discover.error = None;
        let (generation, client, tx) = (self.discover.generation, self.client.clone(), self.tx.clone());
        tokio::spawn(async move {
            let started = Instant::now();
            let result = client.catalog(&addon, &def, &extra).await;
            let ms = started.elapsed().as_millis();
            let (items, error) = match result {
                Ok(items) => (items, None),
                Err(e) => (Vec::new(), Some(format!("{e:#}"))),
            };
            let _ = tx.send(Ev::Catalog { generation, items, append, ms, error });
        });
    }

    /// Indices into discover.items after the year filter and sort.
    pub fn visible(&self) -> Vec<usize> {
        let d = &self.discover;
        let mut indices: Vec<usize> = (0..d.items.len()).filter(|&i| d.year.as_deref().is_none_or(|y| d.items[i].year() == y)).collect();
        match SORTS[d.sort] {
            "Year" => indices.sort_by(|&a, &b| d.items[b].year().cmp(d.items[a].year())),
            "Title" => indices.sort_by_key(|&i| d.items[i].name.to_lowercase()),
            "Rating" => indices.sort_by(|&a, &b| {
                let rate = |m: &Meta| m.rating.parse::<f32>().unwrap_or(0.0);
                rate(&d.items[b]).total_cmp(&rate(&d.items[a]))
            }),
            _ => {}
        }
        indices
    }

    pub fn selected_meta(&self) -> Option<Meta> {
        match self.screen {
            Screen::Discover => match self.discover.focus {
                DiscoverFocus::Continue => self.state.continuing().get(self.discover.continue_cursor).map(|p| p.meta.clone()),
                _ => self.visible().get(self.discover.cursor).map(|&i| self.discover.items[i].clone()),
            },
            Screen::Library => self.state.library.get(self.library_cursor).cloned(),
            Screen::Search => self.search_order().get(self.search.cursor).map(|&i| self.search.results[i].clone()),
            Screen::Details => self.details.as_ref().map(|d| d.meta.clone()),
            _ => None,
        }
    }

    /// Search results grouped for display: movies, series, other, local files.
    pub fn search_order(&self) -> Vec<usize> {
        let results = &self.search.results;
        let group = |m: &Meta| {
            if m.id.starts_with(crate::local::ID_PREFIX) {
                3
            } else if m.kind == "movie" {
                0
            } else if m.kind == "series" {
                1
            } else {
                2
            }
        };
        let mut order: Vec<usize> = (0..results.len()).collect();
        order.sort_by_key(|&i| group(&results[i]));
        order
    }

    fn schedule_preview(&mut self) {
        self.preview_generation += 1;
        let (generation, tx) = (self.preview_generation, self.tx.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(350)).await;
            let _ = tx.send(Ev::PreviewDue(generation));
        });
    }

    fn fetch_meta(&mut self, kind: &str, id: &str) {
        if self.meta_cache.contains_key(id) {
            return;
        }
        let Some(addon) = self.addons.iter().find(|a| a.enabled && a.manifest.supports("meta", kind, id)).cloned() else { return };
        let (client, tx, kind, id) = (self.client.clone(), self.tx.clone(), kind.to_string(), id.to_string());
        tokio::spawn(async move {
            if let Ok(meta) = client.meta(&addon, &kind, &id).await {
                let _ = tx.send(Ev::Meta(id, Box::new(meta)));
            }
        });
    }

    fn fetch_streams(&mut self, kind: &str, video_id: &str) {
        if self.stream_cache.get(video_id).is_some_and(|set| set.pending > 0 || set.errors.is_empty()) {
            return;
        }
        let targets: Vec<(usize, Addon)> = self.addons.iter().cloned().enumerate().filter(|(_, a)| a.enabled && a.manifest.supports("stream", kind, video_id)).collect();
        self.stream_cache.insert(video_id.to_string(), StreamSet { pending: targets.len(), total: targets.len(), ..Default::default() });
        if targets.is_empty() {
            self.streams_settled(video_id);
        }
        for (order, addon) in targets {
            let (client, tx, kind, video_id) = (self.client.clone(), self.tx.clone(), kind.to_string(), video_id.to_string());
            tokio::spawn(async move {
                let started = Instant::now();
                let (found, error) = match client.streams(&addon, &kind, &video_id).await {
                    Ok(found) => (found, None),
                    Err(_) => (Vec::new(), Some(addon.manifest.name.clone())),
                };
                let rows = found.into_iter().map(|s| streams::to_row(s, &addon.manifest.name, order)).collect();
                let _ = tx.send(Ev::Streams { video_id, rows, ms: started.elapsed().as_millis(), error });
            });
        }
    }

    fn fetch_subs(&mut self, kind: &str, video_id: &str) {
        if self.sub_cache.contains_key(video_id) {
            return;
        }
        self.sub_cache.insert(video_id.to_string(), Vec::new());
        let addons = self.addons.clone();
        let (client, tx, kind, video_id) = (self.client.clone(), self.tx.clone(), kind.to_string(), video_id.to_string());
        tokio::spawn(async move {
            let subs = cli::fetch_subtitles(&client, &addons, &kind, &video_id).await;
            let _ = tx.send(Ev::Subs(video_id, subs));
        });
    }

    pub fn open_details(&mut self, meta: Meta) {
        let back = if self.screen == Screen::Details { Screen::Discover } else { self.screen };
        let is_series = meta.is_series();
        let video_id = meta.id.clone();
        let full = self.meta_cache.get(&meta.id).cloned();
        let mut merged = meta.clone();
        if let Some(full) = full {
            merged.merge_detail(full);
        }
        self.fetch_meta(&meta.kind, &meta.id);
        let mut details = Details {
            meta: merged,
            back,
            focus: if is_series { DetailFocus::Episodes } else { DetailFocus::Streams },
            season: 1,
            episode_cursor: 0,
            video_id: video_id.clone(),
            stream_cursor: 0,
            cursor_touched: false,
            sub_choice: None,
            sub_touched: false,
        };
        if is_series {
            details.video_id.clear();
        } else {
            self.fetch_streams(&meta.kind, &video_id);
            self.fetch_subs(&meta.kind, &video_id);
        }
        self.details = Some(details);
        self.screen = Screen::Details;
        self.sync_series_position();
        self.sync_stream_cursor();
        self.sync_subtitle_choice();
    }

    /// Once a series' episode list is known, land on the episode the user last watched.
    fn sync_series_position(&mut self) {
        let last_watched = self.details.as_ref().and_then(|d| self.state.progress.get(&d.meta.id).map(|p| p.video_id.clone()));
        let mut to_load = None;
        if let Some(d) = &mut self.details
            && d.meta.is_series()
            && d.video_id.is_empty()
            && !d.meta.videos.is_empty()
        {
            let target = last_watched
                .and_then(|id| d.meta.videos.iter().find(|v| v.id == id).cloned())
                .or_else(|| d.meta.videos.iter().find(|v| v.season > 0).cloned())
                .or_else(|| d.meta.videos.first().cloned());
            if let Some(video) = target {
                d.season = video.season;
                let in_season: Vec<_> = d.meta.videos.iter().filter(|v| v.season == video.season).collect();
                d.episode_cursor = in_season.iter().position(|v| v.id == video.id).unwrap_or(0);
                d.video_id = video.id.clone();
                to_load = Some((d.meta.kind.clone(), video.id));
            }
        }
        if let Some((kind, id)) = to_load {
            self.fetch_streams(&kind, &id);
            self.fetch_subs(&kind, &id);
            self.sync_stream_cursor();
            self.sync_subtitle_choice();
        }
    }

    pub fn current_streams(&self) -> Option<&StreamSet> {
        self.details.as_ref().and_then(|d| self.stream_cache.get(&d.video_id))
    }

    pub fn current_subs(&self) -> &[Subtitle] {
        self.details.as_ref().and_then(|d| self.sub_cache.get(&d.video_id)).map(Vec::as_slice).unwrap_or(&[])
    }

    fn sync_stream_cursor(&mut self) {
        let prefer = self.config.streams.clone();
        let best = self.current_streams().and_then(|s| streams::best_index(&s.rows, &prefer));
        if let Some(d) = &mut self.details
            && !d.cursor_touched
        {
            d.stream_cursor = best.unwrap_or(0);
        }
    }

    fn sync_subtitle_choice(&mut self) {
        let langs = streams::subtitle_languages(self.current_subs());
        let wanted = self.config.subtitles.languages.iter().find(|l| langs.contains(l)).cloned();
        let auto = self.config.subtitles.auto;
        if let Some(d) = &mut self.details
            && !d.sub_touched
        {
            d.sub_choice = if auto { wanted } else { None };
        }
    }

    pub fn episodes_in_season(&self) -> Vec<&addon::Video> {
        self.details.as_ref().map(|d| d.meta.videos.iter().filter(|v| v.season == d.season).collect()).unwrap_or_default()
    }

    pub fn seasons(&self) -> Vec<u32> {
        let mut seasons: Vec<u32> = self.details.as_ref().map(|d| d.meta.videos.iter().map(|v| v.season).collect()).unwrap_or_default();
        seasons.sort_unstable();
        seasons.dedup();
        seasons
    }

    /// Plays the highest-priority stream of a title, opening details when a choice is needed.
    fn play_best(&mut self, meta: Meta) {
        if meta.is_series() || self.config.streams.autoplay == "ask" {
            self.open_details(meta);
            return;
        }
        let id = meta.id.clone();
        self.fetch_streams(&meta.kind, &id);
        self.fetch_subs(&meta.kind, &id);
        self.toast(true, format!("finding streams for {}", meta.name));
        self.pending_play = Some((meta, id));
        self.check_pending_play();
    }

    fn check_pending_play(&mut self) {
        let Some((_, video_id)) = &self.pending_play else { return };
        let Some(set) = self.stream_cache.get(video_id) else { return };
        // Play as soon as every addon answered.
        if set.pending > 0 {
            return;
        }
        let (meta, video_id) = self.pending_play.take().unwrap();
        let rows = self.stream_cache.get(&video_id).map(|s| s.rows.clone()).unwrap_or_default();
        match streams::best_index(&rows, &self.config.streams) {
            Some(i) => self.start_play(meta, video_id, rows[i].clone(), None, PlayMode::Configured),
            None => {
                self.toast(false, format!("no streams found for {}", meta.name));
                self.open_details(meta);
            }
        }
    }

    fn streams_settled(&mut self, video_id: &str) {
        self.sync_stream_cursor();
        let _ = video_id;
        self.check_pending_play();
    }

    fn build_up_next(&self, meta: &Meta, video_id: &str) -> Vec<UpNext> {
        if meta.is_series() {
            let mut videos: Vec<_> = meta.videos.iter().filter(|v| v.season > 0).collect();
            videos.sort_by_key(|v| (v.season, v.episode));
            let position = videos.iter().position(|v| v.id == video_id).map(|i| i + 1).unwrap_or(0);
            return videos[position.min(videos.len())..]
                .iter()
                .take(5)
                .map(|v| UpNext { meta: meta.clone(), video_id: v.id.clone(), label: v.title.clone(), detail: format!("S{:02}E{:02}", v.season, v.episode) })
                .collect();
        }
        self.queue
            .iter()
            .skip_while(|m| m.id != meta.id)
            .skip(1)
            .take(5)
            .map(|m| UpNext { meta: m.clone(), video_id: m.id.clone(), label: m.name.clone(), detail: m.year().to_string() })
            .collect()
    }

    fn ensure_video_host(&mut self) -> Option<Arc<VideoHost>> {
        if self.video_host.is_none() {
            self.video_host = VideoHost::create().map(Arc::new);
        }
        self.video_host.clone()
    }

    pub fn start_play(&mut self, meta: Meta, video_id: String, row: StreamRow, sub_choice: Option<Option<String>>, mode: PlayMode) {
        let Some(bin) = self.mpv_bin.clone() else {
            self.toast(false, "mpv not found: install it or set [player] bin in config.toml");
            return;
        };
        self.stop_current();
        self.session += 1;
        let session = self.session;

        // A list view starts playback, so remember what comes after this title.
        if self.screen == Screen::Discover || self.details.as_ref().is_some_and(|d| d.back == Screen::Discover) {
            let order = self.visible();
            if order.iter().any(|&i| self.discover.items[i].id == meta.id) {
                self.queue = order.iter().map(|&i| self.discover.items[i].clone()).collect();
            }
        }

        let episode = meta.videos.iter().find(|v| v.id == video_id);
        let title = match episode {
            Some(v) => format!("{} · S{:02}E{:02} {}", meta.name, v.season, v.episode, v.title),
            None if meta.year().is_empty() => meta.name.clone(),
            None => format!("{} ({})", meta.name, meta.year()),
        };

        let subs = self.sub_cache.get(&video_id).cloned().unwrap_or_default();
        let default_lang = self.config.subtitles.languages.iter().find(|l| subs.iter().any(|s| lang_code(&s.lang) == **l)).cloned();
        let selected = sub_choice.unwrap_or(if self.config.subtitles.auto { default_lang } else { None });
        let mut languages = self.config.subtitles.languages.clone();
        if let Some(lang) = &selected
            && !languages.contains(lang)
        {
            languages.insert(0, lang.clone());
        }
        let picked = cli::pick_subtitles(&subs, &languages);

        let saved = self.state.progress.get(&meta.id).filter(|p| p.video_id == video_id && self.config.player.resume);
        let start = saved.map(|p| p.position).filter(|&p| p > 30.0);
        let output = self.config.player.output.clone();
        let wants_embed = mode == PlayMode::Embedded || (mode == PlayMode::Configured && output == "embedded");
        let wid = if wants_embed { self.ensure_video_host().map(|h| h.wid()) } else { None };
        let fullscreen = mode == PlayMode::Fullscreen || (mode == PlayMode::Configured && output == "fullscreen");

        let hash = if row.stream.url.is_none() { row.stream.info_hash.clone() } else { None };
        let up_next = self.build_up_next(&meta, &video_id);
        self.playing = Some(Playing {
            session,
            meta: meta.clone(),
            video_id: video_id.clone(),
            title: title.clone(),
            quality: row.quality.clone(),
            release: row.release.clone(),
            addon: row.addon.clone(),
            hash: hash.clone(),
            phase: if hash.is_some() { Phase::Resolving } else { Phase::Loading },
            state: PlayerState { volume: self.config.player.volume as f64, ..Default::default() },
            control: None,
            log: VecDeque::new(),
            torrent: None,
            last_saved: Instant::now(),
            eof: false,
            up_next,
            fullscreen,
            embedded: wid.is_some(),
            theater: false,
            video_fullscreen: false,
            mpv_fullscreen: None,
        });
        self.screen = Screen::Playing;
        self.needs_clear = true;

        let mut request = PlayRequest {
            target: streams::display_url(&row),
            title,
            start,
            volume: self.config.player.volume,
            fullscreen,
            audio_only: mode == PlayMode::AudioOnly,
            wid,
            high_quality: self.config.player.high_quality,
            terminal_vo: None,
            extra_args: self.config.player.args.clone(),
            subs: picked,
            sub_select: selected,
        };
        let (tx, engine) = (self.tx.clone(), self.engine.clone());
        let stream = row.stream.clone();
        tokio::spawn(async move {
            if let Some(hash) = &hash {
                let opened = async {
                    let engine = engine.get_or_try_init(Engine::start).await?;
                    engine.open(hash, stream.file_idx, &stream.trackers).await
                }
                .await;
                match opened {
                    Ok(opened) => {
                        request.target = opened.url;
                        let _ = tx.send(Ev::PlayResolved { session, hash: Some(hash.clone()), filename: opened.filename });
                    }
                    Err(e) => {
                        let _ = tx.send(Ev::PlayFailed { session, error: format!("{e:#}") });
                        return;
                    }
                }
            }
            let (events_tx, mut events) = mpsc::unbounded_channel();
            let forward_tx = tx.clone();
            tokio::spawn(async move {
                while let Some(event) = events.recv().await {
                    if forward_tx.send(Ev::Mpv(session, event)).is_err() {
                        break;
                    }
                }
            });
            match Mpv::spawn(&bin, &request, events_tx).await {
                Ok(mut player) => {
                    let _ = tx.send(Ev::MpvStarted { session, control: player.control() });
                    let _ = player.child.wait().await;
                }
                Err(e) => {
                    let _ = tx.send(Ev::PlayFailed { session, error: format!("{e:#}") });
                }
            }
        });
    }

    fn stop_current(&mut self) {
        self.save_progress();
        if let Some(playing) = self.playing.take() {
            if let Some(control) = &playing.control {
                control.command(json!(["quit"]));
            }
            self.release_torrent(playing.hash);
        }
    }

    fn release_torrent(&self, hash: Option<String>) {
        if let (Some(hash), Some(engine)) = (hash, self.engine.get().cloned()) {
            tokio::spawn(async move { engine.close(&hash).await });
        }
    }

    fn save_progress(&mut self) {
        let Some(p) = &mut self.playing else { return };
        p.last_saved = Instant::now();
        if p.state.duration <= 0.0 || p.state.position <= 0.0 {
            return;
        }
        let entry = Progress { meta: p.meta.clone(), video_id: p.video_id.clone(), position: p.state.position, duration: p.state.duration, updated: now_secs() };
        self.state.progress.insert(p.meta.id.clone(), entry);
        self.state.save();
    }

    fn on_mpv_closed(&mut self, session: u64) {
        let Some(playing) = &self.playing else { return };
        if playing.session != session {
            return;
        }
        let autoplay = playing.eof && self.config.streams.autoplay_next;
        let next = playing.up_next.first().map(|n| (n.meta.clone(), n.video_id.clone()));
        if playing.eof {
            // Finished titles leave the Continue list.
            let id = playing.meta.id.clone();
            self.state.progress.remove(&id);
            self.state.save();
        } else {
            self.save_progress();
        }
        let hash = self.playing.take().and_then(|p| p.hash);
        self.release_torrent(hash);
        match next.filter(|_| autoplay) {
            Some((meta, video_id)) => self.queue_next(meta, video_id),
            None if self.screen == Screen::Playing => self.leave_playing(),
            None => {}
        }
    }

    fn leave_playing(&mut self) {
        self.screen = if self.details.is_some() { Screen::Details } else { Screen::Discover };
        self.needs_clear = true;
    }

    fn queue_next(&mut self, meta: Meta, video_id: String) {
        self.fetch_streams(&meta.kind, &video_id);
        self.fetch_subs(&meta.kind, &video_id);
        self.toast(true, format!("up next: {}", meta.name));
        self.pending_play = Some((meta, video_id));
        self.check_pending_play();
    }

    pub fn handle(&mut self, event: Ev) {
        match event {
            Ev::Key(key) => self.on_key(key),
            Ev::Mouse(mouse) => self.on_mouse(mouse),
            Ev::Paste(text) => self.on_paste(&text),
            Ev::Resize => self.needs_clear = true,
            Ev::Tick => self.on_tick(),
            Ev::AddonReady(url, addon) => {
                if let Some(slot) = self.addons.iter_mut().find(|a| a.url == url) {
                    let enabled = slot.enabled;
                    *slot = *addon;
                    slot.enabled = enabled;
                }
                let had_catalog = self.current_catalog().is_some();
                self.rebuild_catalogs();
                if !had_catalog && self.current_catalog().is_some() {
                    self.load_catalog(false);
                }
            }
            Ev::Catalog { generation, items, append, ms, error } => {
                if generation != self.discover.generation {
                    return;
                }
                self.discover.loading = false;
                self.discover.ms = Some(ms);
                self.discover.error = error;
                self.discover.has_more = !items.is_empty();
                if append {
                    let known: Vec<String> = self.discover.items.iter().map(|m| m.id.clone()).collect();
                    let fresh: Vec<Meta> = items.into_iter().filter(|m| !known.contains(&m.id)).collect();
                    self.discover.has_more = !fresh.is_empty();
                    self.discover.items.extend(fresh);
                } else {
                    self.discover.items = items;
                }
                self.schedule_preview();
            }
            Ev::Meta(id, meta) => {
                self.meta_cache.insert(id.clone(), (*meta).clone());
                if let Some(d) = &mut self.details
                    && d.meta.id == id
                {
                    d.meta.merge_detail(*meta);
                }
                self.sync_series_position();
            }
            Ev::Streams { video_id, rows, ms, error } => {
                if let Some(set) = self.stream_cache.get_mut(&video_id) {
                    set.errors.extend(error);
                    set.rows.extend(rows);
                    set.pending = set.pending.saturating_sub(1);
                    set.ms = set.ms.max(ms);
                    streams::sort_rows(&mut set.rows);
                }
                self.streams_settled(&video_id);
            }
            Ev::Subs(video_id, subs) => {
                self.sub_cache.insert(video_id, subs);
                self.sync_subtitle_choice();
            }
            Ev::PreviewDue(generation) => {
                if generation == self.preview_generation
                    && let Some(meta) = self.selected_meta()
                {
                    self.fetch_meta(&meta.kind, &meta.id);
                    if !meta.is_series() {
                        self.fetch_streams(&meta.kind, &meta.id);
                        if self.screen == Screen::Search {
                            self.fetch_subs(&meta.kind, &meta.id);
                        }
                    }
                }
            }
            Ev::SearchDue(generation) => {
                if generation == self.search.generation {
                    self.run_search();
                }
            }
            Ev::Search { generation, items, ms } => {
                if generation == self.search.generation {
                    self.search.results = items;
                    self.search.searching = false;
                    self.search.ms = Some(ms);
                    self.search.cursor = 0;
                    self.schedule_preview();
                }
            }
            Ev::PosterRaw(url, image) => self.posters.on_raw(url, image),
            Ev::PosterEncoded(key, protocol) => self.posters.on_encoded(key, protocol),
            Ev::Installed(result) => match result {
                Ok(addon) => {
                    let name = addon.manifest.name.clone();
                    match self.addons.iter().position(|a| a.url == addon.url) {
                        Some(i) => self.addons[i] = *addon,
                        None => {
                            self.addons.push(*addon);
                            self.addons_view.cursor = self.addons.len() - 1;
                        }
                    }
                    self.save_addons();
                    self.rebuild_catalogs();
                    self.stream_cache.clear();
                    self.addons_view.status = Some((true, format!("{name} installed · {}", clock())));
                }
                Err(error) => self.addons_view.status = Some((false, error)),
            },
            Ev::PlayResolved { session, hash, filename } => {
                if let Some(p) = &mut self.playing
                    && p.session == session
                {
                    p.phase = Phase::Loading;
                    p.hash = hash;
                    p.log.push_back((clock(), "torrent".into(), filename));
                }
            }
            Ev::PlayFailed { session, error } => {
                if self.playing.as_ref().is_some_and(|p| p.session == session) {
                    self.toast(false, error);
                    let hash = self.playing.take().and_then(|p| p.hash);
                    self.release_torrent(hash);
                    if self.screen == Screen::Playing {
                        self.leave_playing();
                    }
                }
            }
            Ev::MpvStarted { session, control } => {
                if let Some(p) = &mut self.playing
                    && p.session == session
                {
                    p.control = Some(control);
                    p.phase = Phase::Playing;
                    p.log.push_back((clock(), "ipc".into(), "connected".into()));
                } else {
                    control.command(json!(["quit"]));
                }
            }
            Ev::Mpv(session, event) => self.on_mpv(session, event),
        }
    }

    fn on_mpv(&mut self, session: u64, event: MpvEvent) {
        if let MpvEvent::Closed = event {
            self.on_mpv_closed(session);
            return;
        }
        let Some(p) = &mut self.playing else { return };
        if p.session != session {
            return;
        }
        match event {
            MpvEvent::Property(name, value) => {
                p.state.apply(&name, &value);
                if name == "fullscreen" {
                    let now = value.as_bool().unwrap_or(false);
                    if p.embedded && p.mpv_fullscreen.is_some_and(|before| before != now) {
                        p.video_fullscreen = !p.video_fullscreen;
                    }
                    p.mpv_fullscreen = Some(now);
                    return;
                }
                let detail = match name.as_str() {
                    "track-list" => {
                        let count = |k: &str| p.state.tracks.iter().filter(|t| t.kind == k).count();
                        Some(format!("video {} · audio {} · sub {}", count("video"), count("audio"), count("sub")))
                    }
                    "pause" => Some(if p.state.paused { "paused" } else { "resumed" }.to_string()),
                    "paused-for-cache" if p.state.buffering => Some("buffering".to_string()),
                    _ => None,
                };
                if let Some(detail) = detail {
                    p.log.push_back((clock(), name, detail));
                }
            }
            MpvEvent::Log(name, detail) => {
                let detail = if name == "seek" { format!("from {}", mpv::format_time(p.state.position)) } else { detail };
                p.log.push_back((clock(), name, detail));
            }
            MpvEvent::EndOfFile => p.eof = true,
            MpvEvent::Connected | MpvEvent::Closed => {}
        }
        while p.log.len() > 50 {
            p.log.pop_front();
        }
    }

    fn on_tick(&mut self) {
        if self.toast.as_ref().is_some_and(|t| t.2.elapsed() > Duration::from_secs(6)) {
            self.toast = None;
        }
        crate::embed::hide_console_stub();
        let engine = self.engine.get().cloned();
        let mut save = false;
        if let Some(p) = &mut self.playing {
            if let (Some(hash), Some(engine)) = (&p.hash, engine) {
                p.torrent = engine.status(hash);
                engine.pace(hash, p.state.duration, p.state.cache_seconds, p.state.buffering);
            }
            save = p.last_saved.elapsed() > Duration::from_secs(10);
        }
        if save {
            self.save_progress();
        }
    }

    fn on_paste(&mut self, text: &str) {
        let clean = text.trim();
        if let Some(input) = &mut self.addons_view.input {
            input.push_str(clean);
        } else if self.screen == Screen::Search {
            self.search.query.push_str(clean);
            self.schedule_search();
        } else if self.screen == Screen::Addons {
            self.addons_view.input = Some(clean.to_string());
        }
    }

    fn schedule_search(&mut self) {
        self.search.generation += 1;
        let query = self.search.query.trim().to_string();
        if query.is_empty() {
            self.search.results.clear();
            self.search.searching = false;
            self.search.ms = None;
            return;
        }
        self.search.searching = true;
        let (generation, tx) = (self.search.generation, self.tx.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let _ = tx.send(Ev::SearchDue(generation));
        });
    }

    fn run_search(&mut self) {
        let query = self.search.query.trim().to_string();
        let kind = (self.search.kind > 0).then(|| SEARCH_KINDS[self.search.kind].to_string());
        let (generation, client, addons, tx) = (self.search.generation, self.client.clone(), self.addons.clone(), self.tx.clone());
        tokio::spawn(async move {
            let started = Instant::now();
            let items = cli::search_all(&client, &addons, &query, kind.as_deref()).await;
            let _ = tx.send(Ev::Search { generation, items, ms: started.elapsed().as_millis() });
        });
    }

    fn go(&mut self, screen: Screen) {
        if self.screen != screen {
            self.screen = screen;
            self.needs_clear = true;
            if screen == Screen::Discover || screen == Screen::Search {
                self.schedule_preview();
            }
        }
    }

    pub fn inserting(&self) -> bool {
        self.screen == Screen::Search || (self.screen == Screen::Addons && self.addons_view.input.is_some())
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        if self.overlay != Overlay::None {
            self.on_overlay_key(key);
            return;
        }
        match self.screen {
            Screen::Search => return self.on_search_key(key),
            Screen::Addons if self.addons_view.input.is_some() => return self.on_install_key(key),
            _ => {}
        }
        // Screens that bind these keys themselves.
        let playing_screen = self.screen == Screen::Playing;
        if let KeyCode::Char(c) = key.code {
            match c {
                '1' => return self.go(Screen::Discover),
                '2' => return self.go(Screen::Search),
                '3' => return self.go(Screen::Library),
                '4' => return self.go(Screen::Addons),
                '5' => return self.go(Screen::Settings),
                '0' if self.playing.is_some() => return self.go(Screen::Playing),
                '?' => {
                    self.overlay = Overlay::Help;
                    return;
                }
                _ => {}
            }
            let keys = &self.config.keys;
            if keys.search.starts_with(c) && !playing_screen {
                return self.go(Screen::Search);
            }
            if keys.addons.starts_with(c) && !playing_screen && self.screen != Screen::Addons {
                return self.go(Screen::Addons);
            }
        }
        match self.screen {
            Screen::Discover => self.on_discover_key(key),
            Screen::Library => self.on_library_key(key),
            Screen::Details => self.on_details_key(key),
            Screen::Playing => self.on_playing_key(key),
            Screen::Addons => self.on_addons_key(key),
            Screen::Settings => self.on_settings_key(key),
            Screen::Search => {}
        }
    }

    fn press(&mut self, code: KeyCode) {
        self.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.overlay == Overlay::Help {
                    self.overlay = Overlay::None;
                    self.needs_clear = true;
                    return;
                }
                if let Some((area, hit)) = self.hit_at(mouse.column, mouse.row) {
                    self.on_click(hit, area, mouse.column);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((area, hit @ (Hit::Seek | Hit::Volume))) = self.hit_at(mouse.column, mouse.row) {
                    self.on_click(hit, area, mouse.column);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => self.press(KeyCode::Esc),
            MouseEventKind::ScrollUp => self.on_scroll(-1),
            MouseEventKind::ScrollDown => self.on_scroll(1),
            _ => {}
        }
    }

    fn hit_at(&self, column: u16, row: u16) -> Option<(Rect, Hit)> {
        // Later entries were drawn on top.
        self.hits.borrow().iter().rev().find(|(area, _)| column >= area.x && column < area.right() && row >= area.y && row < area.bottom()).cloned()
    }

    fn on_scroll(&mut self, direction: isize) {
        let (cols, _) = self.grid_shape.get();
        match self.screen {
            Screen::Discover if self.discover.focus == DiscoverFocus::Catalogs => {
                let next = (self.discover.selected as isize + direction).max(0) as usize;
                self.select_catalog(next);
            }
            Screen::Discover => self.move_cursor(direction * cols as isize),
            Screen::Library => self.press(if direction > 0 { KeyCode::Down } else { KeyCode::Up }),
            Screen::Search => self.press(if direction > 0 { KeyCode::Down } else { KeyCode::Up }),
            Screen::Details => self.press(if direction > 0 { KeyCode::Char('j') } else { KeyCode::Char('k') }),
            Screen::Addons if self.addons_view.input.is_none() => self.press(if direction > 0 { KeyCode::Char('j') } else { KeyCode::Char('k') }),
            Screen::Settings => self.press(if direction > 0 { KeyCode::Char('j') } else { KeyCode::Char('k') }),
            Screen::Playing => self.press(if direction > 0 { KeyCode::Down } else { KeyCode::Up }),
            _ => {}
        }
    }

    /// A first click selects; clicking what is already selected opens it.
    fn on_click(&mut self, hit: Hit, area: Rect, column: u16) {
        let ratio = (column.saturating_sub(area.x) as f64 + 0.5) / area.width.max(1) as f64;
        match hit {
            Hit::Key(code) => self.press(code),
            Hit::Go(screen) => self.go(screen),
            Hit::Grid(i) => {
                if self.discover.focus == DiscoverFocus::Grid && self.discover.cursor == i {
                    self.press(KeyCode::Enter);
                } else {
                    self.discover.focus = DiscoverFocus::Grid;
                    self.discover.cursor = i;
                    self.schedule_preview();
                }
            }
            Hit::LibraryItem(i) => {
                if self.library_cursor == i {
                    self.press(KeyCode::Enter);
                } else {
                    self.library_cursor = i;
                }
            }
            Hit::Catalog(i) => {
                self.discover.focus = DiscoverFocus::Catalogs;
                self.select_catalog(i);
            }
            Hit::ContinueItem(i) => {
                if self.discover.focus == DiscoverFocus::Continue && self.discover.continue_cursor == i {
                    self.press(KeyCode::Enter);
                } else {
                    self.discover.focus = DiscoverFocus::Continue;
                    self.discover.continue_cursor = i;
                }
            }
            Hit::SearchRow(i) => {
                if self.search.cursor == i {
                    self.press(KeyCode::Enter);
                } else {
                    self.search.cursor = i;
                    self.schedule_preview();
                }
            }
            Hit::SearchKind(i) => {
                self.search.kind = i;
                self.schedule_search();
            }
            Hit::Stream(i) => {
                let Some(d) = &mut self.details else { return };
                let again = d.focus == DetailFocus::Streams && d.stream_cursor == i;
                d.focus = DetailFocus::Streams;
                d.stream_cursor = i;
                d.cursor_touched = true;
                if again {
                    self.press(KeyCode::Enter);
                }
            }
            Hit::Episode(i) => {
                let Some(d) = &mut self.details else { return };
                let again = d.focus == DetailFocus::Episodes && d.episode_cursor == i;
                d.focus = DetailFocus::Episodes;
                d.episode_cursor = i;
                if again {
                    self.press(KeyCode::Enter);
                }
            }
            Hit::Subtitle(choice) => {
                if let Some(d) = &mut self.details {
                    d.sub_touched = true;
                    d.sub_choice = choice;
                }
            }
            Hit::Addon(i) => {
                self.addons_view.cursor = i;
                self.addons_view.confirm_remove = false;
            }
            Hit::AddonToggle(i) => {
                self.addons_view.cursor = i;
                self.press(KeyCode::Char(' '));
            }
            Hit::Setting(i) => {
                if self.settings_cursor == i {
                    self.press(KeyCode::Enter);
                } else {
                    self.settings_cursor = i;
                }
            }
            Hit::PlayWith(i) => {
                self.overlay = Overlay::PlayWith(i);
                self.press(KeyCode::Enter);
            }
            Hit::UpNext(i) => {
                let next = self.playing.as_ref().and_then(|p| p.up_next.get(i)).map(|n| (n.meta.clone(), n.video_id.clone()));
                if let Some((meta, video_id)) = next {
                    self.queue_next(meta, video_id);
                }
            }
            Hit::Track(kind, id) => {
                if let Some(control) = self.playing.as_ref().and_then(|p| p.control.as_ref()) {
                    let property = if kind == "audio" { "aid" } else { "sid" };
                    control.command(if id == 0 { json!(["set_property", property, "no"]) } else { json!(["set_property", property, id]) });
                }
            }
            Hit::Seek => {
                if let Some(p) = &self.playing
                    && let Some(control) = &p.control
                    && p.state.duration > 0.0
                {
                    control.command(json!(["seek", ratio * p.state.duration, "absolute"]));
                }
            }
            Hit::Volume => {
                if let Some(control) = self.playing.as_ref().and_then(|p| p.control.as_ref()) {
                    control.command(json!(["set_property", "volume", (ratio * 100.0).round()]));
                }
            }
        }
    }

    fn on_overlay_key(&mut self, key: KeyEvent) {
        match self.overlay {
            Overlay::PlayWith(cursor) => match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.overlay = Overlay::PlayWith((cursor + 1).min(4)),
                KeyCode::Char('k') | KeyCode::Up => self.overlay = Overlay::PlayWith(cursor.saturating_sub(1)),
                KeyCode::Enter => {
                    self.overlay = Overlay::None;
                    self.needs_clear = true;
                    match cursor {
                        0 => self.play_selected_row(PlayMode::Embedded),
                        1 => self.play_selected_row(PlayMode::Window),
                        2 => self.play_selected_row(PlayMode::Fullscreen),
                        3 => self.play_selected_row(PlayMode::AudioOnly),
                        _ => self.copy_selected_url(),
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('P') => {
                    self.overlay = Overlay::None;
                    self.needs_clear = true;
                }
                _ => {}
            },
            _ => {
                self.overlay = Overlay::None;
                self.needs_clear = true;
            }
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.visible().len();
        if len == 0 {
            return;
        }
        let next = (self.discover.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        if next != self.discover.cursor {
            self.discover.cursor = next;
            self.schedule_preview();
        }
        let (cols, rows) = self.grid_shape.get();
        if next + cols * rows * 2 >= len && self.discover.has_more && !self.discover.loading {
            self.load_catalog(true);
        }
    }

    fn select_catalog(&mut self, index: usize) {
        if index != self.discover.selected && index < self.discover.catalogs.len() {
            self.discover.selected = index;
            self.discover.genre = None;
            self.discover.year = None;
            self.load_catalog(false);
        }
    }

    fn on_discover_key(&mut self, key: KeyEvent) {
        let (cols, rows) = self.grid_shape.get();
        let play_key = self.config.keys.play.chars().next().unwrap_or('p');
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => {
                self.discover.focus = match self.discover.focus {
                    DiscoverFocus::Catalogs => DiscoverFocus::Grid,
                    DiscoverFocus::Grid if !self.state.continuing().is_empty() => DiscoverFocus::Continue,
                    DiscoverFocus::Grid | DiscoverFocus::Continue => DiscoverFocus::Catalogs,
                };
            }
            KeyCode::Char('S') => {
                self.discover.sort = (self.discover.sort + 1) % SORTS.len();
                self.discover.cursor = 0;
            }
            KeyCode::Char('G') => self.cycle_genre(),
            KeyCode::Char('Y') => self.cycle_year(),
            KeyCode::PageDown | KeyCode::Char('>') | KeyCode::Char('.') => self.move_cursor((cols * rows) as isize),
            KeyCode::PageUp | KeyCode::Char('<') | KeyCode::Char(',') => self.move_cursor(-((cols * rows) as isize)),
            KeyCode::Char('+') => self.toggle_library(),
            KeyCode::Char(c) if c == play_key => {
                if let Some(meta) = self.selected_meta() {
                    self.play_best(meta);
                }
            }
            code => match self.discover.focus {
                DiscoverFocus::Catalogs => match code {
                    KeyCode::Char('j') | KeyCode::Down => self.select_catalog(self.discover.selected + 1),
                    KeyCode::Char('k') | KeyCode::Up => self.select_catalog(self.discover.selected.saturating_sub(1)),
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.discover.focus = DiscoverFocus::Grid,
                    _ => {}
                },
                DiscoverFocus::Grid => match code {
                    KeyCode::Char('h') | KeyCode::Left => {
                        if self.discover.cursor % cols == 0 {
                            self.discover.focus = DiscoverFocus::Catalogs;
                        } else {
                            self.move_cursor(-1);
                        }
                    }
                    KeyCode::Char('l') | KeyCode::Right => self.move_cursor(1),
                    KeyCode::Char('j') | KeyCode::Down => self.move_cursor(cols as isize),
                    KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-(cols as isize)),
                    KeyCode::Enter => {
                        if let Some(meta) = self.selected_meta() {
                            self.open_details(meta);
                        }
                    }
                    _ => {}
                },
                DiscoverFocus::Continue => {
                    let count = self.state.continuing().len();
                    match code {
                        KeyCode::Char('j') | KeyCode::Down => self.discover.continue_cursor = (self.discover.continue_cursor + 1).min(count.saturating_sub(1)),
                        KeyCode::Char('k') | KeyCode::Up => self.discover.continue_cursor = self.discover.continue_cursor.saturating_sub(1),
                        KeyCode::Enter => {
                            if let Some(meta) = self.selected_meta() {
                                self.open_details(meta);
                            }
                        }
                        _ => {}
                    }
                }
            },
        }
    }

    fn cycle_genre(&mut self) {
        let Some(row) = self.current_catalog() else { return };
        let genres = row.def.genres.clone();
        let required = row.def.required.iter().any(|r| r == "genre");
        if genres.is_empty() {
            self.toast(false, "this catalog has no genre filter");
            return;
        }
        self.discover.genre = match self.discover.genre.as_ref().and_then(|g| genres.iter().position(|x| x == g)) {
            None => Some(genres[if required { 1.min(genres.len() - 1) } else { 0 }].clone()),
            Some(i) if i + 1 < genres.len() => Some(genres[i + 1].clone()),
            Some(_) if required => Some(genres[0].clone()),
            Some(_) => None,
        };
        self.load_catalog(false);
    }

    fn cycle_year(&mut self) {
        let mut years: Vec<String> = self.discover.items.iter().map(|m| m.year().to_string()).filter(|y| !y.is_empty()).collect();
        years.sort_unstable_by(|a, b| b.cmp(a));
        years.dedup();
        self.discover.year = match self.discover.year.as_ref().and_then(|y| years.iter().position(|x| x == y)) {
            None => years.first().cloned(),
            Some(i) => years.get(i + 1).cloned(),
        };
        self.discover.cursor = 0;
    }

    fn toggle_library(&mut self) {
        if let Some(meta) = self.selected_meta() {
            let added = self.state.toggle_library(&meta);
            self.state.save();
            self.toast(true, format!("{} {} library", meta.name, if added { "added to" } else { "removed from" }));
            self.library_cursor = self.library_cursor.min(self.state.library.len().saturating_sub(1));
        }
    }

    fn on_library_key(&mut self, key: KeyEvent) {
        let (cols, _) = self.grid_shape.get();
        let len = self.state.library.len();
        let play_key = self.config.keys.play.chars().next().unwrap_or('p');
        let step = |cursor: usize, delta: isize| (cursor as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.go(Screen::Discover),
            KeyCode::Char('h') | KeyCode::Left => self.library_cursor = step(self.library_cursor, -1),
            KeyCode::Char('l') | KeyCode::Right => self.library_cursor = step(self.library_cursor, 1),
            KeyCode::Char('j') | KeyCode::Down => self.library_cursor = step(self.library_cursor, cols as isize),
            KeyCode::Char('k') | KeyCode::Up => self.library_cursor = step(self.library_cursor, -(cols as isize)),
            KeyCode::Char('+') | KeyCode::Char('d') => self.toggle_library(),
            KeyCode::Enter => {
                if let Some(meta) = self.selected_meta() {
                    self.open_details(meta);
                }
            }
            KeyCode::Char(c) if c == play_key => {
                if let Some(meta) = self.selected_meta() {
                    self.play_best(meta);
                }
            }
            _ => {}
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.search.results.len();
        match key.code {
            KeyCode::Esc => self.go(Screen::Discover),
            KeyCode::Up => {
                self.search.cursor = self.search.cursor.saturating_sub(1);
                self.schedule_preview();
            }
            KeyCode::Down => {
                self.search.cursor = (self.search.cursor + 1).min(len.saturating_sub(1));
                self.schedule_preview();
            }
            KeyCode::Tab => {
                self.search.kind = (self.search.kind + 1) % SEARCH_KINDS.len();
                self.schedule_search();
            }
            KeyCode::Enter => {
                self.state.push_recent(&self.search.query.clone());
                self.state.save();
                if let Some(meta) = self.selected_meta() {
                    self.open_details(meta);
                }
            }
            KeyCode::Char('p') if ctrl => {
                self.state.push_recent(&self.search.query.clone());
                if let Some(meta) = self.selected_meta() {
                    self.play_best(meta);
                }
            }
            KeyCode::Char('r') if ctrl => {
                if !self.state.recent.is_empty() {
                    self.search.query = self.state.recent[self.search.recent_cursor % self.state.recent.len()].clone();
                    self.search.recent_cursor += 1;
                    self.schedule_search();
                }
            }
            KeyCode::Char('u') if ctrl => {
                self.search.query.clear();
                self.schedule_search();
            }
            KeyCode::Char(c) if !ctrl => {
                self.search.query.push(c);
                self.schedule_search();
            }
            KeyCode::Backspace => {
                self.search.query.pop();
                self.schedule_search();
            }
            _ => {}
        }
    }

    fn on_details_key(&mut self, key: KeyEvent) {
        let play_key = self.config.keys.play.chars().next().unwrap_or('p');
        let Some(d) = &self.details else { return };
        let is_series = d.meta.is_series();
        let focus = d.focus;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                let back = d.back;
                self.go(back);
            }
            KeyCode::Tab if is_series => {
                let d = self.details.as_mut().unwrap();
                d.focus = if d.focus == DetailFocus::Episodes { DetailFocus::Streams } else { DetailFocus::Episodes };
            }
            KeyCode::Char('+') => self.toggle_library(),
            KeyCode::Char('y') => self.copy_selected_url(),
            KeyCode::Char('s') => self.cycle_subtitle(),
            KeyCode::Char('P') => self.overlay = Overlay::PlayWith(0),
            KeyCode::Char(c) if c == play_key => {
                let best = self.current_streams().and_then(|s| streams::best_index(&s.rows, &self.config.streams));
                if let (Some(best), Some(d)) = (best, &mut self.details) {
                    d.stream_cursor = best;
                    self.play_selected_row(PlayMode::Configured);
                }
            }
            code if focus == DetailFocus::Episodes => self.on_episode_key(code),
            KeyCode::Char('j') | KeyCode::Down => self.move_stream(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_stream(-1),
            KeyCode::Enter => self.play_selected_row(PlayMode::Configured),
            _ => {}
        }
    }

    fn move_stream(&mut self, delta: isize) {
        let len = self.current_streams().map(|s| s.rows.len()).unwrap_or(0);
        if let Some(d) = &mut self.details {
            d.cursor_touched = true;
            d.stream_cursor = (d.stream_cursor as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
        }
    }

    fn on_episode_key(&mut self, code: KeyCode) {
        let count = self.episodes_in_season().len();
        let seasons = self.seasons();
        let Some(d) = &mut self.details else { return };
        match code {
            KeyCode::Char('j') | KeyCode::Down => d.episode_cursor = (d.episode_cursor + 1).min(count.saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => d.episode_cursor = d.episode_cursor.saturating_sub(1),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Char('h') | KeyCode::Left => {
                let forward = matches!(code, KeyCode::Char('l') | KeyCode::Right);
                let at = seasons.iter().position(|s| *s == d.season).unwrap_or(0);
                let next = if forward { at + 1 } else { at.saturating_sub(1) };
                if let Some(season) = seasons.get(next) {
                    d.season = *season;
                    d.episode_cursor = 0;
                }
            }
            KeyCode::Enter => {
                let chosen = self.episodes_in_season().get(self.details.as_ref().unwrap().episode_cursor).map(|v| v.id.clone());
                if let Some(id) = chosen {
                    let d = self.details.as_mut().unwrap();
                    d.video_id = id.clone();
                    d.focus = DetailFocus::Streams;
                    d.cursor_touched = false;
                    let kind = d.meta.kind.clone();
                    self.fetch_streams(&kind, &id);
                    self.fetch_subs(&kind, &id);
                    self.sync_stream_cursor();
                    self.sync_subtitle_choice();
                }
            }
            _ => {}
        }
    }

    fn cycle_subtitle(&mut self) {
        let langs = streams::subtitle_languages(self.current_subs());
        let Some(d) = &mut self.details else { return };
        d.sub_touched = true;
        d.sub_choice = match d.sub_choice.as_ref().and_then(|c| langs.iter().position(|l| l == c)) {
            None => langs.first().cloned(),
            Some(i) => langs.get(i + 1).cloned(),
        };
    }

    fn selected_row(&self) -> Option<StreamRow> {
        let d = self.details.as_ref()?;
        self.stream_cache.get(&d.video_id)?.rows.get(d.stream_cursor).cloned()
    }

    fn copy_selected_url(&mut self) {
        match self.selected_row() {
            Some(row) => {
                copy_to_clipboard(&streams::display_url(&row));
                self.toast(true, "stream URL copied");
            }
            None => self.toast(false, "no stream selected"),
        }
    }

    fn play_selected_row(&mut self, mode: PlayMode) {
        let Some(row) = self.selected_row() else {
            self.toast(false, "no stream selected");
            return;
        };
        if row.stream.external_url.is_some() && row.stream.url.is_none() {
            open_external(row.stream.external_url.as_deref().unwrap_or_default());
            self.toast(true, "opened in browser");
            return;
        }
        let d = self.details.as_ref().unwrap();
        let (meta, video_id, sub) = (d.meta.clone(), d.video_id.clone(), d.sub_choice.clone());
        self.start_play(meta, video_id, row, Some(sub), mode);
    }

    fn on_playing_key(&mut self, key: KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let Some(p) = &mut self.playing else {
            self.leave_playing();
            return;
        };
        let Some(control) = p.control.clone() else {
            // Still connecting: only stop and leave work.
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                self.stop_current();
                self.leave_playing();
            }
            return;
        };
        match key.code {
            KeyCode::Char(' ') => control.command(json!(["cycle", "pause"])),
            KeyCode::Left => control.command(json!(["seek", if shift { -60 } else { -5 }])),
            KeyCode::Right => control.command(json!(["seek", if shift { 60 } else { 5 }])),
            KeyCode::Up => control.command(json!(["add", "volume", 5])),
            KeyCode::Down => control.command(json!(["add", "volume", -5])),
            KeyCode::Char('[') => control.command(json!(["multiply", "speed", 0.9])),
            KeyCode::Char(']') => control.command(json!(["multiply", "speed", 1.1])),
            KeyCode::Char('\\') => control.command(json!(["set_property", "speed", 1.0])),
            KeyCode::Char('s') => {
                control.command(json!(["cycle", "sid"]));
                p.log.push_back((clock(), "set sid".into(), "next".into()));
            }
            KeyCode::Char('a') => {
                control.command(json!(["cycle", "aid"]));
                p.log.push_back((clock(), "set aid".into(), "next".into()));
            }
            KeyCode::Char('c') => control.command(json!(["add", "chapter", 1])),
            KeyCode::Char('C') => control.command(json!(["add", "chapter", -1])),
            KeyCode::Char('z') => control.command(json!(["add", "sub-delay", -0.1])),
            KeyCode::Char('x') => control.command(json!(["add", "sub-delay", 0.1])),
            KeyCode::Char('Z') => control.command(json!(["add", "sub-scale", -0.1])),
            KeyCode::Char('X') => control.command(json!(["add", "sub-scale", 0.1])),
            KeyCode::Char('l') => control.command(json!(["cycle-values", "loop-file", "inf", "no"])),
            KeyCode::Char('f') | KeyCode::F(11) if p.embedded => p.video_fullscreen = !p.video_fullscreen,
            KeyCode::Char('t') if p.embedded => {
                p.theater = !p.theater;
                self.needs_clear = true;
            }
            KeyCode::F(11) => control.command(json!(["cycle", "fullscreen"])),
            KeyCode::Esc if p.video_fullscreen => p.video_fullscreen = false,
            KeyCode::Char('f') => control.command(json!(["cycle", "fullscreen"])),
            KeyCode::Char('A') => {
                self.config.streams.autoplay_next = !self.config.streams.autoplay_next;
                self.save_config();
            }
            KeyCode::Char('n') => {
                let next = p.up_next.first().map(|n| (n.meta.clone(), n.video_id.clone()));
                match next {
                    Some((meta, video_id)) => self.queue_next(meta, video_id),
                    None => self.toast(false, "nothing up next"),
                }
            }
            KeyCode::Char('q') => control.command(json!(["quit"])),
            KeyCode::Esc => self.leave_playing(),
            _ => {}
        }
    }

    fn on_install_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let view = &mut self.addons_view;
        let Some(input) = &mut view.input else { return };
        match key.code {
            KeyCode::Esc => view.input = None,
            KeyCode::Char('u') if ctrl => input.clear(),
            KeyCode::Char(c) if !ctrl => input.push(c),
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Enter => {
                let text = view.input.take().unwrap_or_default();
                self.install_addon(&text);
            }
            _ => {}
        }
    }

    fn install_addon(&mut self, text: &str) {
        let url = match addon::normalize_url(text) {
            Ok(url) => url,
            Err(e) => {
                self.addons_view.status = Some((false, e.to_string()));
                return;
            }
        };
        self.addons_view.status = Some((true, format!("installing {url}")));
        let (client, tx) = (self.client.clone(), self.tx.clone());
        tokio::spawn(async move {
            let added = client.fetch_addon(&AddonEntry { url, enabled: true }).await;
            let result = match added.error.clone() {
                Some(error) => Err(error),
                None => Ok(Box::new(added)),
            };
            let _ = tx.send(Ev::Installed(result));
        });
    }

    fn on_addons_key(&mut self, key: KeyEvent) {
        let len = self.addons.len();
        let view = &mut self.addons_view;
        if key.code != KeyCode::Char('d') {
            view.confirm_remove = false;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.go(Screen::Discover),
            KeyCode::Char('j') | KeyCode::Down => view.cursor = (view.cursor + 1).min(len.saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => view.cursor = view.cursor.saturating_sub(1),
            KeyCode::Char('i') => view.input = Some(String::new()),
            KeyCode::Char(' ') => {
                let cursor = view.cursor;
                if let Some(addon) = self.addons.get_mut(cursor) {
                    addon.enabled = !addon.enabled;
                }
                self.after_addon_change();
            }
            KeyCode::Char('J') => {
                if view.cursor + 1 < len {
                    self.addons.swap(view.cursor, view.cursor + 1);
                    view.cursor += 1;
                    self.after_addon_change();
                }
            }
            KeyCode::Char('K') => {
                if view.cursor > 0 {
                    self.addons.swap(view.cursor, view.cursor - 1);
                    view.cursor -= 1;
                    self.after_addon_change();
                }
            }
            KeyCode::Char('u') => {
                if let Some(addon) = self.addons.get(view.cursor) {
                    let url = addon.url.clone();
                    self.install_addon(&url);
                }
            }
            KeyCode::Char('U') => {
                let urls: Vec<String> = self.addons.iter().map(|a| a.url.clone()).collect();
                for url in urls {
                    self.install_addon(&url);
                }
            }
            KeyCode::Char('o') => {
                if let Some(addon) = self.addons.get(view.cursor).filter(|a| !a.is_local()) {
                    copy_to_clipboard(&addon.url);
                    open_external(&addon.url);
                }
            }
            KeyCode::Char('c') => {
                let target = self.addons.get(view.cursor).and_then(Addon::configure_url);
                match target {
                    Some(url) => {
                        open_external(&url);
                        self.toast(true, "opened configure page · paste the resulting link with i");
                    }
                    None => self.toast(false, "this addon has no configuration"),
                }
            }
            KeyCode::Char('d') => {
                if !view.confirm_remove {
                    view.confirm_remove = true;
                    let name = self.addons.get(view.cursor).map(|a| a.manifest.name.clone()).unwrap_or_default();
                    self.toast(false, format!("press d again to remove {name}"));
                } else {
                    view.confirm_remove = false;
                    if view.cursor < len {
                        let removed = self.addons.remove(view.cursor);
                        self.addons_view.cursor = self.addons_view.cursor.min(self.addons.len().saturating_sub(1));
                        self.toast(true, format!("removed {}", removed.manifest.name));
                        self.after_addon_change();
                    }
                }
            }
            _ => {}
        }
    }

    fn after_addon_change(&mut self) {
        self.save_addons();
        self.rebuild_catalogs();
        self.stream_cache.clear();
        self.sub_cache.clear();
    }

    fn on_settings_key(&mut self, key: KeyEvent) {
        let forward = match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Esc => return self.go(Screen::Discover),
            KeyCode::Char('j') | KeyCode::Down => {
                self.settings_cursor = (self.settings_cursor + 1).min(SETTINGS.len() - 1);
                return;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.settings_cursor = self.settings_cursor.saturating_sub(1);
                return;
            }
            KeyCode::Char('e') => {
                open_external(&Config::path().display().to_string());
                return;
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => true,
            KeyCode::Char('h') | KeyCode::Left => false,
            _ => return,
        };
        self.change_setting(forward);
        self.save_config();
    }

    fn change_setting(&mut self, forward: bool) {
        fn cycle(current: &str, options: &[&str], forward: bool) -> String {
            let at = options.iter().position(|o| *o == current).unwrap_or(0);
            let next = if forward { (at + 1) % options.len() } else { (at + options.len() - 1) % options.len() };
            options[next].to_string()
        }
        let delta = |step: i64| if forward { step } else { -step };
        let c = &mut self.config;
        match self.settings_cursor {
            0 => c.ui.accent = cycle(&c.ui.accent, &crate::theme::ACCENTS, forward),
            1 => {
                c.ui.posters = cycle(&c.ui.posters, &["auto", "kitty", "sixel", "blocks", "off"], forward);
                let mode = c.ui.posters.clone();
                self.posters.set_mode(&mode);
                self.needs_clear = true;
            }
            2 => c.player.output = cycle(&c.player.output, &["embedded", "window", "fullscreen"], forward),
            3 => c.player.resume = !c.player.resume,
            4 => c.player.volume = (c.player.volume as i64 + delta(5)).clamp(0, 130) as u32,
            5 => {
                const PRESETS: [&[&str]; 4] = [&["2160p", "1080p", "720p"], &["1080p", "720p"], &["720p", "1080p"], &["480p", "720p"]];
                let at = PRESETS.iter().position(|p| *p == c.streams.prefer.iter().map(String::as_str).collect::<Vec<_>>().as_slice()).unwrap_or(1);
                let next = if forward { (at + 1) % PRESETS.len() } else { (at + PRESETS.len() - 1) % PRESETS.len() };
                c.streams.prefer = PRESETS[next].iter().map(|s| s.to_string()).collect();
            }
            6 => c.streams.autoplay = cycle(&c.streams.autoplay, &["best", "ask"], forward),
            7 => c.streams.autoplay_next = !c.streams.autoplay_next,
            8 => c.streams.timeout = (c.streams.timeout as i64 + delta(1)).clamp(2, 60) as u64,
            9 => c.subtitles.auto = !c.subtitles.auto,
            10 => c.player.high_quality = !c.player.high_quality,
            11 => {
                const SIZES: [u64; 4] = [10, 25, 50, 100];
                let at = SIZES.iter().position(|s| *s == c.streams.max_size_gb).unwrap_or(1);
                let next = if forward { (at + 1) % SIZES.len() } else { (at + SIZES.len() - 1) % SIZES.len() };
                c.streams.max_size_gb = SIZES[next];
            }
            _ => {}
        }
    }
}
