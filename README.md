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

## YouTube

The Catalogs panel has a YouTube section. Music, Gaming, News, Science, Tech and search need no account. They are topic searches, because YouTube's public pages list nothing without one. Signing in is optional and adds Home, Subscriptions, Watch later, Liked and History. Videos play in reel through mpv and `yt-dlp`, which fetch the video stream directly, so YouTube's player ads aren't part of it. Sponsor segments inside a video are not removed, unless the video's chapters mark them, in which case a Skip button appears.

It needs `yt-dlp` and Deno (`winget install yt-dlp.yt-dlp DenoLand.Deno`), and Firefox if you want to sign in. Press `L` on a YouTube catalog, or run `reel youtube login`: Google's own sign-in page opens in a separate Firefox profile. Sign in there and close the window. reel never sees your password; `yt-dlp` reads the session from that profile, stored in `~/.config/reel/youtube-profile`. Anyone with access to that folder has your YouTube session. Sign out under Settings or with `reel youtube logout`, which deletes the folder. Using these tools may be against YouTube's terms of service, so your account is at your own risk.

## Watch together

Press `6` for rooms: synced playback with friends over an end-to-end encrypted peer-to-peer connection. There is no password.

Every reel has a private key, kept in `~/.config/reel/identity.key`, and a public reel ID (`reel id`, or `y` on the Room screen). To invite someone, get their reel ID, press `n` then `i`, and paste it. Send them the invite that creates. They press `i` and paste it, and you both compare the four safety words before you approve them.

- **A copied or guessed invite is useless.** It names one reel ID, and joining means proving you hold that ID's private key during the TLS 1.3 handshake. Anyone else is refused before receiving anything.
- **Invites** work once, expire after 15 minutes, are signed by the host, and die with the room.
- **Safety words** are derived from the session keys. If they match on a call, nobody swapped an ID or invite on the way.
- **Peers aren't trusted.** Messages are size-limited and strictly validated, and can only name catalog content, never a URL, file or command. Local files are never shared. Guests are asked before each title starts, and find it with their own addons, so a host can't push a torrent or link onto them.
- **No background network.** Nothing connects until you open or join a room.

People in a room can see each other's IP address. Connections use iroh (QUIC). Its public relay and discovery servers help peers find each other and can relay traffic, which they can't read, but they can see that two IDs are connecting. Protect `identity.key`: anyone with that file can act as your reel ID. If it leaks, delete it and reel makes a new ID.

## Picture quality

reel picks streams for you with `p`: it skips dead torrents and camrips (shown as a red `CAM`), then picks the largest file with a healthy swarm, up to `[streams] max_size_gb` (25 by default). Bigger files look better but need more bandwidth. mpv runs with its `high-quality` profile, which you can turn off under Settings as "Video quality".

## Configuration

`~/.config/reel/config.toml` holds settings and `addons.toml` holds addons, where order is priority.
Resume positions, library and recent searches live in `state.json`. Posters are cached in `~/.cache/reel`.

reel honours [`NO_COLOR`](https://no-color.org): with it set, the UI has no colours, though posters still do.

`REEL_KEYS` is a test hook that types keys into the TUI: `~` waits two seconds and `^` presses Enter.
