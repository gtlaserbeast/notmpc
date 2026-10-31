# notmpc

A terminal MPD client drawn with [notcurses](https://notcurses.com).

## Building

Needs notcurses ≥ 3.0.8 with headers, pkg-config, and clang (for bindgen).

    cargo build --release

## Connecting

Follows mpc: `MPD_HOST` (`[password@]host` or a socket path) and `MPD_PORT`.
Without them it tries `$XDG_RUNTIME_DIR/mpd/socket`, then `localhost:6600`.

## Directories

MPD's database as rmpc-style columns: parent, current folder, preview. The
preview shows the folder's cover (pixel graphics in kitty), album, year,
track count, length and format, and its contents; for a song, its tags.
Folders first, natural order (`Disc 2` before `Disc 10`); songs by disc and
track. The playing song and the folders containing it are highlighted.

| key            | action                                              |
|----------------|-----------------------------------------------------|
| j / k, gg / G  | move                                                |
| l / Enter      | open folder (Enter on a song: queue it and play)    |
| h              | up                                                  |
| a / A          | add selection / everything here to the end          |
| t              | play next (insert after the current song)           |
| Space          | mark (a / t then act on the marks)                  |
| C              | open the playing song's folder                      |

Mouse: click to select, double-click to open / play, click the left column
to go up, click an entry in the preview to open its folder. Adding starts
playback on the first new song if nothing is playing.

## Queueing from a file manager

    notmpc add PATH...      # append files / directory trees
    notmpc insert PATH...   # place them right after the current song

Directories are walked in natural order (`Disc 2` before `Disc 10`).
Files under MPD's music directory (symlinks resolved) are added by
database URI, others as `file://`. If nothing is playing, the first new
song starts. For yazi:

    [[mgr.prepend_keymap]]
    on  = [ "m", "a" ]
    run = "shell 'notmpc add %s' --orphan"

If playback-follows-cursor has moved the playing song when an insert
arrives, the insert wins: the new songs play next.

## Configuration

`~/.config/notmpc/config.toml`, all optional; see `config.example.toml`.
The defaults reproduce an rmpc setup: rmpc keybindings (rmpc names and
notation, so snippets carry over) and the gtlaserbeast theme.

## Keys

rmpc 0.11's defaults (`rmpc config`), including multi-key sequences; Search
is on 3 since the Artists/Albums/Playlists tabs don't exist. notmpc adds `t`
(play next), `F` (follow the highlight), `H`/`L` (collapse/expand all albums), `h`/`l` on the queue
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
| t                            | play next (moves the highlight here)       |
| d / D                        | delete / clear queue (asks first)          |
| J / K                        | move song down / up                        |
| C                            | jump to current song                       |
| X                            | shuffle                                    |
| Esc                          | clear marks                                |

**Playback follows the highlight** (foobar2000-style; `F` toggles). The
queue has two cursors:

- the *highlight* (orange bar) sits on the playing song and moves along
  with playback;
- the *browse cursor* (dotted `┆` rails, faint tint) is what j/k, gg/G, etc.
  move. Enter, d, Space, J/K, h/l act on it. It never affects playback.

`t` (PlayNext) moves the highlight to the browse cursor: that song plays
next, without cutting the current one short, and playback carries on from
there. Clicking a row does the same; double-click plays it now. `»` marks
the next song, the footer names it. Under the hood the playing song is
moved to just before the highlighted one (so MPD's transition stays
gapless) and back when the next song starts; the display always shows the
real order. Off in random or single mode. With following off, `t` moves
the song to play next instead.

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

1. Spectrum visualizer behind the header, from MPD's FIFO output
2. Search tab
3. Waveform seek bar
