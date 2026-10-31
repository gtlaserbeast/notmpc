# notmpc

A terminal MPD client drawn with [notcurses](https://notcurses.com).

## Building

Needs notcurses ≥ 3.0.8 with headers, pkg-config, and clang (for bindgen).

    cargo build --release

## Connecting

Follows mpc: `MPD_HOST` (`[password@]host` or a socket path) and `MPD_PORT`.
Without them it tries `$XDG_RUNTIME_DIR/mpd/socket`, then `localhost:6600`.

## Configuration

`~/.config/notmpc/config.toml`, all optional; see `config.example.toml`.
The defaults reproduce an rmpc setup: rmpc keybindings (rmpc names and
notation, so snippets carry over) and the gtlaserbeast theme.

## Keys

rmpc 0.11's defaults (`rmpc config`), including multi-key sequences; Search
is on 3 since the Artists/Albums/Playlists tabs don't exist. notmpc adds `F`
(follow cursor), `H`/`L` (collapse/expand all albums), `h`/`l` on the queue
(collapse/expand the album under the cursor), Home/End, and Del.

| key                          | action                                     |
|------------------------------|--------------------------------------------|
| 1 / 2 / 3, Tab / S-Tab, gt/gT | Queue / Directories / Search, cycle tabs  |
| p / s                        | play-pause / stop                          |
| > / <                        | next / previous                            |
| f / b                        | seek ±5s                                   |
| . / ,                        | volume ±5                                  |
| z x c v                      | repeat / random / consume / single         |
| F                            | playback follows cursor (see below)        |
| C-u / C-U                    | update / rescan (C-u is half-page in lists) |
| ?, oI, oo, op                | help, song info, outputs, decoders (TODO)  |
| q                            | quit                                       |
| j / k, ↓ / ↑                 | move                                       |
| C-d / C-u, C-f / C-b         | half page / page                           |
| gg / G                       | top / bottom                               |
| h / l                        | collapse / expand album                    |
| H / L                        | collapse / expand all albums               |
| Enter                        | play (an album header plays the album)     |
| Space / C-Space              | mark / invert marks                        |
| d / D                        | delete / clear queue (asks first)          |
| J / K                        | move song down / up                        |
| C                            | jump to current song                       |
| X                            | shuffle                                    |
| Esc                          | clear marks                                |

**Playback follows cursor** (foobar2000-style, on by default): the song under
the queue cursor plays next, without cutting the current one short, and
playback carries on from there. When a song starts, the cursor moves to the
one after it, so leaving the cursor alone plays the queue in order. The `»`
marker and the footer show what's next. Under the hood the playing song is
moved to just before the chosen one (so MPD's transition stays gapless) and
moved back when the next song starts; the display always shows the real
order. Off in random or single mode.

Mouse: click a tab, a queue row (double-click plays; on an album header,
plays the album), an album's ▾/▸ arrow (collapses / expands it), a mode word
in the header (toggles it), or the progress bar (seeks there); the wheel
scrolls the queue, or changes volume over the volume ramp.

Untagged files show artist, album, year, and title guessed from the path,
in italics.

## Layout

- `build.rs` generates bindings from the system notcurses headers. The
  `libnotcurses-sys` crate wants notcurses ≥ 3.0.11, newer than Gentoo ships.
  `wrap_static_fns` turns notcurses' `static inline` API into linkable shims.
- `src/nc/` is a thin safe wrapper over the calls we use; `nc::sys` is raw.
- `src/mpd.rs` is a small blocking protocol client. A second connection
  sits in `idle` on its own thread and tells the UI loop which subsystems
  changed.
- `src/config.rs` / `src/keys.rs`: config file, theme, rmpc-style keymap.
- `src/meta.rs`: display metadata, inferred from the path for untagged files.
- `src/app.rs` holds state and actions, `src/ui.rs` draws; `src/art.rs` fetches covers
  (`readpicture`, then `albumart`), decodes them with notcurses' ffmpeg
  backend, and blits them onto their own planes — pixel graphics when the
  terminal supports them, half-blocks otherwise.

## Roadmap

1. Directories tab, plus `notmpc add` / `notmpc insert` for yazi
2. Spectrum visualizer behind the header, from MPD's FIFO output
3. Search tab
4. Waveform seek bar
