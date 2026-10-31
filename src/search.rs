//! The Search tab: a query line searched live (case-insensitive substring)
//! as you type, results grouped by album.
//!
//! Query syntax: words match any tag or the file path; `field:value`
//! limits a word to one tag (artist, album, albumartist, title, genre,
//! date, composer, file); `"double quotes"` keep spaces. Terms are ANDed.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::enqueue::natural_cmp;
use crate::meta::Meta;
use crate::mpd::{Conn, Song, quote};
use crate::nc::{Input, key};
use crate::queue::Queue;

/// Pause after the last keystroke before searching.
pub const DEBOUNCE: Duration = Duration::from_millis(250);
/// Most results fetched per search.
const LIMIT: usize = 1000;
const FIELDS: &[&str] = &["artist", "album", "albumartist", "title", "genre", "date", "composer", "file"];

pub struct Search {
    pub query: String,
    /// Keys go to the query line (vs. the results).
    pub editing: bool,
    pub results: Queue,
    /// The query `results` are for.
    ran: Option<String>,
    edited_at: Option<Instant>,
    pub truncated: bool,
}

pub enum Edit {
    /// Handled as text editing.
    Consumed,
    /// Enter: search now and move to the results.
    Submit,
    /// Esc / Down: move to the results.
    Leave,
    /// Not an editing key; let the keymap have it.
    Pass,
}

impl Search {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            editing: true,
            results: Queue::new(true),
            ran: None,
            edited_at: None,
            truncated: false,
        }
    }

    pub fn edit(&mut self, k: Input) -> Edit {
        let changed = match k.id {
            key::ENTER => return Edit::Submit,
            key::ESC | key::DOWN => return Edit::Leave,
            key::BACKSPACE => self.query.pop().is_some(),
            _ if k.ctrl && k.id == 'u' as u32 => !std::mem::take(&mut self.query).is_empty(),
            _ if k.ctrl && k.id == 'w' as u32 => {
                let trimmed = self.query.trim_end();
                let cut = trimmed.rfind(' ').map_or(0, |i| i + 1);
                self.query.truncate(cut);
                true
            }
            id if key::is_printable(id) && !k.ctrl && !k.alt => {
                self.query.extend(char::from_u32(id));
                true
            }
            _ => return Edit::Pass,
        };
        if changed {
            self.edited_at = Some(Instant::now());
        }
        Edit::Consumed
    }

    /// Searches once typing pauses; true if it did.
    pub fn settle(&mut self, conn: &mut Conn) -> Result<bool> {
        if self.edited_at.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
            self.run(conn)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// When `settle` will next have work to do.
    pub fn settles_at(&self) -> Option<Instant> {
        self.edited_at.map(|t| t + DEBOUNCE)
    }

    pub fn run(&mut self, conn: &mut Conn) -> Result<()> {
        self.edited_at = None;
        if self.ran.as_deref() == Some(self.query.as_str()) {
            return Ok(());
        }
        self.ran = Some(self.query.clone());
        let mut songs: Vec<Song> = Vec::new();
        self.truncated = false;
        for filter in filters(&self.query) {
            let found = Song::parse_list(&conn.command(&format!("search {} window 0:{LIMIT}", quote(&filter)))?);
            self.truncated |= found.len() >= LIMIT;
            for s in found {
                if !songs.iter().any(|x| x.file == s.file) {
                    songs.push(s);
                }
            }
        }
        // Album order: album artist, year, album, disc, track.
        let mut keyed: Vec<(Meta, Song)> = songs.into_iter().map(|s| (Meta::of(&s), s)).collect();
        keyed.sort_by(|(a, x), (b, y)| {
            natural_cmp(&a.album_artist, &b.album_artist)
                .then_with(|| a.year.cmp(&b.year))
                .then_with(|| natural_cmp(&a.album, &b.album))
                .then_with(|| natural_cmp(&a.dir, &b.dir))
                .then_with(|| (a.disc, a.track).cmp(&(b.disc, b.track)))
                .then_with(|| natural_cmp(&x.file, &y.file))
        });
        // Results aren't queue entries; ids only key marks and albums.
        let songs = keyed
            .into_iter()
            .enumerate()
            .map(|(i, (_, mut s))| {
                s.id = Some(i as u32);
                s
            })
            .collect();
        self.results.collapsed.clear();
        self.results.marked.clear();
        self.results.set_songs(songs, None);
        self.results.sel = 0;
        self.results.scroll = 0;
        Ok(())
    }

    pub fn is_empty_query(&self) -> bool {
        self.query.trim().is_empty()
    }
}

/// MPD filter expressions for a query. Plain words match any tag or the
/// file path; MPD filters have no OR, so that's two searches unioned.
pub fn filters(query: &str) -> Vec<String> {
    let terms: Vec<(Option<&str>, String)> = tokens(query)
        .into_iter()
        .filter_map(|t| match t.split_once(':') {
            Some((f, v)) if FIELDS.contains(&f.to_ascii_lowercase().as_str()) => {
                (!v.is_empty()).then(|| (Some(field(f)), v.trim_matches('"').to_owned()))
            }
            _ => Some((None, t.trim_matches('"').to_owned())),
        })
        .filter(|(_, v)| !v.is_empty())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let build = |plain: &str| {
        let parts: Vec<String> =
            terms.iter().map(|(f, v)| format!("({} contains '{}')", f.unwrap_or(plain), escape(v))).collect();
        if parts.len() == 1 { parts[0].clone() } else { format!("({})", parts.join(" AND ")) }
    };
    let mut out = vec![build("any")];
    if terms.iter().any(|(f, _)| f.is_none()) {
        out.push(build("file"));
    }
    out
}

fn field(f: &str) -> &'static str {
    let f = f.to_ascii_lowercase();
    FIELDS.iter().copied().find(|x| *x == f).unwrap_or("any")
}

/// Splits on spaces, keeping `"quoted phrases"` (also after `field:`).
fn tokens(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in q.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Escapes a value for a single-quoted MPD filter string.
fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_filters() {
        assert!(filters("  ").is_empty());
        assert_eq!(filters("kiska"), vec!["(any contains 'kiska')", "(file contains 'kiska')"]);
        assert_eq!(
            filters(r#"artist:autechre album:"tri repetae""#),
            vec!["((artist contains 'autechre') AND (album contains 'tri repetae'))"]
        );
        assert_eq!(
            filters("it's Artist:x"),
            vec![
                "((any contains 'it\\'s') AND (artist contains 'x'))",
                "((file contains 'it\\'s') AND (artist contains 'x'))"
            ]
        );
        // an unknown field is just a word
        assert_eq!(filters("foo:bar")[0], "(any contains 'foo:bar')");
    }
}
