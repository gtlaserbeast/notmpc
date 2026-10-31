//! The Directories tab: MPD's database as Miller columns (parent, current,
//! preview), like rmpc. Folders come first, everything in natural order;
//! songs by disc and track.

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::art;
use crate::enqueue::natural_cmp;
use crate::meta::Meta;
use crate::mpd::{Conn, Song, quote};
use crate::nc::Visual;

/// How long the cursor rests before the preview (and its cover) loads, so
/// holding j doesn't fetch every folder on the way.
pub const PREVIEW_DELAY: Duration = Duration::from_millis(120);
/// Decoded covers kept for folders already previewed.
const ART_CACHE: usize = 24;

#[derive(Clone, Debug)]
pub enum Entry {
    Dir(String),
    Song(Box<Song>, Box<Meta>),
}

impl Entry {
    pub fn uri(&self) -> &str {
        match self {
            Entry::Dir(u) => u,
            Entry::Song(s, _) => &s.file,
        }
    }

    /// The last path component.
    pub fn name(&self) -> &str {
        let u = self.uri();
        u.rsplit('/').next().unwrap_or(u)
    }
}

/// A folder's listing.
#[derive(Clone, Debug, Default)]
pub struct Level {
    pub uri: String,
    pub entries: Vec<Entry>,
    pub sel: usize,
    pub scroll: usize,
}

/// What the preview column shows for the entry under the cursor.
#[derive(Clone, Debug)]
pub struct Preview {
    /// The entry it's for.
    pub uri: String,
    /// A folder's contents (empty for a song).
    pub entries: Vec<Entry>,
}

#[derive(Default)]
pub struct Browser {
    /// Root first; the last is the current folder.
    pub levels: Vec<Level>,
    pub preview: Option<Preview>,
    /// When the cursor last moved, until the preview catches up.
    pub moved_at: Option<Instant>,
    /// Marked entries, by URI.
    pub marked: HashSet<String>,
    covers: HashMap<String, Option<Rc<Visual>>>,
    cover_order: VecDeque<String>,
}

fn list(conn: &mut Conn, uri: &str) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut song: Option<Song> = None;
    let flush = |song: &mut Option<Song>, entries: &mut Vec<Entry>| {
        if let Some(s) = song.take() {
            let m = Meta::of(&s);
            entries.push(Entry::Song(Box::new(s), Box::new(m)));
        }
    };
    for (k, v) in conn.command(&format!("lsinfo {}", quote(uri)))? {
        match k.as_str() {
            "directory" => {
                flush(&mut song, &mut entries);
                entries.push(Entry::Dir(v));
            }
            "file" => {
                flush(&mut song, &mut entries);
                song = Some(Song { file: v, ..Default::default() });
            }
            // playlists aren't browsable here
            "playlist" => flush(&mut song, &mut entries),
            _ => {
                if let Some(s) = song.as_mut() {
                    s.apply(&k, &v);
                }
            }
        }
    }
    flush(&mut song, &mut entries);
    entries.sort_by(|a, b| match (a, b) {
        (Entry::Dir(x), Entry::Dir(y)) => natural_cmp(x, y),
        (Entry::Dir(_), _) => std::cmp::Ordering::Less,
        (_, Entry::Dir(_)) => std::cmp::Ordering::Greater,
        (Entry::Song(x, mx), Entry::Song(y, my)) => (mx.disc.unwrap_or(0), mx.track.unwrap_or(0))
            .cmp(&(my.disc.unwrap_or(0), my.track.unwrap_or(0)))
            .then_with(|| natural_cmp(&x.file, &y.file)),
    });
    Ok(entries)
}

impl Browser {
    pub fn is_open(&self) -> bool {
        !self.levels.is_empty()
    }

    pub fn open_root(&mut self, conn: &mut Conn) -> Result<()> {
        self.levels = vec![Level { entries: list(conn, "")?, ..Default::default() }];
        self.touch();
        Ok(())
    }

    pub fn current(&self) -> &Level {
        self.levels.last().expect("browser is open")
    }

    fn current_mut(&mut self) -> &mut Level {
        self.levels.last_mut().expect("browser is open")
    }

    pub fn parent(&self) -> Option<&Level> {
        self.levels.len().checked_sub(2).map(|i| &self.levels[i])
    }

    pub fn selected(&self) -> Option<&Entry> {
        let l = self.current();
        l.entries.get(l.sel)
    }

    fn touch(&mut self) {
        self.moved_at = Some(Instant::now());
    }

    pub fn move_sel(&mut self, by: isize) {
        let l = self.current_mut();
        l.sel = l.sel.saturating_add_signed(by).min(l.entries.len().saturating_sub(1));
        self.touch();
    }

    pub fn set_sel(&mut self, i: usize) {
        let l = self.current_mut();
        l.sel = i.min(l.entries.len().saturating_sub(1));
        self.touch();
    }

    /// Marks or unmarks the entry under the cursor, and moves down.
    pub fn toggle_mark_selected(&mut self) {
        if let Some(u) = self.selected().map(|e| e.uri().to_owned()) {
            toggle(&mut self.marked, u);
        }
        self.move_sel(1);
    }

    pub fn invert_marks(&mut self) {
        let all: Vec<String> = self.current().entries.iter().map(|e| e.uri().to_owned()).collect();
        for u in all {
            toggle(&mut self.marked, u);
        }
    }

    /// Opens the folder under the cursor. Returns false on a song.
    pub fn enter(&mut self, conn: &mut Conn) -> Result<bool> {
        let Some(Entry::Dir(uri)) = self.selected().cloned() else { return Ok(false) };
        let entries = match &self.preview {
            Some(p) if p.uri == uri => p.entries.clone(),
            _ => list(conn, &uri)?,
        };
        self.levels.push(Level { uri, entries, ..Default::default() });
        self.touch();
        Ok(true)
    }

    pub fn up(&mut self) {
        if self.levels.len() > 1 {
            self.levels.pop();
            self.touch();
        }
    }

    /// Reloads every open folder after a database update, keeping cursors
    /// on the same entries where they still exist.
    pub fn refresh(&mut self, conn: &mut Conn) -> Result<()> {
        for l in &mut self.levels {
            let at = l.entries.get(l.sel).map(|e| e.uri().to_owned());
            l.entries = list(conn, &l.uri)?;
            if let Some(at) = at {
                l.sel = l.entries.iter().position(|e| e.uri() == at).unwrap_or(l.sel);
            }
            l.sel = l.sel.min(l.entries.len().saturating_sub(1));
        }
        self.preview = None;
        self.touch();
        Ok(())
    }

    /// Opens the folders down to `uri` (a song or folder) and puts the
    /// cursor on it.
    pub fn reveal(&mut self, conn: &mut Conn, uri: &str) -> Result<()> {
        self.open_root(conn)?;
        let parts: Vec<&str> = uri.split('/').collect();
        for depth in 1..=parts.len() {
            let target = parts[..depth].join("/");
            let Some(i) = self.current().entries.iter().position(|e| e.uri() == target) else { break };
            self.set_sel(i);
            if depth < parts.len() && !self.enter(conn)? {
                break;
            }
        }
        Ok(())
    }

    /// Loads the preview for the entry under the cursor once it has
    /// rested. Returns the cover to show (`None` = no change needed).
    pub fn settle(&mut self, conn: &mut Conn) -> Result<Option<(String, Option<Rc<Visual>>)>> {
        if !self.moved_at.is_some_and(|t| t.elapsed() >= PREVIEW_DELAY) {
            return Ok(None);
        }
        self.moved_at = None;
        let Some(entry) = self.selected().cloned() else {
            self.preview = None;
            return Ok(None);
        };
        let (entries, cover_song) = match &entry {
            Entry::Dir(uri) => {
                let entries = list(conn, uri)?;
                let cover = entries.iter().find_map(|e| match e {
                    Entry::Song(s, _) => Some(s.file.clone()),
                    Entry::Dir(_) => None,
                });
                (entries, cover)
            }
            Entry::Song(s, _) => (Vec::new(), Some(s.file.clone())),
        };
        self.preview = Some(Preview { uri: entry.uri().to_owned(), entries });

        // Covers are per folder: songs in one folder share one fetch.
        let Some(song) = cover_song else { return Ok(Some((String::new(), None))) };
        let key = song.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
        if !self.covers.contains_key(&key) {
            let v = art::fetch(conn, &song, art::PREVIEW_MAX_PX)?.map(Rc::new);
            self.covers.insert(key.clone(), v);
            self.cover_order.push_back(key.clone());
            if self.cover_order.len() > ART_CACHE
                && let Some(old) = self.cover_order.pop_front()
            {
                self.covers.remove(&old);
            }
        }
        Ok(Some((key.clone(), self.covers[&key].clone())))
    }

    /// URIs to act on: the marked entries, or the one under the cursor.
    pub fn targets(&self) -> Vec<String> {
        if !self.marked.is_empty() {
            let l = self.current();
            // keep listing order where possible
            let mut v: Vec<String> =
                l.entries.iter().map(|e| e.uri().to_owned()).filter(|u| self.marked.contains(u)).collect();
            let rest: Vec<String> = self.marked.iter().filter(|u| !v.contains(u)).cloned().collect();
            v.extend(rest);
            return v;
        }
        self.selected().map(|e| vec![e.uri().to_owned()]).unwrap_or_default()
    }
}

fn toggle(set: &mut HashSet<String>, u: String) {
    if !set.remove(&u) {
        set.insert(u);
    }
}
