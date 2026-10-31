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
    /// How long a reply may take; None waits forever (for `idle`).
    timeout: Option<Duration>,
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
}

impl Conn {
    /// A connection whose replies may take any time, as `idle`'s do.
    pub fn connect(target: &Target) -> Result<Self> {
        Self::open(target, None)
    }

    /// A connection that gives up on a reply after `timeout`, so a hung
    /// server can't freeze the caller.
    pub fn connect_timeout(target: &Target, timeout: Duration) -> Result<Self> {
        Self::open(target, Some(timeout))
    }

    fn open(target: &Target, timeout: Option<Duration>) -> Result<Self> {
        let (r, w): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match &target.addr {
            Addr::Unix(p) => {
                let s = UnixStream::connect(p).with_context(|| format!("connecting to {}", p.display()))?;
                s.set_read_timeout(timeout)?;
                (Box::new(s.try_clone()?), Box::new(s))
            }
            Addr::Tcp(a) => {
                let s = TcpStream::connect(a).with_context(|| format!("connecting to {a}"))?;
                s.set_read_timeout(timeout)?;
                (Box::new(s.try_clone()?), Box::new(s))
            }
        };
        Self::start(target.clone(), timeout, BufReader::new(r), w)
    }

    /// Reads the greeting and logs in over an open stream.
    fn start(
        target: Target,
        timeout: Option<Duration>,
        reader: BufReader<Box<dyn Read + Send>>,
        writer: Box<dyn Write + Send>,
    ) -> Result<Self> {
        let password = target.password.clone();
        let mut conn = Self { target, timeout, reader, writer };

        let greeting = conn.read_line()?;
        if !greeting.starts_with("OK MPD ") {
            bail!("unexpected greeting from MPD: {greeting:?}");
        }
        if let Some(pw) = password {
            conn.exchange(&format!("password {}", quote(&pw)))?;
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
            let size = pairs.iter().find(|(k, _)| k == "size").and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
            if data.len() >= size {
                return Ok(Some(data));
            }
        }
    }

    /// Sends `cmd`. Any failure but MPD's own refusal leaves the
    /// connection unusable (a reply may be half read), so it reconnects.
    /// Only when MPD had dropped the connection is `cmd` retried: then it
    /// never ran. After a timeout or a garbled reply it may have, so the
    /// error is returned instead.
    fn request(&mut self, cmd: &str) -> Result<(Pairs, Option<Vec<u8>>)> {
        match self.exchange(cmd) {
            Err(e) if !e.is::<Ack>() => {
                *self = Self::open(&self.target, self.timeout)?;
                if dropped(&e) { self.exchange(cmd) } else { Err(e) }
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
            let (k, v) = line.split_once(": ").ok_or_else(|| anyhow!("malformed line from MPD: {line:?}"))?;
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
            return Err(Closed.into());
        }
        line.truncate(line.trim_end_matches('\n').len());
        Ok(line)
    }
}

/// The server closed the connection.
#[derive(Debug)]
struct Closed;

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MPD closed the connection")
    }
}

impl std::error::Error for Closed {}

/// Whether `e` means the connection was already gone when the command was
/// sent (MPD's idle timeout, a restart), rather than lost mid-reply.
fn dropped(e: &anyhow::Error) -> bool {
    use std::io::ErrorKind::*;
    e.is::<Closed>()
        || e.downcast_ref::<std::io::Error>()
            .is_some_and(|io| matches!(io.kind(), BrokenPipe | ConnectionReset | ConnectionAborted | NotConnected))
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
    /// Queue position and id of the current song.
    pub song_pos: Option<usize>,
    pub song_id: Option<u32>,
    /// Id of the song MPD will play next.
    pub next_id: Option<u32>,
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
                "songid" => s.song_id = v.parse().ok(),
                "nextsongid" => s.next_id = v.parse().ok(),
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
    /// `samplerate:bits:channels`.
    pub format: Option<String>,
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
            } else if let Some(s) = songs.last_mut() {
                s.apply(k, v);
            }
        }
        songs
    }

    /// Takes one `key: value` line of a song's description.
    pub fn apply(&mut self, k: &str, v: &str) {
        let num = |v: &str| v.split('/').next().and_then(|n| n.trim().parse().ok());
        match k {
            "Title" => self.title = Some(v.to_owned()),
            "Artist" => self.artist = Some(v.to_owned()),
            "Album" => self.album = Some(v.to_owned()),
            "AlbumArtist" => self.album_artist = Some(v.to_owned()),
            "Date" => self.date = Some(v.to_owned()),
            // "3" or "3/12"
            "Track" => self.track = num(v),
            "Disc" => self.disc = num(v),
            "duration" => self.duration = secs(v),
            "Format" => self.format = Some(v.to_owned()),
            "Id" => self.id = v.parse().ok(),
            _ => {}
        }
    }
}

fn secs(v: &str) -> Duration {
    v.parse::<f64>().ok().and_then(|f| Duration::try_from_secs_f64(f).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Collects what the client sends.
    #[derive(Clone, Default)]
    struct Sent(Arc<Mutex<Vec<u8>>>);

    impl Write for Sent {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A connection to a fake server that replies with `replies` in turn
    /// (after the greeting and the `binarylimit` reply).
    fn conn(replies: &[u8]) -> (Conn, Sent) {
        let mut script = b"OK MPD 0.24.0\nOK\n".to_vec();
        script.extend_from_slice(replies);
        let reader: Box<dyn Read + Send> = Box::new(Cursor::new(script));
        let sent = Sent::default();
        let target = Target { addr: Addr::Unix("/nonexistent".into()), password: None };
        let c = Conn::start(target, None, BufReader::new(reader), Box::new(sent.clone())).unwrap();
        (c, sent)
    }

    #[test]
    fn pairs_and_refusals() {
        let (mut c, sent) = conn(b"volume: 40\nstate: play\nOK\nACK [50@0] {playid} No such song\n");
        let pairs = c.command("status").unwrap();
        assert_eq!(pairs, vec![("volume".into(), "40".into()), ("state".into(), "play".into())]);
        let err = c.command("playid 9").unwrap_err();
        assert!(err.is::<Ack>(), "{err}");
        assert_eq!(err.to_string(), "MPD: [50@0] {playid} No such song");
        let sent = String::from_utf8(sent.0.lock().unwrap().clone()).unwrap();
        assert_eq!(sent, "binarylimit 1048576\nstatus\nplayid 9\n");
    }

    #[test]
    fn picture_arrives_in_chunks() {
        let mut replies = b"size: 6\ntype: image/png\nbinary: 4\n".to_vec();
        // payloads may contain newlines; each is followed by one more
        replies.extend_from_slice(b"ab\nc\nOK\nsize: 6\nbinary: 2\nde\nOK\n");
        let (mut c, sent) = conn(&replies);
        assert_eq!(c.picture("a/b.flac").unwrap().as_deref(), Some(&b"ab\ncde"[..]));
        let sent = String::from_utf8(sent.0.lock().unwrap().clone()).unwrap();
        assert!(sent.ends_with("readpicture \"a/b.flac\" 0\nreadpicture \"a/b.flac\" 4\n"), "{sent}");
    }

    #[test]
    fn no_picture_falls_back_to_cover_file() {
        // readpicture: nothing embedded; albumart: refused (no cover file)
        let (mut c, _) = conn(b"OK\nACK [50@0] {albumart} No file exists\n");
        assert_eq!(c.picture("x.mp3").unwrap(), None);
    }

    #[test]
    fn a_closed_connection_is_a_drop_but_a_timeout_is_not() {
        let (mut c, _) = conn(b"");
        let e = c.exchange("status").unwrap_err();
        assert!(dropped(&e), "{e}");
        let timeout = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        assert!(!dropped(&timeout));
        assert!(dropped(&std::io::Error::from(std::io::ErrorKind::BrokenPipe).into()));
    }

    #[test]
    fn parses_status_and_songs() {
        let pairs = |s: &str| -> Pairs {
            s.lines().map(|l| l.split_once(": ").map(|(k, v)| (k.into(), v.into())).unwrap()).collect()
        };
        let s = Status::parse(&pairs(
            "volume: -1\nrepeat: 1\nsingle: oneshot\nconsume: 0\nstate: pause\nsong: 2\nsongid: 7\n\
             elapsed: 61.5\nduration: 300.25\nbitrate: 0\nnextsongid: 8",
        ));
        assert_eq!((s.state, s.song_pos, s.song_id, s.next_id), (State::Pause, Some(2), Some(7), Some(8)));
        assert_eq!((s.volume, s.bitrate), (None, None), "no mixer, no bitrate");
        assert!(s.repeat && s.single && !s.consume && !s.random);
        assert_eq!(s.elapsed, Duration::from_millis(61_500));

        let songs = Song::parse_list(&pairs(
            "file: a.flac\nTitle: A\nTrack: 3/12\nDisc: 2\nduration: 10.5\nId: 1\nfile: b.flac\nTrack: x",
        ));
        assert_eq!(songs.len(), 2);
        assert_eq!((songs[0].title.as_deref(), songs[0].track, songs[0].disc), (Some("A"), Some(3), Some(2)));
        assert_eq!((songs[0].id, songs[0].duration), (Some(1), Duration::from_millis(10_500)));
        assert_eq!((songs[1].file.as_str(), songs[1].track), ("b.flac", None));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(r#"say "hi" \o/"#), r#""say \"hi\" \\o/""#);
    }
}
