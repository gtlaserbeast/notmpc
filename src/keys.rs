//! Keybindings, written the way rmpc writes them so config snippets carry
//! over: keys and sequences like `p`, `gg`, `oI`, `<C-w>k`, `<S-Tab>`,
//! and actions like `TogglePause` or `SwitchToTab("Queue")`. The defaults
//! are rmpc 0.11's.

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
    ShowDownloads,
    Partition,
    AddRandom,
    CommandMode,
    /// notmpc: playback follows the queue cursor (foobar2000-style).
    ToggleFollow,
    /// notmpc: love the playing song on Last.fm (via mpdscribble).
    Love,
    /// notmpc: nudge the visualizer's delay to match the speakers.
    VisualizerDelayDown,
    VisualizerDelayUp,
    // navigation
    Up,
    Down,
    Left,
    Right,
    UpHalf,
    DownHalf,
    PageUp,
    PageDown,
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
    Rate,
    ContextMenu,
    // queue
    Play,
    DeleteAll,
    JumpToCurrent,
    Shuffle,
    Save,
    AddToPlaylist,
    /// notmpc: the browse cursor's song plays next (queue: moves the
    /// highlight there; directories: inserts after the current song).
    PlayNext,
    /// notmpc: fold / unfold every album group.
    CollapseAll,
    ExpandAll,
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
        // Other rmpc actions take options notmpc doesn't use (`Save(kind: ...)`).
        let s = s.split_once('(').map_or(s, |(name, _)| name.trim());
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
            "ShowDownloads" => ShowDownloads,
            "Partition" => Partition,
            "ToggleFollow" => ToggleFollow,
            "Love" => Love,
            "VisualizerDelayDown" => VisualizerDelayDown,
            "VisualizerDelayUp" => VisualizerDelayUp,
            "AddRandom" => AddRandom,
            "CommandMode" => CommandMode,
            "Up" => Up,
            "Down" => Down,
            "Left" => Left,
            "Right" => Right,
            "UpHalf" => UpHalf,
            "DownHalf" => DownHalf,
            "PageUp" => PageUp,
            "PageDown" => PageDown,
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
            "Rate" => Rate,
            "ContextMenu" => ContextMenu,
            "Play" => Play,
            "DeleteAll" => DeleteAll,
            "JumpToCurrent" => JumpToCurrent,
            "Shuffle" => Shuffle,
            "Save" => Save,
            "AddToPlaylist" => AddToPlaylist,
            "PlayNext" => PlayNext,
            "CollapseAll" => CollapseAll,
            "ExpandAll" => ExpandAll,
            "None" => Unbind,
            _ => bail!("unknown action {s:?}"),
        })
    }
}

/// Parses a key sequence (`gg`, `<C-w>k`, `<S-Tab>`, `<`) into the
/// normalized [`Input`]s notcurses produces.
pub fn parse_seq(s: &str) -> Result<Vec<Input>> {
    let mut keys = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        // `<...>` is a named key unless it's just a literal `<`
        let token = match rest.find('>') {
            Some(end) if c == '<' && end > 1 => &rest[..=end],
            _ => &rest[..c.len_utf8()],
        };
        keys.push(parse_key(token)?);
        rest = &rest[token.len()..];
    }
    if keys.is_empty() {
        bail!("empty key");
    }
    Ok(keys)
}

/// Parses one key in rmpc notation.
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
/// `<C-U>` means Ctrl+Shift+U, which only kitty-protocol terminals report.
fn normalize(mut k: Input) -> Input {
    if let Some(c) = char::from_u32(k.id).filter(char::is_ascii_alphabetic) {
        if k.ctrl {
            k.shift |= c.is_ascii_uppercase();
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

/// Where a key was pressed: decides which sections are searched, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctx {
    /// queue, then navigation, then global
    Queue,
    /// navigation, then global
    #[allow(dead_code)] // the Directories tab
    List,
    /// global only
    Global,
}

pub enum Lookup<'a> {
    Action(&'a Action),
    /// The keys so far start a longer binding; wait for more.
    Prefix,
    None,
}

type Bindings = HashMap<Vec<Input>, Action>;

#[derive(Clone, Debug, Default)]
pub struct Keymap {
    pub global: Bindings,
    pub navigation: Bindings,
    pub queue: Bindings,
}

impl Keymap {
    fn sections(&self, ctx: Ctx) -> Vec<&Bindings> {
        match ctx {
            Ctx::Queue => vec![&self.queue, &self.navigation, &self.global],
            Ctx::List => vec![&self.navigation, &self.global],
            Ctx::Global => vec![&self.global],
        }
    }

    /// Resolves the keys typed so far. An exact match wins over a longer
    /// binding it's a prefix of (as in rmpc, `g` alone isn't bound).
    pub fn lookup(&self, ctx: Ctx, seq: &[Input]) -> Lookup<'_> {
        let sections = self.sections(ctx);
        if let Some(a) = sections.iter().find_map(|m| m.get(seq)) {
            return if *a == Action::Unbind { Lookup::None } else { Lookup::Action(a) };
        }
        let prefix = sections
            .iter()
            .flat_map(|m| m.iter())
            .any(|(k, a)| *a != Action::Unbind && k.len() > seq.len() && k.starts_with(seq));
        if prefix { Lookup::Prefix } else { Lookup::None }
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
            let seq = parse_seq(k).map_err(|e| anyhow!("[keys.{section}] {k:?}: {e}"))?;
            let action = a.parse().map_err(|e| anyhow!("[keys.{section}] {k:?}: {e}"))?;
            map.insert(seq, action);
        }
        Ok(())
    }
}

/// rmpc 0.11's default keybindings (`rmpc config`). Tabs notmpc doesn't
/// have are dropped and Search moves to 3. notmpc additions are marked.
const DEFAULTS: &[(&str, &[(&str, &str)])] = &[
    (
        "global",
        &[
            ("q", "Quit"),
            ("?", "ShowHelp"),
            (":", "CommandMode"),
            ("oI", "ShowCurrentSongInfo"),
            ("oo", "ShowOutputs"),
            ("op", "ShowDecoders"),
            ("od", "ShowDownloads"),
            ("oP", "Partition()"),
            ("z", "ToggleRepeat"),
            ("x", "ToggleRandom"),
            ("c", "ToggleConsume"),
            ("v", "ToggleSingle"),
            ("p", "TogglePause"),
            ("s", "Stop"),
            (">", "NextTrack"),
            ("<", "PreviousTrack"),
            ("f", "SeekForward"),
            ("b", "SeekBack"),
            (".", "VolumeUp"),
            (",", "VolumeDown"),
            ("<Tab>", "NextTab"),
            ("gt", "NextTab"),
            ("<S-Tab>", "PreviousTab"),
            ("gT", "PreviousTab"),
            ("1", r#"SwitchToTab("Queue")"#),
            ("2", r#"SwitchToTab("Directories")"#),
            ("3", r#"SwitchToTab("Search")"#),
            ("<C-u>", "Update"),
            ("<C-U>", "Rescan"),
            ("R", "AddRandom"),
            // notmpc
            ("F", "ToggleFollow"),
            ("*", "Love"),
            ("[", "VisualizerDelayDown"),
            ("]", "VisualizerDelayUp"),
        ],
    ),
    (
        "navigation",
        &[
            ("<C-c>", "Close"),
            ("<Esc>", "Close"),
            ("<CR>", "Confirm"),
            ("k", "Up"),
            ("<Up>", "Up"),
            ("j", "Down"),
            ("<Down>", "Down"),
            ("h", "Left"),
            ("<Left>", "Left"),
            ("l", "Right"),
            ("<Right>", "Right"),
            ("<C-w>k", "PaneUp"),
            ("<C-Up>", "PaneUp"),
            ("<C-w>j", "PaneDown"),
            ("<C-Down>", "PaneDown"),
            ("<C-w>h", "PaneLeft"),
            ("<C-Left>", "PaneLeft"),
            ("<C-w>l", "PaneRight"),
            ("<C-Right>", "PaneRight"),
            ("K", "MoveUp"),
            ("J", "MoveDown"),
            ("<C-u>", "UpHalf"),
            ("<C-d>", "DownHalf"),
            ("<C-b>", "PageUp"),
            ("<PageUp>", "PageUp"),
            ("<C-f>", "PageDown"),
            ("<PageDown>", "PageDown"),
            ("gg", "Top"),
            ("G", "Bottom"),
            ("<Space>", "Select"),
            ("<C-Space>", "InvertSelection"),
            ("/", "EnterSearch"),
            ("n", "NextResult"),
            ("N", "PreviousResult"),
            ("a", "Add"),
            ("A", "AddAll"),
            ("D", "Delete"),
            ("<C-r>", "Rename"),
            ("i", "FocusInput"),
            ("oi", "ShowInfo"),
            ("<C-z>", "ContextMenu()"),
            ("<C-s>s", "Save(kind: Modal(all: false, duplicates_strategy: Ask))"),
            ("<C-s>a", "Save(kind: Modal(all: true, duplicates_strategy: Ask))"),
            ("r", "Rate()"),
            // notmpc
            ("t", "PlayNext"),
            ("C", "JumpToCurrent"),
            ("<Home>", "Top"),
            ("<End>", "Bottom"),
        ],
    ),
    (
        "queue",
        &[
            ("d", "Delete"),
            ("D", "DeleteAll"),
            ("<CR>", "Play"),
            ("C", "JumpToCurrent"),
            ("X", "Shuffle"),
            // notmpc
            ("<Del>", "Delete"),
            ("H", "CollapseAll"),
            ("L", "ExpandAll"),
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
    fn parses_sequences() {
        let k = |c: char| input(c as u32, false, false, false);
        assert_eq!(parse_seq("gg").unwrap(), vec![k('g'), k('g')]);
        assert_eq!(parse_seq("<C-w>k").unwrap(), vec![input('w' as u32, true, false, false), k('k')]);
        assert_eq!(parse_seq("<").unwrap(), vec![k('<')]);
        assert_eq!(parse_seq("<C-U>").unwrap(), vec![input('u' as u32, true, false, true)]);
        assert_eq!(parse_seq("<S-Tab>").unwrap(), vec![input(key::TAB, false, false, true)]);
    }

    #[test]
    fn lookup_resolves_sequences_and_sections() {
        let km = Keymap::defaults();
        let seq = |s| parse_seq(s).unwrap();
        // D is Delete in navigation but DeleteAll in the queue
        assert!(matches!(km.lookup(Ctx::Queue, &seq("D")), Lookup::Action(Action::DeleteAll)));
        assert!(matches!(km.lookup(Ctx::List, &seq("D")), Lookup::Action(Action::Delete)));
        assert!(matches!(km.lookup(Ctx::Queue, &seq("g")), Lookup::Prefix));
        assert!(matches!(km.lookup(Ctx::Queue, &seq("gg")), Lookup::Action(Action::Top)));
        assert!(matches!(km.lookup(Ctx::Queue, &seq("gT")), Lookup::Action(Action::PreviousTab)));
        assert!(matches!(km.lookup(Ctx::Queue, &seq("o")), Lookup::Prefix));
        assert!(matches!(km.lookup(Ctx::Queue, &seq("oq")), Lookup::None));
        // <C-u>: UpHalf in lists shadows the global Update, as in rmpc
        assert!(matches!(km.lookup(Ctx::Queue, &seq("<C-u>")), Lookup::Action(Action::UpHalf)));
        assert!(matches!(km.lookup(Ctx::Global, &seq("<C-u>")), Lookup::Action(Action::Update)));
        assert!(matches!(km.lookup(Ctx::Queue, &seq("r")), Lookup::Action(Action::Rate)));
    }
}
