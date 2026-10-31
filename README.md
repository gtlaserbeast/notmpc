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

Defaults, as in rmpc:

| key                     | action                                    |
|-------------------------|-------------------------------------------|
| 1 / 2 / 3, Tab / S-Tab  | Queue / Directories / Search, cycle tabs  |
| p                       | play / pause                              |
| s                       | stop                                      |
| > / <                   | next / previous                           |
| f / b                   | seek ±5s                                  |
| . / ,                   | volume ±5                                 |
| z x c v                 | repeat / random / consume / single        |
| u / U                   | update / rescan database                  |
| q                       | quit                                      |
| j / k, ↓ / ↑            | move                                      |
| C-d / C-u, PgDn / PgUp  | half page                                 |
| g / G                   | top / bottom                              |
| Enter                   | play                                      |
| Space / C-Space         | mark / invert marks                       |
| d                       | delete (marked songs, else the cursor)    |
| D                       | clear the queue (asks first)              |
| J / K                   | move song down / up                       |
| C                       | jump to current song                      |
| X                       | shuffle                                   |
| Esc                     | clear marks                               |

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

1. Mouse
2. Directories tab, plus `notmpc add` / `notmpc insert` for yazi
3. Spectrum visualizer behind the header, from MPD's FIFO output
4. Search tab
5. Waveform seek bar
