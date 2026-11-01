//! Modal panels over a dimmed backdrop: help, song info, outputs, decoders.

use anyhow::Result;

use crate::keys::{Action, Keymap};
use crate::mpd::{Conn, Song, quote};
use crate::nc::{Input, key};

#[derive(Clone, Debug)]
pub struct Output {
    pub id: u32,
    pub name: String,
    pub plugin: String,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Help,
    Info,
    Outputs(Vec<Output>),
    Decoders,
}

/// One line of a panel: a label column and a value (either may be empty).
#[derive(Clone, Debug)]
pub struct Line {
    pub label: String,
    pub value: String,
    /// A section heading.
    pub heading: bool,
}

impl Line {
    fn heading(s: impl Into<String>) -> Self {
        Self { label: s.into(), value: String::new(), heading: true }
    }

    fn pair(l: impl Into<String>, v: impl Into<String>) -> Self {
        Self { label: l.into(), value: v.into(), heading: false }
    }
}

pub struct Overlay {
    pub kind: Kind,
    pub title: String,
    pub lines: Vec<Line>,
    /// First visible line; for Outputs, also the selected output.
    pub scroll: usize,
    pub sel: usize,
}

impl Overlay {
    pub fn help(keys: &Keymap) -> Self {
        let mut lines = Vec::new();
        for (name, map) in [("Global", &keys.global), ("Lists", &keys.navigation), ("Queue", &keys.queue)] {
            lines.push(Line::heading(name));
            let mut entries: Vec<(String, String)> = map
                .iter()
                .filter(|(_, a)| **a != Action::Unbind)
                .map(|(k, a)| (seq_name(k), format!("{a:?}")))
                .collect();
            // group by action, keys together: "j, <Down>  Down"
            entries.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            let mut merged: Vec<(String, String)> = Vec::new();
            for (k, a) in entries {
                match merged.last_mut() {
                    Some((ks, last)) if *last == a => *ks = format!("{ks}, {k}"),
                    _ => merged.push((k, a)),
                }
            }
            lines.extend(merged.into_iter().map(|(k, a)| Line::pair(k, a)));
        }
        lines.push(Line::heading("Mouse"));
        for (k, v) in [
            ("click", "select; on the queue the song also plays next"),
            ("double-click", "play now (an album header: the album)"),
            ("▾ / ▸", "fold / unfold an album"),
            ("progress bar", "seek"),
            ("Repeat … Follow", "toggle"),
            ("wheel", "scroll"),
        ] {
            lines.push(Line::pair(k, v));
        }
        Self { kind: Kind::Help, title: "Keys".into(), lines, scroll: 0, sel: 0 }
    }

    /// Everything MPD knows about a song: its tags (`lsinfo`), any extra
    /// ones from the file (`readcomments`: ReplayGain, comments...).
    pub fn info(conn: &mut Conn, uri: &str) -> Result<Self> {
        let mut lines = vec![Line::heading("Tags")];
        let pairs = conn.command(&format!("lsinfo {}", quote(uri)))?;
        let mut seen = std::collections::HashSet::new();
        for (k, v) in &pairs {
            if k == "file" || k == "Last-Modified" || k == "Added" {
                continue;
            }
            let label = match k.as_str() {
                "duration" => {
                    lines.push(Line::pair("Length", fmt_secs(v)));
                    continue;
                }
                "Time" => continue,
                "Format" => "Format (rate:bits:ch)",
                other => other,
            };
            seen.insert(k.to_ascii_lowercase());
            lines.push(Line::pair(label, v.clone()));
        }
        if let Ok(extra) = conn.command(&format!("readcomments {}", quote(uri))) {
            let extra: Vec<_> = extra.into_iter().filter(|(k, _)| !seen.contains(&k.to_ascii_lowercase())).collect();
            if !extra.is_empty() {
                lines.push(Line::heading("From the file"));
                lines.extend(extra.into_iter().map(|(k, v)| Line::pair(k, v)));
            }
        }
        lines.push(Line::heading("File"));
        lines.push(Line::pair("Path", uri));
        if let Some((_, m)) = pairs.iter().find(|(k, _)| k == "Last-Modified") {
            lines.push(Line::pair("Modified", m.clone()));
        }
        let title = Song::parse_list(&pairs).first().and_then(|s| s.title.clone()).unwrap_or_else(|| "Song".into());
        Ok(Self { kind: Kind::Info, title, lines, scroll: 0, sel: 0 })
    }

    pub fn outputs(conn: &mut Conn) -> Result<Self> {
        let mut outs: Vec<Output> = Vec::new();
        for (k, v) in conn.command("outputs")? {
            match k.as_str() {
                "outputid" => {
                    outs.push(Output { id: v.parse()?, name: String::new(), plugin: String::new(), enabled: false })
                }
                "outputname" => outs.last_mut().into_iter().for_each(|o| o.name = v.clone()),
                "plugin" => outs.last_mut().into_iter().for_each(|o| o.plugin = v.clone()),
                "outputenabled" => outs.last_mut().into_iter().for_each(|o| o.enabled = v == "1"),
                _ => {}
            }
        }
        let mut o = Self { kind: Kind::Outputs(outs), title: "Outputs".into(), lines: Vec::new(), scroll: 0, sel: 0 };
        o.relabel();
        Ok(o)
    }

    fn relabel(&mut self) {
        if let Kind::Outputs(outs) = &self.kind {
            self.lines = outs
                .iter()
                .map(|o| Line::pair(if o.enabled { "[x]" } else { "[ ]" }, format!("{}  ({})", o.name, o.plugin)))
                .collect();
        }
    }

    pub fn decoders(conn: &mut Conn) -> Result<Self> {
        let mut lines = Vec::new();
        let mut cur: Option<(String, Vec<String>)> = None;
        let flush = |cur: &mut Option<(String, Vec<String>)>, lines: &mut Vec<Line>| {
            if let Some((name, suffixes)) = cur.take() {
                lines.push(Line::pair(name, suffixes.join(" ")));
            }
        };
        for (k, v) in conn.command("decoders")? {
            match k.as_str() {
                "plugin" => {
                    flush(&mut cur, &mut lines);
                    cur = Some((v, Vec::new()));
                }
                "suffix" => cur.iter_mut().for_each(|(_, s)| s.push(v.clone())),
                _ => {}
            }
        }
        flush(&mut cur, &mut lines);
        Ok(Self { kind: Kind::Decoders, title: "Decoders (file types)".into(), lines, scroll: 0, sel: 0 })
    }

    pub fn selectable(&self) -> bool {
        matches!(self.kind, Kind::Outputs(_))
    }

    /// Moves the selection (outputs) or scrolls (others).
    pub fn move_by(&mut self, by: isize, visible: usize) {
        let last = self.lines.len().saturating_sub(1);
        if self.selectable() {
            self.sel = self.sel.saturating_add_signed(by).min(last);
            if self.sel < self.scroll {
                self.scroll = self.sel;
            } else if self.sel >= self.scroll + visible {
                self.scroll = self.sel + 1 - visible;
            }
        } else {
            self.scroll = self.scroll.saturating_add_signed(by).min(self.lines.len().saturating_sub(visible));
        }
    }

    /// The MPD command that toggles the selected output.
    pub fn toggle(&self) -> Option<String> {
        let Kind::Outputs(outs) = &self.kind else { return None };
        let o = outs.get(self.sel)?;
        Some(format!("{} {}", if o.enabled { "disableoutput" } else { "enableoutput" }, o.id))
    }

    /// Takes the outputs from a fresh listing, keeping the selection.
    pub fn reload_outputs(&mut self, fresh: Overlay) {
        let (sel, scroll) = (self.sel, self.scroll);
        *self = fresh;
        self.sel = sel.min(self.lines.len().saturating_sub(1));
        self.scroll = scroll;
    }
}

fn fmt_secs(v: &str) -> String {
    let s = v.parse::<f64>().unwrap_or(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// A key sequence in rmpc notation, for display.
pub fn seq_name(seq: &[Input]) -> String {
    seq.iter().map(key_name).collect()
}

fn key_name(k: &Input) -> String {
    let named = match k.id {
        key::ENTER => Some("CR"),
        key::TAB => Some("Tab"),
        key::ESC => Some("Esc"),
        key::SPACE => Some("Space"),
        key::BACKSPACE => Some("BS"),
        key::DEL => Some("Del"),
        key::UP => Some("Up"),
        key::DOWN => Some("Down"),
        key::LEFT => Some("Left"),
        key::RIGHT => Some("Right"),
        key::PGUP => Some("PageUp"),
        key::PGDOWN => Some("PageDown"),
        key::HOME => Some("Home"),
        key::END => Some("End"),
        _ => None,
    };
    let base = named.map(str::to_owned).unwrap_or_else(|| char::from_u32(k.id).map(String::from).unwrap_or_default());
    let mut mods = String::new();
    if k.ctrl {
        mods += "C-";
    }
    if k.alt {
        mods += "A-";
    }
    if k.shift {
        mods += "S-";
    }
    if mods.is_empty() && named.is_none() {
        base
    } else {
        // <C-U> is how rmpc writes Ctrl+Shift+U
        let base = if k.ctrl && k.shift && base.len() == 1 { base.to_uppercase() } else { base };
        let mods = if k.ctrl && k.shift && named.is_none() { mods.replace("S-", "") } else { mods };
        format!("<{mods}{base}>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_seq;

    #[test]
    fn names_round_trip() {
        for s in ["gg", "<C-w>k", "<S-Tab>", "<CR>", "<C-U>", "oI", "<", "<Space>"] {
            assert_eq!(seq_name(&parse_seq(s).unwrap()), s);
        }
    }

    #[test]
    fn help_lists_bindings() {
        let o = Overlay::help(&Keymap::defaults());
        assert!(o.lines.iter().any(|l| l.label.contains("gg") && l.value == "Top"));
        assert!(o.lines.iter().any(|l| l.label == "t" && l.value == "PlayNext"));
    }
}
