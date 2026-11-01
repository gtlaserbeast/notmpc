//! App state and what each action does. Drawing lives in `ui.rs`; the
//! queue's display state lives in `queue.rs`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::art::Art;
use crate::browser::{Browser, Entry};
use crate::config::{Config, ProgressBar};
use crate::enqueue::{self, Item, Where};
use crate::keys::{Action, Ctx, Lookup};
use crate::marquee::Marquee;
use crate::meta::Meta;
use crate::mpd::{Ack, Conn, Song, State, Status};
use crate::nc::{Input, Mouse, Nc, OwnedPlane};
use crate::overlay::Overlay;
use crate::pixelbar::PixelBar;
use crate::queue::{Displaced, Queue, Row};
use crate::search::{Edit, Search};
use crate::viz::Visualizer;

/// Idle subsystems that mean the queue changed / status may have changed.
const QUEUE_EVENTS: &[&str] = &["playlist"];
const STATUS_EVENTS: &[&str] = &["player", "mixer", "options", "playlist", "update"];
const DATABASE_EVENTS: &[&str] = &["database"];
/// Enough to refetch everything.
pub const ALL_EVENTS: &[&str] = &["playlist", "player"];

/// How long a message stays in the footer.
const MESSAGE_TTL: Duration = Duration::from_secs(4);
/// Two clicks on the same row within this count as a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows a mouse wheel step scrolls a list (Directories: moves its cursor).
const WHEEL_ROWS: isize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Queue,
    Directories,
    Search,
}

pub const TABS: [(Tab, &str); 3] = [(Tab::Queue, "Queue"), (Tab::Directories, "Directories"), (Tab::Search, "Search")];

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
    Row(List, Row),
    /// The fold arrow on an album header.
    Caret(List, usize),
    /// The Search tab's query line.
    SearchInput,
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
    /// A footer message and when it expires; `None`: at the next key.
    pub(crate) message: Option<(String, Option<Instant>)>,
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
    pub(crate) overlay_plane: Option<OwnedPlane<'nc>>,
    /// The progress bar in pixels; None for the symbol bar.
    pub(crate) pixel_bar: Option<PixelBar<'nc>>,
    pub(crate) viz: Option<Visualizer>,
    /// Whether the spectrum is drawn (`viz` stays alive once started).
    pub(crate) viz_on: bool,
    /// The spectrum's plane and the header's above it; see `layer_header`.
    pub(crate) header_plane: Option<OwnedPlane<'nc>>,
    pub(crate) viz_plane: Option<OwnedPlane<'nc>>,
    /// Each bar's (steps lit, brightness step) as last drawn.
    pub(crate) viz_shown: Vec<(u32, u8)>,
    /// Scrolls too-long text on highlighted rows.
    pub(crate) marquee: Marquee,
}

impl<'nc> App<'nc> {
    pub fn new(nc: &'nc Nc, cfg: Config, conn: Conn) -> Result<Self> {
        let pixel_bar = if cfg.progress_bar == ProgressBar::Pixel { PixelBar::new(nc) } else { None };
        let marquee = Marquee::new(cfg.marquee_speed, cfg.marquee_count.0);
        let art_style = cfg.art_style;
        let mut app = Self {
            nc,
            marquee,
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
            art: Art::new(nc, crate::art::MAX_PX, art_style),
            browser: Browser::default(),
            preview_art: Art::new(nc, crate::art::PREVIEW_MAX_PX, art_style),
            search: Search::new(),
            overlay: None,
            overlay_plane: None,
            pixel_bar,
            viz: None,
            viz_on: false,
            header_plane: None,
            viz_plane: None,
            viz_shown: Vec::new(),
        };
        if app.cfg.visualizer {
            app.viz = Some(Visualizer::new(app.cfg.visualizer_fifo.clone(), app.cfg.visualizer_delay()));
            app.viz_on = true;
        }
        app.update(ALL_EVENTS)?;
        if let Some(cur) = app.current_pos() {
            app.queue.sel = app.queue.row_of_song(cur);
        }
        Ok(app)
    }

    pub fn is_playing(&self) -> bool {
        self.status.state == State::Play && self.offline.is_none()
    }

    /// Forces every image to be re-sent on the next draw. notcurses clears
    /// the screen on a resize, and kitty drops its images with it, even
    /// when the size didn't change and so nothing would be redrawn.
    pub fn forget_images(&mut self) {
        self.art.hide();
        self.preview_art.hide();
        if let Some(bar) = self.pixel_bar.as_mut() {
            bar.hide();
        }
    }

    /// How long the main loop may wait for input: until the screen next
    /// changes by itself (a visualizer frame, the clock's next second, a
    /// search or preview due, a message expiring, text scrolling), at most
    /// a second.
    pub fn frame_interval(&self) -> Duration {
        // wake just after a deadline, not just before it
        const MARGIN: Duration = Duration::from_millis(2);
        let mut wait = Duration::from_secs(1);
        if self.viz_on && self.viz.as_ref().is_some_and(Visualizer::active) {
            wait = wait.min(Duration::from_secs(1) / self.cfg.visualizer_fps);
        }
        if let Some(step) = self.marquee.next_step() {
            wait = wait.min(step + MARGIN);
        }
        if self.is_playing() {
            let into = Duration::from_nanos(self.elapsed().subsec_nanos().into());
            wait = wait.min(Duration::from_secs(1) - into + MARGIN);
        }
        let now = Instant::now();
        let deadlines = [
            self.search.settles_at().filter(|_| self.tab == Tab::Search),
            self.browser.settles_at().filter(|_| self.tab == Tab::Directories),
            self.message.as_ref().and_then(|(_, until)| *until).filter(|_| self.current_message().is_some()),
        ];
        for d in deadlines.into_iter().flatten() {
            wait = wait.min(d.saturating_duration_since(now) + MARGIN);
        }
        wait
    }

    /// What changes on screen with time alone, apart from the visualizer.
    pub fn clock(&self) -> (u64, bool, Option<(u64, Option<usize>)>) {
        (self.elapsed().as_secs(), self.current_message().is_some(), self.marquee.step())
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
            // A cover that won't load mustn't hold up the rest.
            if let Err(e) = self.art.load(&mut self.conn, self.song.as_ref().map(|(s, _)| s.file.as_str())) {
                self.say(format!("cover: {e:#}"));
            }

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
        let cmds: Vec<String> =
            fresh.iter().enumerate().map(|(k, id)| format!("moveid {id} {}", cur + 1 + k)).collect();
        self.conn.command_list(&cmds)?;
        self.fetch_queue()
    }

    /// Called every loop iteration; true if it changed what's on screen.
    pub fn tick(&mut self) -> Result<bool> {
        let mut changed = false;
        // a little early, so polls ride the clock's wakeups
        let due = self.cfg.status_interval().saturating_sub(Duration::from_millis(100));
        if self.is_playing() && self.fetched_at.elapsed() >= due {
            self.poll_status()?;
            changed = true;
        }
        if self.tab == Tab::Search {
            changed |= self.search.settle(&mut self.conn)?;
        }
        if self.tab == Tab::Directories && self.browser.moved_at.is_some() {
            if let Some((key, cover)) = self.browser.settle(&mut self.conn)? {
                self.preview_art.set(&key, cover.as_deref());
            }
            changed |= self.browser.moved_at.is_none();
        }
        Ok(changed)
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
        let Some(to) = self.queue.follow_to(cur, target) else {
            return self.restore();
        };
        if self.displaced.is_none() {
            self.displaced = Some(self.queue.displaced_from(cur));
        }
        if to != cur {
            self.mpd(&format!("moveid {cur_id} {to}"))?;
        }
        self.fetch_queue()
    }

    /// Moves the displaced song back to its home position.
    fn restore(&mut self) -> Result<()> {
        let Some(d) = self.displaced.take() else { return Ok(()) };
        self.fetch_queue()?;
        let (Some(m), Some(to)) = (self.queue.pos_of_id(d.id), self.queue.home_of(d)) else { return Ok(()) };
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
        self.message = Some((msg.into(), Some(Instant::now() + MESSAGE_TTL)));
    }

    /// A message that stays until the next key, for one that mustn't be
    /// missed.
    pub fn say_until_key(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), None));
    }

    pub fn current_message(&self) -> Option<&str> {
        self.message.as_ref().filter(|(_, until)| until.is_none_or(|t| Instant::now() < t)).map(|(m, _)| m.as_str())
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
        if matches!(self.message, Some((_, None))) {
            self.message = None;
        }
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
            Some(Confirm | Select) => self.toggle_output()?,
            _ if input.id == crate::nc::key::ESC || (input.id == 'q' as u32 && !input.ctrl) => self.overlay = None,
            Some(Close | Quit | ShowHelp | ShowInfo | ShowCurrentSongInfo | ShowOutputs | ShowDecoders) => {
                self.overlay = None;
            }
            _ => {}
        }
        Ok(())
    }

    /// Toggles the output selected in the outputs panel, then shows what
    /// MPD actually did (it may refuse).
    fn toggle_output(&mut self) -> Result<()> {
        let Some(cmd) = self.overlay.as_ref().and_then(Overlay::toggle) else { return Ok(()) };
        self.mpd(&cmd)?;
        let fresh = Overlay::outputs(&mut self.conn)?;
        if let Some(o) = self.overlay.as_mut() {
            o.reload_outputs(fresh);
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
                let file = if action == ShowInfo {
                    self.selected_file()
                } else {
                    self.song.as_ref().map(|(s, _)| s.file.clone())
                };
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
            _ => {}
        }
        if self.tab == Tab::Directories && self.act_dirs(&action)? {
            return Ok(true);
        }
        if self.tab == Tab::Search && self.act_search(&action)? {
            return Ok(true);
        }
        if let Some(by) = action.list_step(self.page()) {
            self.queue.move_sel(by);
            return Ok(true);
        }
        let s = &self.status;
        let step = self.cfg.volume_step;
        match action {
            Quit => {
                // Put the song follow-mode moved back, or the queue stays
                // reordered for every other client.
                let _ = self.restore();
                return Ok(false);
            }
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
                // One reader for the session (two would split the FIFO's
                // data); it pauses while the visualizer is off.
                self.viz_on = !self.viz_on;
                match &self.viz {
                    Some(viz) => viz.set_reading(self.viz_on),
                    None if self.viz_on => {
                        self.viz = Some(Visualizer::new(self.cfg.visualizer_fifo.clone(), self.cfg.visualizer_delay()));
                    }
                    None => {}
                }
                let on = self.viz_on;
                let saved = crate::config::State::update(|s| s.visualizer = Some(on));
                self.say(format!("visualizer {}{saved}", if on { "on" } else { "off" }));
            }
            VisualizerDelayDown | VisualizerDelayUp => {
                let Some(viz) = self.viz.as_mut().filter(|_| self.viz_on) else {
                    self.say("the visualizer is off");
                    return Ok(true);
                };
                const STEP: i64 = 5;
                let ms = viz.delay().as_millis() as i64 + if action == VisualizerDelayUp { STEP } else { -STEP };
                let ms = ms.clamp(0, 2000) as u64;
                viz.set_delay(Duration::from_millis(ms));
                let saved = crate::config::State::update(|s| s.visualizer_delay_ms = Some(ms));
                self.say(format!("visualizer delay {ms} ms{saved}"));
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
            // `/` outside Search: go there and type
            EnterSearch => {
                self.set_tab(Tab::Search)?;
                self.search.editing = true;
            }
            Update => {
                self.mpd("update")?;
                self.say("updating database…");
            }
            Rescan => {
                self.mpd("rescan")?;
                self.say("rescanning database…");
            }

            Left => self.queue.collapse_selected(),
            Right => self.queue.expand_selected(),
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
            Select => self.queue.toggle_mark_selected(),
            InvertSelection => self.queue.invert_marks(),
            Close => self.queue.marked.clear(),
            PaneUp | PaneDown | PaneLeft | PaneRight | Unbind => {}
            other => self.say(format!("{other:?} isn't implemented yet")),
        }
        Ok(true)
    }

    /// Rows a page-up/down moves.
    fn page(&self) -> isize {
        self.list_height.max(2) as isize
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
        if let Some(by) = action.list_step(self.page()) {
            self.browser.move_sel(by);
            return Ok(true);
        }
        let b = &mut self.browser;
        match action {
            Left => b.up(),
            Right => _ = b.enter(&mut self.conn)?,
            Confirm => {
                if !b.enter(&mut self.conn)?
                    && let Some(Entry::Song(s, _)) = b.selected()
                {
                    // Enter on a song: queue it and play it now.
                    let item = vec![s.file.clone()];
                    self.enqueue(&[item], Where::End, true)?;
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
            Select => b.toggle_mark_selected(),
            InvertSelection => b.invert_marks(),
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
        self.enqueue(&items, at, false)?;
        self.browser.marked.clear();
        Ok(())
    }

    /// Adds `items` (`at` the end or next), optionally playing the first
    /// right away, and says so in the footer.
    fn enqueue(&mut self, items: &[Item], at: Where, play: bool) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        // Insert positions are relative to the real order.
        self.restore()?;
        let ids = enqueue::enqueue(&mut self.conn, items, at)?;
        if play && let Some(id) = ids.first() {
            self.mpd(&format!("playid {id}"))?;
        }
        self.say_queued(ids.len(), at);
        Ok(())
    }

    /// Search-tab actions on the results. Returns false for ones it
    /// doesn't handle.
    fn act_search(&mut self, action: &Action) -> Result<bool> {
        use Action::*;
        if let Some(by) = action.list_step(self.page()) {
            self.search.results.move_sel(by);
            return Ok(true);
        }
        let r = &mut self.search.results;
        match action {
            Left => r.collapse_selected(),
            Right => r.expand_selected(),
            FocusInput | EnterSearch => self.search.editing = true,
            Close if r.marked.is_empty() => self.search.editing = true,
            Close => r.marked.clear(),
            Select => r.toggle_mark_selected(),
            InvertSelection => r.invert_marks(),
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
        self.enqueue(&items, at, play)?;
        self.search.results.marked.clear();
        Ok(())
    }

    fn say_queued(&mut self, n: usize, at: Where) {
        let verb = if at == Where::Next { "playing next" } else { "queued" };
        self.say(format!("{verb}: {n} song{}", if n == 1 { "" } else { "s" }));
    }

    /// Whether a click on `t` completes a double-click; remembers it as
    /// the first click otherwise.
    fn double_click(&mut self, t: Target) -> bool {
        let double = self.last_click.is_some_and(|(at, prev)| prev == t && at.elapsed() < DOUBLE_CLICK);
        self.last_click = if double { None } else { Some((Instant::now(), t)) };
        double
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
                    self.toggle_output()?;
                }
                Some(Target::OverlayPanel | Target::OverlayLine(_)) => {}
                // a click outside the panel closes it
                _ => self.overlay = None,
            }
            return Ok(());
        }
        let wheel = match m.button {
            4 => -WHEEL_ROWS,
            5 => WHEEL_ROWS,
            _ => 0,
        };
        if wheel != 0 {
            let h = self.list_height;
            match self.tab {
                Tab::Directories => self.browser.move_sel(wheel),
                // Lists scroll the view; the cursor stays put.
                Tab::Search => self.search.results.scroll_by(wheel, h),
                Tab::Queue => self.queue.scroll_by(wheel, h),
            }
            return Ok(());
        }
        if m.button != 1 {
            return Ok(());
        }
        match hit.map(|h| (h, h.target)) {
            Some((_, t @ Target::Entry(col, i))) => {
                let double = self.double_click(t);
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
            Some((_, Target::SearchInput)) => self.search.editing = true,
            Some((_, t @ Target::Row(List::Search, row))) => {
                self.search.results.select_row(row);
                self.search.editing = false;
                if self.double_click(t) {
                    self.queue_results(false, Where::End, true)?;
                }
            }
            Some((_, Target::Caret(List::Search, g))) => self.search.results.toggle_collapsed(g),
            Some((_, Target::Tab(t))) => self.set_tab(t)?,
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
            Some((_, Target::Caret(List::Queue, g))) => self.queue.toggle_collapsed(g),
            Some((_, t @ Target::Row(List::Queue, row))) => {
                self.queue.select_row(row);
                if self.double_click(t) {
                    self.play_selected()?;
                } else if self.follow && self.can_follow() {
                    // A click moves the highlight too: that song plays next.
                    self.play_next()?;
                    // That moves the playing song, renumbering the clicked
                    // row's MPD position; re-key the first click to the row
                    // the cursor (kept on the same song) is on now, or the
                    // second click wouldn't match it.
                    if let (Some((at, _)), Some(r)) = (self.last_click, self.queue.selected()) {
                        self.last_click = Some((at, Target::Row(List::Queue, r)));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
