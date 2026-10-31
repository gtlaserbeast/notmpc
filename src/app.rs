//! App state and what each action does. Drawing lives in `ui.rs`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::art::Art;
use crate::config::Config;
use crate::keys::Action;
use crate::meta::Meta;
use crate::mpd::{Ack, Conn, Song, State, Status};
use crate::nc::{Input, Nc};

/// Idle subsystems that mean the queue changed / status may have changed.
const QUEUE_EVENTS: &[&str] = &["playlist"];
const STATUS_EVENTS: &[&str] = &["player", "mixer", "options", "playlist", "update"];

/// How long a message stays in the footer.
const MESSAGE_TTL: Duration = Duration::from_secs(4);

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

/// A line in the queue list: an album header or a song (index into `queue`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Group(usize),
    Song(usize),
}

/// A run of consecutive queue entries from one album.
#[derive(Clone, Debug)]
pub struct Group {
    pub first: usize,
    pub len: usize,
    pub total: Duration,
    /// Tracks by more than one artist (show the artist per track).
    pub various: bool,
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
    pub(crate) queue: Vec<Song>,
    pub(crate) metas: Vec<Meta>,
    pub(crate) groups: Vec<Group>,
    pub(crate) rows: Vec<Row>,
    /// `rows` index of each queue entry.
    pub(crate) song_row: Vec<usize>,
    pub(crate) fetched_at: Instant,
    pub(crate) tab: Tab,
    /// Queue cursor (a queue index) and the first visible row.
    pub(crate) sel: usize,
    pub(crate) scroll: usize,
    /// Marked songs, by queue id.
    pub(crate) marked: HashSet<u32>,
    pub(crate) prompt: Option<Prompt>,
    pub(crate) message: Option<(String, Instant)>,
    /// Rows the queue list showed last frame, for paging.
    pub(crate) list_height: usize,
    pub(crate) art: Art<'nc>,
}

impl<'nc> App<'nc> {
    pub fn new(nc: &'nc Nc, cfg: Config, conn: Conn) -> Result<Self> {
        let mut app = Self {
            nc,
            cfg,
            conn,
            status: Status::default(),
            song: None,
            queue: Vec::new(),
            metas: Vec::new(),
            groups: Vec::new(),
            rows: Vec::new(),
            song_row: Vec::new(),
            fetched_at: Instant::now(),
            tab: Tab::Queue,
            sel: 0,
            scroll: 0,
            marked: HashSet::new(),
            prompt: None,
            message: None,
            list_height: 20,
            art: Art::new(nc),
        };
        app.update(&["playlist", "player"])?;
        app.sel = app.status.song_pos.unwrap_or(0);
        Ok(app)
    }

    pub fn is_playing(&self) -> bool {
        self.status.state == State::Play
    }

    /// Refetches whatever the idle subsystems in `changed` affect.
    pub fn update<S: AsRef<str>>(&mut self, changed: &[S]) -> Result<()> {
        let any = |set: &[&str]| changed.iter().any(|c| set.contains(&c.as_ref()));
        if any(QUEUE_EVENTS) {
            self.queue = Song::parse_list(&self.conn.command("playlistinfo")?);
            self.metas = self.queue.iter().map(Meta::of).collect();
            self.regroup();
            self.sel = self.sel.min(self.queue.len().saturating_sub(1));
            let ids: HashSet<u32> = self.queue.iter().filter_map(|s| s.id).collect();
            self.marked.retain(|id| ids.contains(id));
        }
        if any(STATUS_EVENTS) {
            self.status = Status::parse(&self.conn.command("status")?);
            self.song = Song::parse(&self.conn.command("currentsong")?).map(|s| {
                let m = Meta::of(&s);
                (s, m)
            });
            self.fetched_at = Instant::now();
            self.art.load(&mut self.conn, self.song.as_ref().map(|(s, _)| s.file.as_str()))?;
        }
        Ok(())
    }

    fn regroup(&mut self) {
        self.groups.clear();
        self.rows.clear();
        self.song_row.clear();
        for (i, m) in self.metas.iter().enumerate() {
            let same = |g: &Group| {
                let first = &self.metas[g.first];
                first.album == m.album && first.album_artist == m.album_artist
            };
            match self.groups.last_mut() {
                Some(g) if self.cfg.group_by_album && same(g) => {
                    g.len += 1;
                    g.total += self.queue[i].duration;
                    g.various |= self.metas[g.first].artist != m.artist;
                }
                _ => {
                    if self.cfg.group_by_album {
                        self.rows.push(Row::Group(self.groups.len()));
                    }
                    self.groups.push(Group { first: i, len: 1, total: self.queue[i].duration, various: false });
                }
            }
            self.song_row.push(self.rows.len());
            self.rows.push(Row::Song(i));
        }
    }

    /// Elapsed time, advanced locally between fetches while playing.
    pub fn elapsed(&self) -> Duration {
        let e = match self.status.state {
            State::Play => self.status.elapsed + self.fetched_at.elapsed(),
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
                    Prompt::ClearQueue => self.mpd("clear")?,
                }
            }
            return Ok(true);
        }
        let action = match self.tab {
            Tab::Queue => self.cfg.keys.queue_action(input),
            // The other tabs only have global keys until they're built.
            _ => self.cfg.keys.global.get(&input),
        };
        match action.cloned() {
            Some(a) => self.act(a),
            None => Ok(true),
        }
    }

    fn act(&mut self, action: Action) -> Result<bool> {
        use Action::*;
        let s = &self.status;
        let step = self.cfg.volume_step;
        match action {
            Quit => return Ok(false),
            TogglePause => {
                let cmd = if s.state == State::Play { "pause 1" } else { "play" };
                self.mpd(cmd)?;
            }
            Stop => self.mpd("stop")?,
            NextTrack => self.mpd("next")?,
            PreviousTrack => self.mpd("previous")?,
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

            Up => self.move_sel(-1),
            Down => self.move_sel(1),
            UpHalf => self.move_sel(-(self.list_height as isize / 2).max(1)),
            DownHalf => self.move_sel((self.list_height as isize / 2).max(1)),
            Top => self.sel = 0,
            Bottom => self.sel = self.queue.len().saturating_sub(1),
            Confirm | Play => {
                if let Some(id) = self.selected_id() {
                    self.mpd(&format!("playid {id}"))?;
                }
            }
            Delete => self.delete()?,
            DeleteAll => self.prompt = Some(Prompt::ClearQueue),
            MoveUp | MoveDown => self.move_song(action == MoveUp)?,
            JumpToCurrent => self.sel = self.status.song_pos.unwrap_or(self.sel),
            Shuffle => self.mpd("shuffle")?,
            Select => {
                if let Some(id) = self.selected_id()
                    && !self.marked.remove(&id)
                {
                    self.marked.insert(id);
                }
                self.move_sel(1);
            }
            InvertSelection => {
                self.marked = self.queue.iter().filter_map(|s| s.id).filter(|id| !self.marked.contains(id)).collect();
            }
            Close => self.marked.clear(),
            Left | Right | PaneUp | PaneDown | PaneLeft | PaneRight => {}
            Unbind => {}
            other => self.say(format!("{other:?} isn't implemented yet")),
        }
        Ok(true)
    }

    fn move_sel(&mut self, by: isize) {
        let last = self.queue.len().saturating_sub(1);
        self.sel = self.sel.saturating_add_signed(by).min(last);
    }

    fn selected_id(&self) -> Option<u32> {
        self.queue.get(self.sel).and_then(|s| s.id)
    }

    /// Deletes the marked songs, or the selected one when nothing is marked.
    fn delete(&mut self) -> Result<()> {
        let ids: Vec<u32> = if self.marked.is_empty() {
            self.selected_id().into_iter().collect()
        } else {
            self.marked.drain().collect()
        };
        let cmds: Vec<String> = ids.iter().map(|id| format!("deleteid {id}")).collect();
        match self.conn.command_list(&cmds) {
            Err(e) if e.is::<Ack>() => self.say(e.to_string()),
            r => r?,
        }
        Ok(())
    }

    fn move_song(&mut self, up: bool) -> Result<()> {
        let Some(id) = self.selected_id() else { return Ok(()) };
        let to = if up { self.sel.checked_sub(1) } else { Some(self.sel + 1).filter(|&p| p < self.queue.len()) };
        if let Some(to) = to {
            self.mpd(&format!("moveid {id} {to}"))?;
            self.sel = to;
        }
        Ok(())
    }
}
