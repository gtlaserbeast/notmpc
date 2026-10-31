//! The queue as displayed: MPD's order (with at most one song shown back
//! at its home position, see `App::retarget`), grouped into album runs that
//! can be collapsed, plus the cursor, marks, and scroll position. No MPD
//! calls here.

use std::collections::HashSet;
use std::time::Duration;

use crate::meta::Meta;
use crate::mpd::Song;

/// A line in the queue list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// An album header (index into `groups`).
    Group(usize),
    /// A song (index into `songs`, i.e. its MPD position).
    Song(usize),
}

/// A run of consecutive songs from one album, in display order.
#[derive(Clone, Debug)]
pub struct Group {
    /// First display index.
    pub start: usize,
    pub len: usize,
    pub total: Duration,
    /// Tracks by more than one artist (show the artist per track).
    pub various: bool,
    /// Identifies the album across refreshes (folder, album, album
    /// artist), so collapsing survives the queue changing around it: songs
    /// deleted, added, or moved for playback-follows-the-highlight.
    pub key: u64,
    /// Another group in the list has the same album and artist (show the
    /// folder to tell them apart).
    pub ambiguous: bool,
}

fn album_key(m: &Meta) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&m.dir, &m.album, &m.album_artist).hash(&mut h);
    h.finish()
}

/// A song shown away from its MPD position: `id` belongs right after
/// `after` (or first, for None).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Displaced {
    pub id: u32,
    pub after: Option<u32>,
}

/// What the cursor is on, stable across refreshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CursorKey {
    Song(u32),
    Group(u64),
}

#[derive(Default)]
pub struct Queue {
    /// In MPD order.
    pub songs: Vec<Song>,
    pub metas: Vec<Meta>,
    /// Display index → song index.
    pub order: Vec<usize>,
    /// Song index → display index.
    pub display_of: Vec<usize>,
    pub groups: Vec<Group>,
    /// Display index → group.
    pub group_of: Vec<usize>,
    /// Visible rows.
    pub rows: Vec<Row>,
    pub collapsed: HashSet<u64>,
    pub grouping: bool,
    /// Index into `rows`.
    pub sel: usize,
    pub scroll: usize,
    /// What the cursor was on at the last draw: the view only jumps to the
    /// cursor when it moved, so the wheel can scroll away from it.
    drawn_cursor: Option<CursorKey>,
    /// Marked songs, by id.
    pub marked: HashSet<u32>,
}

impl Queue {
    pub fn new(grouping: bool) -> Self {
        Self { grouping, ..Default::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.songs.is_empty()
    }

    pub fn pos_of_id(&self, id: u32) -> Option<usize> {
        self.songs.iter().position(|s| s.id == Some(id))
    }

    /// Replaces the songs, keeping the cursor on the same song or album
    /// and the view on the same top row.
    pub fn set_songs(&mut self, songs: Vec<Song>, displaced: Option<Displaced>) {
        let key = self.cursor_key();
        let top = self.row_key(self.scroll);
        self.metas = songs.iter().map(Meta::of).collect();
        self.songs = songs;
        let ids: HashSet<u32> = self.songs.iter().filter_map(|s| s.id).collect();
        self.marked.retain(|id| ids.contains(id));

        self.order = (0..self.songs.len()).collect();
        if let Some(d) = displaced
            && let Some(q) = self.pos_of_id(d.id)
        {
            self.order.retain(|&i| i != q);
            let at = match d.after.and_then(|a| self.pos_of_id(a)) {
                Some(a) => self.order.iter().position(|&i| i == a).map_or(0, |p| p + 1),
                None => 0,
            };
            self.order.insert(at, q);
        }
        self.display_of = vec![0; self.songs.len()];
        for (d, &q) in self.order.iter().enumerate() {
            self.display_of[q] = d;
        }

        self.regroup();
        let keys: HashSet<u64> = self.groups.iter().map(|g| g.key).collect();
        self.collapsed.retain(|k| keys.contains(k));
        self.rebuild_rows();
        self.restore_cursor(key);
        self.restore_top(top);
    }

    fn regroup(&mut self) {
        self.groups.clear();
        self.group_of.clear();
        for d in 0..self.order.len() {
            let q = self.order[d];
            let m = &self.metas[q];
            let joins = self.groups.last().is_some_and(|g| {
                let first = &self.metas[self.order[g.start]];
                self.grouping && first.album == m.album && first.album_artist == m.album_artist && first.dir == m.dir
            });
            if joins {
                let g = self.groups.last_mut().unwrap();
                g.len += 1;
                g.total += self.songs[q].duration;
                g.various |= self.metas[self.order[g.start]].artist != m.artist;
            } else {
                self.groups.push(Group {
                    start: d,
                    len: 1,
                    total: self.songs[q].duration,
                    various: false,
                    key: album_key(m),
                    ambiguous: false,
                });
            }
            self.group_of.push(self.groups.len() - 1);
        }
        let mut seen: std::collections::HashMap<(&str, &str), usize> = std::collections::HashMap::new();
        for g in &self.groups {
            let m = &self.metas[self.order[g.start]];
            *seen.entry((m.album.as_str(), m.album_artist.as_str())).or_default() += 1;
        }
        let ambiguous: Vec<bool> = self
            .groups
            .iter()
            .map(|g| {
                let m = &self.metas[self.order[g.start]];
                seen[&(m.album.as_str(), m.album_artist.as_str())] > 1
            })
            .collect();
        for (g, a) in self.groups.iter_mut().zip(ambiguous) {
            g.ambiguous = a;
        }
    }

    /// Recomputes visible rows after grouping or collapsing changes.
    pub fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (g, group) in self.groups.iter().enumerate() {
            if self.grouping {
                self.rows.push(Row::Group(g));
                if self.collapsed.contains(&group.key) {
                    continue;
                }
            }
            for d in group.start..group.start + group.len {
                self.rows.push(Row::Song(self.order[d]));
            }
        }
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    fn cursor_key(&self) -> Option<CursorKey> {
        self.row_key(self.sel)
    }

    fn row_key(&self, r: usize) -> Option<CursorKey> {
        match self.rows.get(r)? {
            Row::Song(q) => self.songs.get(*q)?.id.map(CursorKey::Song),
            Row::Group(g) => self.groups.get(*g).map(|g| CursorKey::Group(g.key)),
        }
    }

    /// Whether the cursor is on something else than at the last call (the
    /// view should bring it into sight).
    pub fn cursor_moved(&mut self) -> bool {
        let now = self.cursor_key();
        let moved = now != self.drawn_cursor;
        self.drawn_cursor = now;
        moved
    }

    /// Scrolls so `top` (or the album now holding it) is the first row.
    fn restore_top(&mut self, top: Option<CursorKey>) {
        let Some(top) = top else { return };
        if let Some(r) = self.find_row(top) {
            self.scroll = r;
        }
    }

    fn find_row(&self, key: CursorKey) -> Option<usize> {
        let exact = self.rows.iter().position(|r| match (r, key) {
            (Row::Song(q), CursorKey::Song(id)) => self.songs[*q].id == Some(id),
            (Row::Group(g), CursorKey::Group(k)) => self.groups[*g].key == k,
            _ => false,
        });
        exact.or_else(|| match key {
            CursorKey::Song(id) => self.pos_of_id(id).map(|q| self.row_of_song(q)),
            CursorKey::Group(_) => None,
        })
    }

    /// Puts the cursor back on `key`; a song now hidden in a collapsed
    /// album lands on the album.
    fn restore_cursor(&mut self, key: Option<CursorKey>) {
        if let Some(r) = key.and_then(|k| self.find_row(k)) {
            self.sel = r;
        }
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    pub fn group_of_song(&self, q: usize) -> usize {
        self.group_of[self.display_of[q]]
    }

    /// The row showing song `q`: its own, or its collapsed group's.
    pub fn row_of_song(&self, q: usize) -> usize {
        let g = self.group_of_song(q);
        self.rows
            .iter()
            .position(|r| *r == Row::Song(q) || (*r == Row::Group(g) && self.collapsed.contains(&self.groups[g].key)))
            .unwrap_or(0)
    }

    pub fn selected(&self) -> Option<Row> {
        self.rows.get(self.sel).copied()
    }

    /// Songs under the cursor: one, or a whole album on its header.
    pub fn selected_songs(&self) -> Vec<usize> {
        match self.selected() {
            Some(Row::Song(q)) => vec![q],
            Some(Row::Group(g)) => self.group_songs(g),
            None => Vec::new(),
        }
    }

    pub fn group_songs(&self, g: usize) -> Vec<usize> {
        let group = &self.groups[g];
        (group.start..group.start + group.len).map(|d| self.order[d]).collect()
    }

    /// The song that would play first for the cursor's row.
    pub fn selected_first_song(&self) -> Option<usize> {
        self.selected_songs().first().copied()
    }

    /// The song after `q` in display order.
    pub fn display_next(&self, q: usize) -> Option<usize> {
        self.order.get(self.display_of[q] + 1).copied()
    }

    pub fn display_prev(&self, q: usize) -> Option<usize> {
        self.display_of[q].checked_sub(1).map(|d| self.order[d])
    }

    /// For following: the MPD position to move the playing song `cur` to
    /// so that `target` plays right after it. None when `target` already
    /// comes next in display order (or is the playing song).
    pub fn follow_to(&self, cur: usize, target: usize) -> Option<usize> {
        if target == cur || self.display_next(cur) == Some(target) {
            return None;
        }
        Some(if cur < target { target - 1 } else { target })
    }

    /// Where song `q` lives in display order, to put it back later.
    pub fn displaced_from(&self, q: usize) -> Displaced {
        let after = self.display_prev(q).and_then(|p| self.songs[p].id);
        Displaced { id: self.songs[q].id.unwrap_or_default(), after }
    }

    /// The MPD position that puts a displaced song back home; None if it's
    /// gone, or its neighbour is (then it stays where it is).
    pub fn home_of(&self, d: Displaced) -> Option<usize> {
        let m = self.pos_of_id(d.id)?;
        match d.after.map(|a| self.pos_of_id(a)) {
            None => Some(0),
            Some(Some(a)) if a < m => Some(a + 1),
            Some(Some(a)) => Some(a),
            Some(None) => None,
        }
    }

    /// Scrolls the view (not the cursor), e.g. for the mouse wheel.
    pub fn scroll_by(&mut self, by: isize, visible: usize) {
        self.scroll = self.scroll.saturating_add_signed(by).min(self.rows.len().saturating_sub(visible));
    }

    pub fn move_sel(&mut self, by: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.sel = self.sel.saturating_add_signed(by).min(last);
    }

    /// Collapses or expands group `g`. A cursor on one of its songs lands
    /// on its header.
    pub fn set_collapsed(&mut self, g: usize, collapse: bool) {
        let cursor = self.cursor_key();
        let top = self.row_key(self.scroll);
        let key = self.groups[g].key;
        if collapse {
            self.collapsed.insert(key);
        } else {
            self.collapsed.remove(&key);
        }
        self.rebuild_rows();
        self.restore_cursor(cursor);
        self.restore_top(top);
    }

    /// Folds the album under the cursor (on a song, the one it's in).
    pub fn collapse_selected(&mut self) {
        match self.selected() {
            Some(Row::Group(g)) => self.set_collapsed(g, true),
            Some(Row::Song(q)) if self.grouping => self.set_collapsed(self.group_of_song(q), true),
            _ => {}
        }
    }

    pub fn expand_selected(&mut self) {
        if let Some(Row::Group(g)) = self.selected() {
            self.set_collapsed(g, false);
        }
    }

    /// Marks the songs under the cursor, or unmarks them if all already
    /// are, and moves down.
    pub fn toggle_mark_selected(&mut self) {
        let ids: Vec<u32> = self.selected_songs().iter().filter_map(|&q| self.songs[q].id).collect();
        let all = ids.iter().all(|id| self.marked.contains(id));
        for id in ids {
            if all {
                self.marked.remove(&id);
            } else {
                self.marked.insert(id);
            }
        }
        self.move_sel(1);
    }

    pub fn invert_marks(&mut self) {
        self.marked = self.songs.iter().filter_map(|s| s.id).filter(|id| !self.marked.contains(id)).collect();
    }

    pub fn set_all_collapsed(&mut self, collapse: bool) {
        let cursor = self.cursor_key();
        let top = self.row_key(self.scroll);
        self.collapsed = if collapse { self.groups.iter().map(|g| g.key).collect() } else { HashSet::new() };
        self.rebuild_rows();
        self.restore_cursor(cursor);
        self.restore_top(top);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: u32, album: &str) -> Song {
        Song {
            file: format!("{album}/{id}.flac"),
            title: Some(format!("t{id}")),
            artist: Some("a".into()),
            album: Some(album.into()),
            id: Some(id),
            ..Default::default()
        }
    }

    fn queue() -> Queue {
        let mut q = Queue::new(true);
        q.set_songs(vec![song(1, "A"), song(2, "A"), song(3, "B"), song(4, "B"), song(5, "C")], None);
        q
    }

    #[test]
    fn groups_and_rows() {
        let q = queue();
        assert_eq!(q.groups.len(), 3);
        assert_eq!(
            q.rows,
            vec![Row::Group(0), Row::Song(0), Row::Song(1), Row::Group(1), Row::Song(2), Row::Song(3), Row::Group(2), Row::Song(4)]
        );
    }

    #[test]
    fn collapsing_hides_songs_and_keeps_cursor_on_group() {
        let mut q = queue();
        q.sel = 4; // song 3 (album B)
        q.set_collapsed(1, true);
        assert_eq!(q.rows, vec![Row::Group(0), Row::Song(0), Row::Song(1), Row::Group(1), Row::Group(2), Row::Song(4)]);
        assert_eq!(q.selected(), Some(Row::Group(1)));
        assert_eq!(q.selected_songs(), vec![2, 3]);
        q.set_collapsed(1, false);
        assert_eq!(q.rows.len(), 8);
    }

    #[test]
    fn collapse_survives_queue_changes() {
        let mut q = queue();
        q.set_collapsed(1, true); // album B: songs 3, 4
        // B's first song is deleted and a song is added in front
        q.set_songs(vec![song(9, "Z"), song(1, "A"), song(2, "A"), song(4, "B"), song(5, "C")], None);
        let b = q.groups.iter().position(|g| q.metas[q.order[g.start]].album == "B").unwrap();
        assert!(q.collapsed.contains(&q.groups[b].key));
        assert!(!q.rows.contains(&Row::Song(3)), "B's song stays hidden");
    }

    #[test]
    fn displaced_song_shows_at_home() {
        let mut q = Queue::new(true);
        // MPD moved song 1 (playing) to just before song 4.
        let songs = vec![song(2, "A"), song(3, "B"), song(1, "A"), song(4, "B"), song(5, "C")];
        q.set_songs(songs, Some(Displaced { id: 1, after: None }));
        let ids: Vec<u32> = q.order.iter().map(|&i| q.songs[i].id.unwrap()).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);
        assert_eq!(q.groups.len(), 3);
        // display order says song 2 follows song 1, even though MPD has 4 next
        assert_eq!(q.display_next(2).map(|i| q.songs[i].id), Some(Some(2)));
    }

    /// MPD's `moveid`.
    fn moveid(songs: &mut Vec<Song>, id: u32, to: usize) {
        let from = songs.iter().position(|s| s.id == Some(id)).unwrap();
        let s = songs.remove(from);
        songs.insert(to, s);
    }

    fn ids(songs: &[Song]) -> Vec<u32> {
        songs.iter().map(|s| s.id.unwrap()).collect()
    }

    #[test]
    fn following_moves_the_playing_song_and_back() {
        for (cur, target) in [(1, 4), (4, 2), (5, 1), (1, 3)] {
            let mut mpd: Vec<Song> = (1..=5).map(|i| song(i, "A")).collect();
            let mut q = Queue::new(true);
            q.set_songs(mpd.clone(), None);
            let (c, t) = (q.pos_of_id(cur).unwrap(), q.pos_of_id(target).unwrap());
            let d = q.displaced_from(c);
            let Some(to) = q.follow_to(c, t) else {
                assert_eq!(target, cur + 1, "only a song that's already next needs no move");
                continue;
            };
            moveid(&mut mpd, cur, to);
            let after = ids(&mpd)[mpd.iter().position(|s| s.id == Some(cur)).unwrap() + 1];
            assert_eq!(after, target, "{cur}→{target}: MPD plays the target next");
            q.set_songs(mpd.clone(), Some(d));
            let shown: Vec<u32> = q.order.iter().map(|&i| q.songs[i].id.unwrap()).collect();
            assert_eq!(shown, vec![1, 2, 3, 4, 5], "the display keeps the real order");
            moveid(&mut mpd, cur, q.home_of(d).unwrap());
            assert_eq!(ids(&mpd), vec![1, 2, 3, 4, 5], "{cur}→{target}: restored");
        }
    }

    #[test]
    fn cursor_survives_refresh() {
        let mut q = queue();
        q.sel = 5; // song 4
        q.set_songs(vec![song(9, "Z"), song(1, "A"), song(2, "A"), song(3, "B"), song(4, "B"), song(5, "C")], None);
        assert_eq!(q.selected(), Some(Row::Song(4)));
        assert_eq!(q.songs[4].id, Some(4));
    }
}
