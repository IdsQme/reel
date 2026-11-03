//! Launches mpv and talks to it over its JSON IPC socket (a named pipe on Windows).
use crate::addon::Subtitle;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

const OBSERVED: [&str; 21] = [
    "time-pos",
    "duration",
    "pause",
    "volume",
    "speed",
    "sub-delay",
    "sub-scale",
    "loop-file",
    "chapter",
    "chapter-list",
    "track-list",
    "demuxer-cache-duration",
    "cache-speed",
    "video-codec",
    "width",
    "height",
    "container-fps",
    "hwdec-current",
    "frame-drop-count",
    "paused-for-cache",
    "fullscreen",
];

#[derive(Clone, Debug)]
pub enum MpvEvent {
    Connected,
    Property(String, Value),
    /// Event name plus a short detail for the IPC log.
    Log(String, String),
    /// Playback reached the end of the file on its own.
    EndOfFile,
    Closed,
}

pub fn find_bin(configured: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "mpv.exe" } else { "mpv" };
    let configured_path = PathBuf::from(configured);
    if configured_path.is_absolute() && configured_path.exists() {
        return Some(configured_path);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for candidate in [dir.join(exe), dir.join(configured)] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let mut known: Vec<PathBuf> = vec![PathBuf::from(r"C:\Program Files\MPV Player\mpv.exe"), PathBuf::from(r"C:\Program Files\mpv\mpv.exe")];
    if let Some(home) = dirs::home_dir() {
        known.push(home.join(r"scoop\apps\mpv\current\mpv.exe"));
    }
    known.into_iter().find(|p| p.is_file())
}

pub fn socket_path() -> String {
    if cfg!(windows) {
        format!(r"\\.\pipe\reel-mpv-{}", std::process::id())
    } else {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("reel");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("mpv.sock").display().to_string()
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlayRequest {
    pub target: String,
    pub title: String,
    pub start: Option<f64>,
    pub volume: u32,
    pub fullscreen: bool,
    pub audio_only: bool,
    /// Draw into this window handle, inside the terminal.
    pub wid: Option<isize>,
    pub high_quality: bool,
    /// "tct" or "kitty" draws video in the terminal, for `reel play --output`.
    pub terminal_vo: Option<String>,
    pub extra_args: Vec<String>,
    pub subs: Vec<Subtitle>,
    /// Language code of the subtitle to select, if any.
    pub sub_select: Option<String>,
}

impl PlayRequest {
    pub fn args(&self, socket: &str) -> Vec<String> {
        let mut args = vec![
            format!("--input-ipc-server={socket}"),
            format!("--force-media-title={}", self.title),
            format!("--volume={}", self.volume),
            "--network-timeout=120".to_string(),
            "--cache=yes".to_string(),
            // mpv's default cache is the bulk of its memory; torrents are on disk anyway.
            "--demuxer-max-bytes=32M".to_string(),
            "--demuxer-max-back-bytes=8M".to_string(),
        ];
        match &self.terminal_vo {
            Some(vo) => args.push(format!("--vo={vo}")),
            None => {
                args.push("--no-terminal".to_string());
                if self.high_quality {
                    args.push("--profile=high-quality".to_string());
                    args.push("--deband=yes".to_string());
                }
                args.push(if self.audio_only { "--no-video".to_string() } else { "--force-window=immediate".to_string() });
            }
        }
        if let Some(wid) = self.wid {
            args.push(format!("--wid={wid}"));
            args.push("--no-window-dragging".to_string());
            // reel draws the controls.
            args.push("--no-osc".to_string());
        }
        if self.fullscreen {
            args.push("--fullscreen".to_string());
        }
        if let Some(start) = self.start {
            args.push(format!("--start={start:.0}"));
        }
        args.extend(self.extra_args.iter().cloned());
        args.push("--".to_string());
        args.push(self.target.clone());
        args
    }
}

pub struct Mpv {
    pub child: Child,
    writer: mpsc::UnboundedSender<String>,
}

#[derive(Clone)]
pub struct MpvControl {
    writer: mpsc::UnboundedSender<String>,
}

impl MpvControl {
    pub fn command(&self, args: Value) {
        let _ = self.writer.send(json!({ "command": args }).to_string());
    }
}

impl Mpv {
    pub fn control(&self) -> MpvControl {
        MpvControl { writer: self.writer.clone() }
    }

    /// Spawns mpv, connects to its IPC socket and forwards events until it exits.
    pub async fn spawn(bin: &PathBuf, request: &PlayRequest, events: mpsc::UnboundedSender<MpvEvent>) -> Result<Self> {
        let socket = socket_path();
        let inherit = request.terminal_vo.is_some();
        let mut command = Command::new(bin);
        command.args(request.args(&socket)).env("PATH", crate::youtube::child_path()).kill_on_drop(true);
        if !inherit {
            command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        }
        let mut child = command.spawn().with_context(|| format!("starting {}", bin.display()))?;
        let (reader, mut write_half) = connect(&socket, &mut child).await?;
        let (writer, mut outgoing) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            while let Some(line) = outgoing.recv().await {
                if write_half.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        let mpv = Self { child, writer };
        let control = mpv.control();
        for (i, name) in OBSERVED.iter().enumerate() {
            control.command(json!(["observe_property", i + 1, name]));
        }
        if request.wid.is_some() {
            control.command(json!(["keybind", "MBTN_LEFT", "cycle pause"]));
        }
        let _ = events.send(MpvEvent::Connected);
        let request = request.clone();

        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                // mpv rejects sub-add until the file has loaded.
                if value.get("event").and_then(Value::as_str) == Some("file-loaded") {
                    add_subtitles(&control, &request);
                }
                forward(&value, &events);
            }
            let _ = events.send(MpvEvent::Closed);
        });
        Ok(mpv)
    }
}

fn add_subtitles(control: &MpvControl, request: &PlayRequest) {
    for sub in &request.subs {
        let code = crate::addon::lang_code(&sub.lang);
        let flag = if request.sub_select.as_deref() == Some(code.as_str()) { "select" } else { "auto" };
        control.command(json!(["sub-add", sub.url, flag, code.to_uppercase(), code]));
    }
    if request.sub_select.is_none() {
        control.command(json!(["set_property", "sid", "no"]));
    }
}

fn forward(value: &Value, events: &mpsc::UnboundedSender<MpvEvent>) {
    let Some(event) = value.get("event").and_then(Value::as_str) else { return };
    match event {
        "property-change" => {
            if let Some(name) = value.get("name").and_then(Value::as_str) {
                let _ = events.send(MpvEvent::Property(name.to_string(), value.get("data").cloned().unwrap_or(Value::Null)));
            }
        }
        "end-file" => {
            let reason = value.get("reason").and_then(Value::as_str).unwrap_or_default();
            let detail = value.get("file_error").and_then(Value::as_str).map(|e| format!("{reason}: {e}")).unwrap_or_else(|| reason.to_string());
            let _ = events.send(MpvEvent::Log(event.to_string(), detail));
            if reason == "eof" {
                let _ = events.send(MpvEvent::EndOfFile);
            }
        }
        "file-loaded" | "seek" | "playback-restart" | "start-file" => {
            let _ = events.send(MpvEvent::Log(event.to_string(), String::new()));
        }
        _ => {}
    }
}

#[cfg(windows)]
async fn connect(socket: &str, child: &mut Child) -> Result<(tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>, tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>)> {
    use tokio::net::windows::named_pipe::ClientOptions;
    for _ in 0..100 {
        if let Ok(pipe) = ClientOptions::new().open(socket) {
            return Ok(tokio::io::split(pipe));
        }
        if let Some(status) = child.try_wait()? {
            bail!("mpv exited during startup ({status})");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("timed out connecting to mpv IPC at {socket}")
}

#[cfg(unix)]
async fn connect(socket: &str, child: &mut Child) -> Result<(tokio::io::ReadHalf<tokio::net::UnixStream>, tokio::io::WriteHalf<tokio::net::UnixStream>)> {
    for _ in 0..100 {
        if let Ok(stream) = tokio::net::UnixStream::connect(socket).await {
            return Ok(tokio::io::split(stream));
        }
        if let Some(status) = child.try_wait()? {
            bail!("mpv exited during startup ({status})");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("timed out connecting to mpv IPC at {socket}")
}

#[derive(Clone, Debug, Default)]
pub struct Track {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub lang: String,
    pub codec: String,
    pub selected: bool,
    pub external: bool,
    pub channels: i64,
    pub height: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Chapter {
    pub title: String,
    pub time: f64,
}

/// Everything the Now Playing screen and the CLI progress line show.
#[derive(Clone, Debug)]
pub struct PlayerState {
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub buffering: bool,
    pub volume: f64,
    pub speed: f64,
    pub sub_delay: f64,
    pub sub_scale: f64,
    pub looping: bool,
    pub chapter: i64,
    pub chapters: Vec<Chapter>,
    pub tracks: Vec<Track>,
    pub cache_seconds: f64,
    pub cache_speed: f64,
    pub codec: String,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub hwdec: String,
    pub dropped: i64,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            position: 0.0,
            duration: 0.0,
            paused: false,
            buffering: false,
            volume: 70.0,
            speed: 1.0,
            sub_delay: 0.0,
            sub_scale: 1.0,
            looping: false,
            chapter: -1,
            chapters: Vec::new(),
            tracks: Vec::new(),
            cache_seconds: 0.0,
            cache_speed: 0.0,
            codec: String::new(),
            width: 0,
            height: 0,
            fps: 0.0,
            hwdec: String::new(),
            dropped: 0,
        }
    }
}

impl PlayerState {
    pub fn apply(&mut self, name: &str, value: &Value) {
        let num = value.as_f64().unwrap_or(0.0);
        let text = || value.as_str().unwrap_or_default().to_string();
        match name {
            "time-pos" => self.position = num,
            "duration" => self.duration = num,
            "pause" => self.paused = value.as_bool().unwrap_or(false),
            "paused-for-cache" => self.buffering = value.as_bool().unwrap_or(false),
            "volume" => self.volume = num,
            "speed" => self.speed = num,
            "sub-delay" => self.sub_delay = num,
            "sub-scale" => self.sub_scale = num,
            "loop-file" => self.looping = value.as_bool().map(|b| b).unwrap_or_else(|| value.as_str().is_some_and(|s| s != "no")),
            "chapter" => self.chapter = value.as_i64().unwrap_or(-1),
            "chapter-list" => {
                self.chapters = value
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|c| Chapter {
                                title: c.get("title").and_then(Value::as_str).unwrap_or_default().to_string(),
                                time: c.get("time").and_then(Value::as_f64).unwrap_or(0.0),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            }
            "track-list" => {
                self.tracks = value
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|t| {
                                let s = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                                let n = |k: &str| t.get(k).and_then(Value::as_i64).unwrap_or(0);
                                Track {
                                    id: n("id"),
                                    kind: s("type"),
                                    title: s("title"),
                                    lang: s("lang"),
                                    codec: s("codec"),
                                    selected: t.get("selected").and_then(Value::as_bool).unwrap_or(false),
                                    external: t.get("external").and_then(Value::as_bool).unwrap_or(false),
                                    channels: n("demux-channel-count"),
                                    height: n("demux-h"),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            }
            "demuxer-cache-duration" => self.cache_seconds = num,
            "cache-speed" => self.cache_speed = num,
            "video-codec" => self.codec = text(),
            "width" => self.width = value.as_i64().unwrap_or(0),
            "height" => self.height = value.as_i64().unwrap_or(0),
            "container-fps" => self.fps = num,
            "hwdec-current" => self.hwdec = text(),
            "frame-drop-count" => self.dropped = value.as_i64().unwrap_or(0),
            _ => {}
        }
    }

    pub fn progress(&self) -> f64 {
        if self.duration > 0.0 { (self.position / self.duration).clamp(0.0, 1.0) } else { 0.0 }
    }
}

pub fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
}

/// Short "1:32" form used in the Continue panel.
pub fn format_short(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{}:{:02}", total / 3600, total / 60 % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_times() {
        assert_eq!(format_time(2472.0), "00:41:12");
        assert_eq!(format_short(5520.0), "1:32");
    }

    #[test]
    fn applies_properties() {
        let mut state = PlayerState::default();
        state.apply("time-pos", &json!(12.5));
        state.apply("duration", &json!(50.0));
        state.apply("loop-file", &json!("inf"));
        state.apply("track-list", &json!([{"id":1,"type":"sub","lang":"en","selected":true,"external":true}]));
        assert_eq!(state.progress(), 0.25);
        assert!(state.looping);
        assert_eq!(state.tracks[0].lang, "en");
        assert!(state.tracks[0].selected);
    }

    #[test]
    fn high_quality_comes_before_user_args_so_they_can_override_it() {
        let request = PlayRequest { target: "x".into(), high_quality: true, extra_args: vec!["--scale=bilinear".into()], ..Default::default() };
        let args = request.args("sock");
        let profile = args.iter().position(|a| a == "--profile=high-quality").unwrap();
        assert!(profile < args.iter().position(|a| a == "--scale=bilinear").unwrap());
        assert!(!PlayRequest::default().args("sock").contains(&"--profile=high-quality".to_string()));
    }

    #[test]
    fn embedded_video_has_no_mpv_controls() {
        let embedded = PlayRequest { target: "x".into(), wid: Some(1), ..Default::default() };
        assert!(embedded.args("sock").contains(&"--no-osc".to_string()));
        let windowed = PlayRequest { target: "x".into(), ..Default::default() };
        assert!(!windowed.args("sock").contains(&"--no-osc".to_string()));
    }

    #[test]
    fn builds_args_with_target_after_separator() {
        let request = PlayRequest { target: "http://x/y".into(), title: "T".into(), volume: 70, start: Some(61.2), ..Default::default() };
        let args = request.args("sock");
        assert_eq!(args.last().unwrap(), "http://x/y");
        assert_eq!(args[args.len() - 2], "--");
        assert!(args.contains(&"--start=61".to_string()));
    }
}
