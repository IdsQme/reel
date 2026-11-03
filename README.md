# reel

A Catppuccin Mocha streaming TUI for [mpv](https://mpv.io). It browses and streams from
[Stremio-compatible addons](https://github.com/Stremio/stremio-addon-sdk), including
[Torrentio](https://torrentio.strem.fun/configure). Posters are the real movie artwork, drawn
in the terminal with sixel, kitty graphics or half blocks.

Run `reel` with no arguments to open the TUI.

## Build

Needs Rust and mpv. On Windows, `build.ps1` builds without Visual Studio, using the `stable-x86_64-pc-windows-gnullvm` toolchain and MSYS2's clang64:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnullvm --profile minimal
.\build.ps1
```

The result is `target\release\reel.exe`, a single file with no extra DLLs. reel finds mpv on `PATH`,
in `C:\Program Files\MPV Player`, in the other common install folders, or from `[player] bin` in the config.

Use Windows Terminal 1.22 or newer, or another terminal with sixel or kitty graphics, for full-resolution posters.
Other terminals get half-block posters.

## Addons

Cinemeta, Torrentio, OpenSubtitles v3 and the built-in Local Files are installed on first run.

To change Torrentio's providers, sorting or debrid service: open **Addons** (`4`), select Torrentio and press `c`.
That opens its configure page. Copy the resulting install link, then press `i` in reel and paste it.
reel accepts `stremio://` links, manifest URLs and `/configure` URLs.

```bash
reel addon install "stremio://torrentio.strem.fun/sort=qualitysize/manifest.json"
```

When the configured addon replaces the default, remove the old entry with `d`.

Torrentio without a debrid service returns torrents. reel streams those with its built-in BitTorrent engine, which
**uploads to other peers and exposes your IP address to the swarm while playing**, like any torrent client. Downloaded data
is deleted when playback stops or the terminal window is closed, or at the next launch after a crash. Once mpv has a healthy buffer, reel limits the download to about twice the film's bitrate, so it doesn't fetch far ahead of you. With a debrid service configured, streams are direct HTTPS and no torrent traffic happens.
You are responsible for what you stream and whether it is legal where you live.

## CLI

```bash
reel addon install https://v3-cinemeta.strem.io/manifest.json
reel addon list
reel search metropolis --type movie
reel streams tt0017136
reel play tt0017136 --quality 1080p --sub en
reel play tt0944947:1:2
reel play ~/Films/Detour.1945.mkv
reel play tt0017136 --output tct
```

In the TUI, video plays inside reel's Now Playing screen, `t` switches to theater mode and `f` or `F11` to fullscreen (`esc` leaves). This uses mpv's `--wid` with a child of the terminal window, so it needs Windows Terminal or the classic Windows console. Elsewhere, or with `output = "window"` in the config, mpv opens its own window. `P` on a stream picks for one play.

The mouse works everywhere: click tabs, buttons and rows (a second click opens what is selected), scroll lists, and right-click to go back. Now Playing has a control bar with a seek bar and volume you can click or drag. On the video itself, click to pause and double-click for fullscreen. In fullscreen, use the keyboard to seek. Hold `Shift` while dragging to select text in the terminal.

`--output` takes `window`, `fullscreen` or an mpv terminal video output such as `tct`, `kitty` or `sixel`.

## Keys

Press `?` in the app for the full list.

| Keys | Action |
| --- | --- |
| `1`–`5`, `0` | Discover, Search, Library, Addons, Settings, Now Playing |
| `hjkl` or arrows | Move |
| `tab` | Next panel |
| `⏎` | Open, or play the selected stream |
| `p` | Play the best stream |
| `+` | Add to or remove from library |
| `S` `G` `Y` | Cycle sort, genre, year |
| `q` | Quit, or stop on Now Playing |

## Picture quality

reel picks streams for you with `p`: it skips dead torrents and camrips (shown as a red `CAM`), then picks the largest file with a healthy swarm, up to `[streams] max_size_gb` (25 by default). Bigger files look better but need more bandwidth. mpv runs with its `high-quality` profile, which you can turn off under Settings as "Video quality".

## Configuration

`~/.config/reel/config.toml` holds settings and `addons.toml` holds addons, where order is priority.
Resume positions, library and recent searches live in `state.json`. Posters are cached in `~/.cache/reel`.

reel honours [`NO_COLOR`](https://no-color.org): with it set, the UI has no colours, though posters still do.

`REEL_KEYS` is a test hook that types keys into the TUI: `~` waits two seconds and `^` presses Enter.
