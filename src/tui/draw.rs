//! Renders each screen of the design. Reads App state only.
use super::app::*;
use crate::addon::{self, Meta};
use crossterm::event::KeyCode;
use crate::mpv::{self, format_time};
use crate::streams;
use crate::theme::*;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui_image::Image;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn hit(app: &App, area: Rect, action: Hit) {
    app.hits.borrow_mut().push((area, action));
}

fn key_code(label: &str) -> Option<KeyCode> {
    match label {
        "⏎" => Some(KeyCode::Enter),
        "esc" => Some(KeyCode::Esc),
        "tab" => Some(KeyCode::Tab),
        "space" => Some(KeyCode::Char(' ')),
        _ => {
            let mut chars = label.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(KeyCode::Char(c)),
                _ => None,
            }
        }
    }
}

fn fg(color: Color) -> Style {
    Style::new().fg(color)
}

fn bold(color: Color) -> Style {
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}

fn s<'a>(text: impl Into<String>, color: Color) -> Span<'a> {
    Span::styled(text.into(), fg(color))
}

fn b<'a>(text: impl Into<String>, color: Color) -> Span<'a> {
    Span::styled(text.into(), bold(color))
}

/// Shortens text to fit a cell width, adding an ellipsis when it had to cut.
fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn line_at(buf: &mut Buffer, x: u16, y: u16, width: u16, line: &Line) -> u16 {
    if y >= buf.area.bottom() {
        return x;
    }
    buf.set_line(x, y, line, width).0
}

fn fill(buf: &mut Buffer, area: Rect, color: Color) {
    buf.set_style(area.intersection(buf.area), Style::new().bg(color));
}

fn row(area: Rect, y: u16) -> Rect {
    Rect::new(area.x, y, area.width, 1)
}

/// A bordered panel with an inset title. Returns the inner area.
fn panel(f: &mut Frame, area: Rect, title: &str, right: &str, focused: bool, accent: Color) -> Rect {
    let border = if focused { accent } else { SURFACE1 };
    let title_color = if focused { accent } else { LAVENDER };
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(border))
        .style(Style::new().bg(BASE).fg(TEXT))
        .title(Line::from(b(format!(" {title} "), title_color)));
    if !right.is_empty() {
        block = block.title_top(Line::from(s(format!(" {right} "), OVERLAY2)).alignment(Alignment::Right));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

fn pad(area: Rect, horizontal: u16) -> Rect {
    Rect::new(area.x + horizontal, area.y, area.width.saturating_sub(horizontal * 2), area.height)
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    app.video_cells.set(None);
    app.hits.borrow_mut().clear();
    app.frame_size.set((area.width, area.height));
    f.render_widget(Block::new().style(Style::new().bg(BASE).fg(TEXT)), area);
    if area.width < 60 || area.height < 16 {
        f.render_widget(Paragraph::new(format!("reel needs at least 60×16 (now {}×{})", area.width, area.height)).style(fg(RED)), area);
        return;
    }
    let [header, _, body, status, keys] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0), Constraint::Length(1), Constraint::Length(1)]).areas(area);
    draw_header(f, app, pad(header, 1));
    let body = pad(body, 1);
    match app.screen {
        Screen::Discover => draw_discover(f, app, body),
        Screen::Search => draw_search(f, app, body),
        Screen::Library => draw_library(f, app, body),
        Screen::Addons => draw_addons(f, app, body),
        Screen::Settings => draw_settings(f, app, body),
        Screen::Details => draw_details(f, app, body),
        Screen::Playing => draw_playing(f, app, body),
    }
    draw_status(f, app, pad(status, 1));
    draw_keys(f, app, pad(keys, 1));
    if app.overlay != Overlay::None {
        // Popups draw in the terminal, so the video window has to step aside.
        app.video_cells.set(None);
        app.hits.borrow_mut().clear();
    }
    match app.overlay {
        Overlay::Help => draw_help(f, app, area),
        Overlay::PlayWith(cursor) => draw_play_with(f, app, area, cursor),
        Overlay::None => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let accent = app.accent();
    let tabs: [(&str, &str, bool); 5] = [
        ("1", "Discover", matches!(app.screen, Screen::Discover | Screen::Details)),
        ("2", "Search", app.screen == Screen::Search),
        ("3", "Library", app.screen == Screen::Library),
        ("4", "Addons", app.screen == Screen::Addons),
        ("5", "Settings", app.screen == Screen::Settings),
    ];
    let mut spans = vec![Span::styled(" ▶ reel ", Style::new().bg(accent).fg(CRUST).add_modifier(Modifier::BOLD)), Span::raw("  ")];
    let mut push_tab = |key: &str, name: &str, active: bool| {
        let bg = if active { SURFACE0 } else { BASE };
        spans.push(Span::styled(format!(" {key}"), Style::new().bg(bg).fg(PEACH)));
        let style = if active { bold(accent).bg(bg) } else { fg(SUBTEXT1).bg(bg) };
        spans.push(Span::styled(format!(" {name} "), style));
        spans.push(Span::raw(" "));
    };
    for (key, name, active) in tabs {
        push_tab(key, name, active);
    }
    if app.playing.is_some() {
        push_tab("0", "Playing", app.screen == Screen::Playing);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let targets = [Screen::Discover, Screen::Search, Screen::Library, Screen::Addons, Screen::Settings];
    let mut x = area.x + 10;
    for ((_, name, _), target) in tabs.iter().zip(targets) {
        let width = name.width() as u16 + 4;
        hit(app, Rect::new(x, area.y, width, 1), Hit::Go(target));
        x += width + 1;
    }
    if app.playing.is_some() {
        hit(app, Rect::new(x, area.y, 11, 1), Hit::Go(Screen::Playing));
    }

    let mpv_state: Line = match (&app.playing, &app.mpv_bin) {
        (_, None) => Line::from(vec![s("● ", RED), s("mpv not found", OVERLAY2)]),
        (Some(p), _) if p.state.paused => Line::from(vec![s("● ", YELLOW), s("mpv paused", OVERLAY2)]),
        (Some(_), _) => Line::from(vec![s("● ", GREEN), s("mpv playing", OVERLAY2)]),
        (None, _) => Line::from(vec![s("● ", GREEN), s("mpv ready", OVERLAY2)]),
    };
    let mut right = mpv_state.spans;
    right.push(s(format!("   {} addons", app.enabled_addons()), OVERLAY2));
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let accent = app.accent();
    let (mode, mode_color) = if app.screen == Screen::Playing {
        ("PLAY", GREEN)
    } else if app.inserting() {
        ("INS", BLUE)
    } else {
        ("NOR", accent)
    };
    fill(f.buffer_mut(), area, MANTLE);
    let mut crumb = crumb(app);
    let mut right = status_right(app);
    if let Some((ok, message, _)) = &app.toast {
        crumb = String::new();
        right = vec![];
        let toast = Line::from(vec![Span::styled(format!(" {mode} "), Style::new().bg(mode_color).fg(CRUST).add_modifier(Modifier::BOLD)), Span::raw("  "), s(if *ok { "✓ " } else { "✗ " }, if *ok { GREEN } else { RED }), s(fit(message, area.width.saturating_sub(12) as usize), if *ok { SUBTEXT1 } else { RED })]);
        f.render_widget(Paragraph::new(toast).style(Style::new().bg(MANTLE)), area);
    }
    if app.toast.is_none() {
        let line = Line::from(vec![Span::styled(format!(" {mode} "), Style::new().bg(mode_color).fg(CRUST).add_modifier(Modifier::BOLD)), Span::raw("  "), s(std::mem::take(&mut crumb), SUBTEXT1)]);
        f.render_widget(Paragraph::new(line).style(Style::new().bg(MANTLE)), area);
        right.push(Span::raw(" "));
        f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), area);
    }
}

fn crumb(app: &App) -> String {
    match app.screen {
        Screen::Discover => {
            let catalog = app.current_catalog().map(|c| format!("{} › {}", app.addons[c.addon].manifest.name, catalog_label(&c.def))).unwrap_or_default();
            format!("Discover › {catalog}")
        }
        Screen::Search => format!("Search › \"{}\"", app.search.query),
        Screen::Library => "Library".to_string(),
        Screen::Addons => format!("Addons › {}", app.addons.get(app.addons_view.cursor).map(|a| a.manifest.name.as_str()).unwrap_or("")),
        Screen::Settings => "Settings".to_string(),
        Screen::Details => {
            let d = app.details.as_ref();
            let back = match d.map(|d| d.back) {
                Some(Screen::Search) => "Search",
                Some(Screen::Library) => "Library",
                _ => "Discover",
            };
            format!("{back} › {}", d.map(|d| d.meta.name.as_str()).unwrap_or(""))
        }
        Screen::Playing => {
            let Some(p) = &app.playing else { return String::new() };
            let chapter = usize::try_from(p.state.chapter).ok().and_then(|c| p.state.chapters.get(c)).map(|c| c.title.clone()).filter(|t| !t.is_empty());
            match chapter {
                Some(chapter) => format!("{} · {chapter}", p.title),
                None => p.title.clone(),
            }
        }
    }
}

fn status_right<'a>(app: &App) -> Vec<Span<'a>> {
    let mut parts = Vec::new();
    match app.screen {
        Screen::Discover => {
            let total = app.visible().len();
            parts.push(s(format!("{}/{total}   ", if total == 0 { 0 } else { app.discover.cursor + 1 }), OVERLAY2));
            if app.discover.loading {
                parts.push(s("… loading", YELLOW));
            } else if app.discover.error.is_some() {
                parts.push(s("✗ catalog failed", RED));
            } else if let Some(ms) = app.discover.ms {
                parts.push(s("✓ ", GREEN));
                parts.push(s(format!("catalog {ms} ms"), OVERLAY2));
            }
        }
        Screen::Search => {
            let total = app.search.results.len();
            parts.push(s(format!("{}/{total}   ", if total == 0 { 0 } else { app.search.cursor + 1 }), OVERLAY2));
            if app.search.searching {
                parts.push(s("… searching", YELLOW));
            } else {
                parts.push(s("✓ ", GREEN));
                let searchable = app.addons.iter().filter(|a| a.enabled && a.manifest.catalogs.iter().any(|c| c.supports_search())).count();
                parts.push(s(format!("{searchable} addons answered"), OVERLAY2));
            }
        }
        Screen::Library => parts.push(s(format!("{} saved", app.state.library.len()), OVERLAY2)),
        Screen::Addons => {
            parts.push(s(format!("{}/{}   ", app.addons_view.cursor + 1, app.addons.len()), OVERLAY2));
            parts.push(s("~/.config/reel/addons.toml", OVERLAY2));
        }
        Screen::Settings => parts.push(s("~/.config/reel/config.toml", OVERLAY2)),
        Screen::Details => {
            if let Some(set) = app.current_streams() {
                let cursor = app.details.as_ref().map(|d| d.stream_cursor + 1).unwrap_or(0);
                parts.push(s(format!("stream {}/{}   ", if set.rows.is_empty() { 0 } else { cursor }, set.rows.len()), OVERLAY2));
                if set.pending > 0 {
                    parts.push(s(format!("… streams {}/{}", set.total - set.pending, set.total), YELLOW));
                } else {
                    parts.push(s("✓ ", GREEN));
                    parts.push(s(format!("streams {}/{} · {} ms", set.total, set.total, set.ms), OVERLAY2));
                }
            }
        }
        Screen::Playing => {
            if let Some(p) = &app.playing {
                parts.push(s(format!("{:.0}%   ", p.state.progress() * 100.0), OVERLAY2));
                if p.control.is_some() {
                    parts.push(s("✓ ", GREEN));
                    parts.push(s("ipc connected", OVERLAY2));
                } else {
                    parts.push(s("… connecting", YELLOW));
                }
            }
        }
    }
    parts
}

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let play = app.config.keys.play.clone();
    let search = app.config.keys.search.clone();
    let addons = app.config.keys.addons.clone();
    let (left, right): (Vec<(&str, &str)>, (&str, &str)) = match app.screen {
        Screen::Discover => (
            vec![("hjkl", "move"), ("⏎", "open"), (&play, "play"), (&search, "search"), ("tab", "panel"), ("+", "library"), (&addons, "addons"), ("S/G/Y", "sort·genre·year"), ("?", "help")],
            ("q", "quit"),
        ),
        Screen::Search => (vec![("↑/↓", "select"), ("tab", "type filter"), ("⏎", "open"), ("ctrl-p", "play best"), ("ctrl-r", "recent")], ("esc", "close")),
        Screen::Library => (vec![("hjkl", "move"), ("⏎", "open"), (&play, "play"), ("d", "remove"), ("?", "help")], ("esc", "back")),
        Screen::Addons => {
            if app.addons_view.input.is_some() {
                (vec![("⏎", "install"), ("ctrl-u", "clear")], ("esc", "cancel"))
            } else {
                (vec![("j/k", "select"), ("space", "on/off"), ("J/K", "reorder"), ("i", "install"), ("c", "configure"), ("u", "update"), ("d", "remove"), ("U", "update all")], ("esc", "back"))
            }
        }
        Screen::Settings => (vec![("j/k", "select"), ("h/l", "change"), ("e", "edit file")], ("esc", "back")),
        Screen::Details => {
            let series = app.details.as_ref().is_some_and(|d| d.meta.is_series());
            let mut keys: Vec<(&str, &str)> = if series { vec![("tab", "panel"), ("h/l", "season")] } else { vec![] };
            keys.extend([("j/k", "stream"), ("⏎", "play in mpv"), ("P", "play with…"), ("s", "subtitles"), ("y", "copy URL"), ("+", "library")]);
            (keys, ("esc", "back"))
        }
        Screen::Playing => (
            vec![("space", "pause"), ("←/→", "5 s"), ("⇧←/→", "60 s"), ("↑/↓", "volume"), ("[ ]", "speed"), ("s", "subs"), ("a", "audio"), ("c", "chapter"), ("n", "next"), ("f", "fullscreen")],
            ("q", "stop"),
        ),
    };
    let mut spans = Vec::new();
    let mut x = area.x;
    for (key, label) in left {
        let width = key.width() as u16 + label.width() as u16 + 1;
        if let Some(code) = key_code(key) {
            hit(app, Rect::new(x, area.y, width, 1), Hit::Key(code));
        }
        x += width + 3;
        spans.push(b(key.to_string(), PEACH));
        spans.push(s(format!(" {label}   "), OVERLAY2));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let right_width = right.0.width() as u16 + right.1.width() as u16 + 1;
    if let Some(code) = key_code(right.0) {
        hit(app, Rect::new(area.right().saturating_sub(right_width), area.y, right_width, 1), Hit::Key(code));
    }
    f.render_widget(Paragraph::new(Line::from(vec![b(right.0.to_string(), PEACH), s(format!(" {}", right.1), OVERLAY2)])).alignment(Alignment::Right), area);
}

fn catalog_label(def: &addon::CatalogDef) -> String {
    let kind = match def.kind.as_str() {
        "movie" => "Movies",
        "series" => "Series",
        "anime" => "Anime",
        "channel" => "Channels",
        "tv" => "TV",
        _ => "",
    };
    if def.name.starts_with('~') || kind.is_empty() { def.name.clone() } else { format!("{} · {kind}", def.name) }
}

fn draw_discover(f: &mut Frame, app: &App, area: Rect) {
    let accent = app.accent();
    let side = (area.width / 5).clamp(26, 36);
    let [aside, main] = Layout::horizontal([Constraint::Length(side), Constraint::Min(0)]).spacing(1).areas(area);
    let continuing = app.state.continuing();
    let continue_h = if continuing.is_empty() { 0 } else { (continuing.len().min(3) as u16) * 2 + 2 };
    let [catalogs, cont] = Layout::vertical([Constraint::Min(0), Constraint::Length(continue_h)]).areas(aside);

    let d = &app.discover;
    let inner = panel(f, catalogs, "Catalogs", "", d.focus == DiscoverFocus::Catalogs, accent);
    let inner = pad(inner, 1);
    let mut y = inner.y;
    let mut last_addon = usize::MAX;
    // Scroll so the selected catalog stays on screen.
    let mut lines: Vec<(Option<usize>, String, String)> = Vec::new();
    for (i, c) in d.catalogs.iter().enumerate() {
        if c.addon != last_addon {
            if last_addon != usize::MAX {
                lines.push((None, String::new(), String::new()));
            }
            let count = d.catalogs.iter().filter(|x| x.addon == c.addon).count();
            lines.push((None, app.addons[c.addon].manifest.name.clone(), count.to_string()));
            last_addon = c.addon;
        }
        lines.push((Some(i), catalog_label(&c.def), String::new()));
    }
    let selected_line = lines.iter().position(|l| l.0 == Some(d.selected)).unwrap_or(0);
    let skip = selected_line.saturating_sub(inner.height.saturating_sub(2) as usize);
    for (index, name, count) in lines.iter().skip(skip).take(inner.height as usize) {
        let buf = f.buffer_mut();
        match index {
            None => {
                line_at(buf, inner.x, y, inner.width, &Line::from(s(fit(name, inner.width.saturating_sub(4) as usize), OVERLAY2)));
                let w = count.width() as u16;
                line_at(buf, inner.right().saturating_sub(w), y, w, &Line::from(s(count.clone(), OVERLAY2)));
            }
            Some(i) if *i == d.selected => {
                hit(app, row(inner, y), Hit::Catalog(*i));
                fill(buf, row(inner, y), SURFACE0);
                line_at(buf, inner.x, y, inner.width, &Line::from(b(format!("▸ {}", fit(name, inner.width.saturating_sub(2) as usize)), accent)));
            }
            Some(i) => {
                hit(app, row(inner, y), Hit::Catalog(*i));
                line_at(buf, inner.x + 2, y, inner.width.saturating_sub(2), &Line::from(s(fit(name, inner.width.saturating_sub(2) as usize), TEXT)));
            }
        }
        y += 1;
    }
    if d.catalogs.is_empty() {
        line_at(f.buffer_mut(), inner.x, inner.y, inner.width, &Line::from(s("Loading addons…", OVERLAY2)));
    }

    if continue_h > 0 {
        let inner = pad(panel(f, cont, "Continue", "", d.focus == DiscoverFocus::Continue, accent), 1);
        for (i, p) in continuing.iter().take(3).enumerate() {
            let y = inner.y + i as u16 * 2;
            let selected = d.focus == DiscoverFocus::Continue && i == d.continue_cursor;
            hit(app, Rect::new(inner.x, y, inner.width, 2), Hit::ContinueItem(i));
            let title_style = if selected { bold(accent) } else { fg(TEXT) };
            line_at(f.buffer_mut(), inner.x, y, inner.width, &Line::from(Span::styled(fit(&p.meta.name, inner.width as usize), title_style)));
            let time = format!("{} / {}", mpv::format_short(p.position), mpv::format_short(p.duration));
            let w = time.width() as u16;
            line_at(f.buffer_mut(), inner.x, y + 1, inner.width, &Line::from(s(time, OVERLAY2)));
            let bar = inner.width.saturating_sub(w + 1);
            draw_mini_bar(f.buffer_mut(), inner.x + w + 1, y + 1, bar, p.position / p.duration, tint(&p.meta.id));
        }
    }

    // Main grid panel.
    let visible = app.visible();
    let title = app.current_catalog().map(|c| format!("{} · {}", app.addons[c.addon].manifest.name, catalog_label(&c.def))).unwrap_or_else(|| "Discover".to_string());
    let plus = if d.has_more { "+" } else { "" };
    let right = format!("{}{plus} titles", visible.len());
    let inner = pad(panel(f, main, &title, &right, d.focus == DiscoverFocus::Grid, accent), 1);
    if inner.height < 6 {
        return;
    }
    let [toolbar, _, grid, divider, info] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0), Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let chip = |text: String| Span::styled(format!(" {text} ▾ "), Style::new().bg(SURFACE0).fg(SUBTEXT1));
    let bar = Line::from(vec![
        chip(format!("Sort: {}", SORTS[d.sort])),
        Span::raw(" "),
        chip(format!("Genre: {}", d.genre.as_deref().unwrap_or("All"))),
        Span::raw(" "),
        chip(format!("Year: {}", d.year.as_deref().unwrap_or("All"))),
    ]);
    let mut chip_x = toolbar.x;
    for (span, key) in bar.spans.iter().step_by(2).zip(['S', 'G', 'Y']) {
        let width = span.width() as u16;
        hit(app, Rect::new(chip_x, toolbar.y, width, 1), Hit::Key(KeyCode::Char(key)));
        chip_x += width + 1;
    }
    f.render_widget(Paragraph::new(bar), toolbar);
    hit(app, Rect::new(toolbar.right().saturating_sub(3), toolbar.y, 2, 1), Hit::Key(KeyCode::Char('<')));
    hit(app, Rect::new(toolbar.right().saturating_sub(1), toolbar.y, 1, 1), Hit::Key(KeyCode::Char('>')));

    let (cols, rows) = grid_layout(app, grid);
    app.grid_shape.set((cols, rows));
    let page_size = cols * rows;
    let pages = visible.len().div_ceil(page_size).max(1);
    let page = d.cursor / page_size;
    let pager = Line::from(vec![s(format!("page {}/{}{plus}  ", page + 1, pages), OVERLAY2), s("‹ ›", PEACH)]);
    f.render_widget(Paragraph::new(pager).alignment(Alignment::Right), toolbar);

    if let Some(error) = &d.error {
        f.render_widget(Paragraph::new(s(error.clone(), RED)).wrap(Wrap { trim: true }), grid);
    } else if visible.is_empty() {
        let message = if d.loading { "Loading catalog…" } else { "Nothing here yet" };
        f.render_widget(Paragraph::new(s(message, OVERLAY2)), grid);
    } else {
        let start = page * page_size;
        let items: Vec<&Meta> = visible[start..visible.len().min(start + page_size)].iter().map(|&i| &d.items[i]).collect();
        draw_grid(f, app, grid, &items, cols, rows, Some(d.cursor - start).filter(|_| d.focus == DiscoverFocus::Grid), start, Hit::Grid);
    }

    let buf = f.buffer_mut();
    line_at(buf, divider.x, divider.y, divider.width, &Line::from(s("─".repeat(divider.width as usize), SURFACE1)));
    if let Some(selected) = visible.get(d.cursor).map(|&i| &d.items[i]) {
        draw_info_row(f, app, info, selected);
    }
}

fn draw_info_row(f: &mut Frame, app: &App, area: Rect, meta: &Meta) {
    let full = app.meta_cache.get(&meta.id).unwrap_or(meta);
    let mut facts = vec![meta.year().to_string()];
    let runtime = addon::format_runtime(&full.runtime);
    if !runtime.is_empty() {
        facts.push(runtime);
    }
    if !full.genres.is_empty() {
        facts.push(full.genres.iter().take(2).cloned().collect::<Vec<_>>().join(", "));
    }
    if let Some(set) = app.stream_cache.get(&meta.id) {
        if set.pending == 0 {
            facts.push(format!("{} streams from {} addons", set.rows.len(), set.addons_with_results()));
        } else {
            facts.push("finding streams…".to_string());
        }
    }
    let play = app.config.keys.play.clone();
    let hints = Line::from(vec![b("⏎", PEACH), s(" details  ", OVERLAY2), b(play, PEACH), s(" play best", OVERLAY2)]);
    let reserve = hints.width() as u16 + 2;
    let name_width = meta.name.width().min(40);
    let line = Line::from(vec![b(fit(&meta.name, name_width), TEXT), s("  ", OVERLAY2), s(fit(&facts.join(" · "), (area.width.saturating_sub(reserve) as usize).saturating_sub(name_width + 2)), OVERLAY2)]);
    f.render_widget(Paragraph::new(line), area);
    f.render_widget(Paragraph::new(hints).alignment(Alignment::Right), area);
}

fn draw_mini_bar(buf: &mut Buffer, x: u16, y: u16, width: u16, ratio: f64, color: Color) {
    if width == 0 {
        return;
    }
    let filled = ((ratio.clamp(0.0, 1.0) * width as f64) as usize).min(width as usize);
    let line = Line::from(vec![s("━".repeat(filled), color), s("─".repeat(width as usize - filled), SURFACE2)]);
    line_at(buf, x, y, width, &line);
}

/// How many poster cards fit, keeping real poster proportions (2:3).
fn grid_layout(app: &App, grid: Rect) -> (usize, usize) {
    let rows = ((grid.height + 1) / 13).max(1) as usize;
    let card_h = (grid.height + 1) / rows as u16;
    let poster_w = poster_width(app, card_h.saturating_sub(5));
    let cols = ((grid.width + 2) / (poster_w + 2 + 2).max(18)).max(1) as usize;
    (cols, rows)
}

/// Cell width of a poster with this inner height, at a 2:3 picture ratio.
fn poster_width(app: &App, inner_h: u16) -> u16 {
    ((inner_h as f32 * app.posters.cell_aspect() / 1.5).round() as u16).max(6)
}

#[allow(clippy::too_many_arguments)]
fn draw_grid(f: &mut Frame, app: &App, grid: Rect, items: &[&Meta], cols: usize, rows: usize, selected: Option<usize>, start: usize, on_click: fn(usize) -> Hit) {
    let accent = app.accent();
    let cell_w = (grid.width.saturating_sub((cols as u16 - 1) * 2)) / cols as u16;
    let card_h = (grid.height + 1) / rows as u16;
    let inner_h = card_h.saturating_sub(5);
    let poster_w = (poster_width(app, inner_h) + 2).min(cell_w);
    for (i, meta) in items.iter().enumerate() {
        let (cx, cy) = ((i % cols) as u16, (i / cols) as u16);
        let x = grid.x + cx * (cell_w + 2);
        let y = grid.y + cy * card_h;
        let is_selected = selected == Some(i);
        let box_area = Rect::new(x, y, poster_w, inner_h + 2);
        hit(app, Rect::new(x, y, cell_w, inner_h + 4), on_click(start + i));
        draw_poster(f, app, box_area, meta, is_selected);
        let progress = app.state.progress.get(&meta.id).filter(|p| p.duration > 0.0).map(|p| p.position / p.duration);
        let buf = f.buffer_mut();
        let title_y = y + inner_h + 2;
        let title = if is_selected { format!("▶ {}", meta.name) } else { meta.name.clone() };
        let style = if is_selected { bold(accent) } else { fg(TEXT) };
        line_at(buf, x, title_y, cell_w, &Line::from(Span::styled(fit(&title, cell_w as usize), style)));
        let genre = meta.genres.first().cloned().or_else(|| app.meta_cache.get(&meta.id).and_then(|m| m.genres.first().cloned()));
        let mut facts = vec![s(meta.year().to_string(), OVERLAY2)];
        if let Some(genre) = genre {
            facts.push(s(format!(" · {genre}"), OVERLAY2));
        }
        if let Some(p) = progress {
            facts.push(s(" · ", OVERLAY2));
            facts.push(s(format!("{:.0}%", p * 100.0), PINK));
        }
        line_at(buf, x, title_y + 1, cell_w, &Line::from(facts));
    }
}

/// A framed poster: the real image when it has loaded, otherwise a halftone placeholder.
fn draw_poster(f: &mut Frame, app: &App, area: Rect, meta: &Meta, focused: bool) {
    let border = if focused { app.accent() } else { SURFACE1 };
    let block = Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(fg(border)).style(Style::new().bg(MANTLE));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if let Some(url) = meta.poster_url()
        && let Some(protocol) = app.posters.get(&url, inner.width, inner.height)
    {
        f.render_widget(Image::new(protocol), inner);
        return;
    }
    let color = tint(&meta.id);
    let buf = f.buffer_mut();
    for y in inner.y..inner.bottom() {
        for x in inner.x..inner.right() {
            if (x + y) % 2 == 0 {
                buf[(x, y)].set_char('·').set_fg(SURFACE1).set_bg(MANTLE);
            }
        }
    }
    let caption = fit(&meta.name.to_uppercase(), inner.width.saturating_sub(1) as usize);
    let band_y = inner.bottom() - 1;
    fill(buf, row(inner, band_y), MANTLE);
    for x in inner.x..inner.right() {
        buf[(x, band_y)].set_char(' ');
    }
    line_at(buf, inner.x, band_y, inner.width, &Line::from(Span::styled(format!(" {caption}"), bold(ROSEWATER))));
    if inner.height >= 3 {
        let glyph_y = inner.y + (inner.height - 1) / 2;
        let glyph_x = inner.x + inner.width / 2;
        buf[(glyph_x, glyph_y)].set_char('▶').set_fg(color).set_bg(MANTLE);
    }
}

fn draw_library(f: &mut Frame, app: &App, area: Rect) {
    let right = format!("{} titles", app.state.library.len());
    let inner = pad(panel(f, area, "Library", &right, true, app.accent()), 1);
    let (cols, rows) = grid_layout(app, inner);
    app.grid_shape.set((cols, rows));
    if app.state.library.is_empty() {
        f.render_widget(Paragraph::new(vec![Line::from(s("Your library is empty.", TEXT)), Line::from(vec![s("Press ", OVERLAY2), b("+", PEACH), s(" on any title to save it here.", OVERLAY2)])]), inner);
        return;
    }
    let page_size = cols * rows;
    let start = app.library_cursor / page_size * page_size;
    let items: Vec<&Meta> = app.state.library.iter().skip(start).take(page_size).collect();
    draw_grid(f, app, inner, &items, cols, rows, Some(app.library_cursor - start), start, Hit::LibraryItem);
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let Some(d) = &app.details else { return };
    let accent = app.accent();
    let meta = &d.meta;
    let actions_h = 6;
    let poster_h = area.height.saturating_sub(actions_h).clamp(8, 26);
    let poster_inner_w = poster_width(app, poster_h.saturating_sub(2));
    let side_w = (poster_inner_w + 2).max(24).min(area.width / 2);
    let [aside, main] = Layout::horizontal([Constraint::Length(side_w), Constraint::Min(0)]).spacing(2).areas(area);
    let [poster, play_btn, buttons] = Layout::vertical([Constraint::Length(poster_h.min(aside.height.saturating_sub(actions_h))), Constraint::Length(3), Constraint::Length(3)]).areas(aside);
    draw_poster(f, app, poster, meta, false);

    let best = app.current_streams().and_then(|set| streams::best_index(&set.rows, &app.config.streams).map(|i| set.rows[i].quality.clone()));
    let play_label = match &best {
        Some(q) => format!("▶ Play best · {q}"),
        None if app.current_streams().is_some_and(|s| s.pending > 0) => "… finding streams".to_string(),
        None => "▶ Play best".to_string(),
    };
    fill(f.buffer_mut(), play_btn, accent);
    hit(app, play_btn, Hit::Key(KeyCode::Char('p')));
    hit(app, lib_btn_area(buttons).0, Hit::Key(KeyCode::Char('+')));
    hit(app, lib_btn_area(buttons).1, Hit::Key(KeyCode::Char('y')));
    let mid = Rect::new(play_btn.x + 1, play_btn.y + 1, play_btn.width.saturating_sub(2), 1);
    f.render_widget(Paragraph::new(Span::styled(play_label, bold(CRUST).bg(accent))), mid);
    f.render_widget(Paragraph::new(Span::styled("⏎", bold(CRUST).bg(accent))).alignment(Alignment::Right), mid);
    let [lib_btn, copy_btn] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).spacing(1).areas(buttons);
    let in_library = app.state.in_library(&meta.id);
    for (rect, key, label) in [(lib_btn, "+", if in_library { "Saved" } else { "Library" }), (copy_btn, "y", "Copy URL")] {
        fill(f.buffer_mut(), rect, SURFACE0);
        let mid = Rect::new(rect.x, rect.y + 1, rect.width, 1);
        f.render_widget(Paragraph::new(Line::from(vec![b(key.to_string(), PEACH).bg(SURFACE0), Span::styled(format!(" {label}"), fg(TEXT).bg(SURFACE0))])).alignment(Alignment::Center), mid);
    }

    // Right column: facts, then streams, then subtitles.
    let description_w = main.width.min(100);
    let desc_lines = (meta.description.width() as u16).div_ceil(description_w.max(1)).clamp(1, 4);
    let has_people = !meta.director.is_empty() || !meta.cast.is_empty();
    let info_h = 3 + 1 + desc_lines + if has_people { 3 } else { 1 };
    let [info, streams_row, subs] = Layout::vertical([Constraint::Length(info_h), Constraint::Min(5), Constraint::Length(3)]).areas(main);

    let buf = f.buffer_mut();
    line_at(buf, info.x, info.y, info.width, &Line::from(Span::styled(spaced_caps(&meta.name, info.width as usize), bold(ROSEWATER))));
    let mut facts: Vec<Span> = Vec::new();
    let mut push_fact = |text: String, color: Color| {
        if text.is_empty() {
            return;
        }
        if !facts.is_empty() {
            facts.push(s("  ·  ", OVERLAY2));
        }
        facts.push(s(text, color));
    };
    push_fact(meta.release_info.clone(), YELLOW);
    push_fact(addon::format_runtime(&meta.runtime), TEXT);
    push_fact(meta.genres.iter().take(3).cloned().collect::<Vec<_>>().join(", "), TEXT);
    push_fact(if meta.rating.is_empty() { String::new() } else { format!("★ {}", meta.rating) }, YELLOW);
    push_fact(meta.id.clone(), OVERLAY2);
    line_at(f.buffer_mut(), info.x, info.y + 2, info.width, &Line::from(facts));
    let description_area = Rect::new(info.x, info.y + 4, description_w, desc_lines);
    let description = if meta.description.is_empty() { "No description." } else { meta.description.as_str() };
    f.render_widget(Paragraph::new(s(description.to_string(), SUBTEXT1)).wrap(Wrap { trim: true }), description_area);
    if has_people {
        let y = info.y + 4 + desc_lines + 1;
        let label_w = 11;
        let buf = f.buffer_mut();
        if !meta.director.is_empty() {
            line_at(buf, info.x, y, info.width, &Line::from(vec![s(format!("{:<label_w$}", "Director"), OVERLAY2), s(fit(&meta.director.join(", "), info.width as usize - label_w), TEXT)]));
        }
        if !meta.cast.is_empty() {
            line_at(buf, info.x, y + 1, info.width, &Line::from(vec![s(format!("{:<label_w$}", "Cast"), OVERLAY2), s(fit(&meta.cast.iter().take(5).cloned().collect::<Vec<_>>().join(", "), info.width as usize - label_w), TEXT)]));
        }
    }

    let (episodes_area, streams_area) = if meta.is_series() {
        let [a, b] = Layout::horizontal([Constraint::Length(streams_row.width.min(44)), Constraint::Min(0)]).spacing(1).areas(streams_row);
        (Some(a), b)
    } else {
        (None, streams_row)
    };
    if let Some(area) = episodes_area {
        draw_episodes(f, app, area, d);
    }
    draw_streams(f, app, streams_area, d);
    draw_subtitles(f, app, subs, d);
}

fn lib_btn_area(buttons: Rect) -> (Rect, Rect) {
    let [library, copy] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).spacing(1).areas(buttons);
    (library, copy)
}

fn spaced_caps(name: &str, width: usize) -> String {
    let caps = name.to_uppercase();
    let spaced: String = caps.chars().flat_map(|c| [c, ' ']).collect();
    if spaced.width() <= width { spaced.trim_end().to_string() } else { fit(&caps, width) }
}

fn draw_episodes(f: &mut Frame, app: &App, area: Rect, d: &Details) {
    let accent = app.accent();
    let right = format!("season {} ‹ ›", d.season);
    let inner = pad(panel(f, area, "Episodes", &right, d.focus == DetailFocus::Episodes, accent), 1);
    let episodes = app.episodes_in_season();
    if episodes.is_empty() {
        f.render_widget(Paragraph::new(s("Loading episodes…", OVERLAY2)), inner);
        return;
    }
    let height = inner.height as usize;
    let skip = d.episode_cursor.saturating_sub(height.saturating_sub(1));
    for (i, video) in episodes.iter().enumerate().skip(skip).take(height) {
        let y = inner.y + (i - skip) as u16;
        let buf = f.buffer_mut();
        let selected = i == d.episode_cursor && d.focus == DetailFocus::Episodes;
        hit(app, row(inner, y), Hit::Episode(i));
        let current = video.id == d.video_id;
        if selected {
            fill(buf, row(inner, y), SURFACE0);
        }
        let marker = if current { s("▶ ", accent) } else { s("  ", TEXT) };
        let number = s(format!("{:>2} ", video.episode), PEACH);
        let title_w = inner.width.saturating_sub(5) as usize;
        let title_style = if selected { bold(TEXT) } else { fg(TEXT) };
        let line = Line::from(vec![marker, number, Span::styled(fit(&video.title, title_w), title_style)]);
        line_at(buf, inner.x, y, inner.width, &line);
    }
}

fn draw_streams(f: &mut Frame, app: &App, area: Rect, d: &Details) {
    let accent = app.accent();
    let set = app.current_streams();
    let right = set.map(|s| format!("{} from {} addons · by quality", s.rows.len(), s.addons_with_results())).unwrap_or_default();
    let focused = d.focus == DetailFocus::Streams;
    let inner = pad(panel(f, area, "Streams", &right, focused, accent), 1);
    if inner.height < 2 {
        return;
    }
    let rows = set.map(|s| s.rows.as_slice()).unwrap_or(&[]);
    if rows.is_empty() {
        let message = match set {
            Some(set) if set.pending > 0 => "Asking addons for streams…".to_string(),
            Some(set) if !set.errors.is_empty() => format!("{} did not answer. Press esc and open this title again to retry.", set.errors.join(", ")),
            Some(_) => "No streams found. Try installing another stream addon (4).".to_string(),
            None if d.meta.is_series() => "Pick an episode.".to_string(),
            None => String::new(),
        };
        f.render_widget(Paragraph::new(s(message, OVERLAY2)), inner);
        return;
    }
    let wide = inner.width >= 90;
    let widths: [u16; 6] = if wide { [3, 9, 0, 14, 10, 18] } else { [3, 9, 0, 0, 10, 0] };
    let release_w = inner.width.saturating_sub(widths.iter().sum::<u16>() + 8);
    let seeds_w = 7;
    let columns = |buf: &mut Buffer, y: u16, cells: [Span; 7]| {
        let mut x = inner.x;
        let sizes = [widths[0], widths[1], release_w, seeds_w, widths[3], widths[4], widths[5]];
        for (cell, size) in cells.into_iter().zip(sizes) {
            if size > 0 {
                let text = fit(&cell.content, size.saturating_sub(1) as usize);
                line_at(buf, x, y, size, &Line::from(Span::styled(text, cell.style)));
                x += size;
            }
        }
    };
    let footer = 3u16.min(inner.height.saturating_sub(2));
    let list_h = inner.height.saturating_sub(1 + footer) as usize;
    columns(
        f.buffer_mut(),
        inner.y,
        [s("", OVERLAY2), s("QUALITY", OVERLAY2), s("RELEASE", OVERLAY2), s("SEEDS", OVERLAY2), s("FORMAT", OVERLAY2), s("SIZE", OVERLAY2), s("ADDON", OVERLAY2)],
    );
    let skip = d.stream_cursor.saturating_sub(list_h.saturating_sub(1));
    for (i, stream) in rows.iter().enumerate().skip(skip).take(list_h) {
        let y = inner.y + 1 + (i - skip) as u16;
        let selected = i == d.stream_cursor;
        hit(app, row(inner, y), Hit::Stream(i));
        let buf = f.buffer_mut();
        if selected && focused {
            fill(buf, row(inner, y), SURFACE0);
        }
        let dim = if selected { SUBTEXT1 } else { TEXT };
        let quality = if selected { b(stream.quality.clone(), quality_color(&stream.quality)) } else { s(stream.quality.clone(), quality_color(&stream.quality)) };
        let release = if selected { b(stream.release.clone(), TEXT) } else { s(stream.release.clone(), TEXT) };
        let seeds = stream.seeds.map(|n| n.to_string()).unwrap_or_default();
        columns(
            buf,
            y,
            [
                if selected { b("▶", accent) } else { s("", TEXT) },
                quality,
                release,
                s(seeds, if stream.seeds == Some(0) { RED } else { dim }),
                s(stream.format.clone(), dim),
                s(stream.size.clone(), dim),
                s(stream.addon.clone(), dim),
            ],
        );
    }
    if footer >= 3 {
        let y = inner.bottom() - 3;
        let buf = f.buffer_mut();
        line_at(buf, inner.x, y, inner.width, &Line::from(s("─".repeat(inner.width as usize), SURFACE1)));
        if let Some(stream) = rows.get(d.stream_cursor) {
            let url = streams::display_url(stream);
            let label_w = 5usize;
            let width = (inner.width as usize).saturating_sub(label_w);
            line_at(buf, inner.x, y + 1, inner.width, &Line::from(vec![s(format!("{:<label_w$}", "url"), OVERLAY2), s(fit(&url, width), BLUE)]));
            let sub = d.sub_choice.as_ref().map(|l| format!(" --sub-add={l}")).unwrap_or_default();
            let args = format!("{}{sub} --force-media-title=\"{}\"", app.config.player.args.join(" "), d.meta.name);
            line_at(buf, inner.x, y + 2, inner.width, &Line::from(vec![s(format!("{:<label_w$}", "mpv"), OVERLAY2), s(fit(&args, width), SUBTEXT1)]));
        }
    }
}

fn draw_subtitles(f: &mut Frame, app: &App, area: Rect, d: &Details) {
    let subs = app.current_subs();
    let source = subs.first().map(|s| s.addon.clone()).unwrap_or_default();
    let inner = pad(panel(f, area, "Subtitles", &source, false, app.accent()), 1);
    let langs = streams::subtitle_languages(subs);
    if langs.is_empty() {
        f.render_widget(Paragraph::new(s("None available", OVERLAY2)), inner);
        return;
    }
    let mut spans = Vec::new();
    let mut used = 0usize;
    let budget = inner.width.saturating_sub(24) as usize;
    let shown = langs.iter().take_while(|l| {
        used += l.len() + 5;
        used < budget
    });
    let mut chip_x = inner.x;
    for lang in shown {
        let on = d.sub_choice.as_deref() == Some(lang.as_str());
        let text = if on { format!(" ✓ {} ", lang.to_uppercase()) } else { format!(" {} ", lang.to_uppercase()) };
        let width = text.width() as u16;
        hit(app, Rect::new(chip_x, inner.y, width, 1), Hit::Subtitle(Some(lang.clone())));
        chip_x += width + 1;
        spans.push(if on { Span::styled(text, bold(CRUST).bg(GREEN)) } else { Span::styled(text, fg(TEXT).bg(SURFACE0)) });
        spans.push(Span::raw(" "));
    }
    let off_on = d.sub_choice.is_none();
    hit(app, Rect::new(chip_x, inner.y, if off_on { 7 } else { 5 }, 1), Hit::Subtitle(None));
    spans.push(if off_on { Span::styled(" ✓ off ", bold(CRUST).bg(GREEN)) } else { Span::styled(" off ", fg(TEXT).bg(SURFACE0)) });
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
    let auto = if app.config.subtitles.auto { format!("auto-pick {}", app.config.subtitles.languages.first().map(|l| l.to_uppercase()).unwrap_or_default()) } else { "manual".to_string() };
    f.render_widget(Paragraph::new(s(auto, OVERLAY2)).alignment(Alignment::Right), inner);
}

fn draw_playing(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.playing else { return };
    let accent = app.accent();
    let st = &p.state;
    if p.embedded && p.theater {
        let [video, controls, transport] = Layout::vertical([Constraint::Min(0), Constraint::Length(1), Constraint::Length(1)]).areas(area);
        draw_video(f, app, video);
        draw_controls(f, app, controls);
        draw_transport(f, app, transport);
        return;
    }
    let ipc_h = if p.embedded { 5 } else { 7 };
    let [now, panels, ipc] = Layout::vertical([Constraint::Length(7), Constraint::Min(0), Constraint::Length(ipc_h)]).areas(area);

    let output = if p.embedded {
        "in reel"
    } else if p.fullscreen {
        "mpv window · fullscreen"
    } else {
        "mpv window"
    };
    let inner = pad(panel(f, now, "Now playing", output, false, accent), 1);
    let (chip, chip_color) = match p.phase {
        Phase::Resolving => (" ⋯ FINDING PEERS ", YELLOW),
        Phase::Loading => (" ⋯ LOADING ", YELLOW),
        Phase::Playing if st.buffering => (" ⋯ BUFFERING ", YELLOW),
        Phase::Playing if st.paused => (" ‖ PAUSED ", YELLOW),
        Phase::Playing => (" ▶ PLAYING ", GREEN),
    };
    let title = Line::from(vec![Span::styled(chip, bold(CRUST).bg(chip_color)), Span::raw("  "), b(fit(&p.title, inner.width.saturating_sub(20) as usize / 2), TEXT)]);
    f.render_widget(Paragraph::new(title), row(inner, inner.y));
    let source = Line::from(vec![b(p.quality.clone(), quality_color(&p.quality)), s(format!(" · {} ", fit(&p.release, inner.width as usize / 3)), SUBTEXT1), s("via ", OVERLAY2), s(p.addon.clone(), SUBTEXT1)]);
    f.render_widget(Paragraph::new(source).alignment(Alignment::Right), row(inner, inner.y));

    // Progress with the buffered range ahead of the playhead.
    draw_transport(f, app, row(inner, inner.y + 1));
    let bar_x = inner.x + 10;
    let bar_w = inner.width.saturating_sub(20);
    let buf = f.buffer_mut();

    // Chapter labels, or the remaining time when the file has no chapters.
    let y = inner.y + 2;
    line_at(buf, inner.right().saturating_sub(8), y, 8, &Line::from(s(format!("-{}", short_remaining(st.duration - st.position)), OVERLAY2)));
    for (i, chapter) in st.chapters.iter().enumerate() {
        if st.duration <= 0.0 {
            break;
        }
        let x = bar_x + (chapter.time / st.duration * bar_w as f64) as u16;
        let next = st.chapters.get(i + 1).map(|c| bar_x + (c.time / st.duration * bar_w as f64) as u16).unwrap_or(bar_x + bar_w);
        let current = st.chapter == i as i64;
        let label = if chapter.title.is_empty() { format!("{}", i + 1) } else { chapter.title.clone() };
        let width = next.saturating_sub(x);
        let style = if current { bold(accent) } else { fg(OVERLAY2) };
        let text = format!("│{}", fit(&label, width.saturating_sub(2) as usize));
        line_at(buf, x, y, width, &Line::from(Span::styled(text, style)));
    }

    // Technical line.
    draw_controls(f, app, row(inner, inner.y + 3));
    let buf = f.buffer_mut();
    let y = inner.y + 4;
    let mut tech = vec![s(video_summary(st), OVERLAY2), s("   ", OVERLAY2)];
    if !st.hwdec.is_empty() && st.hwdec != "no" {
        tech.push(s(format!("hwdec {}   ", st.hwdec), OVERLAY2));
    }
    let meter = ((st.cache_seconds / 60.0).clamp(0.0, 1.0) * 10.0) as usize;
    tech.push(s("▮".repeat(meter), GREEN));
    tech.push(s("▮".repeat(10 - meter), SURFACE2));
    tech.push(s(format!(" cache {:.0} s ahead   ", st.cache_seconds), OVERLAY2));
    match &p.torrent {
        Some(t) => tech.push(s(format!("{:.1} MB/s · {} peers · {:.0}%", t.speed_mbps, t.peers, t.progress * 100.0), OVERLAY2)),
        None => tech.push(s(format!("{:.1} MB/s", st.cache_speed / 1_048_576.0), OVERLAY2)),
    }
    line_at(buf, inner.x, y, inner.width, &Line::from(tech));
    let dropped = format!("dropped frames {}", st.dropped);
    let w = dropped.width() as u16;
    if inner.width > 100 {
        line_at(buf, inner.right() - w, y, w, &Line::from(s(dropped, OVERLAY2)));
    }

    // Embedded video takes the middle, with the panels stacked beside it.
    if p.embedded {
        let aspect = app.posters.cell_aspect();
        let wanted = (panels.height.saturating_sub(2) as f32 * aspect * 16.0 / 9.0) as u16 + 2;
        let side = panels.width.saturating_sub(wanted + 1).max(36);
        if panels.width <= side + 30 {
            draw_video(f, app, panels);
        } else {
            let [video, column] = Layout::horizontal([Constraint::Min(0), Constraint::Length(side)]).spacing(1).areas(panels);
            draw_video(f, app, video);
            let [tracks, playback, next] = Layout::vertical([Constraint::Min(5), Constraint::Length(9), Constraint::Length(6)]).areas(column);
            draw_tracks(f, app, tracks);
            draw_playback(f, app, playback);
            draw_up_next(f, app, next);
        }
    } else {
        let [tracks, playback, next] = Layout::horizontal([Constraint::Ratio(1, 3), Constraint::Ratio(1, 3), Constraint::Ratio(1, 3)]).spacing(1).areas(panels);
        draw_tracks(f, app, tracks);
        draw_playback(f, app, playback);
        draw_up_next(f, app, next);
    }

    // IPC log.
    let inner = pad(panel(f, ipc, "mpv · IPC", &mpv::socket_path(), false, accent), 1);
    let height = inner.height as usize;
    let skip = p.log.len().saturating_sub(height);
    for (i, (time, name, detail)) in p.log.iter().skip(skip).enumerate() {
        let is_command = name.starts_with("set") || name == "seek" || name == "torrent";
        let line = Line::from(vec![s(format!("{time:<10}"), OVERLAY2), s(format!("{name:<24}"), if is_command { TEAL } else { BLUE }), s(fit(detail, inner.width.saturating_sub(34) as usize), SUBTEXT1)]);
        line_at(f.buffer_mut(), inner.x, inner.y + i as u16, inner.width, &line);
    }
}

/// Elapsed time, progress bar with the buffered range, and duration, on one row.
fn draw_transport(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.playing else { return };
    let accent = app.accent();
    let st = &p.state;
    let y = area.y;
    let buf = f.buffer_mut();
    line_at(buf, area.x, y, 8, &Line::from(b(format_time(st.position), TEXT)));
    line_at(buf, area.right().saturating_sub(8), y, 8, &Line::from(s(format_time(st.duration), OVERLAY2)));
    let bar_w = area.width.saturating_sub(20);
    let filled = (st.progress() * bar_w as f64) as u16;
    let cached = if st.duration > 0.0 { ((st.cache_seconds / st.duration) * bar_w as f64) as u16 } else { 0 };
    let cached = cached.min(bar_w.saturating_sub(filled + 1));
    let rest = bar_w.saturating_sub(filled + 1 + cached);
    let track = Line::from(vec![s("━".repeat(filled as usize), accent), s("●", accent), s("━".repeat(cached as usize), SURFACE2), s("─".repeat(rest as usize), SURFACE2)]);
    line_at(buf, area.x + 10, y, bar_w, &track);
    hit(app, Rect::new(area.x + 10, y, bar_w, 1), Hit::Seek);
}

/// The clickable transport buttons, volume bar and toggles.
fn draw_controls(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.playing else { return };
    let accent = app.accent();
    let st = &p.state;
    let char_key = |c: char| Some(KeyCode::Char(c));
    let play = if st.paused { " ▶ Play " } else { " ‖ Pause " };
    let view = if p.theater { " ▣ Panels " } else { " ▣ Theater " };
    let loop_label = if st.looping { " ⟳ Loop on " } else { " ⟳ Loop " };
    let mut left: Vec<(String, Option<KeyCode>, Style)> = vec![
        (" |◀ ".to_string(), char_key('C'), fg(TEXT).bg(SURFACE0)),
        (" ◀◀ ".to_string(), Some(KeyCode::Left), fg(TEXT).bg(SURFACE0)),
        (play.to_string(), char_key(' '), bold(CRUST).bg(accent)),
        (" ▶▶ ".to_string(), Some(KeyCode::Right), fg(TEXT).bg(SURFACE0)),
        (" ▶| ".to_string(), char_key('c'), fg(TEXT).bg(SURFACE0)),
        (" Next ".to_string(), char_key('n'), fg(TEXT).bg(SURFACE0)),
        (" − ".to_string(), char_key('['), fg(TEXT).bg(SURFACE0)),
        (format!(" {:.2}× ", st.speed), Some(KeyCode::Char('\\')), fg(TEXT).bg(MANTLE)),
        (" + ".to_string(), char_key(']'), fg(TEXT).bg(SURFACE0)),
        (" Subs ".to_string(), char_key('s'), fg(TEXT).bg(SURFACE0)),
        (" Audio ".to_string(), char_key('a'), fg(TEXT).bg(SURFACE0)),
        (loop_label.to_string(), char_key('l'), fg(TEXT).bg(SURFACE0)),
        (view.to_string(), char_key('t'), fg(TEXT).bg(SURFACE0)),
        (" ▢ Fullscreen ".to_string(), char_key('f'), fg(TEXT).bg(SURFACE0)),
        (" ■ Stop ".to_string(), char_key('q'), fg(RED).bg(SURFACE0)),
    ];
    if !p.embedded {
        left.retain(|(_, code, _)| *code != char_key('t'));
    }
    let volume_w = 20u16;
    let mut x = area.x;
    let limit = area.right().saturating_sub(volume_w + 12);
    for (label, code, style) in left {
        let width = label.width() as u16;
        if x + width > limit {
            break;
        }
        line_at(f.buffer_mut(), x, area.y, width, &Line::from(Span::styled(label, style)));
        if let Some(code) = code {
            hit(app, Rect::new(x, area.y, width, 1), Hit::Key(code));
        }
        x += width + 1;
    }
    let bar_x = area.right().saturating_sub(volume_w + 6);
    let filled = ((st.volume / 100.0).clamp(0.0, 1.0) * volume_w as f64) as usize;
    let line = Line::from(vec![s("━".repeat(filled), accent), s("─".repeat(volume_w as usize - filled), SURFACE2), s(format!(" {:>3.0}%", st.volume), TEXT)]);
    line_at(f.buffer_mut(), bar_x, area.y, volume_w + 5, &line);
    line_at(f.buffer_mut(), bar_x.saturating_sub(4), area.y, 4, &Line::from(s("Vol ", OVERLAY2)));
    hit(app, Rect::new(bar_x, area.y, volume_w, 1), Hit::Volume);
}

/// The panel mpv's video window sits on.
fn draw_video(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.playing else { return };
    let right = if p.theater { "t panels · f fullscreen" } else { "t theater · f fullscreen" };
    let inner = panel(f, area, "Video", right, false, app.accent());
    hit(app, Rect::new(area.right().saturating_sub(right.width() as u16 + 3), area.y, right.width() as u16 + 2, 1), Hit::Key(KeyCode::Char('t')));
    fill(f.buffer_mut(), inner, CRUST);
    app.video_cells.set(Some(inner));
}

fn short_remaining(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    if total >= 3600 { format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60) } else { format!("{:02}:{:02}", total / 60, total % 60) }
}

fn video_summary(st: &mpv::PlayerState) -> String {
    if st.width == 0 {
        return "waiting for video".to_string();
    }
    let codec = st.codec.split_whitespace().next().unwrap_or("").to_string();
    let fps = if st.fps > 0.0 { format!(" · {:.0} fps", st.fps) } else { String::new() };
    format!("{codec} · {}×{}{fps}", st.width, st.height)
}

fn draw_tracks(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.playing else { return };
    let accent = app.accent();
    let inner = pad(panel(f, area, "Tracks", "", true, accent), 1);
    let mut lines: Vec<Line> = Vec::new();
    let row_hit = |index: usize, action: Hit| {
        if (index as u16) < inner.height {
            hit(app, row(inner, inner.y + index as u16), action);
        }
    };
    for (label, kind) in [("Video", "video"), ("Audio", "audio"), ("Subtitles", "sub")] {
        // Tall panels get a gap between groups; short ones keep Subtitles on screen.
        if !lines.is_empty() && inner.height >= 10 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(s(label, OVERLAY2)));
        for track in p.state.tracks.iter().filter(|t| t.kind == kind) {
            let name = track_label(track);
            match kind {
                "audio" => row_hit(lines.len(), Hit::Track("audio", track.id)),
                "sub" => row_hit(lines.len(), Hit::Track("sub", track.id)),
                _ => {}
            }
            let row = if track.selected && kind == "sub" {
                Line::from(b(format!("▸ {name}"), accent)).style(Style::new().bg(SURFACE0))
            } else if track.selected {
                Line::from(s(format!("▸ {name}"), TEXT))
            } else {
                Line::from(s(format!("  {name}"), TEXT))
            };
            lines.push(row);
        }
        if kind == "sub" {
            let off = !p.state.tracks.iter().any(|t| t.kind == "sub" && t.selected);
            row_hit(lines.len(), Hit::Track("sub", 0));
            lines.push(if off { Line::from(b("▸ off", accent)).style(Style::new().bg(SURFACE0)) } else { Line::from(s("  off", TEXT)) });
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn track_label(track: &mpv::Track) -> String {
    let mut parts: Vec<String> = Vec::new();
    match track.kind.as_str() {
        "video" => {
            parts.push(track.codec.clone());
            if track.height > 0 {
                parts.push(format!("{}p", track.height));
            }
        }
        "audio" => {
            if !track.title.is_empty() {
                parts.push(track.title.clone());
            } else if !track.lang.is_empty() {
                parts.push(track.lang.to_uppercase());
            }
            parts.push(track.codec.clone());
            if track.channels > 0 {
                parts.push(format!("{}ch", track.channels));
            }
        }
        _ => {
            parts.push(if track.lang.is_empty() { track.title.clone() } else { track.lang.to_uppercase() });
            parts.push(if track.external { "addon".to_string() } else { "embedded".to_string() });
            parts.push(track.codec.clone());
        }
    }
    format!("{}  {}", track.id, parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · "))
}

fn draw_playback(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.playing else { return };
    let accent = app.accent();
    let st = &p.state;
    let inner = pad(panel(f, area, "Playback", "", false, accent), 1);
    let volume = (st.volume / 10.0).round().clamp(0.0, 10.0) as usize;
    let label = |text: &str| s(format!("{text:<11}"), OVERLAY2);
    let lines = vec![
        Line::from(vec![label("Speed"), s(format!("{:.2}×", st.speed), TEXT)]),
        Line::from(vec![label("Volume"), s("▮".repeat(volume), accent), s("▮".repeat(10 - volume), SURFACE2), s(format!(" {:.0}%", st.volume), TEXT)]),
        Line::from(vec![label("Sub delay"), s(format!("{:+.1} s", st.sub_delay), TEXT), s("   z/x", PEACH)]),
        Line::from(vec![label("Sub size"), s(format!("{:.1}×", st.sub_scale), TEXT), s("   Z/X", PEACH)]),
        Line::from(vec![label("Loop"), s(if st.looping { "on" } else { "off" }, TEXT), s("   l", PEACH)]),
        Line::from(vec![label("Output"), s(if p.embedded { "in reel" } else if p.fullscreen { "fullscreen" } else { "mpv window" }, TEXT), s("   f", PEACH)]),
        Line::from(vec![label("Resume"), s(if app.config.player.resume { "saved every 10 s" } else { "off" }, TEXT)]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_up_next(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.playing else { return };
    let inner = pad(panel(f, area, "Up next", "", false, app.accent()), 1);
    let buf = f.buffer_mut();
    if p.up_next.is_empty() {
        line_at(buf, inner.x, inner.y, inner.width, &Line::from(s("Nothing queued", OVERLAY2)));
    }
    let list_h = inner.height.saturating_sub(2) as usize;
    for (i, next) in p.up_next.iter().take(list_h).enumerate() {
        let detail = fit(&next.detail, 8);
        let title_w = inner.width.saturating_sub(detail.width() as u16 + 4) as usize;
        let y = inner.y + i as u16;
        hit(app, row(inner, y), Hit::UpNext(i));
        line_at(buf, inner.x, y, inner.width, &Line::from(vec![s(format!("{}  ", i + 1), PEACH), s(fit(&next.label, title_w), TEXT)]));
        let w = detail.width() as u16;
        line_at(buf, inner.right() - w, y, w, &Line::from(s(detail, OVERLAY2)));
    }
    if inner.height >= 2 {
        let y = inner.bottom() - 2;
        line_at(buf, inner.x, y, inner.width, &Line::from(s("─".repeat(inner.width as usize), SURFACE1)));
        let on = app.config.streams.autoplay_next;
        line_at(buf, inner.x, y + 1, inner.width, &Line::from(vec![s("autoplay next  ", OVERLAY2), s(if on { "on" } else { "off" }, if on { GREEN } else { OVERLAY2 }), s("   A", PEACH)]));
    }
}

fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let accent = app.accent();
    let q = &app.search;
    let [top, bottom] = Layout::vertical([Constraint::Length(4), Constraint::Min(0)]).areas(area);
    let right = match (q.searching, q.ms) {
        (true, _) => "searching…".to_string(),
        (false, Some(ms)) => format!("{} results · {ms} ms", q.results.len()),
        _ => String::new(),
    };
    let inner = pad(panel(f, top, "Search", &right, true, accent), 1);
    let width = inner.width.saturating_sub(16) as usize;
    let shown: String = {
        let excess = q.query.chars().count().saturating_sub(width);
        q.query.chars().skip(excess).collect()
    };
    let input = Line::from(vec![b("/ ", accent), b(shown.clone(), TEXT), Span::styled(" ", Style::new().bg(ROSEWATER))]);
    f.render_widget(Paragraph::new(input), row(inner, inner.y));
    f.render_widget(Paragraph::new(s("ctrl-u clear", OVERLAY2)).alignment(Alignment::Right), row(inner, inner.y));
    let mut filters = vec![s("type ", OVERLAY2)];
    let mut chip_x = inner.x + 5;
    for (i, kind) in SEARCH_KINDS.iter().enumerate() {
        let width = kind.width() as u16 + 2;
        hit(app, Rect::new(chip_x, inner.y + 1, width, 1), Hit::SearchKind(i));
        chip_x += width + 1;
        filters.push(if i == q.kind { Span::styled(format!(" {kind} "), bold(CRUST).bg(accent)) } else { Span::styled(format!(" {kind} "), fg(TEXT).bg(SURFACE0)) });
        filters.push(Span::raw(" "));
    }
    if !app.state.recent.is_empty() {
        filters.push(s("    recent ", OVERLAY2));
        filters.push(s(app.state.recent.iter().take(4).cloned().collect::<Vec<_>>().join(" · "), SUBTEXT1));
    }
    f.render_widget(Paragraph::new(Line::from(filters)), row(inner, inner.y + 1));

    let preview_w = (area.width * 2 / 5).clamp(36, 56);
    let [results, preview] = Layout::horizontal([Constraint::Min(0), Constraint::Length(preview_w)]).spacing(1).areas(bottom);
    draw_search_results(f, app, results);
    draw_preview(f, app, preview);
}

const BADGE_COLORS: [Color; 6] = [BLUE, GREEN, PEACH, TEAL, MAUVE, YELLOW];

fn badge_color(app: &App, source: &str) -> Color {
    let index = app.addons.iter().position(|a| a.manifest.name == source).unwrap_or(0);
    BADGE_COLORS[index % BADGE_COLORS.len()]
}

fn draw_search_results(f: &mut Frame, app: &App, area: Rect) {
    let accent = app.accent();
    let q = &app.search;
    let inner = pad(panel(f, area, "Results", "merged by id", false, accent), 1);
    let order = app.search_order();
    let groups: [(&str, fn(&Meta) -> bool); 4] = [
        ("Movies", |m| m.kind == "movie" && !m.id.starts_with(crate::local::ID_PREFIX)),
        ("Series", |m| m.kind == "series"),
        ("Other", |m| m.kind != "movie" && m.kind != "series"),
        ("Local Files", |m| m.id.starts_with(crate::local::ID_PREFIX)),
    ];
    let list_h = inner.height.saturating_sub(2);
    let mut rows: Vec<Option<(usize, usize)>> = Vec::new();
    let mut headers: Vec<(usize, String, usize)> = Vec::new();
    let mut position = 0;
    for (name, test) in groups {
        let members: Vec<usize> = order.iter().copied().filter(|&i| test(&q.results[i])).collect();
        if members.is_empty() && (name == "Other" || name == "Local Files") {
            continue;
        }
        if q.results.is_empty() {
            continue;
        }
        if !rows.is_empty() {
            rows.push(None);
            headers.push((rows.len() - 1, String::new(), 0));
        }
        headers.push((rows.len(), name.to_string(), members.len()));
        rows.push(None);
        for i in members {
            rows.push(Some((i, position)));
            position += 1;
        }
    }
    let cursor_row = rows.iter().position(|r| r.is_some_and(|(_, p)| p == q.cursor)).unwrap_or(0);
    let skip = cursor_row.saturating_sub(list_h.saturating_sub(1) as usize);
    let title_w = inner.width.saturating_sub(3 + 6 + 8 + 10) as usize;
    let needle = q.query.trim().to_lowercase();
    for (line, entry) in rows.iter().enumerate().skip(skip).take(list_h as usize) {
        let y = inner.y + (line - skip) as u16;
        let buf = f.buffer_mut();
        match entry {
            None => {
                if let Some((_, name, count)) = headers.iter().find(|(at, ..)| *at == line) {
                    if !name.is_empty() {
                        line_at(buf, inner.x, y, inner.width, &Line::from(s(name.clone(), OVERLAY2)));
                        let text = count.to_string();
                        line_at(buf, inner.right() - text.width() as u16, y, text.width() as u16, &Line::from(s(text, OVERLAY2)));
                    }
                }
            }
            Some((index, pos)) => {
                let meta = &q.results[*index];
                let selected = *pos == q.cursor;
                hit(app, row(inner, y), Hit::SearchRow(*pos));
                if selected {
                    fill(buf, row(inner, y), SURFACE0);
                }
                let dim = if selected { SUBTEXT1 } else { OVERLAY2 };
                let mut x = inner.x;
                line_at(buf, x, y, 3, &Line::from(if selected { b("▸", accent) } else { s("", TEXT) }));
                x += 3;
                line_at(buf, x, y, title_w as u16, &highlight(&fit(&meta.name, title_w), &needle));
                x += title_w as u16;
                line_at(buf, x, y, 6, &Line::from(s(meta.year().to_string(), dim)));
                x += 6;
                line_at(buf, x, y, 8, &Line::from(s(addon::format_runtime(&meta.runtime), dim)));
                x += 8;
                let mut badges = Vec::new();
                for source in &meta.sources {
                    badges.push(s(source.chars().next().unwrap_or('?').to_string(), badge_color(app, source)));
                    badges.push(Span::raw(" "));
                }
                line_at(buf, x, y, 10, &Line::from(badges));
            }
        }
    }
    if q.results.is_empty() {
        let message = if q.query.trim().is_empty() {
            "Type to search every addon's catalogs."
        } else if q.searching {
            "Searching…"
        } else {
            "No results."
        };
        line_at(f.buffer_mut(), inner.x, inner.y, inner.width, &Line::from(s(message, OVERLAY2)));
    }
    if inner.height >= 2 {
        let y = inner.bottom() - 1;
        let mut legend = Vec::new();
        for addon in app.addons.iter().filter(|a| a.enabled && a.manifest.catalogs.iter().any(|c| c.supports_search())) {
            let name = &addon.manifest.name;
            legend.push(b(name.chars().next().unwrap_or('?').to_string(), badge_color(app, name)));
            legend.push(s(format!(" {name}   "), OVERLAY2));
        }
        line_at(f.buffer_mut(), inner.x, y, inner.width, &Line::from(legend));
    }
}

/// Bolds the part of a title matching the search, in yellow.
fn highlight<'a>(text: &str, needle: &str) -> Line<'a> {
    let lower = text.to_lowercase();
    match lower.find(needle).filter(|_| !needle.is_empty() && lower.len() == text.len()) {
        Some(at) => Line::from(vec![s(text[..at].to_string(), TEXT), b(text[at..at + needle.len()].to_string(), YELLOW), s(text[at + needle.len()..].to_string(), TEXT)]),
        None => Line::from(s(text.to_string(), TEXT)),
    }
}

fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let inner = pad(panel(f, area, "Preview", "", false, app.accent()), 1);
    let Some(selected) = app.search_order().get(app.search.cursor).map(|&i| &app.search.results[i]) else { return };
    let meta = app.meta_cache.get(&selected.id).unwrap_or(selected);
    let poster_h = (inner.height / 2).clamp(8, 14);
    let poster_w = poster_width(app, poster_h - 2) + 2;
    let poster = Rect::new(inner.x, inner.y, poster_w.min(inner.width / 2), poster_h);
    let shown = Meta { poster: if meta.poster.is_empty() { selected.poster.clone() } else { meta.poster.clone() }, ..meta.clone() };
    draw_poster(f, app, poster, &shown, false);

    let text_x = poster.right() + 2;
    let text_w = inner.right().saturating_sub(text_x);
    let mut facts = vec![meta.year().to_string()];
    let runtime = addon::format_runtime(&meta.runtime);
    if !runtime.is_empty() {
        facts.push(runtime);
    }
    if let Some(genre) = meta.genres.first() {
        facts.push(genre.clone());
    }
    let mut lines = vec![
        Line::from(Span::styled(meta.name.to_uppercase(), bold(ROSEWATER))),
        Line::from(s(facts.join(" · "), OVERLAY2)),
        Line::from(s(meta.id.clone(), OVERLAY2)),
    ];
    if !meta.director.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(s("Director", OVERLAY2)));
        lines.push(Line::from(s(meta.director.join(", "), TEXT)));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), Rect::new(text_x, inner.y, text_w, poster_h));

    let below = inner.y + poster_h + 1;
    let footer_y = inner.bottom().saturating_sub(4);
    let description_h = footer_y.saturating_sub(below).saturating_sub(1);
    if description_h > 0 {
        f.render_widget(Paragraph::new(s(meta.description.clone(), SUBTEXT1)).wrap(Wrap { trim: true }), Rect::new(inner.x, below, inner.width, description_h));
    }

    let label = |text: &str| s(format!("{text:<10}"), OVERLAY2);
    let streams_line = match app.stream_cache.get(&meta.id) {
        _ if meta.is_series() => Line::from(vec![label("Streams"), s("pick an episode", TEXT)]),
        Some(set) if set.pending == 0 => Line::from(vec![label("Streams"), s(format!("{} from {} addons", set.rows.len(), set.addons_with_results()), TEXT)]),
        _ => Line::from(vec![label("Streams"), s("looking…", OVERLAY2)]),
    };
    let best = app.stream_cache.get(&meta.id).and_then(|set| streams::best_index(&set.rows, &app.config.streams).map(|i| &set.rows[i]));
    let best_line = match best {
        Some(row) => Line::from(vec![label("Best"), b(row.quality.clone(), quality_color(&row.quality)), s(format!(" · {}", row.addon), TEXT)]),
        None => Line::from(vec![label("Best"), s("—", OVERLAY2)]),
    };
    let subs = app.sub_cache.get(&meta.id).map(|s| streams::subtitle_languages(s)).unwrap_or_default();
    let subs_text = if subs.is_empty() { "—".to_string() } else { subs.iter().take(6).map(|l| l.to_uppercase()).collect::<Vec<_>>().join(" ") };
    let subs_line = Line::from(vec![label("Subtitles"), s(subs_text, TEXT)]);
    let buf = f.buffer_mut();
    line_at(buf, inner.x, footer_y, inner.width, &streams_line);
    line_at(buf, inner.x, footer_y + 1, inner.width, &best_line);
    line_at(buf, inner.x, footer_y + 2, inner.width, &subs_line);
    line_at(buf, inner.x, footer_y + 3, inner.width, &Line::from(vec![s("Open details ", TEXT), b("⏎", PEACH)]));
    hit(app, Rect::new(inner.x, footer_y + 3, 15, 1), Hit::Key(KeyCode::Enter));
}

fn draw_addons(f: &mut Frame, app: &App, area: Rect) {
    let accent = app.accent();
    let view = &app.addons_view;
    let [top, bottom] = Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(area);
    let status = view.status.as_ref().map(|(ok, text)| format!("{} {text}", if *ok { "✓" } else { "✗" })).unwrap_or_default();
    let inner = pad(panel(f, top, "Install addon", &fit(&status, top.width.saturating_sub(20) as usize), view.input.is_some(), accent), 1);
    let field = Rect::new(inner.x, inner.y, inner.width.saturating_sub(14), 1);
    fill(f.buffer_mut(), field, MANTLE);
    let mut spans = vec![b("› ", PEACH).bg(MANTLE), s("manifest URL  ", OVERLAY2).bg(MANTLE)];
    match &view.input {
        Some(text) => {
            let width = field.width.saturating_sub(18) as usize;
            let excess = text.chars().count().saturating_sub(width);
            spans.push(s(text.chars().skip(excess).collect::<String>(), TEXT).bg(MANTLE));
            spans.push(Span::styled(" ", Style::new().bg(ROSEWATER)));
        }
        None => spans.push(s("https://…/manifest.json or stremio://…   press i", OVERLAY2).bg(MANTLE)),
    }
    f.render_widget(Paragraph::new(Line::from(spans)), field);
    let button = Rect::new(field.right() + 1, inner.y, 13, 1);
    let install_key = if view.input.is_some() { KeyCode::Enter } else { KeyCode::Char('i') };
    hit(app, button, Hit::Key(install_key));
    if view.input.is_none() {
        hit(app, field, Hit::Key(KeyCode::Char('i')));
    }
    f.render_widget(Paragraph::new(Span::styled(" ⏎ Install ", bold(CRUST).bg(accent))), button);
    f.render_widget(Paragraph::new(s("Any Stremio-compatible addon works. reel only sends content ids and search text to it.", OVERLAY2)), row(inner, inner.y + 2));

    let manifest_w = (area.width * 2 / 5).clamp(36, 56);
    let [list, manifest] = Layout::horizontal([Constraint::Min(0), Constraint::Length(manifest_w)]).spacing(1).areas(bottom);
    let on = app.enabled_addons();
    let inner = pad(panel(f, list, "Installed", &format!("{on} of {} on", app.addons.len()), view.input.is_none(), accent), 1);
    let chips_w = 28u16.min(inner.width / 3);
    let status_w = 9u16;
    let ver_w = 9u16;
    let name_w = inner.width.saturating_sub(3 + 4 + ver_w + chips_w + status_w);
    let columns_x = [inner.x, inner.x + 3, inner.x + 7, inner.x + 7 + name_w, inner.x + 7 + name_w + ver_w, inner.x + 7 + name_w + ver_w + chips_w];
    let buf = f.buffer_mut();
    for (x, label) in columns_x.iter().zip(["#", "", "ADDON", "VER", "RESOURCES", "STATUS"]) {
        line_at(buf, *x, inner.y, 12, &Line::from(s(label, OVERLAY2)));
    }
    for (i, addon) in app.addons.iter().enumerate() {
        let y = inner.y + 1 + i as u16 * 2;
        if y + 1 >= inner.bottom().saturating_sub(1) {
            break;
        }
        let selected = i == view.cursor;
        hit(app, Rect::new(inner.x, y, inner.width, 2), Hit::Addon(i));
        hit(app, Rect::new(columns_x[1], y, 3, 1), Hit::AddonToggle(i));
        if selected {
            fill(buf, Rect::new(inner.x, y, inner.width, 2), SURFACE0);
        }
        let chip_bg = if selected { BASE } else { SURFACE0 };
        let dim = if selected { SUBTEXT0 } else { OVERLAY2 };
        line_at(buf, columns_x[0], y, 3, &Line::from(s((i + 1).to_string(), PEACH)));
        line_at(buf, columns_x[1], y, 4, &Line::from(if addon.enabled { s("[x]", GREEN) } else { s("[ ]", OVERLAY2) }));
        let name = fit(&addon.manifest.name, name_w.saturating_sub(1) as usize);
        let name_span = if selected { b(name, accent) } else if addon.enabled { s(name, TEXT) } else { s(name, OVERLAY2) };
        line_at(buf, columns_x[2], y, name_w, &Line::from(name_span));
        line_at(buf, columns_x[2], y + 1, name_w, &Line::from(s(fit(&addon.manifest.types.join(" · "), name_w.saturating_sub(1) as usize), dim)));
        line_at(buf, columns_x[3], y, ver_w, &Line::from(s(fit(&addon.manifest.version, ver_w as usize - 1), if addon.enabled { SUBTEXT1 } else { OVERLAY2 })));
        let mut chips = Vec::new();
        for resource in addon.manifest.resource_names() {
            let color = if addon.enabled { resource_color(&resource) } else { OVERLAY2 };
            chips.push(Span::styled(format!(" {resource} "), fg(color).bg(chip_bg)));
            chips.push(Span::raw(" "));
        }
        line_at(buf, columns_x[4], y, chips_w, &Line::from(chips));
        let state = if !addon.enabled {
            Line::from(s("○ off", OVERLAY2))
        } else if addon.is_local() {
            Line::from(vec![s("● ", GREEN), s("local", dim)])
        } else if addon.error.is_some() {
            Line::from(vec![s("● ", RED), s("error", dim)])
        } else {
            match addon.latency {
                Some(ms) => Line::from(vec![s("● ", GREEN), s(format!("{ms}ms"), dim)]),
                None => Line::from(s("…", dim)),
            }
        };
        line_at(buf, columns_x[5], y, status_w, &state);
    }
    let y = inner.bottom().saturating_sub(1);
    line_at(buf, inner.x, y, inner.width, &Line::from(s("Order is priority: streams and subtitles are listed in addon order.", OVERLAY2)));

    let Some(addon) = app.addons.get(view.cursor) else { return };
    let m = &addon.manifest;
    let inner = pad(panel(f, manifest, "Manifest", &format!("v{}", m.version), false, accent), 1);
    let label = |text: &str| s(format!("{text:<11}"), OVERLAY2);
    let width = inner.width.saturating_sub(11) as usize;
    let mut resources: Vec<Span> = vec![label("resources")];
    for (i, resource) in m.resource_names().iter().enumerate() {
        if i > 0 {
            resources.push(s(" · ", OVERLAY2));
        }
        resources.push(s(resource.clone(), resource_color(resource)));
    }
    let catalogs = m.catalogs.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
    let mut catalogs_unique = catalogs.clone();
    catalogs_unique.dedup();
    let mut extras: Vec<String> = m.catalogs.iter().flat_map(|c| c.extras.iter().cloned()).collect();
    extras.sort();
    extras.dedup();
    let description_h = 4u16;
    let text = vec![Line::from(b(m.name.clone(), ROSEWATER)), Line::from(s(m.id.clone(), OVERLAY2))];
    f.render_widget(Paragraph::new(text), Rect::new(inner.x, inner.y, inner.width, 2));
    f.render_widget(Paragraph::new(s(m.description.clone(), SUBTEXT1)).wrap(Wrap { trim: true }), Rect::new(inner.x, inner.y + 3, inner.width, description_h));
    let or_none = |text: String| if text.is_empty() { "none".to_string() } else { text };
    let table = vec![
        Line::from(vec![label("url"), s(fit(&addon.url, width), BLUE)]),
        Line::from(resources),
        Line::from(vec![label("types"), s(fit(&or_none(m.types.join(", ")), width), TEXT)]),
        Line::from(vec![label("idPrefixes"), s(fit(&or_none(m.id_prefixes.join(", ")), width), TEXT)]),
        Line::from(vec![label("catalogs"), s(fit(&or_none(catalogs_unique.join(", ")), width), TEXT)]),
        Line::from(vec![label("extra"), s(fit(&or_none(extras.join(" · ")), width), TEXT)]),
        Line::from(vec![label("config"), if m.configurable { s("c opens the addon's configure page", TEXT) } else { s("none", OVERLAY2) }]),
    ];
    let table_y = inner.y + 3 + description_h;
    f.render_widget(Paragraph::new(table), Rect::new(inner.x, table_y, inner.width, inner.bottom().saturating_sub(table_y).saturating_sub(5)));
    if let Some(error) = &addon.error {
        let y = inner.bottom().saturating_sub(6);
        line_at(f.buffer_mut(), inner.x, y, inner.width, &Line::from(s(fit(error, inner.width as usize), RED)));
    }

    // Action buttons, two by two, as in the design.
    if inner.height < 14 {
        return;
    }
    let buttons_y = inner.bottom() - 4;
    let half = inner.width.saturating_sub(1) / 2;
    let actions: [(&str, &str, Color, bool); 4] =
        [("u", "Update", TEXT, !addon.is_local()), ("o", "Open URL", TEXT, !addon.is_local()), ("c", "Configure", TEXT, addon.configure_url().is_some()), ("d", "Remove", RED, true)];
    for (i, (key, name, color, active)) in actions.into_iter().enumerate() {
        let x = inner.x + (i as u16 % 2) * (half + 1);
        let y = buttons_y + (i as u16 / 2) * 2;
        let rect = Rect::new(x, y, half, 1);
        let buf = f.buffer_mut();
        if active {
            hit(app, rect, Hit::Key(KeyCode::Char(key.chars().next().unwrap_or(' '))));
            fill(buf, rect, SURFACE0);
            let key_color = if color == RED { RED } else { PEACH };
            f.render_widget(Paragraph::new(Line::from(vec![b(key.to_string(), key_color).bg(SURFACE0), Span::styled(format!(" {name}"), fg(color).bg(SURFACE0))])).alignment(Alignment::Center), rect);
        } else {
            f.render_widget(Paragraph::new(s(format!("{key} {name}"), OVERLAY2)).alignment(Alignment::Center), rect);
        }
    }
}

fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let accent = app.accent();
    let c = &app.config;
    let [list, notes] = Layout::horizontal([Constraint::Min(0), Constraint::Length((area.width * 2 / 5).clamp(36, 56))]).spacing(1).areas(area);
    let inner = pad(panel(f, list, "Settings", "saved to config.toml", true, accent), 1);
    let on_off = |v: bool| if v { "on".to_string() } else { "off".to_string() };
    let values = [
        c.ui.accent.clone(),
        c.ui.posters.clone(),
        c.player.output.clone(),
        on_off(c.player.resume),
        format!("{}%", c.player.volume),
        c.streams.prefer.join(" › "),
        c.streams.autoplay.clone(),
        on_off(c.streams.autoplay_next),
        format!("{} s", c.streams.timeout),
        on_off(c.subtitles.auto),
        if c.player.high_quality { "high".to_string() } else { "standard".to_string() },
        format!("{} GB", c.streams.max_size_gb),
    ];
    for (i, (label, value)) in SETTINGS.iter().zip(values).enumerate() {
        let y = inner.y + i as u16;
        let selected = i == app.settings_cursor;
        hit(app, row(inner, y), Hit::Setting(i));
        let buf = f.buffer_mut();
        if selected {
            fill(buf, row(inner, y), SURFACE0);
        }
        let marker = if selected { b("▸ ", accent) } else { s("  ", TEXT) };
        let name = if selected { b(format!("{label:<18}"), accent) } else { s(format!("{label:<18}"), SUBTEXT1) };
        line_at(buf, inner.x, y, inner.width, &Line::from(vec![marker, name, s(value, TEXT), s(if selected { "   ‹ ›" } else { "" }, PEACH)]));
    }
    let inner = pad(panel(f, notes, "About", "", false, accent), 1);
    let bin = app.mpv_bin.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not found".to_string());
    let label = |text: &str| s(format!("{text:<11}"), OVERLAY2);
    let lines = vec![
        Line::from(vec![label("theme"), s(c.ui.theme.clone(), TEXT)]),
        Line::from(vec![label("posters"), s(if app.posters.enabled() { app.posters.protocol_name() } else { "off" }, TEXT)]),
        Line::from(vec![label("mpv"), s(bin, TEXT)]),
        Line::from(vec![label("mpv args"), s(c.player.args.join(" "), TEXT)]),
        Line::from(vec![label("subtitles"), s(c.subtitles.languages.join(", "), TEXT)]),
        Line::from(vec![label("local"), s(c.local.dirs.join(", "), TEXT)]),
        Line::raw(""),
        Line::from(vec![b("e", PEACH), s(" opens config.toml to edit args, languages and folders. Those need a restart.", OVERLAY2)]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let (w, h) = (width.min(area.width), height.min(area.height));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let popup = centered(area, 80, 24);
    f.render_widget(Clear, popup);
    let inner = pad(panel(f, popup, "Keys", "any key closes", true, app.accent()), 1);
    let entry = |key: &str, what: &str| Line::from(vec![b(format!("{key:<12}"), PEACH), s(what.to_string(), SUBTEXT1)]);
    let lines = vec![
        entry("1 2 3 4 5", "Discover, Search, Library, Addons, Settings"),
        entry("0", "back to Now Playing"),
        entry("/  a", "jump to Search, Addons"),
        entry("hjkl ↑↓←→", "move"),
        entry("tab", "next panel"),
        entry("⏎", "open, or play the selected stream"),
        entry("p", "play the best stream"),
        entry("+", "add to or remove from library"),
        entry("S G Y", "cycle sort, genre, year"),
        entry("< >", "previous or next page"),
        Line::raw(""),
        Line::from(s("In Now Playing", LAVENDER)),
        entry("space", "pause"),
        entry("←/→  ⇧←/→", "seek 5 s, 60 s"),
        entry("[ ] \\", "slower, faster, reset speed"),
        entry("s a c n", "next subtitle, audio, chapter, title"),
        entry("z x  Z X", "subtitle delay, subtitle size"),
        entry("l A", "loop, autoplay next"),
        entry("f F11  t", "fullscreen (esc leaves), theater"),
        entry("q  esc", "stop playback, keep playing in background"),
        entry("mouse", "click buttons and rows; click again to open; right-click goes back"),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_play_with(f: &mut Frame, app: &App, area: Rect, cursor: usize) {
    let accent = app.accent();
    let popup = centered(area, 40, 7);
    f.render_widget(Clear, popup);
    let inner = pad(panel(f, popup, "Play with…", "", true, accent), 1);
    for (i, label) in ["in reel", "mpv window", "mpv fullscreen", "audio only", "copy stream URL"].iter().enumerate() {
        let y = inner.y + i as u16;
        hit(app, row(inner, y), Hit::PlayWith(i));
        let buf = f.buffer_mut();
        if i == cursor {
            fill(buf, row(inner, y), SURFACE0);
        }
        line_at(buf, inner.x, y, inner.width, &Line::from(if i == cursor { b(format!("▸ {label}"), accent) } else { s(format!("  {label}"), TEXT) }));
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::{Addon, CatalogDef, Client, Manifest, ResourceDef, Stream, Subtitle};
    use crate::config::{Config, State};
    use crate::mpv::{Chapter, PlayerState, Track};
    use crate::streams::to_row;
    use crate::tui::poster::Posters;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui_image::picker::Picker;
    use std::collections::VecDeque;
    use std::time::Instant;

    fn manifest(name: &str, resources: &[&str], catalogs: Vec<CatalogDef>) -> Manifest {
        Manifest {
            id: format!("test.{name}"),
            name: name.into(),
            version: "1.0.0".into(),
            description: format!("{name} test addon."),
            types: vec!["movie".into(), "series".into()],
            id_prefixes: vec!["tt".into()],
            resources: resources.iter().map(|r| ResourceDef { name: r.to_string(), types: vec![], id_prefixes: vec![] }).collect(),
            catalogs,
            configurable: name == "Torrentio",
        }
    }

    fn sample_meta(id: &str, name: &str, year: &str, genre: &str) -> Meta {
        Meta {
            id: id.into(),
            kind: "movie".into(),
            name: name.into(),
            release_info: year.into(),
            runtime: "94 min".into(),
            genres: vec![genre.into()],
            description: "An estate agent is sent to a remote castle to close a property sale.".into(),
            director: vec!["F. W. Murnau".into()],
            cast: vec!["Max Schreck".into(), "Greta Schröder".into()],
            sources: vec!["Cinemeta".into()],
            ..Default::default()
        }
    }

    fn app() -> App {
        let client = Client::new(5).unwrap();
        let catalog = CatalogDef { kind: "movie".into(), id: "top".into(), name: "Popular".into(), genres: vec!["Horror".into()], extras: vec!["search".into(), "genre".into()], required: vec![] };
        let addon = |name: &str, resources: &[&str], catalogs: Vec<CatalogDef>| Addon {
            url: format!("https://{name}.test/manifest.json"),
            enabled: true,
            manifest: manifest(name, resources, catalogs),
            latency: Some(142),
            error: None,
        };
        let addons = vec![addon("Cinemeta", &["meta"], vec![catalog]), addon("Torrentio", &["stream"], vec![]), addon("OpenSubtitles v3", &["subtitles"], vec![])];
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let posters = Posters::new(Picker::halfblocks(), "blocks");
        let mut app = App::new(Config::default(), State::default(), client, addons, Vec::new(), posters, tx);
        app.discover.items = (0..12).map(|i| sample_meta(&format!("tt{i:07}"), &format!("Movie Number {i}"), "1922", "Horror")).collect();
        app.discover.has_more = true;
        app.discover.ms = Some(142);
        app
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..h {
            let mut skip = 0;
            for x in 0..w {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer[(x, y)].symbol();
                skip = symbol.width().saturating_sub(1);
                out.push_str(symbol);
            }
            out.push('\n');
        }
        out
    }

    fn details_app() -> App {
        let mut app = app();
        let meta = sample_meta("tt0013442", "Nosferatu", "1922", "Horror");
        let stream = Stream { info_hash: Some("abc".into()), name: "Torrentio\n1080p".into(), title: "Nosferatu.1922.1080p.BluRay.x264.mkv\n👤 42 💾 2.1 GB ⚙️ YTS".into(), ..Default::default() };
        let other = Stream { info_hash: Some("def".into()), name: "Torrentio\n720p".into(), title: "Nosferatu.1922.720p.WEB.x265.mp4\n👤 0 💾 980 MB ⚙️ TPB".into(), ..Default::default() };
        let rows = vec![to_row(stream, "Torrentio", 1), to_row(other, "Torrentio", 1)];
        app.stream_cache.insert(meta.id.clone(), StreamSet { rows, pending: 0, total: 1, ms: 98, errors: Vec::new() });
        app.sub_cache.insert(meta.id.clone(), vec![Subtitle { url: "u".into(), lang: "eng".into(), addon: "OpenSubtitles v3".into() }, Subtitle { url: "v".into(), lang: "ger".into(), addon: "OpenSubtitles v3".into() }]);
        app.details = Some(Details {
            meta,
            back: Screen::Discover,
            focus: DetailFocus::Streams,
            season: 1,
            episode_cursor: 0,
            video_id: "tt0013442".into(),
            stream_cursor: 0,
            cursor_touched: false,
            sub_choice: Some("en".into()),
            sub_touched: false,
        });
        app.screen = Screen::Details;
        app
    }

    #[tokio::test]
    async fn discover_shows_design_elements() {
        let app = app();
        let frame = render(&app, 152, 38);
        for expected in ["▶ reel", "1 Discover", "4 Addons", "● mpv", "3 addons", "Catalogs", "Popular · Movies", "Sort: Popular ▾", "Genre: All ▾", "page 1/", "▶ Movie Number 0", "1922 · Horror", "NOR", "catalog 142 ms", "hjkl move", "q quit", "12+ titles"] {
            assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
        }
    }

    #[tokio::test]
    async fn details_shows_streams_and_subtitles() {
        let app = details_app();
        let frame = render(&app, 152, 38);
        for expected in ["N O S F E R A T U", "1h 34m", "Director", "F. W. Murnau", "Streams", "QUALITY", "1080p", "720p", "mkv · h264", "2.1 GB", "Torrentio", "magnet:?xt=urn:btih:abc", "▶ Play best · 1080p", "✓ EN", "DE", "off", "OpenSubtitles v3", "stream 1/2", "play in mpv"] {
            assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
        }
    }

    #[tokio::test]
    async fn search_shows_highlighted_results_and_legend() {
        let mut app = app();
        app.screen = Screen::Search;
        app.search.query = "sherlock".into();
        app.search.results = vec![sample_meta("tt0015324", "Sherlock Jr.", "1924", "Comedy"), sample_meta("tt0000001", "Sherlock Holmes", "1916", "Crime")];
        app.search.ms = Some(210);
        let frame = render(&app, 152, 38);
        for expected in ["/ sherlock", "ctrl-u clear", " all ", " movie ", " series ", "Results", "Movies", "Sherlock Jr.", "1924", "Preview", "SHERLOCK JR.", "INS", "\"sherlock\"", "C Cinemeta", "Open details"] {
            assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
        }
    }

    #[tokio::test]
    async fn addons_shows_table_and_manifest() {
        let mut app = app();
        app.screen = Screen::Addons;
        app.addons_view.cursor = 1;
        let frame = render(&app, 152, 38);
        for expected in ["Install addon", "manifest URL", "⏎ Install", "Installed", "3 of 3 on", "[x]", "Torrentio", "stream", "subtitles", "142ms", "Manifest", "idPrefixes", "c Configure", "d Remove", "Order is priority", "addons.toml"] {
            assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
        }
    }

    #[tokio::test]
    async fn playing_shows_transport_tracks_and_ipc() {
        let mut app = app();
        app.screen = Screen::Playing;
        let state = PlayerState {
            position: 2472.0,
            duration: 5646.0,
            cache_seconds: 38.0,
            codec: "h264 (H.264)".into(),
            width: 1920,
            height: 1080,
            fps: 24.0,
            hwdec: "d3d11va".into(),
            chapters: vec![Chapter { title: "Act I".into(), time: 0.0 }, Chapter { title: "Act II".into(), time: 2000.0 }],
            chapter: 1,
            tracks: vec![
                Track { id: 1, kind: "video".into(), codec: "h264".into(), height: 1080, selected: true, ..Default::default() },
                Track { id: 1, kind: "sub".into(), lang: "en".into(), codec: "subrip".into(), external: true, selected: true, ..Default::default() },
            ],
            ..Default::default()
        };
        let log = VecDeque::from([("21:04:11".to_string(), "file-loaded".to_string(), String::new())]);
        app.playing = Some(Playing {
            session: 1,
            meta: sample_meta("tt0013442", "Nosferatu", "1922", "Horror"),
            video_id: "tt0013442".into(),
            title: "Nosferatu (1922)".into(),
            quality: "1080p".into(),
            release: "Restored".into(),
            addon: "Torrentio".into(),
            hash: None,
            phase: Phase::Playing,
            state,
            control: None,
            log,
            torrent: None,
            last_saved: Instant::now(),
            eof: false,
            up_next: vec![UpNext { meta: Meta::default(), video_id: String::new(), label: "Metropolis".into(), detail: "1927".into() }],
            fullscreen: false,
            embedded: false,
            theater: false,
            video_fullscreen: false,
            mpv_fullscreen: None,
        });
        let frame = render(&app, 152, 38);
        assert!(app.video_cells.get().is_none());
        for expected in ["0 Playing", "mpv playing", "Now playing", "▶ PLAYING", "Nosferatu (1922)", "00:41:12", "01:34:06", "Act II", "h264 · 1920×1080 · 24 fps", "hwdec d3d11va", "cache 38 s ahead", "Tracks", "▸ 1  EN", "Playback", "1.00×", "Up next", "Metropolis", "autoplay next", "mpv · IPC", "file-loaded", "PLAY", "space pause", "q stop"] {
            assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
        }
    }

    #[tokio::test]
    async fn embedded_video_claims_cells_only_on_now_playing() {
        let mut app = app();
        app.screen = Screen::Playing;
        app.playing = Some(Playing {
            session: 1,
            meta: sample_meta("tt0013442", "Nosferatu", "1922", "Horror"),
            video_id: "tt0013442".into(),
            title: "Nosferatu (1922)".into(),
            quality: "1080p".into(),
            release: "Restored".into(),
            addon: "Torrentio".into(),
            hash: None,
            phase: Phase::Playing,
            state: PlayerState::default(),
            control: None,
            log: VecDeque::new(),
            torrent: None,
            last_saved: Instant::now(),
            eof: false,
            up_next: Vec::new(),
            fullscreen: false,
            embedded: true,
            theater: false,
            video_fullscreen: false,
            mpv_fullscreen: None,
        });
        for (w, h) in [(80, 24), (100, 30), (152, 38), (237, 62)] {
            let frame = render(&app, w, h);
            let cells = app.video_cells.get().expect("video has cells");
            assert!(frame.contains("Video"), "{frame}");
            assert!(cells.right() <= w && cells.bottom() <= h);
        }
        let frame = render(&app, 152, 38);
        for expected in ["Tracks", "Playback", "Up next", "mpv · IPC", "in reel", "t theater", "Fullscreen"] {
            assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
        }
        let normal = app.video_cells.get().unwrap();
        for expected in ["Pause", "Theater", "Fullscreen", "Stop", "Vol", "Next", "Subs"] {
            assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
        }
        assert!(app.hits.borrow().iter().any(|(_, h)| *h == Hit::Seek));
        assert!(app.hits.borrow().iter().any(|(_, h)| *h == Hit::Volume));

        // With real tracks, Tracks still reaches its Subtitles group beside the video.
        app.playing.as_mut().unwrap().state.tracks = vec![
            Track { id: 1, kind: "video".into(), codec: "h264".into(), selected: true, ..Default::default() },
            Track { id: 1, kind: "audio".into(), codec: "aac".into(), selected: true, ..Default::default() },
            Track { id: 1, kind: "sub".into(), lang: "en".into(), codec: "subrip".into(), external: true, selected: true, ..Default::default() },
        ];
        let frame = render(&app, 152, 40);
        for expected in ["Subtitles", "▸ 1  EN", "off"] {
            assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
        }

        app.playing.as_mut().unwrap().theater = true;
        render(&app, 152, 38);
        assert!(app.video_cells.get().unwrap().height > normal.height);

        app.overlay = Overlay::Help;
        render(&app, 152, 38);
        assert!(app.video_cells.get().is_none(), "popups must hide the video");
        app.overlay = Overlay::None;

        app.screen = Screen::Discover;
        render(&app, 152, 38);
        assert!(app.video_cells.get().is_none(), "other screens must hide the video");
    }

    fn click(app: &mut App, column: u16, row: u16) {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: crossterm::event::KeyModifiers::NONE });
    }

    fn center_of(app: &App, wanted: &Hit) -> (u16, u16) {
        let hits = app.hits.borrow();
        let (area, _) = hits.iter().find(|(_, h)| h == wanted).unwrap_or_else(|| panic!("no hit for {wanted:?}"));
        (area.x + area.width / 2, area.y + area.height / 2)
    }

    #[tokio::test]
    async fn clicking_tabs_changes_screen() {
        let mut app = app();
        render(&app, 152, 38);
        let (x, y) = center_of(&app, &Hit::Go(Screen::Addons));
        click(&mut app, x, y);
        assert_eq!(app.screen, Screen::Addons);
    }

    #[tokio::test]
    async fn clicking_a_card_selects_then_opens_it() {
        let mut app = app();
        render(&app, 152, 38);
        let (x, y) = center_of(&app, &Hit::Grid(2));
        click(&mut app, x, y);
        assert_eq!(app.discover.cursor, 2);
        assert_eq!(app.screen, Screen::Discover);
        click(&mut app, x, y);
        assert_eq!(app.screen, Screen::Details);
        assert_eq!(app.details.as_ref().unwrap().meta.name, "Movie Number 2");
    }

    #[tokio::test]
    async fn clicking_stream_rows_and_subtitles_works() {
        let mut app = details_app();
        render(&app, 152, 38);
        let (x, y) = center_of(&app, &Hit::Stream(1));
        click(&mut app, x, y);
        assert_eq!(app.details.as_ref().unwrap().stream_cursor, 1);
        let (x, y) = center_of(&app, &Hit::Subtitle(Some("de".into())));
        click(&mut app, x, y);
        assert_eq!(app.details.as_ref().unwrap().sub_choice.as_deref(), Some("de"));
        let (x, y) = center_of(&app, &Hit::Subtitle(None));
        click(&mut app, x, y);
        assert_eq!(app.details.as_ref().unwrap().sub_choice, None);
    }

    #[tokio::test]
    async fn popups_block_clicks_underneath() {
        let mut app = app();
        render(&app, 152, 38);
        let (x, y) = center_of(&app, &Hit::Go(Screen::Addons));
        app.overlay = Overlay::Help;
        render(&app, 152, 38);
        click(&mut app, x, y);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.screen, Screen::Discover);
    }

    #[tokio::test]
    async fn failed_addons_are_reported_instead_of_no_streams() {
        let mut app = details_app();
        app.stream_cache.insert("tt0013442".into(), StreamSet { total: 1, errors: vec!["Torrentio".into()], ..Default::default() });
        let frame = render(&app, 152, 38);
        assert!(frame.contains("Torrentio did not answer"), "{frame}");
        assert!(!frame.contains("No streams found"), "{frame}");
    }

    #[tokio::test]
    async fn every_screen_survives_small_and_odd_sizes() {
        let mut app = details_app();
        let screens = [Screen::Discover, Screen::Search, Screen::Library, Screen::Addons, Screen::Settings, Screen::Details];
        for (w, h) in [(60, 16), (80, 24), (100, 30), (152, 38), (237, 62), (40, 10)] {
            for screen in screens {
                app.screen = screen;
                render(&app, w, h);
            }
        }
    }

    /// Writes every screen to target/frames.txt so layouts can be reviewed by eye.
    #[tokio::test]
    async fn dump_frames_for_review() {
        let mut app = details_app();
        let mut out = String::new();
        for screen in [Screen::Discover, Screen::Details, Screen::Search, Screen::Addons, Screen::Settings, Screen::Library] {
            app.screen = screen;
            if screen == Screen::Search {
                app.search.query = "movie".into();
                app.search.results = app.discover.items.clone();
            }
            out.push_str(&format!("=== {screen:?} ===
{}
", render(&app, 152, 38)));
        }
        std::fs::write("target/frames.txt", out).unwrap();
    }

    #[tokio::test]
    async fn wide_titles_do_not_break_layout() {
        let mut app = details_app();
        app.discover.items[0].name = "七人の侍 — 非常に長いタイトルのテスト".into();
        app.screen = Screen::Discover;
        let frame = render(&app, 120, 30);
        assert!(frame.contains("…"));
    }
}
