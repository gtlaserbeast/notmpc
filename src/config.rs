//! `~/.config/notmpc/config.toml`. Every setting is optional; the defaults
//! are rmpc's keymap and a theme in the terminal's own palette.

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

const BLACK: Color = Color::Palette(0);
const YELLOW: Color = Color::Palette(3);
const BLUE: Color = Color::Palette(4);
const CYAN: Color = Color::Palette(6);
const DARK_GRAY: Color = Color::Palette(8);
/// From the 256-colour grey ramp, which terminal themes leave alone.
const SHADE: Color = Color::Palette(236);
const DARK_SHADE: Color = Color::Palette(235);

impl Default for Theme {
    /// Terminal palette colours, so it follows the terminal's theme, and
    /// symbols any monospace font has.
    fn default() -> Self {
        Self {
            text: Style::fg(Color::Default),
            state: Style::fg(BLUE).bold(),
            title: Style::fg(Color::Default).bold(),
            artist: Style::fg(YELLOW).bold(),
            volume: Style::fg(BLUE),
            mode_on: Style::fg(Color::Default).bold(),
            mode_off: Style::fg(DARK_GRAY),
            tab_active: Style::fg(BLACK).on(BLUE).bold(),
            tab_inactive: Style::fg(Color::Default),
            cursor: Style::fg(BLACK).on(BLUE).bold(),
            browse: Style::fg(Color::Default).on(SHADE),
            browse_rail: Style::fg(CYAN).bold(),
            current: Style::fg(BLUE).bold(),
            album: Style::fg(Color::Default),
            date: Style::fg(YELLOW),
            group_header: Style::fg(BLUE).bold(),
            dim: Style::fg(DARK_GRAY),
            progress_elapsed: Style::fg(BLUE),
            progress_thumb: Style::fg(CYAN),
            progress_track: Style::fg(DARK_GRAY),
            progress_symbols: ["", "━", "●", "─", ""].map(String::from),
            label: Style::fg(YELLOW),
            album_artist: Style::fg(CYAN),
            panel: Style::fg(Color::Default).on(DARK_SHADE),
            panel_border: Style::fg(BLUE).on(DARK_SHADE),
            visualizer_colors: palette("aurora").expect("built-in palette"),
            dir_symbol: "▸".into(),
            song_symbol: "♪".into(),
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

    /// Loads the state file, applies `change` and saves it; returns
    /// " (saved)" or why not, for a status message.
    pub fn update(change: impl FnOnce(&mut Self)) -> String {
        let mut state = Self::load();
        change(&mut state);
        state.save().map_or_else(|e| format!(" (not saved: {e})"), |()| " (saved)".into())
    }

    fn save(&self) -> Result<()> {
        let path = Self::path().context("no HOME")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, toml::to_string(self)?)?;
        Ok(())
    }
}

/// The config file's keys, one to one; anything left out keeps its default.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub volume_step: u8,
    pub seek_step: u32,
    /// Share of the queue tab's width given to album art, in percent.
    pub art_width: u32,
    pub group_by_album: bool,
    /// Playback follows the queue cursor (toggle with ToggleFollow).
    pub follow_cursor: bool,
    /// How often to refresh bitrate and position while playing.
    pub status_update_interval_ms: u64,
    /// The progress bar in pixels when the terminal has pixel graphics;
    /// false for the symbol bar (`progress_symbols`).
    pub pixel_progress: bool,
    /// Spectrum behind the header, fed by MPD's `fifo` output.
    pub visualizer: bool,
    pub visualizer_fifo: PathBuf,
    /// Header rows it fills (the header is 2 rows, plus the progress bar).
    pub visualizer_rows: u32,
    pub visualizer_fps: u32,
    /// How far the speakers lag MPD's FIFO (PipeWire buffering). Tuned
    /// live with [ and ], which saves it to the state file.
    pub visualizer_delay_ms: u64,
    /// `[keys.*]` and `[theme]` as written; merged into `keys` and `theme`.
    #[serde(rename = "keys")]
    key_overrides: HashMap<String, HashMap<String, String>>,
    #[serde(rename = "theme")]
    theme_overrides: HashMap<String, ThemeEntry>,
    #[serde(skip)]
    pub keys: Keymap,
    #[serde(skip)]
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
            status_update_interval_ms: 1000,
            pixel_progress: true,
            visualizer: true,
            visualizer_fifo: env::var_os("XDG_RUNTIME_DIR")
                .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
                .join("mpd/visualizer.fifo"),
            visualizer_rows: 3,
            visualizer_fps: 30,
            visualizer_delay_ms: 150,
            key_overrides: HashMap::new(),
            theme_overrides: HashMap::new(),
            keys: Keymap::defaults(),
            theme: Theme::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
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
            cfg.visualizer_delay_ms = ms;
        }
        if let Some(on) = state.visualizer {
            cfg.visualizer = on;
        }
        Ok(cfg)
    }

    fn load_config() -> Result<Self> {
        let Some(path) = Self::path().filter(|p| p.exists()) else {
            return Ok(Self::default());
        };
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| path.display().to_string())
    }

    fn parse(text: &str) -> Result<Self> {
        let mut cfg: Self = toml::from_str(text)?;
        cfg.art_width = cfg.art_width.min(90);
        cfg.status_update_interval_ms = cfg.status_update_interval_ms.max(100);
        cfg.visualizer_rows = cfg.visualizer_rows.clamp(1, 3);
        cfg.visualizer_fps = cfg.visualizer_fps.clamp(5, 60);
        for (section, entries) in std::mem::take(&mut cfg.key_overrides) {
            cfg.keys.merge(&section, &entries)?;
        }
        for (name, entry) in std::mem::take(&mut cfg.theme_overrides) {
            cfg.theme.set(&name, entry).with_context(|| format!("[theme] {name}"))?;
        }
        Ok(cfg)
    }

    pub fn status_interval(&self) -> Duration {
        Duration::from_millis(self.status_update_interval_ms)
    }

    pub fn visualizer_delay(&self) -> Duration {
        Duration::from_millis(self.visualizer_delay_ms)
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
    fn example_config_parses() {
        Config::parse(include_str!("../config.example.toml")).unwrap();
    }

    #[test]
    fn parses_a_config_file() {
        let cfg = Config::parse(
            r##"
            volume_step = 2
            visualizer_rows = 9
            [keys.global]
            "<C-p>" = "TogglePause"
            "s" = "None"
            [theme]
            cursor = { fg = "black", bg = "#112233", bold = true }
            progress_symbols = ["[", "=", ">", "-", "]"]
            "##,
        )
        .unwrap();
        assert_eq!(cfg.volume_step, 2);
        assert_eq!(cfg.seek_step, 5, "unset keys keep their defaults");
        assert_eq!(cfg.visualizer_rows, 3, "clamped");
        assert_eq!(cfg.theme.cursor.bg, Color::Rgb(0x112233));
        assert_eq!(cfg.theme.progress_symbols[0], "[");
        assert!(cfg.keys.global.len() > 20, "skipped fields come from Config::default");
        assert!(Config::parse("no_such_key = 1").is_err());
    }
}
