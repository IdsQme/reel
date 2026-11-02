mod addon;
mod cli;
mod config;
mod embed;
mod local;
mod mpv;
mod streams;
mod theme;
mod torrent;
mod tui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "reel", version, about = "Stream with mpv from Stremio addons. No arguments opens the TUI.")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Manage Stremio-compatible addons
    Addon {
        #[command(subcommand)]
        action: AddonAction,
    },
    /// Search all addon catalogs
    Search {
        query: Vec<String>,
        #[arg(long = "type", value_name = "movie|series")]
        kind: Option<String>,
    },
    /// List streams for an id such as tt0017136 or tt0944947:1:2
    Streams {
        id: String,
        #[arg(long = "type", value_name = "movie|series")]
        kind: Option<String>,
    },
    /// Play an id, or any path or URL mpv opens
    Play {
        target: String,
        #[arg(long)]
        quality: Option<String>,
        #[arg(long)]
        sub: Option<String>,
        #[arg(long = "type", value_name = "movie|series")]
        kind: Option<String>,
        /// window, fullscreen, or an mpv terminal output: tct, kitty, sixel
        #[arg(long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum AddonAction {
    /// Install from a manifest URL, stremio:// link or /configure page
    Install { url: String },
    /// List installed addons
    List,
    /// Remove by number or name
    Remove { addon: String },
    /// Turn an addon on
    Enable { addon: String },
    /// Turn an addon off
    Disable { addon: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let result = match args.command {
        None => tui::run().await,
        Some(Command::Addon { action }) => match action {
            AddonAction::Install { url } => cli::addon_install(&url).await,
            AddonAction::List => cli::addon_list().await,
            AddonAction::Remove { addon } => cli::addon_remove(&addon).await,
            AddonAction::Enable { addon } => cli::addon_set_enabled(&addon, true).await,
            AddonAction::Disable { addon } => cli::addon_set_enabled(&addon, false).await,
        },
        Some(Command::Search { query, kind }) => cli::search(&query.join(" "), kind.as_deref()).await,
        Some(Command::Streams { id, kind }) => cli::streams(&id, kind.as_deref()).await,
        Some(Command::Play { target, quality, sub, kind, output }) => cli::play(&target, quality.as_deref(), sub.as_deref(), kind.as_deref(), output.as_deref()).await,
    };
    // Library threads (DHT, trackers) do not need to wind down cleanly.
    if let Err(error) = &result {
        eprintln!("\x1b[38;2;243;139;168mreel: {error:#}\x1b[0m");
        std::process::exit(1);
    }
    std::process::exit(0);
}
