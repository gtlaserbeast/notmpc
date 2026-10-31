# notmpc

A terminal MPD client drawn with [notcurses](https://notcurses.com).

## Building

Needs notcurses ≥ 3.0.8 with headers, pkg-config, and clang (for bindgen).

    cargo build --release

## Connecting

Follows mpc: `MPD_HOST` (`[password@]host` or a socket path) and `MPD_PORT`.
Without them it tries `$XDG_RUNTIME_DIR/mpd/socket`, then `localhost:6600`.

## Keys

Everywhere:

| key            | action                                    |
|----------------|-------------------------------------------|
| 1 / 2 / Tab    | now playing / queue / switch              |
| space / p      | play / pause                              |
| s              | stop                                      |
| n / N (> / <)  | next / previous                           |
| ← / →          | seek ±5s                                  |
| + / -          | volume ±5                                 |
| r z y c        | toggle repeat / random / single / consume |
| q / Esc        | quit                                      |

Queue:

| key               | action                |
|-------------------|-----------------------|
| j / k (↓ / ↑)     | move                  |
| PgDn / PgUp       | page                  |
| g / G (Home/End)  | top / bottom          |
| Enter             | play selected         |
| d / Del           | remove selected       |
| .                 | jump to current song  |

## Layout

- `build.rs` generates bindings from the system notcurses headers. The
  `libnotcurses-sys` crate wants notcurses ≥ 3.0.11, newer than Gentoo ships.
  `wrap_static_fns` turns notcurses' `static inline` API into linkable shims.
- `src/nc/` is a thin safe wrapper over the calls we use; `nc::sys` is raw.
- `src/mpd.rs` is a small blocking protocol client. A second connection
  sits in `idle` on its own thread and tells the UI loop which subsystems
  changed.
- `src/app.rs` holds state, keys, and the views; `src/art.rs` fetches covers
  (`readpicture`, then `albumart`), decodes them with notcurses' ffmpeg
  backend, and blits them onto their own planes — pixel graphics when the
  terminal supports them, half-blocks otherwise.

## Roadmap

1. Library browser
2. Spectrum visualizer from MPD's FIFO output
