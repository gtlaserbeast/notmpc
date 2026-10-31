//! A minimal blocking MPD protocol client.
//!
//! The protocol is line-based: send a command, read `key: value` lines until
//! `OK` or `ACK [...] message`. Album art arrives as `binary: <n>` followed by
//! n raw bytes, in chunks fetched by offset.

use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// Bytes per album art chunk (MPD's default is 8 KiB; needs MPD ≥ 0.22.4).
const BINARY_LIMIT: usize = 1 << 20;

use anyhow::{Context, Result, anyhow, bail};

#[derive(Clone, Debug)]
pub enum Addr {
    Unix(PathBuf),
    Tcp(String),
}

/// Where to reach MPD, plus an optional password.
#[derive(Clone, Debug)]
pub struct Target {
    pub addr: Addr,
    pub password: Option<String>,
}

impl Target {
    /// Follows mpc's conventions: `MPD_HOST` (`[password@]host` or a socket
    /// path) and `MPD_PORT`, falling back to `$XDG_RUNTIME_DIR/mpd/socket`
    /// and then localhost:6600.
    pub fn from_env() -> Self {
        let (password, host) = match env::var("MPD_HOST") {
            Ok(h) => match h.split_once('@') {
                Some((pw, host)) if !pw.is_empty() => (Some(pw.to_owned()), Some(host.to_owned())),
                _ => (None, Some(h)),
            },
            Err(_) => (None, None),
        };
        let port = env::var("MPD_PORT").unwrap_or_else(|_| "6600".into());

        let addr = match host {
            Some(h) if h.starts_with('/') => Addr::Unix(h.into()),
            Some(h) => Addr::Tcp(format!("{h}:{port}")),
            None => env::var_os("XDG_RUNTIME_DIR")
                .map(|d| PathBuf::from(d).join("mpd/socket"))
                .filter(|p| p.exists())
                .map(Addr::Unix)
                .unwrap_or_else(|| Addr::Tcp(format!("localhost:{port}"))),
        };
        Self { addr, password }
    }
}

pub type Pairs = Vec<(String, String)>;

/// A connection that transparently reconnects: MPD drops clients after
/// `connection_timeout` (60s by default) without a command, and restarts.
pub struct Conn {
    target: Target,
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
}

impl Conn {
    pub fn connect(target: &Target) -> Result<Self> {
        let (r, w): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match &target.addr {
            Addr::Unix(p) => {
                let s = UnixStream::connect(p).with_context(|| format!("connecting to {}", p.display()))?;
                (Box::new(s.try_clone()?), Box::new(s))
            }
            Addr::Tcp(a) => {
                let s = TcpStream::connect(a).with_context(|| format!("connecting to {a}"))?;
                s.set_read_timeout(None)?;
                (Box::new(s.try_clone()?), Box::new(s))
            }
        };
        let mut conn = Self { target: target.clone(), reader: BufReader::new(r), writer: w };

        let greeting = conn.read_line()?;
        if !greeting.starts_with("OK MPD ") {
            bail!("unexpected greeting from MPD: {greeting:?}");
        }
        if let Some(pw) = &target.password {
            conn.exchange(&format!("password {}", quote(pw)))?;
        }
        // Older servers reject this; art then just arrives in smaller chunks.
        let _ = conn.exchange(&format!("binarylimit {BINARY_LIMIT}"));
        Ok(conn)
    }

    pub fn command(&mut self, cmd: &str) -> Result<Pairs> {
        Ok(self.request(cmd)?.0)
    }

    /// Runs several commands in one round trip. MPD stops at the first that
    /// fails.
    pub fn command_list(&mut self, cmds: &[String]) -> Result<()> {
        if cmds.is_empty() {
            return Ok(());
        }
        let body = cmds.join("\n");
        self.request(&format!("command_list_begin\n{body}\ncommand_list_end"))?;
        Ok(())
    }

    /// Fetches a song's cover: the embedded picture, else the `cover.*` file
    /// next to it. None when neither exists.
    pub fn picture(&mut self, uri: &str) -> Result<Option<Vec<u8>>> {
        // Server-side refusals (no such file, unsupported command) just mean
        // no picture; I/O errors still propagate.
        for cmd in ["readpicture", "albumart"] {
            match self.fetch_binary(cmd, uri) {
                Ok(Some(p)) => return Ok(Some(p)),
                Ok(None) => {}
                Err(e) if e.is::<Ack>() => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    fn fetch_binary(&mut self, cmd: &str, uri: &str) -> Result<Option<Vec<u8>>> {
        let mut data = Vec::new();
        loop {
            let (pairs, chunk) = self.request(&format!("{cmd} {} {}", quote(uri), data.len()))?;
            let Some(chunk) = chunk.filter(|c| !c.is_empty()) else {
                return Ok((!data.is_empty()).then_some(data));
            };
            data.extend_from_slice(&chunk);
            let size = pairs
                .iter()
                .find(|(k, _)| k == "size")
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= size {
                return Ok(Some(data));
            }
        }
    }

    /// Sends `cmd`. If the connection turns out to be dead, reconnects and
    /// retries once: a dropped connection means MPD never ran the command.
    fn request(&mut self, cmd: &str) -> Result<(Pairs, Option<Vec<u8>>)> {
        match self.exchange(cmd) {
            Err(e) if !e.is::<Ack>() => {
                *self = Self::connect(&self.target)?;
                self.exchange(cmd)
            }
            r => r,
        }
    }

    fn exchange(&mut self, cmd: &str) -> Result<(Pairs, Option<Vec<u8>>)> {
        writeln!(self.writer, "{cmd}")?;
        self.writer.flush()?;

        let mut pairs = Vec::new();
        let mut binary = None;
        loop {
            let line = self.read_line()?;
            if line == "OK" {
                return Ok((pairs, binary));
            }
            if let Some(err) = line.strip_prefix("ACK ") {
                return Err(Ack(err.to_owned()).into());
            }
            let (k, v) = line
                .split_once(": ")
                .ok_or_else(|| anyhow!("malformed line from MPD: {line:?}"))?;
            if k == "binary" {
                let mut buf = vec![0; v.parse()?];
                self.reader.read_exact(&mut buf)?;
                // the payload is followed by a newline
                self.read_line()?;
                binary = Some(buf);
                continue;
            }
            pairs.push((k.to_owned(), v.to_owned()));
        }
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("MPD closed the connection");
        }
        line.truncate(line.trim_end_matches('\n').len());
        Ok(line)
    }
}

/// An error reported by the server (`ACK [code@index] {command} message`).
#[derive(Debug)]
pub struct Ack(pub String);

impl std::fmt::Display for Ack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MPD: {}", self.0)
    }
}

impl std::error::Error for Ack {}

/// Quotes an argument for the MPD protocol.
pub fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    Play,
    Pause,
    #[default]
    Stop,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub state: State,
    pub elapsed: Duration,
    pub duration: Duration,
    /// None when MPD has no mixer.
    pub volume: Option<u8>,
    /// Queue position of the current song.
    pub song_pos: Option<usize>,
    /// kbps, while playing.
    pub bitrate: Option<u32>,
    /// `samplerate:bits:channels`, while playing.
    pub audio: Option<String>,
    /// A database update is running.
    pub updating: bool,
    pub repeat: bool,
    pub random: bool,
    pub single: bool,
    pub consume: bool,
}

impl Status {
    pub fn parse(pairs: &Pairs) -> Self {
        let mut s = Self::default();
        for (k, v) in pairs {
            match k.as_str() {
                "state" => {
                    s.state = match v.as_str() {
                        "play" => State::Play,
                        "pause" => State::Pause,
                        _ => State::Stop,
                    }
                }
                "elapsed" => s.elapsed = secs(v),
                "duration" => s.duration = secs(v),
                "song" => s.song_pos = v.parse().ok(),
                "bitrate" => s.bitrate = v.parse().ok().filter(|&b| b > 0),
                "audio" => s.audio = Some(v.clone()),
                "updating_db" => s.updating = true,
                "volume" => s.volume = v.parse::<i32>().ok().filter(|&v| v >= 0).map(|v| v as u8),
                "repeat" => s.repeat = v == "1",
                "random" => s.random = v == "1",
                // single can also be "oneshot"
                "single" => s.single = v != "0",
                "consume" => s.consume = v != "0",
                _ => {}
            }
        }
        s
    }
}

#[derive(Clone, Debug, Default)]
pub struct Song {
    pub file: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub date: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration: Duration,
    /// Queue id, stable across reordering (unlike the position).
    pub id: Option<u32>,
}

impl Song {
    /// Parses `currentsong`; None when nothing is queued.
    pub fn parse(pairs: &Pairs) -> Option<Self> {
        Self::parse_list(pairs).into_iter().next()
    }

    /// Parses a song list such as `playlistinfo`; each song starts at `file`.
    pub fn parse_list(pairs: &Pairs) -> Vec<Self> {
        let mut songs: Vec<Self> = Vec::new();
        for (k, v) in pairs {
            if k == "file" {
                songs.push(Self { file: v.clone(), ..Default::default() });
                continue;
            }
            let Some(s) = songs.last_mut() else { continue };
            match k.as_str() {
                "Title" => s.title = Some(v.clone()),
                "Artist" => s.artist = Some(v.clone()),
                "Album" => s.album = Some(v.clone()),
                "AlbumArtist" => s.album_artist = Some(v.clone()),
                "Date" => s.date = Some(v.clone()),
                // "3" or "3/12"
                "Track" => s.track = v.split('/').next().and_then(|n| n.trim().parse().ok()),
                "Disc" => s.disc = v.split('/').next().and_then(|n| n.trim().parse().ok()),
                "duration" => s.duration = secs(v),
                "Id" => s.id = v.parse().ok(),
                _ => {}
            }
        }
        songs
    }
}

fn secs(v: &str) -> Duration {
    v.parse::<f64>().ok().and_then(|f| Duration::try_from_secs_f64(f).ok()).unwrap_or_default()
}
