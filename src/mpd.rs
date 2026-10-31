//! A minimal blocking MPD protocol client.
//!
//! The protocol is line-based: send a command, read `key: value` lines until
//! `OK` or `ACK [...] message`. Binary responses (album art) aren't handled yet.

use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

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

pub struct Conn {
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
        let mut conn = Self { reader: BufReader::new(r), writer: w };

        let greeting = conn.read_line()?;
        if !greeting.starts_with("OK MPD ") {
            bail!("unexpected greeting from MPD: {greeting:?}");
        }
        if let Some(pw) = &target.password {
            conn.command(&format!("password {}", quote(pw)))?;
        }
        Ok(conn)
    }

    pub fn command(&mut self, cmd: &str) -> Result<Pairs> {
        writeln!(self.writer, "{cmd}")?;
        self.writer.flush()?;

        let mut pairs = Vec::new();
        loop {
            let line = self.read_line()?;
            if line == "OK" {
                return Ok(pairs);
            }
            if let Some(err) = line.strip_prefix("ACK ") {
                bail!("MPD: {err}");
            }
            let (k, v) = line
                .split_once(": ")
                .ok_or_else(|| anyhow!("malformed line from MPD: {line:?}"))?;
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
}

impl Song {
    /// Parses `currentsong`; None when nothing is queued.
    pub fn parse(pairs: &Pairs) -> Option<Self> {
        let mut s = Self::default();
        for (k, v) in pairs {
            match k.as_str() {
                "file" => s.file = v.clone(),
                "Title" => s.title = Some(v.clone()),
                "Artist" => s.artist = Some(v.clone()),
                "Album" => s.album = Some(v.clone()),
                _ => {}
            }
        }
        (!s.file.is_empty()).then_some(s)
    }

    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .unwrap_or_else(|| self.file.rsplit('/').next().unwrap_or(&self.file))
    }
}

fn secs(v: &str) -> Duration {
    v.parse::<f64>().ok().and_then(|f| Duration::try_from_secs_f64(f).ok()).unwrap_or_default()
}
