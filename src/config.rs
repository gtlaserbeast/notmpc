//! `~/.config/notmpc/config.toml`. Every setting is optional; the defaults
//! reproduce the user's rmpc setup (keymap and gtlaserbeast theme).

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::keys::Keymap;
use crate::nc::Color;

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    const fn fg(fg: Color) -> Self {
        Self { fg, bg: Color::Default, bold: false, italic: false }
    }

    const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    const fn on(mut self, bg: Color) -> Self {
        self.bg = bg;
        self
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub text: Style,
    /// Playback state in the header (`[Playing]`).
    pub state: Style,
    pub title: Style,
    pub artist: Style,
    pub volume: Style,
    pub mode_on: Style,
    pub mode_off: Style,
    pub tab_active: Style,
    pub tab_inactive: Style,
    /// The highlight: the song that's playing, or plays next.
    pub cursor: Style,
    /// The browse cursor's row tint and dotted rails.
    pub browse: Style,
    pub browse_rail: Style,
    /// The song that's playing.
    pub current: Style,
    pub album: Style,
    pub date: Style,
    pub group_header: Style,
    pub dim: Style,
    pub progress_elapsed: Style,
    pub progress_thumb: Style,
    pub progress_track: Style,
    /// start cap, elapsed, thumb, remaining, end cap
    pub progress_symbols: [String; 5],
    /// Field names in the Directories preview.
    pub label: Style,
    /// Album artist on queue album headers.
    pub album_artist: Style,
    /// Overlay panels (help, song info, ...): body and border.
    pub panel: Style,
    pub panel_border: Style,
    /// Spectrum gradient stops, bass to treble (RGB). Each bar takes the
    /// colour at its frequency, brighter the taller it is.
    pub visualizer_colors: Vec<u32>,
    pub dir_symbol: String,
    pub song_symbol: String,
}

const ORANGE: Color = Color::Rgb(0xdd6f00);
const YELLOW: Color = Color::Palette(3);
const WHITE: Color = Color::Palette(15);
const DARK_GRAY: Color = Color::Palette(8);
const BLACK: Color = Color::Palette(0);

impl Default for Theme {
    /// gtlaserbeast, as rmpc draws it.
    fn default() -> Self {
        Self {
            text: Style::fg(Color::Default),
            state: Style::fg(YELLOW).bold(),
            title: Style::fg(Color::Default).bold(),
            artist: Style::fg(YELLOW).bold(),
            volume: Style::fg(ORANGE),
            mode_on: Style::fg(WHITE).bold(),
            mode_off: Style::fg(DARK_GRAY),
            tab_active: Style::fg(BLACK).on(ORANGE).bold(),
            tab_inactive: Style::fg(Color::Default),
            cursor: Style::fg(BLACK).on(ORANGE).bold(),
            browse: Style::fg(Color::Default).on(Color::Rgb(0x2e2010)),
            browse_rail: Style::fg(Color::Rgb(0xa7ff04)).bold(),
            current: Style::fg(ORANGE).bold(),
            album: Style::fg(WHITE),
            date: Style::fg(YELLOW),
            group_header: Style::fg(ORANGE).bold(),
            dim: Style::fg(DARK_GRAY),
            progress_elapsed: Style::fg(ORANGE),
            progress_thumb: Style::fg(Color::Rgb(0xa7ff04)),
            progress_track: Style::fg(ORANGE),
            progress_symbols: ["\u{e0b7}", "─", "\u{f189c}", " ", "\u{e0b5}"].map(String::from),
            label: Style::fg(YELLOW),
            album_artist: Style::fg(Color::Palette(6)),
            panel: Style::fg(Color::Default).on(Color::Rgb(0x16130f)),
            panel_border: Style::fg(ORANGE).on(Color::Rgb(0x16130f)),
            visualizer_colors: palette("aurora").expect("built-in palette"),
            dir_symbol: "🖿".into(),
            song_symbol: "🕪".into(),
        }
    }
}

/// Settings notmpc changes itself (`~/.local/state/notmpc/state.toml`),
/// kept apart from the hand-edited config. They win over the config.
#[derive(Default, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct State {
    pub visualizer_delay_ms: Option<u64>,
    pub visualizer: Option<bool>,
}

impl State {
    fn path() -> Option<PathBuf> {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .map(|d| d.join("notmpc/state.toml"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("no HOME")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, toml::to_string(self)?)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub volume_step: u8,
    pub seek_step: u32,
    /// Share of the queue tab's width given to album art, in percent.
    pub art_width: u32,
    pub group_by_album: bool,
    /// Playback follows the queue cursor (toggle with ToggleFollow).
    pub follow_cursor: bool,
    /// How often to refresh bitrate and position while playing.
    pub status_interval: Duration,
    /// Spectrum behind the header, fed by MPD's `fifo` output.
    pub visualizer: bool,
    pub visualizer_fifo: PathBuf,
    /// Header rows it fills (the header is 2 rows, plus the progress bar).
    pub visualizer_rows: u32,
    pub visualizer_fps: u32,
    /// How far the speakers lag MPD's FIFO (PipeWire buffering). Tuned
    /// live with [ and ], which saves it to the state file.
    pub visualizer_delay: Duration,
    pub keys: Keymap,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            volume_step: 5,
            seek_step: 5,
            art_width: 45,
            group_by_album: true,
            follow_cursor: true,
            status_interval: Duration::from_millis(1000),
            visualizer: true,
            visualizer_fifo: env::var_os("XDG_RUNTIME_DIR")
                .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
                .join("mpd/visualizer.fifo"),
            visualizer_rows: 3,
            visualizer_fps: 30,
            visualizer_delay: Duration::from_millis(150),
            keys: Keymap::defaults(),
            theme: Theme::default(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    volume_step: Option<u8>,
    seek_step: Option<u32>,
    art_width: Option<u32>,
    group_by_album: Option<bool>,
    follow_cursor: Option<bool>,
    status_update_interval_ms: Option<u64>,
    visualizer: Option<bool>,
    visualizer_fifo: Option<String>,
    visualizer_rows: Option<u32>,
    visualizer_fps: Option<u32>,
    visualizer_delay_ms: Option<u64>,
    keys: HashMap<String, HashMap<String, String>>,
    theme: HashMap<String, ThemeEntry>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ThemeEntry {
    /// `accent = "#dd6f00"`
    Color(String),
    /// `cursor = { fg = "black", bg = "#dd6f00", bold = true }`
    Style {
        fg: Option<String>,
        bg: Option<String>,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        italic: bool,
    },
    /// `progress_symbols = [...]`
    Symbols([String; 5]),
    /// `visualizer_colors = ["#..", ...]`
    List(Vec<String>),
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("notmpc/config.toml"))
    }

    /// Loads the config file if there is one, then the saved state.
    pub fn load() -> Result<Self> {
        let mut cfg = Self::load_config()?;
        let state = State::load();
        if let Some(ms) = state.visualizer_delay_ms {
            cfg.visualizer_delay = Duration::from_millis(ms);
        }
        if let Some(on) = state.visualizer {
            cfg.visualizer = on;
        }
        Ok(cfg)
    }

    fn load_config() -> Result<Self> {
        let mut cfg = Self::default();
        let Some(path) = Self::path().filter(|p| p.exists()) else {
            return Ok(cfg);
        };
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let file: File = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

        if let Some(v) = file.volume_step {
            cfg.volume_step = v;
        }
        if let Some(v) = file.seek_step {
            cfg.seek_step = v;
        }
        if let Some(v) = file.art_width {
            cfg.art_width = v.min(90);
        }
        if let Some(v) = file.group_by_album {
            cfg.group_by_album = v;
        }
        if let Some(v) = file.follow_cursor {
            cfg.follow_cursor = v;
        }
        if let Some(v) = file.status_update_interval_ms {
            cfg.status_interval = Duration::from_millis(v.max(100));
        }
        if let Some(v) = file.visualizer {
            cfg.visualizer = v;
        }
        if let Some(v) = file.visualizer_fifo {
            cfg.visualizer_fifo = PathBuf::from(v);
        }
        if let Some(v) = file.visualizer_rows {
            cfg.visualizer_rows = v.clamp(1, 3);
        }
        if let Some(v) = file.visualizer_fps {
            cfg.visualizer_fps = v.clamp(5, 60);
        }
        if let Some(v) = file.visualizer_delay_ms {
            cfg.visualizer_delay = Duration::from_millis(v);
        }
        for (section, entries) in &file.keys {
            cfg.keys.merge(section, entries).with_context(|| path.display().to_string())?;
        }
        for (name, entry) in file.theme {
            cfg.theme.set(&name, entry).with_context(|| format!("{}: [theme] {name}", path.display()))?;
        }
        Ok(cfg)
    }
}

impl Theme {
    fn set(&mut self, name: &str, entry: ThemeEntry) -> Result<()> {
        if let ("dir_symbol" | "song_symbol", ThemeEntry::Color(sym)) = (name, &entry) {
            *if name == "dir_symbol" { &mut self.dir_symbol } else { &mut self.song_symbol } = sym.clone();
            return Ok(());
        }
        if name == "visualizer_colors" {
            let colors = match entry {
                ThemeEntry::Color(name) => palette(&name).ok_or_else(|| anyhow::anyhow!("unknown palette {name:?}"))?,
                ThemeEntry::Style { .. } => bail!("expected a palette name or a list of colours"),
                // a list of exactly five parses as the symbols shape
                ThemeEntry::Symbols(five) => five
                    .iter()
                    .map(|c| match parse_color(c)? {
                        Color::Rgb(rgb) => Ok(rgb),
                        _ => bail!("visualizer colours must be #rrggbb"),
                    })
                    .collect::<Result<Vec<u32>>>()?,
                ThemeEntry::List(list) => list
                    .iter()
                    .map(|c| match parse_color(c)? {
                        Color::Rgb(rgb) => Ok(rgb),
                        _ => bail!("visualizer colours must be #rrggbb"),
                    })
                    .collect::<Result<Vec<u32>>>()?,
            };
            if colors.is_empty() {
                bail!("need at least one colour");
            }
            self.visualizer_colors = colors;
            return Ok(());
        }
        if name == "progress_symbols" {
            let ThemeEntry::Symbols(s) = entry else { bail!("expected a list of 5 strings") };
            self.progress_symbols = s;
            return Ok(());
        }
        let slot = match name {
            "text" => &mut self.text,
            "state" => &mut self.state,
            "title" => &mut self.title,
            "artist" => &mut self.artist,
            "volume" => &mut self.volume,
            "mode_on" => &mut self.mode_on,
            "mode_off" => &mut self.mode_off,
            "tab_active" => &mut self.tab_active,
            "tab_inactive" => &mut self.tab_inactive,
            "cursor" => &mut self.cursor,
            "browse" => &mut self.browse,
            "browse_rail" => &mut self.browse_rail,
            "current" => &mut self.current,
            "album" => &mut self.album,
            "date" => &mut self.date,
            "group_header" => &mut self.group_header,
            "dim" => &mut self.dim,
            "progress_elapsed" => &mut self.progress_elapsed,
            "progress_thumb" => &mut self.progress_thumb,
            "progress_track" => &mut self.progress_track,
            "label" => &mut self.label,
            "album_artist" => &mut self.album_artist,
            "panel" => &mut self.panel,
            "panel_border" => &mut self.panel_border,

            _ => bail!("unknown theme entry"),
        };
        *slot = match entry {
            ThemeEntry::Color(c) => Style::fg(parse_color(&c)?),
            ThemeEntry::Style { fg, bg, bold, italic } => Style {
                fg: fg.as_deref().map(parse_color).transpose()?.unwrap_or(Color::Default),
                bg: bg.as_deref().map(parse_color).transpose()?.unwrap_or(Color::Default),
                bold,
                italic,
            },
            ThemeEntry::Symbols(_) | ThemeEntry::List(_) => bail!("expected a colour or a style table"),
        };
        Ok(())
    }
}

/// Built-in spectrum palettes, bass to treble.
pub fn palette(name: &str) -> Option<Vec<u32>> {
    Some(match name {
        "aurora" => vec![0x7b2ff7, 0x2f80ed, 0x12c2a0],
        "ember" => vec![0xdd6f00, 0xe8b100, 0xa7ff04],
        "ocean" => vec![0x0b3d91, 0x1f8fd6, 0x7fe0f0],
        "sunset" => vec![0xc2185b, 0xff6f3c, 0xffc94a],
        "mono" => vec![0x8a8a8a],
        _ => return None,
    })
}

/// `#rrggbb`, `default`, or an rmpc/ratatui colour name, which maps to the
/// terminal's own palette.
pub fn parse_color(s: &str) -> Result<Color> {
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            return Ok(Color::Rgb(u32::from_str_radix(hex, 16)?));
        }
        bail!("bad colour {s:?}: use #rrggbb");
    }
    let idx = match s.to_ascii_lowercase().replace('-', "_").as_str() {
        "default" | "reset" => return Ok(Color::Default),
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "gray" | "grey" => 7,
        "dark_gray" | "dark_grey" => 8,
        "light_red" => 9,
        "light_green" => 10,
        "light_yellow" => 11,
        "light_blue" => 12,
        "light_magenta" => 13,
        "light_cyan" => 14,
        "white" => 15,
        other => other.parse::<u8>().map_err(|_| anyhow::anyhow!("unknown colour {s:?}"))?,
    };
    Ok(Color::Palette(idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#dd6f00").unwrap(), Color::Rgb(0xdd6f00));
        assert_eq!(parse_color("yellow").unwrap(), Color::Palette(3));
        assert_eq!(parse_color("dark_gray").unwrap(), Color::Palette(8));
        assert_eq!(parse_color("default").unwrap(), Color::Default);
        assert!(parse_color("#fff").is_err());
    }

    #[test]
    fn parses_a_config_file() {
        let file: File = toml::from_str(
            r##"
            volume_step = 2
            [keys.global]
            "<C-p>" = "TogglePause"
            "s" = "None"
            [theme]
            accent_unused_is_rejected_later = "#000000"
            cursor = { fg = "black", bg = "#112233", bold = true }
            progress_symbols = ["[", "=", ">", "-", "]"]
            "##,
        )
        .unwrap();
        assert_eq!(file.volume_step, Some(2));
        let mut theme = Theme::default();
        assert!(theme.set("cursor", file.theme.into_iter().find(|(k, _)| k == "cursor").unwrap().1).is_ok());
        assert_eq!(theme.cursor.bg, Color::Rgb(0x112233));
    }
}
