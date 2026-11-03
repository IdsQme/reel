//! Downloads movie posters and encodes them for the terminal (kitty, sixel or half blocks).
//! Encoding happens off the UI thread; the draw code shows a placeholder until it lands.
use super::app::Ev;
use crate::config::cache_dir;
use image::DynamicImage;
use image::imageops::FilterType;
use ratatui::layout::Size;
use ratatui_image::Resize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};

pub type PosterKey = (String, u16, u16);

/// Encoded posters on screen plus a little history, and decoded images waiting to be encoded.
/// Anything evicted comes back from the disk cache when needed.
pub const MAX_RENDERED: usize = 40;
/// Decoded images never need to be larger than this to fill a poster frame.
const MAX_DECODED_SIZE: (u32, u32) = (480, 720);

pub struct Posters {
    picker: Picker,
    enabled: bool,
    raw: HashMap<String, Option<Arc<DynamicImage>>>,
    rendered: HashMap<PosterKey, Protocol>,
    rendered_order: VecDeque<PosterKey>,
    pending: HashSet<PosterKey>,
    fetching: HashSet<String>,
    wants: RefCell<Vec<PosterKey>>,
    downloads: Arc<Semaphore>,
    encodes: Arc<Semaphore>,
}

fn cache_path(url: &str) -> std::path::PathBuf {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    cache_dir().join("posters").join(format!("{:016x}", hasher.finish()))
}

impl Posters {
    pub fn new(picker: Picker, mode: &str) -> Self {
        let mut posters = Self {
            picker,
            enabled: true,
            raw: HashMap::new(),
            rendered: HashMap::new(),
            rendered_order: VecDeque::new(),
            pending: HashSet::new(),
            fetching: HashSet::new(),
            wants: RefCell::new(Vec::new()),
            // One HTTP/2 connection carries these, so many at once costs little.
            downloads: Arc::new(Semaphore::new(16)),
            encodes: Arc::new(Semaphore::new(std::thread::available_parallelism().map_or(4, |n| n.get().min(8)))),
        };
        posters.set_mode(mode);
        posters
    }

    pub fn set_mode(&mut self, mode: &str) {
        self.enabled = mode != "off";
        match mode {
            "kitty" => self.picker.set_protocol_type(ProtocolType::Kitty),
            "sixel" => self.picker.set_protocol_type(ProtocolType::Sixel),
            "blocks" => self.picker.set_protocol_type(ProtocolType::Halfblocks),
            _ => {}
        }
        self.rendered.clear();
        self.rendered_order.clear();
        self.pending.clear();
    }

    /// Cell height divided by cell width, for sizing posters to keep their shape.
    pub fn cell_aspect(&self) -> f32 {
        let font = self.picker.font_size();
        if font.width == 0 { 2.0 } else { font.height as f32 / font.width as f32 }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn protocol_name(&self) -> &'static str {
        match self.picker.protocol_type() {
            ProtocolType::Halfblocks => "blocks",
            ProtocolType::Sixel => "sixel",
            ProtocolType::Kitty => "kitty",
            ProtocolType::Iterm2 => "iterm2",
        }
    }

    /// The encoded poster if it is ready. Otherwise records the need for `process`.
    pub fn get(&self, url: &str, w: u16, h: u16) -> Option<&Protocol> {
        if !self.enabled || w == 0 || h == 0 {
            return None;
        }
        let key = (url.to_string(), w, h);
        let found = self.rendered.get(&key);
        if found.is_none() {
            self.wants.borrow_mut().push(key);
        }
        found
    }

    /// Starts downloads and encodes for everything the last frame asked for.
    pub fn process(&mut self, http: &reqwest::Client, tx: &mpsc::UnboundedSender<Ev>) {
        let wants: Vec<PosterKey> = self.wants.borrow_mut().drain(..).collect();
        for key in wants {
            if self.pending.contains(&key) {
                continue;
            }
            match self.raw.get(&key.0) {
                Some(Some(image)) => {
                    self.pending.insert(key.clone());
                    self.encode(key, image.clone(), tx);
                }
                Some(None) => {}
                None => self.fetch(key.0, http, tx),
            }
        }
    }

    fn fetch(&mut self, url: String, http: &reqwest::Client, tx: &mpsc::UnboundedSender<Ev>) {
        if !self.fetching.insert(url.clone()) {
            return;
        }
        let (http, tx, permits) = (http.clone(), tx.clone(), self.downloads.clone());
        tokio::spawn(async move {
            let _permit = permits.acquire().await;
            let image = download(&http, &url).await;
            let _ = tx.send(Ev::PosterRaw(url, image));
        });
    }

    fn encode(&self, key: PosterKey, image: Arc<DynamicImage>, tx: &mpsc::UnboundedSender<Ev>) {
        let (tx, picker, permits) = (tx.clone(), self.picker.clone(), self.encodes.clone());
        tokio::spawn(async move {
            let _permit = permits.acquire().await;
            let (w, h) = (key.1, key.2);
            let encoded = tokio::task::spawn_blocking(move || {
                let font = picker.font_size();
                let filled = image.resize_to_fill(w as u32 * font.width as u32, h as u32 * font.height as u32, FilterType::Triangle);
                picker.new_protocol(filled, Size::new(w, h), Resize::Fit(None)).ok()
            })
            .await
            .ok()
            .flatten();
            let _ = tx.send(Ev::PosterEncoded(key, encoded));
        });
    }

    pub fn on_raw(&mut self, url: String, image: Option<DynamicImage>) {
        self.fetching.remove(&url);
        self.raw.insert(url, image.map(Arc::new));
    }

    pub fn on_encoded(&mut self, key: PosterKey, protocol: Option<Protocol>) {
        self.pending.remove(&key);
        // The decoded image is by far the largest part. If this poster is ever needed
        // again, the disk cache brings it back in milliseconds.
        if !self.pending.iter().any(|other| other.0 == key.0) {
            self.raw.remove(&key.0);
        }
        if let Some(protocol) = protocol {
            self.rendered_order.push_back(key.clone());
            self.rendered.insert(key, protocol);
            while self.rendered_order.len() > MAX_RENDERED {
                if let Some(oldest) = self.rendered_order.pop_front() {
                    self.rendered.remove(&oldest);
                }
            }
        }
    }
}

async fn download(http: &reqwest::Client, url: &str) -> Option<DynamicImage> {
    let path = cache_path(url);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            let response = http.get(url).send().await.ok()?.error_for_status().ok()?;
            let bytes = response.bytes().await.ok()?.to_vec();
            if let Some(dir) = path.parent() {
                let _ = tokio::fs::create_dir_all(dir).await;
            }
            let _ = tokio::fs::write(&path, &bytes).await;
            bytes
        }
    };
    tokio::task::spawn_blocking(move || {
        let image = image::load_from_memory(&bytes).ok()?;
        let (w, h) = MAX_DECODED_SIZE;
        Some(if image.width() > w || image.height() > h { image.resize(w, h, FilterType::Triangle) } else { image })
    })
    .await
    .ok()
    .flatten()
}
