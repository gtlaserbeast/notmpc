//! Keybindings, written the way rmpc writes them so config snippets carry
//! over: keys like `p`, `<C-k>`, `<S-Tab>`, `<CR>`, and actions like
//! `TogglePause` or `SwitchToTab("Queue")`.

use std::collections::HashMap;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};

use crate::nc::{Input, key};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    // global
    Quit,
    TogglePause,
    Stop,
    NextTrack,
    PreviousTrack,
    SeekForward,
    SeekBack,
    VolumeUp,
    VolumeDown,
    ToggleRepeat,
    ToggleRandom,
    ToggleSingle,
    ToggleConsume,
    NextTab,
    PreviousTab,
    SwitchToTab(String),
    Update,
    Rescan,
    ShowHelp,
    ShowCurrentSongInfo,
    ShowOutputs,
    ShowDecoders,
    AddRandom,
    CommandMode,
    // navigation
    Up,
    Down,
    Left,
    Right,
    UpHalf,
    DownHalf,
    Top,
    Bottom,
    PaneUp,
    PaneDown,
    PaneLeft,
    PaneRight,
    Select,
    InvertSelection,
    Confirm,
    Close,
    MoveUp,
    MoveDown,
    Delete,
    Add,
    AddAll,
    Insert,
    InsertAll,
    EnterSearch,
    NextResult,
    PreviousResult,
    FocusInput,
    ShowInfo,
    Rename,
    // queue
    Play,
    DeleteAll,
    JumpToCurrent,
    Shuffle,
    Save,
    AddToPlaylist,
    /// Unbinds a key inherited from the defaults (`"None"` in config).
    Unbind,
}

impl FromStr for Action {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        use Action::*;
        let s = s.trim();
        if let Some(arg) = s.strip_prefix("SwitchToTab(").and_then(|r| r.strip_suffix(')')) {
            return Ok(SwitchToTab(arg.trim().trim_matches('"').to_owned()));
        }
        Ok(match s {
            "Quit" => Quit,
            "TogglePause" => TogglePause,
            "Stop" => Stop,
            "NextTrack" => NextTrack,
            "PreviousTrack" => PreviousTrack,
            "SeekForward" => SeekForward,
            "SeekBack" => SeekBack,
            "VolumeUp" => VolumeUp,
            "VolumeDown" => VolumeDown,
            "ToggleRepeat" => ToggleRepeat,
            "ToggleRandom" => ToggleRandom,
            "ToggleSingle" => ToggleSingle,
            "ToggleConsume" => ToggleConsume,
            "NextTab" => NextTab,
            "PreviousTab" => PreviousTab,
            "Update" => Update,
            "Rescan" => Rescan,
            "ShowHelp" => ShowHelp,
            "ShowCurrentSongInfo" => ShowCurrentSongInfo,
            "ShowOutputs" => ShowOutputs,
            "ShowDecoders" => ShowDecoders,
            "AddRandom" => AddRandom,
            "CommandMode" => CommandMode,
            "Up" => Up,
            "Down" => Down,
            "Left" => Left,
            "Right" => Right,
            "UpHalf" => UpHalf,
            "DownHalf" => DownHalf,
            "Top" => Top,
            "Bottom" => Bottom,
            "PaneUp" => PaneUp,
            "PaneDown" => PaneDown,
            "PaneLeft" => PaneLeft,
            "PaneRight" => PaneRight,
            "Select" => Select,
            "InvertSelection" => InvertSelection,
            "Confirm" => Confirm,
            "Close" => Close,
            "MoveUp" => MoveUp,
            "MoveDown" => MoveDown,
            "Delete" => Delete,
            "Add" => Add,
            "AddAll" => AddAll,
            "Insert" => Insert,
            "InsertAll" => InsertAll,
            "EnterSearch" => EnterSearch,
            "NextResult" => NextResult,
            "PreviousResult" => PreviousResult,
            "FocusInput" => FocusInput,
            "ShowInfo" => ShowInfo,
            "Rename" => Rename,
            "Play" => Play,
            "DeleteAll" => DeleteAll,
            "JumpToCurrent" => JumpToCurrent,
            "Shuffle" => Shuffle,
            "Save" => Save,
            "AddToPlaylist" => AddToPlaylist,
            "None" => Unbind,
            _ => bail!("unknown action {s:?}"),
        })
    }
}

/// Parses rmpc key notation into the normalized [`Input`] notcurses produces.
pub fn parse_key(s: &str) -> Result<Input> {
    let mut k = Input { id: 0, ctrl: false, alt: false, shift: false };
    let Some(inner) = s.strip_prefix('<').and_then(|r| r.strip_suffix('>')) else {
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => k.id = c as u32,
            _ => bail!("bad key {s:?}: use one character or <...>"),
        }
        return Ok(normalize(k));
    };

    let mut rest = inner;
    while let Some((m, r)) = rest.split_once('-').filter(|(m, r)| m.len() == 1 && !r.is_empty()) {
        match m {
            "C" => k.ctrl = true,
            "A" | "M" => k.alt = true,
            "S" => k.shift = true,
            _ => bail!("bad modifier {m:?} in {s:?}"),
        }
        rest = r;
    }
    k.id = match rest {
        "CR" | "Enter" => key::ENTER,
        "Tab" => key::TAB,
        "Esc" => key::ESC,
        "Space" => key::SPACE,
        "BS" | "Backspace" => key::BACKSPACE,
        "Del" | "Delete" => key::DEL,
        "Up" => key::UP,
        "Down" => key::DOWN,
        "Left" => key::LEFT,
        "Right" => key::RIGHT,
        "PageUp" => key::PGUP,
        "PageDown" => key::PGDOWN,
        "Home" => key::HOME,
        "End" => key::END,
        _ => {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => c as u32,
                _ => bail!("unknown key {rest:?} in {s:?}"),
            }
        }
    };
    Ok(normalize(k))
}

/// Mirrors `Input::normalize` so parsed keys compare equal to real ones.
fn normalize(mut k: Input) -> Input {
    if let Some(c) = char::from_u32(k.id).filter(char::is_ascii_alphabetic) {
        if k.ctrl {
            k.id = c.to_ascii_lowercase() as u32;
        } else if k.shift {
            k.id = c.to_ascii_uppercase() as u32;
        }
    }
    if key::is_printable(k.id) && !k.ctrl {
        k.shift = false;
    }
    k
}

/// Bindings per context. A context's own section is checked first, then
/// navigation, then global.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    pub global: HashMap<Input, Action>,
    pub navigation: HashMap<Input, Action>,
    pub queue: HashMap<Input, Action>,
}

impl Keymap {
    /// The list-context lookup (queue tab).
    pub fn queue_action(&self, input: Input) -> Option<&Action> {
        [&self.queue, &self.navigation, &self.global]
            .into_iter()
            .find_map(|m| m.get(&input))
            .filter(|a| **a != Action::Unbind)
    }

    /// Overlays `section`'s bindings from the config file.
    pub fn merge(&mut self, section: &str, entries: &HashMap<String, String>) -> Result<()> {
        let map = match section {
            "global" => &mut self.global,
            "navigation" => &mut self.navigation,
            "queue" => &mut self.queue,
            _ => bail!("unknown key section [keys.{section}]"),
        };
        for (k, a) in entries {
            let input = parse_key(k)?;
            let action = a.parse().map_err(|e| anyhow!("[keys.{section}] {k:?}: {e}"))?;
            map.insert(input, action);
        }
        Ok(())
    }
}

/// rmpc keybindings from the user's config, minus the Artists / Album
/// Artists / Albums / Playlists tabs notmpc doesn't have.
const DEFAULTS: &[(&str, &[(&str, &str)])] = &[
    (
        "global",
        &[
            (":", "CommandMode"),
            (",", "VolumeDown"),
            (".", "VolumeUp"),
            ("s", "Stop"),
            ("<Tab>", "NextTab"),
            ("<S-Tab>", "PreviousTab"),
            ("1", r#"SwitchToTab("Queue")"#),
            ("2", r#"SwitchToTab("Directories")"#),
            ("3", r#"SwitchToTab("Search")"#),
            ("q", "Quit"),
            (">", "NextTrack"),
            ("<", "PreviousTrack"),
            ("p", "TogglePause"),
            ("f", "SeekForward"),
            ("b", "SeekBack"),
            ("z", "ToggleRepeat"),
            ("x", "ToggleRandom"),
            ("c", "ToggleConsume"),
            ("v", "ToggleSingle"),
            ("~", "ShowHelp"),
            ("u", "Update"),
            ("U", "Rescan"),
            ("I", "ShowCurrentSongInfo"),
            ("O", "ShowOutputs"),
            ("P", "ShowDecoders"),
            ("R", "AddRandom"),
        ],
    ),
    (
        "navigation",
        &[
            ("k", "Up"),
            ("j", "Down"),
            ("h", "Left"),
            ("l", "Right"),
            ("<Up>", "Up"),
            ("<Down>", "Down"),
            ("<Left>", "Left"),
            ("<Right>", "Right"),
            ("<C-k>", "PaneUp"),
            ("<C-j>", "PaneDown"),
            ("<C-h>", "PaneLeft"),
            ("<C-l>", "PaneRight"),
            ("<C-u>", "UpHalf"),
            ("<C-d>", "DownHalf"),
            ("<PageUp>", "UpHalf"),
            ("<PageDown>", "DownHalf"),
            ("<Home>", "Top"),
            ("<End>", "Bottom"),
            ("N", "PreviousResult"),
            ("n", "NextResult"),
            ("a", "Add"),
            ("A", "AddAll"),
            ("r", "Rename"),
            ("g", "Top"),
            ("G", "Bottom"),
            ("<Space>", "Select"),
            ("<C-Space>", "InvertSelection"),
            ("<CR>", "Confirm"),
            ("i", "FocusInput"),
            ("J", "MoveDown"),
            ("K", "MoveUp"),
            ("/", "EnterSearch"),
            ("<C-c>", "Close"),
            ("<Esc>", "Close"),
            ("D", "Delete"),
            ("B", "ShowInfo"),
            ("t", "Insert"),
            ("T", "InsertAll"),
        ],
    ),
    (
        "queue",
        &[
            ("D", "DeleteAll"),
            ("<CR>", "Play"),
            ("<C-s>", "Save"),
            ("a", "AddToPlaylist"),
            ("d", "Delete"),
            ("<Del>", "Delete"),
            ("C", "JumpToCurrent"),
            ("X", "Shuffle"),
        ],
    ),
];

impl Keymap {
    pub fn defaults() -> Self {
        let mut km = Self::default();
        for (section, binds) in DEFAULTS {
            let entries = binds.iter().map(|(k, a)| (k.to_string(), a.to_string())).collect();
            km.merge(section, &entries).expect("built-in keymap is valid");
        }
        km
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: u32, ctrl: bool, alt: bool, shift: bool) -> Input {
        Input { id, ctrl, alt, shift }
    }

    #[test]
    fn parses_rmpc_notation() {
        assert_eq!(parse_key("p").unwrap(), input('p' as u32, false, false, false));
        assert_eq!(parse_key("G").unwrap(), input('G' as u32, false, false, false));
        assert_eq!(parse_key("<C-k>").unwrap(), input('k' as u32, true, false, false));
        assert_eq!(parse_key("<S-Tab>").unwrap(), input(key::TAB, false, false, true));
        assert_eq!(parse_key("<CR>").unwrap(), input(key::ENTER, false, false, false));
        assert_eq!(parse_key("<C-Space>").unwrap(), input(key::SPACE, true, false, false));
        assert!(parse_key("<Nope>").is_err());
    }

    #[test]
    fn parses_actions() {
        assert_eq!("TogglePause".parse::<Action>().unwrap(), Action::TogglePause);
        assert_eq!(
            r#"SwitchToTab("Queue")"#.parse::<Action>().unwrap(),
            Action::SwitchToTab("Queue".into())
        );
        assert!("Bogus".parse::<Action>().is_err());
    }

    #[test]
    fn queue_section_wins() {
        let km = Keymap::defaults();
        // D is Delete in navigation but DeleteAll in the queue
        assert_eq!(km.queue_action(parse_key("D").unwrap()), Some(&Action::DeleteAll));
        assert_eq!(km.queue_action(parse_key("p").unwrap()), Some(&Action::TogglePause));
    }
}
