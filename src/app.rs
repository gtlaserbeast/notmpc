//! App state and what each action does. Drawing lives in `ui.rs`; the
//! queue's display state lives in `queue.rs`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::art::Art;
use crate::browser::{Browser, Entry};
use crate::enqueue::{self, Item, Where};
use crate::config::Config;
use crate::keys::{Action, Ctx, Lookup};
use crate::meta::Meta;
use crate::mpd::{Ack, Conn, Song, State, Status};
use crate::nc::{Input, Mouse, Nc, OwnedPlane};
use crate::overlay::Overlay;
use crate::scrobble::Scrobble;
use crate::search::{Edit, Search};
use crate::seekbar::{self, SeekBar};
use crate::viz::Visualizer;
use crate::queue::{Displaced, Queue, Row};

/// Idle subsystems that mean the queue changed / status may have changed.
const QUEUE_EVENTS: &[&str] = &["playlist"];
const STATUS_EVENTS: &[&str] = &["player", "mixer", "options", "playlist", "update"];
const DATABASE_EVENTS: &[&str] = &["database"];
const SUBSCRIPTION_EVENTS: &[&str] = &["subscription"];
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

/// Which song list a row belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Queue,
    Search,
}

/// What's under a clickable region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Tab(Tab),
    /// The progress bar's track, starting at `Hit::x`.
    Progress,
    Mode(Mode),
    Volume,
    Row(List, Row),
    /// The fold arrow on an album header.
    Caret(List, usize),
    /// The Search tab's query line.
    SearchInput,
    /// The scrobble badge in the header.
    Scrobble,
    /// The queue's collapse-all / expand-all button.
    FoldAll,
    /// An overlay panel, and one of its lines.
    OverlayPanel,
    OverlayLine(usize),
    /// A Directories entry: column (0 parent, 1 current, 2 preview), index.
    Entry(u8, usize),
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
    pub(crate) browser: Browser,
    pub(crate) preview_art: Art<'nc>,
    pub(crate) search: Search,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) scrobble: Scrobble,
    pub(crate) overlay_plane: Option<OwnedPlane<'nc>>,
    /// The waveform progress bar; None when off or the files aren't local.
    pub(crate) seekbar: Option<SeekBar<'nc>>,
    pub(crate) viz: Option<Visualizer>,
    /// Whether the spectrum is drawn (`viz` stays alive once started).
    pub(crate) viz_on: bool,
    /// The spectrum's plane, and the header's above it (transparent where
    /// there's no text). Recreated when the width changes; the header's
    /// must be created after so it stacks on top.
    pub(crate) header_plane: Option<OwnedPlane<'nc>>,
    pub(crate) viz_plane: Option<OwnedPlane<'nc>>,
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
            browser: Browser::default(),
            preview_art: Art::new(nc),
            search: Search::new(),
            overlay: None,
            scrobble: Scrobble::new(),
            overlay_plane: None,
            seekbar: None,
            viz: None,
            viz_on: false,
            header_plane: None,
            viz_plane: None,
        };
        if app.cfg.visualizer {
            app.viz = Some(Visualizer::new(app.cfg.visualizer_fifo.clone(), app.cfg.visualizer_delay));
            app.viz_on = true;
        }
        app.update(ALL_EVENTS)?;
        app.scrobble.check_channel(&mut app.conn);
        if app.cfg.waveform {
            // Only reported to local clients; without it there are no files.
            let dir = app.conn.command("config").ok().and_then(|c| {
                c.into_iter().find(|(k, _)| k == "music_directory").map(|(_, v)| PathBuf::from(v))
            });
            if dir.is_some() {
                app.seekbar = Some(SeekBar::new(nc, dir));
            }
        }
        app.scrobble.poll();
        if let Some(cur) = app.current_pos() {
            app.queue.sel = app.queue.row_of_song(cur);
        }
        Ok(app)
    }

    pub fn is_playing(&self) -> bool {
        self.status.state == State::Play && self.offline.is_none()
    }

    /// How long the main loop may wait for input before redrawing.
    pub fn frame_interval(&self) -> Duration {
        let rider = self.seekbar.as_ref().is_some_and(|s| s.animating(self.is_playing()));
        if self.viz_on && self.viz.as_ref().is_some_and(Visualizer::active) {
            let viz = Duration::from_secs(1) / self.cfg.visualizer_fps;
            if rider { viz.min(seekbar::rider_interval()) } else { viz }
        } else if rider {
            seekbar::rider_interval()
        } else if self.is_playing() {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(1)
        }
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
            if song_changed {
                self.scrobble.song_changed();
            }
            // The highlight rides along with playback until moved by hand.
            if self.follow && (song_changed || !self.can_follow()) {
                self.primary = self.status.song_id;
            }
        }
        if any(SUBSCRIPTION_EVENTS) {
            self.scrobble.check_channel(&mut self.conn);
        }
        if any(DATABASE_EVENTS) && self.browser.is_open() {
            self.browser.refresh(&mut self.conn)?;
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
        self.scrobble.poll();
        if self.is_playing() && self.fetched_at.elapsed() >= self.cfg.status_interval {
            self.poll_status()?;
        }
        if self.tab == Tab::Search {
            self.search.settle(&mut self.conn)?;
        }
        if self.tab == Tab::Directories
            && let Some((key, cover)) = self.browser.settle(&mut self.conn)?
        {
            self.preview_art.set(&key, cover);
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
        if self.overlay.is_some() {
            self.handle_overlay(input)?;
            return Ok(true);
        }
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
        if self.tab == Tab::Search && self.search.editing && self.pending.is_empty() {
            match self.search.edit(input) {
                Edit::Consumed => return Ok(true),
                Edit::Submit => {
                    self.search.run(&mut self.conn)?;
                    self.search.editing = false;
                    return Ok(true);
                }
                Edit::Leave => {
                    self.search.editing = false;
                    return Ok(true);
                }
                Edit::Pass => {}
            }
        }
        let ctx = match self.tab {
            Tab::Queue => Ctx::Queue,
            Tab::Directories => Ctx::List,
            // While typing a query only global keys apply (Tab, ...).
            Tab::Search if self.search.editing => Ctx::Global,
            Tab::Search => Ctx::List,
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

    /// Keys while a panel is open: move, toggle (outputs), close. Nothing
    /// else, so `q` closes the panel rather than quitting.
    fn handle_overlay(&mut self, input: Input) -> Result<()> {
        use Action::*;
        self.pending.push(input);
        let action = match self.cfg.keys.lookup(Ctx::List, &self.pending) {
            Lookup::Prefix => return Ok(()),
            Lookup::Action(a) => Some(a.clone()),
            Lookup::None => None,
        };
        self.pending.clear();
        let page = self.overlay_rows().max(1) as isize;
        let Some(o) = self.overlay.as_mut() else { return Ok(()) };
        match action {
            Some(Up) => o.move_by(-1, page as usize),
            Some(Down) => o.move_by(1, page as usize),
            Some(UpHalf) => o.move_by(-page / 2, page as usize),
            Some(DownHalf) => o.move_by(page / 2, page as usize),
            Some(PageUp) => o.move_by(-page, page as usize),
            Some(PageDown) => o.move_by(page, page as usize),
            Some(Top) => o.move_by(isize::MIN / 2, page as usize),
            Some(Bottom) => o.move_by(isize::MAX / 2, page as usize),
            Some(Confirm | Select) => {
                if let Some(cmd) = o.toggle() {
                    self.mpd(&cmd)?;
                }
            }
            _ if input.id == crate::nc::key::ESC || (input.id == 'q' as u32 && !input.ctrl) => self.overlay = None,
            Some(Close | Quit | ShowHelp | ShowInfo | ShowCurrentSongInfo | ShowOutputs | ShowDecoders) => {
                self.overlay = None;
            }
            _ => {}
        }
        Ok(())
    }

    /// Lines an overlay panel shows (set while drawing).
    fn overlay_rows(&self) -> usize {
        let (rows, _) = self.nc.stdplane().dim_yx();
        rows.saturating_sub(8) as usize
    }

    /// The file under the current tab's cursor, for "song info".
    fn selected_file(&self) -> Option<String> {
        match self.tab {
            Tab::Queue => self.queue.selected_first_song().map(|q| self.queue.songs[q].file.clone()),
            Tab::Search => {
                let r = &self.search.results;
                r.selected_first_song().map(|q| r.songs[q].file.clone())
            }
            Tab::Directories => match self.browser.selected() {
                Some(Entry::Song(s, _)) => Some(s.file.clone()),
                _ => None,
            },
        }
    }

    fn act(&mut self, action: Action) -> Result<bool> {
        use Action::*;
        match action {
            ShowHelp => {
                self.overlay = Some(Overlay::help(&self.cfg.keys));
                return Ok(true);
            }
            ShowCurrentSongInfo | ShowInfo => {
                let file = if action == ShowInfo { self.selected_file() } else { self.song.as_ref().map(|(s, _)| s.file.clone()) };
                match file {
                    Some(f) => self.overlay = Some(Overlay::info(&mut self.conn, &f)?),
                    None => self.say("no song there"),
                }
                return Ok(true);
            }
            ShowOutputs => {
                self.overlay = Some(Overlay::outputs(&mut self.conn)?);
                return Ok(true);
            }
            ShowDecoders => {
                self.overlay = Some(Overlay::decoders(&mut self.conn)?);
                return Ok(true);
            }
            Love => {
                match Scrobble::love(&mut self.conn) {
                    Ok(()) if self.scrobble.connected => self.say("♥ sent to mpdscribble"),
                    Ok(()) => self.say("mpdscribble isn't connected; nothing to love with"),
                    Err(e) => self.say(format!("love failed: {e:#}")),
                }
                return Ok(true);
            }
            _ => {}
        }
        if self.tab == Tab::Directories && self.act_dirs(&action)? {
            return Ok(true);
        }
        if self.tab == Tab::Search && self.act_search(&action)? {
            return Ok(true);
        }
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
            ToggleVisualizer => {
                // Its reader can't be stopped mid-read, so keep one and only
                // stop drawing; starting another would split the FIFO.
                self.viz_on = !self.viz_on;
                if self.viz_on && self.viz.is_none() {
                    self.viz = Some(Visualizer::new(self.cfg.visualizer_fifo.clone(), self.cfg.visualizer_delay));
                }
                let on = self.viz_on;
                let mut state = crate::config::State::load();
                state.visualizer = Some(on);
                let saved = state.save().map_or_else(|e| format!(" (not saved: {e})"), |()| " (saved)".into());
                self.say(format!("visualizer {}{saved}", if on { "on" } else { "off" }));
            }
            VisualizerDelayDown | VisualizerDelayUp => {
                let Some(viz) = self.viz.as_mut().filter(|_| self.viz_on) else {
                    self.say("the visualizer is off");
                    return Ok(true);
                };
                let ms = viz.delay().as_millis() as i64 + if action == VisualizerDelayUp { 10 } else { -10 };
                let ms = ms.clamp(0, 2000) as u64;
                viz.set_delay(Duration::from_millis(ms));
                let mut state = crate::config::State::load();
                state.visualizer_delay_ms = Some(ms);
                let saved = state.save();
                self.say(match saved {
                    Ok(()) => format!("visualizer delay {ms} ms (saved)"),
                    Err(e) => format!("visualizer delay {ms} ms (not saved: {e})"),
                });
            }
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
                self.set_tab(TABS[i].0)?;
            }
            SwitchToTab(name) => match TABS.iter().find(|(_, label)| label.eq_ignore_ascii_case(&name)) {
                Some((t, _)) => self.set_tab(*t)?,
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

    fn set_tab(&mut self, tab: Tab) -> Result<()> {
        self.tab = tab;
        // Arriving at an empty search: start typing straight away.
        if tab == Tab::Search && self.search.is_empty_query() {
            self.search.editing = true;
        }
        if tab == Tab::Directories && !self.browser.is_open() {
            self.browser.open_root(&mut self.conn)?;
        }
        Ok(())
    }

    /// Directories-tab actions. Returns false for ones it doesn't handle
    /// (global keys like play/pause fall through).
    fn act_dirs(&mut self, action: &Action) -> Result<bool> {
        use Action::*;
        let page = self.list_height.max(2) as isize;
        let b = &mut self.browser;
        match action {
            Up => b.move_sel(-1),
            Down => b.move_sel(1),
            UpHalf => b.move_sel(-page / 2),
            DownHalf => b.move_sel(page / 2),
            PageUp => b.move_sel(-page),
            PageDown => b.move_sel(page),
            Top => b.set_sel(0),
            Bottom => b.set_sel(usize::MAX),
            Left => b.up(),
            Right => _ = b.enter(&mut self.conn)?,
            Confirm => {
                if !b.enter(&mut self.conn)?
                    && let Some(Entry::Song(s, _)) = b.selected()
                {
                    // Enter on a song: queue it and play it now.
                    let uri = s.file.clone();
                    self.restore()?;
                    let pairs = self.conn.command(&format!("addid {}", crate::mpd::quote(&uri)))?;
                    if let Some((_, id)) = pairs.iter().find(|(k, _)| k == "Id") {
                        self.mpd(&format!("playid {id}"))?;
                    }
                }
            }
            Add | PlayNext => {
                let targets = b.targets();
                self.queue_uris(&targets, if *action == Add { Where::End } else { Where::Next })?;
            }
            AddAll => {
                let all: Vec<String> = b.current().entries.iter().map(|e| e.uri().to_owned()).collect();
                self.queue_uris(&all, Where::End)?;
            }
            Select => {
                if let Some(u) = b.selected().map(|e| e.uri().to_owned())
                    && !b.marked.remove(&u)
                {
                    b.marked.insert(u);
                }
                b.move_sel(1);
            }
            InvertSelection => {
                let all: Vec<String> = b.current().entries.iter().map(|e| e.uri().to_owned()).collect();
                for u in all {
                    if !b.marked.remove(&u) {
                        b.marked.insert(u);
                    }
                }
            }
            Close => b.marked.clear(),
            // C on Directories: open the playing song's folder.
            JumpToCurrent => {
                if let Some((s, _)) = &self.song {
                    let uri = s.file.clone();
                    self.browser.reveal(&mut self.conn, &uri)?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Queues songs under each database URI (folders recursively, natural
    /// order); clears marks.
    fn queue_uris(&mut self, uris: &[String], at: Where) -> Result<()> {
        let mut items: Vec<Item> = Vec::new();
        for u in uris {
            items.extend(enqueue::db_songs(&mut self.conn, u)?.into_iter().map(|f| vec![f]));
        }
        if items.is_empty() {
            return Ok(());
        }
        // Insert positions are relative to the real order.
        self.restore()?;
        let n = enqueue::enqueue(&mut self.conn, &items, at)?.len();
        self.browser.marked.clear();
        let verb = if at == Where::Next { "playing next" } else { "queued" };
        self.say(format!("{verb}: {n} song{}", if n == 1 { "" } else { "s" }));
        Ok(())
    }

    /// Search-tab actions on the results. Returns false for ones it
    /// doesn't handle.
    fn act_search(&mut self, action: &Action) -> Result<bool> {
        use Action::*;
        let page = self.list_height.max(2) as isize;
        let r = &mut self.search.results;
        match action {
            Up => r.move_sel(-1),
            Down => r.move_sel(1),
            UpHalf => r.move_sel(-page / 2),
            DownHalf => r.move_sel(page / 2),
            PageUp => r.move_sel(-page),
            PageDown => r.move_sel(page),
            Top => r.sel = 0,
            Bottom => r.sel = r.rows.len().saturating_sub(1),
            Left => match r.selected() {
                Some(Row::Group(g)) => r.set_collapsed(g, true),
                Some(Row::Song(q)) => {
                    let g = r.group_of_song(q);
                    r.set_collapsed(g, true);
                }
                None => {}
            },
            Right => {
                if let Some(Row::Group(g)) = r.selected() {
                    r.set_collapsed(g, false);
                }
            }
            FocusInput | EnterSearch => self.search.editing = true,
            Close if r.marked.is_empty() => self.search.editing = true,
            Close => r.marked.clear(),
            Select => {
                let ids: Vec<u32> = r.selected_songs().iter().filter_map(|&q| r.songs[q].id).collect();
                let all = ids.iter().all(|id| r.marked.contains(id));
                for id in ids {
                    if all {
                        r.marked.remove(&id);
                    } else {
                        r.marked.insert(id);
                    }
                }
                r.move_sel(1);
            }
            InvertSelection => r.marked = r.songs.iter().filter_map(|s| s.id).filter(|id| !r.marked.contains(id)).collect(),
            Add => self.queue_results(false, Where::End, false)?,
            AddAll => self.queue_results(true, Where::End, false)?,
            PlayNext => self.queue_results(false, Where::Next, false)?,
            Confirm => self.queue_results(false, Where::End, true)?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Queues search results: the marked songs or what's under the cursor
    /// (`all`: every result), optionally playing the first right away.
    fn queue_results(&mut self, all: bool, at: Where, play: bool) -> Result<()> {
        let r = &self.search.results;
        let picked: Vec<usize> = if all {
            r.order.clone()
        } else if !r.marked.is_empty() {
            r.order.iter().copied().filter(|&q| r.songs[q].id.is_some_and(|id| r.marked.contains(&id))).collect()
        } else {
            r.selected_songs()
        };
        let items: Vec<Item> = picked.iter().map(|&q| vec![r.songs[q].file.clone()]).collect();
        if items.is_empty() {
            return Ok(());
        }
        self.restore()?;
        let ids = enqueue::enqueue(&mut self.conn, &items, at)?;
        if play && let Some(id) = ids.first() {
            self.mpd(&format!("playid {id}"))?;
        }
        self.search.results.marked.clear();
        let n = ids.len();
        let verb = if at == Where::Next { "playing next" } else { "queued" };
        self.say(format!("{verb}: {n} song{}", if n == 1 { "" } else { "s" }));
        Ok(())
    }

    pub fn click(&mut self, m: Mouse) -> Result<()> {
        self.prompt = None;
        // Last drawn is on top (overlay panels over everything).
        let hit = self.hits.borrow().iter().rev().find(|h| h.y == m.y && (h.x..h.x + h.w).contains(&m.x)).copied();
        if self.overlay.is_some() {
            let rows = self.overlay_rows();
            let Some(o) = self.overlay.as_mut() else { return Ok(()) };
            match hit.map(|h| h.target) {
                _ if m.button == 4 => o.move_by(-WHEEL_ROWS, rows),
                _ if m.button == 5 => o.move_by(WHEEL_ROWS, rows),
                Some(Target::OverlayLine(i)) if m.button == 1 && o.selectable() => {
                    o.sel = i;
                    if let Some(cmd) = o.toggle() {
                        self.mpd(&cmd)?;
                    }
                }
                Some(Target::OverlayPanel | Target::OverlayLine(_)) => {}
                // a click outside the panel closes it
                _ => self.overlay = None,
            }
            return Ok(());
        }
        let (wheel_up, wheel_down) = (m.button == 4, m.button == 5);
        let double = |t: Target, last: Option<(Instant, Target)>| {
            last.is_some_and(|(at, prev)| prev == t && at.elapsed() < DOUBLE_CLICK)
        };
        match hit.map(|h| (h, h.target)) {
            Some((_, Target::Volume)) if wheel_up => _ = self.act(Action::VolumeUp)?,
            Some((_, Target::Volume)) if wheel_down => _ = self.act(Action::VolumeDown)?,
            _ if (wheel_up || wheel_down) && self.tab == Tab::Directories => {
                self.browser.move_sel(if wheel_up { -WHEEL_ROWS } else { WHEEL_ROWS });
            }
            Some((_, t @ Target::Entry(col, i))) if m.button == 1 => {
                let double = double(t, self.last_click);
                self.last_click = if double { None } else { Some((Instant::now(), t)) };
                match col {
                    0 => self.browser.up(),
                    // a click in the preview opens the folder it shows
                    2 if !self.browser.enter(&mut self.conn)? => return Ok(()),
                    _ => {}
                }
                self.browser.set_sel(i);
                if double {
                    self.act_dirs(&Action::Confirm)?;
                }
            }
            _ if (wheel_up || wheel_down) && self.tab == Tab::Search => {
                let h = self.list_height;
                self.search.results.scroll_by(if wheel_up { -WHEEL_ROWS } else { WHEEL_ROWS }, h);
            }
            Some((_, Target::SearchInput)) if m.button == 1 => self.search.editing = true,
            Some((_, t @ Target::Row(List::Search, row))) if m.button == 1 => {
                let r = &mut self.search.results;
                if let Some(i) = r.rows.iter().position(|&x| x == row) {
                    r.sel = i;
                }
                self.search.editing = false;
                if double(t, self.last_click) {
                    self.last_click = None;
                    self.queue_results(false, Where::End, true)?;
                } else {
                    self.last_click = Some((Instant::now(), t));
                }
            }
            Some((_, Target::Caret(List::Search, g))) if m.button == 1 => {
                let r = &mut self.search.results;
                let collapse = !r.collapsed.contains(&r.groups[g].key);
                r.set_collapsed(g, collapse);
            }
            // The wheel scrolls the view; the cursor stays put.
            _ if (wheel_up || wheel_down) && self.tab == Tab::Queue => {
                let h = self.list_height;
                self.queue.scroll_by(if wheel_up { -WHEEL_ROWS } else { WHEEL_ROWS }, h);
            }
            _ if m.button != 1 => {}
            Some((_, Target::Tab(t))) => self.set_tab(t)?,
            Some((_, Target::Scrobble)) => self.overlay = Some(Overlay::scrobble(&self.scrobble)),
            Some((_, Target::FoldAll)) => {
                let all = self.queue.groups.iter().all(|g| self.queue.collapsed.contains(&g.key));
                self.queue.set_all_collapsed(!all);
            }
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
            Some((_, Target::Caret(List::Queue, g))) => {
                let collapse = !self.queue.collapsed.contains(&self.queue.groups[g].key);
                self.queue.set_collapsed(g, collapse);
            }
            Some((_, t @ Target::Row(List::Queue, row))) => {
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
