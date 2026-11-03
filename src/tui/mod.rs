pub mod app;
mod draw;
mod poster;

use crate::addon::{self, Client};
use crate::config::{self, Config, State};
use anyhow::Result;
use app::{App, Ev};
use crossterm::event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use poster::Posters;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_image::picker::Picker;
use std::io::stdout;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const MIN_FRAME_TIME: Duration = Duration::from_millis(33);

fn restore() {
    crate::embed::hide_console_stub();
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen, crossterm::cursor::Show);
}

/// Test hook: REEL_KEYS types keys into the app, for driving it without a person.
/// `~` waits two seconds and `^` presses Enter.
fn spawn_scripted_keys(script: String, tx: mpsc::UnboundedSender<Ev>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        for c in script.chars() {
            let code = match c {
                '~' => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                '^' => KeyCode::Enter,
                other => KeyCode::Char(other),
            };
            let _ = tx.send(Ev::Key(KeyEvent::new(code, KeyModifiers::NONE)));
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    });
}

pub async fn run() -> Result<()> {
    let config = Config::load()?;
    let state = State::load();
    let entries = config::load_addons()?;
    let client = Client::new(config.streams.timeout)?;
    let addons = addon::addons_from_cache(&entries);

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    // The picker asks the terminal what it can draw, so this happens before input starts.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let posters = Posters::new(picker, &config.ui.posters);
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            let forwarded = match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => Ev::Key(key),
                Event::Paste(text) => Ev::Paste(text),
                Event::Resize(..) => Ev::Resize,
                // Plain movement and releases would only cause pointless redraws.
                Event::Mouse(mouse) if !matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Up(_)) => Ev::Mouse(mouse),
                _ => continue,
            };
            if input_tx.send(forwarded).is_err() {
                break;
            }
        }
    });
    let tick_tx = tx.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if tick_tx.send(Ev::Tick).is_err() {
                break;
            }
        }
    });

    if let Ok(script) = std::env::var("REEL_KEYS") {
        spawn_scripted_keys(script, tx.clone());
    }

    let mut app = App::new(config, state, client, addons, entries, posters, tx);
    app.start();

    let result: Result<()> = async {
        terminal.draw(|f| draw::draw(f, &app))?;
        let mut last_frame = Instant::now();
        while let Some(event) = rx.recv().await {
            app.handle(event);
            // Coalesce bursts (posters arriving, key repeat) into one frame.
            while let Ok(event) = rx.try_recv() {
                app.handle(event);
            }
            if app.should_quit {
                break;
            }
            if !app.dirty && !app.needs_clear {
                continue;
            }
            let since_last = last_frame.elapsed();
            if since_last < MIN_FRAME_TIME {
                tokio::time::sleep(MIN_FRAME_TIME - since_last).await;
                while let Ok(event) = rx.try_recv() {
                    app.handle(event);
                }
                if app.should_quit {
                    break;
                }
            }
            if std::mem::take(&mut app.needs_clear) {
                terminal.clear()?;
            }
            app.dirty = false;
            terminal.draw(|f| draw::draw(f, &app))?;
            last_frame = Instant::now();
            app.after_draw();
        }
        Ok(())
    }
    .await;

    app.shutdown().await;
    restore();
    result
}
