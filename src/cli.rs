//! Non-interactive subcommands, styled to match the TUI.
use crate::addon::{self, Addon, Client, Meta, Subtitle};
use crate::config::{self, AddonEntry, Config};
use crate::mpv::{self, Mpv, MpvEvent, PlayRequest, PlayerState};
use crate::streams::{self, StreamRow};
use crate::torrent::Engine;
use anyhow::{Context, Result, bail};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use serde_json::json;
use std::io::Write;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

const GREEN: &str = "\x1b[38;2;166;227;161m";
const SKY: &str = "\x1b[38;2;137;220;235m";
const BLUE: &str = "\x1b[38;2;137;180;250m";
const PEACH: &str = "\x1b[38;2;250;179;135m";
const SUB: &str = "\x1b[38;2;186;194;222m";
const DIM: &str = "\x1b[38;2;147;153;178m";
const MAUVE: &str = "\x1b[38;2;203;166;247m";
const SURFACE2: &str = "\x1b[38;2;88;91;112m";
const RED: &str = "\x1b[38;2;243;139;168m";
const RESET: &str = "\x1b[0m";

fn pad(text: &str, width: usize) -> String {
    let used = text.width();
    format!("{text}{}", " ".repeat(width.saturating_sub(used)))
}

fn quality_ansi(quality: &str) -> &'static str {
    match quality {
        "2160p" | "1440p" | "1080p" => GREEN,
        "720p" => SKY,
        _ => SUB,
    }
}

async fn load_addons(config: &Config) -> Result<(Client, Vec<Addon>)> {
    let client = Client::new(config.streams.timeout)?;
    let entries = config::load_addons()?;
    let fetches = entries.iter().map(|e| client.fetch_addon(e));
    Ok((client.clone(), futures::future::join_all(fetches).await))
}

pub async fn addon_install(input: &str) -> Result<()> {
    let config = Config::load()?;
    let client = Client::new(config.streams.timeout)?;
    let url = addon::normalize_url(input)?;
    let mut entries = config::load_addons()?;
    let entry = AddonEntry { url: url.clone(), enabled: true };
    let added = client.fetch_addon(&entry).await;
    if let Some(error) = added.error {
        bail!("could not install {url}: {error}");
    }
    if !entries.iter().any(|e| e.url == url) {
        entries.push(entry);
        config::save_addons(&entries)?;
    }
    let m = &added.manifest;
    println!("  {GREEN}✓{RESET} {SUB}{} {} · {} · {}{RESET}", m.name, m.version, m.resource_names().join(" "), m.types.join(" "));
    Ok(())
}

pub async fn addon_list() -> Result<()> {
    let config = Config::load()?;
    let (_, addons) = load_addons(&config).await?;
    println!("{DIM}  #  {}{}{}{RESET}", pad("ADDON", 28), pad("VER", 10), "RESOURCES");
    for (i, a) in addons.iter().enumerate() {
        let name = if a.enabled { a.manifest.name.clone() } else { format!("{}  (off)", a.manifest.name) };
        let color = if a.enabled { SUB } else { DIM };
        println!("{color}  {PEACH}{}{color}  {}{}{}{RESET}", i + 1, pad(&name, 28), pad(&a.manifest.version, 10), a.manifest.resource_names().join(" "));
    }
    Ok(())
}

fn find_entry(entries: &[AddonEntry], addons: &[Addon], query: &str) -> Result<usize> {
    if let Ok(n) = query.parse::<usize>()
        && (1..=entries.len()).contains(&n)
    {
        return Ok(n - 1);
    }
    let lower = query.to_lowercase();
    addons.iter().position(|a| a.manifest.name.to_lowercase() == lower || a.url == query).with_context(|| format!("no addon named {query}"))
}

pub async fn addon_remove(query: &str) -> Result<()> {
    let config = Config::load()?;
    let mut entries = config::load_addons()?;
    let (_, addons) = load_addons(&config).await?;
    let i = find_entry(&entries, &addons, query)?;
    entries.remove(i);
    config::save_addons(&entries)?;
    println!("  {GREEN}✓{RESET} {SUB}removed {}{RESET}", addons[i].manifest.name);
    Ok(())
}

pub async fn addon_set_enabled(query: &str, enabled: bool) -> Result<()> {
    let config = Config::load()?;
    let mut entries = config::load_addons()?;
    let (_, addons) = load_addons(&config).await?;
    let i = find_entry(&entries, &addons, query)?;
    entries[i].enabled = enabled;
    config::save_addons(&entries)?;
    println!("  {GREEN}✓{RESET} {SUB}{} {}{RESET}", addons[i].manifest.name, if enabled { "on" } else { "off" });
    Ok(())
}

pub async fn search(query: &str, kind: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    let (client, addons) = load_addons(&config).await?;
    let results = search_all(&client, &addons, query, kind).await;
    if results.is_empty() {
        println!("  {DIM}no results{RESET}");
    }
    for meta in results {
        let badges: Vec<String> = meta.sources.iter().map(|s| s.chars().next().unwrap_or('?').to_string()).collect();
        println!(
            "  {DIM}{}{RESET}  {SUB}{}  {}  {}  {}{RESET}  {BLUE}{}{RESET}",
            pad(&meta.id, 12),
            pad(&meta.name, 36),
            pad(meta.year(), 5),
            pad(&meta.kind, 7),
            pad(&addon::format_runtime(&meta.runtime), 7),
            badges.join(" ")
        );
    }
    Ok(())
}

/// Queries every searchable catalog and merges results by id, in addon order.
pub async fn search_all(client: &Client, addons: &[Addon], query: &str, kind: Option<&str>) -> Vec<Meta> {
    let mut requests = Vec::new();
    for addon in addons.iter().filter(|a| a.enabled) {
        let mut seen: Vec<&str> = Vec::new();
        for catalog in addon.manifest.catalogs.iter().filter(|c| c.supports_search()) {
            if kind.is_some_and(|k| k != catalog.kind) || seen.contains(&catalog.kind.as_str()) {
                continue;
            }
            seen.push(&catalog.kind);
            requests.push(async move { client.catalog(addon, catalog, &[("search", query.to_string())]).await.unwrap_or_default() });
        }
    }
    merge_results(futures::future::join_all(requests).await)
}

pub fn merge_results(lists: Vec<Vec<Meta>>) -> Vec<Meta> {
    let mut merged: Vec<Meta> = Vec::new();
    for meta in lists.into_iter().flatten() {
        match merged.iter_mut().find(|m| m.id == meta.id) {
            Some(existing) => {
                for source in meta.sources {
                    if !existing.sources.contains(&source) {
                        existing.sources.push(source);
                    }
                }
            }
            None => merged.push(meta),
        }
    }
    merged
}

fn guess_kind(id: &str, kind: Option<&str>) -> String {
    kind.map(str::to_string).unwrap_or_else(|| if id.contains(':') { "series" } else { "movie" }.to_string())
}

/// All streams for an id across enabled addons, sorted for display.
pub async fn fetch_streams(client: &Client, addons: &[Addon], kind: &str, id: &str) -> Vec<StreamRow> {
    let requests = addons.iter().enumerate().filter(|(_, a)| a.enabled && a.manifest.supports("stream", kind, id)).map(|(i, a)| async move {
        let found = client.streams(a, kind, id).await.unwrap_or_else(|e| {
            eprintln!("  {RED}✗{RESET} {DIM}{}: {e:#}{RESET}", a.manifest.name);
            Vec::new()
        });
        found.into_iter().map(|s| streams::to_row(s, &a.manifest.name, i)).collect::<Vec<_>>()
    });
    let mut rows: Vec<StreamRow> = futures::future::join_all(requests).await.into_iter().flatten().collect();
    streams::sort_rows(&mut rows);
    rows
}

pub async fn fetch_subtitles(client: &Client, addons: &[Addon], kind: &str, id: &str) -> Vec<Subtitle> {
    let requests = addons
        .iter()
        .filter(|a| a.enabled && !a.is_local() && a.manifest.supports("subtitles", kind, id))
        .map(|a| async move { client.subtitles(a, kind, id).await.unwrap_or_default() });
    futures::future::join_all(requests).await.into_iter().flatten().collect()
}

pub async fn streams(id: &str, kind: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    let (client, addons) = load_addons(&config).await?;
    let kind = guess_kind(id, kind);
    let rows = fetch_streams(&client, &addons, &kind, id).await;
    if rows.is_empty() {
        println!("  {DIM}no streams{RESET}");
    }
    for (i, row) in rows.iter().enumerate() {
        let seeds = row.seeds.map(|s| format!("👤{s}")).unwrap_or_default();
        println!(
            "  {PEACH}{}{RESET}  {}{}{RESET}  {SUB}{}  {}  {}  {}  {}{RESET}",
            pad(&(i + 1).to_string(), 3),
            quality_ansi(&row.quality),
            pad(&row.quality, 6),
            pad(&truncate(&row.release, 60), 60),
            pad(&row.format, 14),
            pad(&row.size, 9),
            pad(&seeds, 8),
            row.addon
        );
    }
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + 1 >= max {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

pub async fn play(target: &str, quality: Option<&str>, sub: Option<&str>, kind: Option<&str>, output: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    let bin = mpv::find_bin(&config.player.bin).context("mpv not found: install it or set [player] bin in config.toml")?;
    let mut request = PlayRequest { volume: config.player.volume, extra_args: config.player.args.clone(), high_quality: config.player.high_quality, ..Default::default() };
    let output = output.unwrap_or(&config.player.output);
    match output {
        "window" | "embedded" => {}
        "fullscreen" => request.fullscreen = true,
        vo => request.terminal_vo = Some(vo.to_string()),
    }

    let is_id = target.starts_with("tt") || target.starts_with("kitsu:");
    let mut engine = None;
    if is_id {
        let (client, addons) = load_addons(&config).await?;
        let kind = guess_kind(target, kind);
        let mut title = target.to_string();
        let base_id = target.split(':').next().unwrap_or(target);
        for a in addons.iter().filter(|a| a.enabled && a.manifest.supports("meta", &kind, target)) {
            if let Ok(meta) = client.meta(a, &kind, base_id).await {
                title = format!("{} ({})", meta.name, meta.year());
                break;
            }
        }
        let rows = fetch_streams(&client, &addons, &kind, target).await;
        let row = match quality {
            Some(q) => streams::by_quality(&rows, q).with_context(|| format!("no {q} stream for {target}"))?,
            None => rows.get(streams::best_index(&rows, &config.streams).context("no streams found")?).context("no streams found")?,
        };
        println!("  {GREEN}▶{RESET} {SUB}{title} · {} · via {}{RESET}", row.quality, row.addon);
        request.title = title;
        request.target = match &row.stream.info_hash {
            Some(hash) if row.stream.url.is_none() => {
                println!("    {DIM}connecting to the swarm…{RESET}");
                let started = Engine::start().await?;
                let opened = started.open(hash, row.stream.file_idx, &row.stream.trackers).await?;
                engine = Some((started, hash.clone()));
                opened.url
            }
            _ => streams::display_url(row),
        };
        let languages = sub.map(|s| vec![s.to_string()]).unwrap_or(config.subtitles.languages.clone());
        if sub.is_some() || config.subtitles.auto {
            let subs = fetch_subtitles(&client, &addons, &kind, target).await;
            request.sub_select = languages.iter().find(|l| subs.iter().any(|s| addon::lang_code(&s.lang) == **l)).cloned();
            request.subs = pick_subtitles(&subs, &languages);
        }
    } else {
        request.title = target.to_string();
        request.target = target.to_string();
    }

    println!("    {DIM}{} {}{RESET}", bin.display(), request.args(&mpv::socket_path()).iter().take(2).cloned().collect::<Vec<_>>().join(" "));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let inherit = request.terminal_vo.is_some();
    let mut player = Mpv::spawn(&bin, &request, tx).await?;
    let control = player.control();
    if inherit {
        // mpv owns the terminal, so only keep the torrent paced until it exits.
        let mut state = PlayerState::default();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = player.child.wait() => break,
                Some(event) = rx.recv() => {
                    if let MpvEvent::Property(name, value) = event {
                        state.apply(&name, &value);
                    }
                }
                _ = tick.tick() => {
                    if let Some((engine, hash)) = &engine {
                        engine.pace(hash, state.duration, state.cache_seconds, state.buffering);
                    }
                }
            }
        }
        if let Some((engine, hash)) = engine {
            engine.close(&hash).await;
        }
        return Ok(());
    }

    println!("    {DIM}space pause · ←/→ seek · q quit{RESET}");
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel();
    crossterm::terminal::enable_raw_mode().ok();
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if let Event::Key(key) = event
                && key.kind != KeyEventKind::Release
                && key_tx.send(key).is_err()
            {
                break;
            }
        }
    });

    let mut state = PlayerState::default();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(MpvEvent::Property(name, value)) => state.apply(&name, &value),
                Some(MpvEvent::Closed) | None => break,
                _ => {}
            },
            Some(key) = key_rx.recv() => {
                let step = if key.modifiers.contains(KeyModifiers::SHIFT) { 60 } else { 5 };
                match key.code {
                    KeyCode::Char(' ') => control.command(json!(["cycle", "pause"])),
                    KeyCode::Left => control.command(json!(["seek", -step])),
                    KeyCode::Right => control.command(json!(["seek", step])),
                    KeyCode::Up => control.command(json!(["add", "volume", 5])),
                    KeyCode::Down => control.command(json!(["add", "volume", -5])),
                    KeyCode::Char('q') | KeyCode::Esc => control.command(json!(["quit"])),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => control.command(json!(["quit"])),
                    _ => {}
                }
            }
            _ = tick.tick() => {
                if let Some((engine, hash)) = &engine {
                    engine.pace(hash, state.duration, state.cache_seconds, state.buffering);
                }
                let filled = (state.progress() * 28.0) as usize;
                print!(
                    "\r    {SUB}{} {MAUVE}{}╸{SURFACE2}{}{SUB} {}  vol {:.0}%{RESET}\x1b[K",
                    mpv::format_time(state.position),
                    "━".repeat(filled),
                    "─".repeat(28 - filled),
                    mpv::format_time(state.duration),
                    state.volume,
                );
                let _ = std::io::stdout().flush();
            }
        }
    }
    crossterm::terminal::disable_raw_mode().ok();
    println!();
    if let Some((engine, hash)) = engine {
        engine.close(&hash).await;
    }
    Ok(())
}

/// One subtitle per wanted language, wanted languages first.
pub fn pick_subtitles(subs: &[Subtitle], languages: &[String]) -> Vec<Subtitle> {
    languages.iter().filter_map(|lang| subs.iter().find(|s| addon::lang_code(&s.lang) == *lang).cloned()).collect()
}
