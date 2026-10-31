//! App state and what each action does. Drawing lives in `ui.rs`; the
//! queue's display state lives in `queue.rs`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::art::Art;
use crate::config::Config;
use crate::keys::{Action, Ctx, Lookup};
use crate::meta::Meta;
use crate::mpd::{Ack, Conn, Song, State, Status};
use crate::nc::{Input, Mouse, Nc};
use crate::queue::{Displaced, Queue, Row};

/// Idle subsystems that mean the queue changed / status may have changed.
const QUEUE_EVENTS: &[&str] = &["playlist"];
const STATUS_EVENTS: &[&str] = &["player", "mixer", "options", "playlist", "update"];
/// Enough to refetch everything.
pub const ALL_EVENTS: &[&str] = &["playlist", "player"];

/// How long a message stays in the footer.
const MESSAGE_TTL: Duration = Duration::from_secs(4);
/// Two clicks on the same row within this count as a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows the mouse wheel moves the queue cursor.
const WHEEL_ROWS: isize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Queue,
    Directories,
    Search,
}

pub const TABS: [(Tab, &str); 3] = [
    (Tab::Queue, "Queue"),
    (Tab::Directories, "Directories"),
    (Tab::Search, "Search"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Repeat,
    Random,
    Consume,
    Single,
    Follow,
}

/// What's under a clickable region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Tab(Tab),
    /// The progress bar's track, starting at `Hit::x`.
    Progress,
    Mode(Mode),
    Volume,
    Row(Row),
    /// The fold arrow on an album header.
    Caret(usize),
}

/// A clickable region recorded while drawing: `w` cells from (y, x).
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub y: u32,
    pub x: u32,
    pub w: u32,
    pub target: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt {
    ClearQueue,
}

pub struct App<'nc> {
    pub(crate) nc: &'nc Nc,
    pub(crate) cfg: Config,
    pub(crate) conn: Conn,
    pub(crate) status: Status,
    pub(crate) song: Option<(Song, Meta)>,
    pub(crate) queue: Queue,
    pub(crate) fetched_at: Instant,
    pub(crate) tab: Tab,
    pub(crate) prompt: Option<Prompt>,
    pub(crate) message: Option<(String, Instant)>,
    /// Why MPD is unreachable, while it is.
    pub(crate) offline: Option<String>,
    /// Rows the queue list showed last frame, for paging.
    pub(crate) list_height: usize,
    /// Clickable regions from the last frame (drawing only has `&self` in
    /// places, hence the RefCell).
    pub(crate) hits: RefCell<Vec<Hit>>,
    last_click: Option<(Instant, Target)>,
    /// Keys typed so far of a multi-key binding (`g` of `gg`).
    pub(crate) pending: Vec<Input>,
    /// Playback follows the highlight (foobar2000-style): the highlighted
    /// song plays next. The highlight sits on the playing song until it's
    /// moved by hand (`PlayNext`, or a click); the browse cursor
    /// (`queue.sel`) moves freely without affecting playback.
    pub(crate) follow: bool,
    /// The highlighted song's id.
    pub(crate) primary: Option<u32>,
    /// The playing song, moved by `retarget` to just before the highlight.
    displaced: Option<Displaced>,
    /// Songs that appeared right after the moved song; see `place_inserted`.
    inserted: Vec<u32>,
    pub(crate) art: Art<'nc>,
}

impl<'nc> App<'nc> {
    pub fn new(nc: &'nc Nc, cfg: Config, conn: Conn) -> Result<Self> {
        let mut app = Self {
            nc,
            queue: Queue::new(cfg.group_by_album),
            follow: cfg.follow_cursor,
            cfg,
            conn,
            status: Status::default(),
            song: None,
            fetched_at: Instant::now(),
            tab: Tab::Queue,
            prompt: None,
            message: None,
            offline: None,
            list_height: 20,
            hits: RefCell::new(Vec::new()),
            last_click: None,
            pending: Vec::new(),
            primary: None,
            displaced: None,
            inserted: Vec::new(),
            art: Art::new(nc),
        };
        app.update(ALL_EVENTS)?;
        if let Some(cur) = app.current_pos() {
            app.queue.sel = app.queue.row_of_song(cur);
        }
        Ok(app)
    }

    pub fn is_playing(&self) -> bool {
        self.status.state == State::Play && self.offline.is_none()
    }

    /// Refetches whatever the idle subsystems in `changed` affect.
    pub fn update<S: AsRef<str>>(&mut self, changed: &[S]) -> Result<()> {
        let any = |set: &[&str]| changed.iter().any(|c| set.contains(&c.as_ref()));
        if any(QUEUE_EVENTS) {
            self.fetch_queue()?;
            self.place_inserted()?;
        }
        if any(STATUS_EVENTS) {
            let before = self.status.song_id;
            self.status = Status::parse(&self.conn.command("status")?);
            self.song = Song::parse(&self.conn.command("currentsong")?).map(|s| {
                let m = Meta::of(&s);
                (s, m)
            });
            self.fetched_at = Instant::now();
            self.art.load(&mut self.conn, self.song.as_ref().map(|(s, _)| s.file.as_str()))?;

            let song_changed = self.status.song_id != before;
            // The moved song is done playing, or following no longer applies.
            if self.displaced.is_some_and(|d| Some(d.id) != self.status.song_id) || !self.can_follow() {
                self.restore()?;
            }
            // The highlight rides along with playback until moved by hand.
            if self.follow && (song_changed || !self.can_follow()) {
                self.primary = self.status.song_id;
            }
        }
        if self.follow && !self.primary.is_some_and(|id| self.queue.pos_of_id(id).is_some()) {
            self.primary = self.status.song_id;
        }
        Ok(())
    }

    fn fetch_queue(&mut self) -> Result<()> {
        let known: HashSet<u32> = self.queue.songs.iter().filter_map(|s| s.id).collect();
        let songs = Song::parse_list(&self.conn.command("playlistinfo")?);
        // Songs inserted right after the moved (playing) song were meant to
        // play next (`notmpc insert`, `mpc insert`). See `place_inserted`.
        if let Some(d) = self.displaced
            && let Some(m) = songs.iter().position(|s| s.id == Some(d.id))
        {
            let fresh: Vec<u32> = songs[m + 1..].iter().map_while(|s| s.id.filter(|id| !known.contains(id))).collect();
            if !fresh.is_empty() {
                self.inserted = fresh;
            }
        }
        self.queue.set_songs(songs, self.displaced);
        Ok(())
    }

    /// Someone inserted songs to play next while the playing song was moved
    /// for following: that wins over the cursor. Put the playing song back
    /// home and the new songs right after it.
    fn place_inserted(&mut self) -> Result<()> {
        let fresh = std::mem::take(&mut self.inserted);
        if fresh.is_empty() {
            return Ok(());
        }
        self.restore()?;
        let Some(cur) = self.status.song_id.and_then(|id| self.queue.pos_of_id(id)) else { return Ok(()) };
        let cmds: Vec<String> = fresh.iter().enumerate().map(|(k, id)| format!("moveid {id} {}", cur + 1 + k)).collect();
        self.conn.command_list(&cmds)?;
        self.fetch_queue()
    }

    /// Called every loop iteration.
    pub fn tick(&mut self) -> Result<()> {
        if self.is_playing() && self.fetched_at.elapsed() >= self.cfg.status_interval {
            self.poll_status()?;
        }
        Ok(())
    }

    /// MPD announces song and state changes but not the bitrate, which
    /// varies as it plays (like rmpc, poll it). Only same-song fields are
    /// taken: song changes go through `update` so following sees them.
    fn poll_status(&mut self) -> Result<()> {
        let new = Status::parse(&self.conn.command("status")?);
        if new.song_id == self.status.song_id && new.state == self.status.state {
            self.status.bitrate = new.bitrate;
            self.status.audio = new.audio;
            self.status.elapsed = new.elapsed;
        }
        self.fetched_at = Instant::now();
        Ok(())
    }

    /// Following only makes sense when MPD plays the queue in order.
    fn can_follow(&self) -> bool {
        let s = &self.status;
        self.follow && !s.random && !s.single && s.state != State::Stop
    }

    /// The playing song's queue position, looked up by id: MPD's reported
    /// position goes stale while songs are being moved.
    pub fn current_pos(&self) -> Option<usize> {
        self.status.song_id.and_then(|id| self.queue.pos_of_id(id))
    }

    /// Makes the highlighted song play next, foobar2000-style, without
    /// interrupting the current one and keeping the queue's order: the
    /// playing song moves to just before it (MPD then flows into it
    /// gaplessly and continues from there), and moves back home once the
    /// next song starts. The display always shows the original order.
    fn retarget(&mut self) -> Result<()> {
        if !self.can_follow() {
            return Ok(());
        }
        let (Some(cur_id), Some(target_id)) = (self.status.song_id, self.primary) else {
            return Ok(());
        };
        self.fetch_queue()?;
        let (Some(cur), Some(target)) = (self.queue.pos_of_id(cur_id), self.queue.pos_of_id(target_id)) else {
            return Ok(());
        };
        if target == cur || Some(target) == self.queue.display_next(cur) {
            return self.restore();
        }
        if self.displaced.is_none() {
            let after = self.queue.display_prev(cur).and_then(|p| self.queue.songs[p].id);
            self.displaced = Some(Displaced { id: cur_id, after });
        }
        let to = if cur < target { target - 1 } else { target };
        if to != cur {
            self.mpd(&format!("moveid {cur_id} {to}"))?;
        }
        self.fetch_queue()
    }

    /// Moves the displaced song back to its home position.
    fn restore(&mut self) -> Result<()> {
        let Some(d) = self.displaced.take() else { return Ok(()) };
        self.fetch_queue()?;
        let Some(m) = self.queue.pos_of_id(d.id) else { return Ok(()) };
        let to = match d.after.map(|a| self.queue.pos_of_id(a)) {
            None => 0,
            Some(Some(a)) if a < m => a + 1,
            Some(Some(a)) => a,
            // Its neighbour is gone; leave it where it is.
            Some(None) => return Ok(()),
        };
        if to != m {
            self.mpd(&format!("moveid {} {to}", d.id))?;
        }
        self.fetch_queue()
    }

    /// Moves the highlight to the browse cursor's song (an album header
    /// means its first song), which then plays next. With following off,
    /// moves that song to right after the current one instead.
    fn play_next(&mut self) -> Result<()> {
        let Some(id) = self.queue.selected_first_song().and_then(|q| self.queue.songs[q].id) else {
            return Ok(());
        };
        if self.follow {
            if !self.can_follow() {
                self.say("the highlight can't lead playback in random or single mode, or while stopped");
                return Ok(());
            }
            self.primary = Some(id);
            return self.retarget();
        }
        if let Some(cur) = self.current_pos()
            && Some(id) != self.status.song_id
        {
            let from = self.queue.pos_of_id(id).unwrap_or(cur);
            let to = if from > cur { cur + 1 } else { cur };
            self.mpd(&format!("moveid {id} {to}"))?;
        }
        Ok(())
    }

    /// Elapsed time, advanced locally between fetches while playing.
    pub fn elapsed(&self) -> Duration {
        let e = match self.status.state {
            State::Play if self.offline.is_none() => self.status.elapsed + self.fetched_at.elapsed(),
            _ => self.status.elapsed,
        };
        if self.status.duration.is_zero() { e } else { e.min(self.status.duration) }
    }

    pub fn say(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    pub fn current_message(&self) -> Option<&str> {
        self.message.as_ref().filter(|(_, at)| at.elapsed() < MESSAGE_TTL).map(|(m, _)| m.as_str())
    }

    /// Runs an MPD command; a server-side refusal becomes a footer message
    /// instead of an error.
    fn mpd(&mut self, cmd: &str) -> Result<()> {
        match self.conn.command(cmd) {
            Err(e) if e.is::<Ack>() => {
                self.say(e.to_string());
                Ok(())
            }
            r => r.map(drop),
        }
    }

    /// Returns false to quit.
    pub fn handle(&mut self, input: Input) -> Result<bool> {
        if let Some(prompt) = self.prompt.take() {
            if input.id == 'y' as u32 && !input.ctrl {
                match prompt {
                    Prompt::ClearQueue => {
                        self.displaced = None;
                        self.mpd("clear")?;
                    }
                }
            }
            return Ok(true);
        }
        let ctx = match self.tab {
            Tab::Queue => Ctx::Queue,
            // The other tabs only have global keys until they're built.
            _ => Ctx::Global,
        };
        self.pending.push(input);
        let action = match self.cfg.keys.lookup(ctx, &self.pending) {
            Lookup::Action(a) => Some(a.clone()),
            Lookup::Prefix => return Ok(true),
            // A key that doesn't continue the sequence starts a new one.
            Lookup::None if self.pending.len() > 1 => {
                self.pending.clear();
                return self.handle(input);
            }
            Lookup::None => None,
        };
        self.pending.clear();
        match action {
            Some(a) => self.act(a),
            None => Ok(true),
        }
    }

    fn act(&mut self, action: Action) -> Result<bool> {
        use Action::*;
        let s = &self.status;
        let step = self.cfg.volume_step;
        let page = self.list_height.max(2) as isize;
        match action {
            Quit => return Ok(false),
            TogglePause => {
                let cmd = if s.state == State::Play { "pause 1" } else { "play" };
                self.mpd(cmd)?;
            }
            Stop => self.mpd("stop")?,
            NextTrack => self.mpd("next")?,
            PreviousTrack => {
                // "previous" means the song before it in the real order
                self.restore()?;
                self.mpd("previous")?;
            }
            SeekForward => self.mpd(&format!("seekcur +{}", self.cfg.seek_step))?,
            SeekBack => self.mpd(&format!("seekcur -{}", self.cfg.seek_step))?,
            VolumeUp | VolumeDown => match s.volume {
                Some(v) => {
                    let v = if action == VolumeUp { v.saturating_add(step).min(100) } else { v.saturating_sub(step) };
                    self.mpd(&format!("setvol {v}"))?;
                }
                None => self.say("MPD has no mixer"),
            },
            ToggleRepeat => self.mpd(&format!("repeat {}", u8::from(!s.repeat)))?,
            ToggleRandom => self.mpd(&format!("random {}", u8::from(!s.random)))?,
            ToggleSingle => self.mpd(&format!("single {}", u8::from(!s.single)))?,
            ToggleConsume => self.mpd(&format!("consume {}", u8::from(!s.consume)))?,
            ToggleFollow => {
                self.follow = !self.follow;
                if self.follow {
                    self.primary = self.status.song_id;
                } else {
                    self.restore()?;
                    self.primary = None;
                }
                self.say(if self.follow { "playback follows the highlight" } else { "playback follows the queue" });
            }
            NextTab | PreviousTab => {
                let i = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
                let n = TABS.len();
                let i = if action == NextTab { (i + 1) % n } else { (i + n - 1) % n };
                self.tab = TABS[i].0;
            }
            SwitchToTab(name) => match TABS.iter().find(|(_, label)| label.eq_ignore_ascii_case(&name)) {
                Some((t, _)) => self.tab = *t,
                None => self.say(format!("no tab named {name:?}")),
            },
            Update => {
                self.mpd("update")?;
                self.say("updating database…");
            }
            Rescan => {
                self.mpd("rescan")?;
                self.say("rescanning database…");
            }

            Up | Down | UpHalf | DownHalf | PageUp | PageDown => {
                let by = match action {
                    Up => -1,
                    Down => 1,
                    UpHalf => -page / 2,
                    DownHalf => page / 2,
                    PageUp => -page,
                    _ => page,
                };
                self.queue.move_sel(by);
            }
            Top | Bottom => {
                self.queue.sel = if action == Top { 0 } else { self.queue.rows.len().saturating_sub(1) };
            }
            Left => match self.queue.selected() {
                Some(Row::Group(g)) => self.queue.set_collapsed(g, true),
                Some(Row::Song(q)) if self.queue.grouping => {
                    let g = self.queue.group_of_song(q);
                    self.queue.set_collapsed(g, true);
                }
                _ => {}
            },
            Right => {
                if let Some(Row::Group(g)) = self.queue.selected() {
                    self.queue.set_collapsed(g, false);
                }
            }
            CollapseAll => self.queue.set_all_collapsed(true),
            ExpandAll => self.queue.set_all_collapsed(false),
            Confirm | Play => self.play_selected()?,
            Delete => self.delete()?,
            DeleteAll => self.prompt = Some(Prompt::ClearQueue),
            MoveUp | MoveDown => self.move_song(action == MoveUp)?,
            JumpToCurrent => {
                if let Some(cur) = self.current_pos() {
                    self.queue.sel = self.queue.row_of_song(cur);
                }
            }
            PlayNext => self.play_next()?,
            Shuffle => {
                self.displaced = None;
                self.mpd("shuffle")?;
            }
            Select => {
                let ids: Vec<u32> = self.queue.selected_songs().iter().filter_map(|&q| self.queue.songs[q].id).collect();
                let all_marked = ids.iter().all(|id| self.queue.marked.contains(id));
                for id in ids {
                    if all_marked {
                        self.queue.marked.remove(&id);
                    } else {
                        self.queue.marked.insert(id);
                    }
                }
                self.queue.move_sel(1);
            }
            InvertSelection => {
                let q = &mut self.queue;
                q.marked = q.songs.iter().filter_map(|s| s.id).filter(|id| !q.marked.contains(id)).collect();
            }
            Close => self.queue.marked.clear(),
            PaneUp | PaneDown | PaneLeft | PaneRight | Unbind => {}
            other => self.say(format!("{other:?} isn't implemented yet")),
        }
        Ok(true)
    }

    fn play_selected(&mut self) -> Result<()> {
        if let Some(id) = self.queue.selected_first_song().and_then(|q| self.queue.songs[q].id) {
            self.mpd(&format!("playid {id}"))?;
        }
        Ok(())
    }

    /// Deletes the marked songs, or what's under the cursor (a song, or a
    /// whole album on its header).
    fn delete(&mut self) -> Result<()> {
        let ids: Vec<u32> = if self.queue.marked.is_empty() {
            self.queue.selected_songs().iter().filter_map(|&q| self.queue.songs[q].id).collect()
        } else {
            self.queue.marked.drain().collect()
        };
        let cmds: Vec<String> = ids.iter().map(|id| format!("deleteid {id}")).collect();
        match self.conn.command_list(&cmds) {
            Err(e) if e.is::<Ack>() => self.say(e.to_string()),
            r => r?,
        }
        Ok(())
    }

    /// Swaps the cursor's song with its neighbour in the real queue order.
    fn move_song(&mut self, up: bool) -> Result<()> {
        self.restore()?;
        let Some(Row::Song(q)) = self.queue.selected() else { return Ok(()) };
        let Some(id) = self.queue.songs[q].id else { return Ok(()) };
        let to = if up { q.checked_sub(1) } else { Some(q + 1).filter(|&p| p < self.queue.songs.len()) };
        if let Some(to) = to {
            self.mpd(&format!("moveid {id} {to}"))?;
            self.fetch_queue()?;
        }
        Ok(())
    }

    pub fn click(&mut self, m: Mouse) -> Result<()> {
        self.prompt = None;
        let hit = self.hits.borrow().iter().find(|h| h.y == m.y && (h.x..h.x + h.w).contains(&m.x)).copied();
        let (wheel_up, wheel_down) = (m.button == 4, m.button == 5);
        let double = |t: Target, last: Option<(Instant, Target)>| {
            last.is_some_and(|(at, prev)| prev == t && at.elapsed() < DOUBLE_CLICK)
        };
        match hit.map(|h| (h, h.target)) {
            Some((_, Target::Volume)) if wheel_up => _ = self.act(Action::VolumeUp)?,
            Some((_, Target::Volume)) if wheel_down => _ = self.act(Action::VolumeDown)?,
            _ if (wheel_up || wheel_down) && self.tab == Tab::Queue => {
                self.queue.move_sel(if wheel_up { -WHEEL_ROWS } else { WHEEL_ROWS });
            }
            _ if m.button != 1 => {}
            Some((_, Target::Tab(t))) => self.tab = t,
            Some((h, Target::Progress)) => {
                let d = self.status.duration.as_secs_f64();
                if d > 0.0 && h.w > 0 {
                    let frac = f64::from(m.x - h.x) / f64::from(h.w);
                    self.mpd(&format!("seekcur {:.1}", d * frac))?;
                }
            }
            Some((_, Target::Mode(mode))) => {
                let a = match mode {
                    Mode::Repeat => Action::ToggleRepeat,
                    Mode::Random => Action::ToggleRandom,
                    Mode::Consume => Action::ToggleConsume,
                    Mode::Single => Action::ToggleSingle,
                    Mode::Follow => Action::ToggleFollow,
                };
                self.act(a)?;
            }
            Some((_, Target::Caret(g))) => {
                let collapse = !self.queue.collapsed.contains(&self.queue.groups[g].key);
                self.queue.set_collapsed(g, collapse);
            }
            Some((_, t @ Target::Row(row))) => {
                if let Some(r) = self.queue.rows.iter().position(|&x| x == row) {
                    self.queue.sel = r;
                }
                if double(t, self.last_click) {
                    self.last_click = None;
                    self.play_selected()?;
                } else {
                    self.last_click = Some((Instant::now(), t));
                    // A click moves the highlight too: that song plays next.
                    if self.follow && self.can_follow() {
                        self.play_next()?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
