//! What mpdscribble is up to, for a header badge. mpdscribble has no status
//! API, so this pieces it together:
//!
//! - connected: it subscribes to MPD's `mpdscribble` channel (which also
//!   takes a `love` message for the playing song);
//! - backlog: entries in its journal of not-yet-submitted scrobbles (saved
//!   every `journal_interval`, 10 minutes by default);
//! - messages: its log (syslog, or a file per its config). At `verbose = 1`
//!   it only logs problems; at 2 it also logs submissions.

use std::collections::VecDeque;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::mpd::Conn;

const CHANNEL: &str = "mpdscribble";
const POLL: Duration = Duration::from_secs(5);
const KEEP_LINES: usize = 40;
/// How much of the log to read at startup for recent messages.
const BACKFILL: u64 = 256 * 1024;
/// Problems older than this no longer raise the warning.
const RECENT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// mpdscribble isn't connected to MPD.
    Absent,
    Ok,
    /// Its latest message is a problem.
    Warning,
}

pub struct Scrobble {
    pub connected: bool,
    pub backlog: usize,
    /// Recent log lines, oldest first ("Oct 31 00:19:24 [last.fm] ...").
    pub recent: VecDeque<String>,
    /// A submission was logged since the current song started.
    pub submitted: bool,
    pub journal: Option<PathBuf>,
    pub log: Option<PathBuf>,
    log_pos: u64,
    polled: Option<Instant>,
}

impl Scrobble {
    /// Reads mpdscribble's config for its journal and log locations.
    pub fn new() -> Self {
        let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let conf = fs::read_to_string(home.join(".config/mpdscribble/mpdscribble.conf")).unwrap_or_default();
        let value = |key: &str| {
            conf.lines()
                .map(str::trim)
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim().to_owned())
        };
        let expand = |v: String| match v.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(v),
        };
        let log = match value("log").as_deref() {
            None | Some("syslog") => Some(PathBuf::from("/var/log/syslog")),
            Some("-") => None,
            Some(path) => Some(expand(path.to_owned())),
        };
        Self {
            connected: false,
            backlog: 0,
            recent: VecDeque::new(),
            submitted: false,
            journal: value("journal").map(expand),
            log,
            log_pos: 0,
            polled: None,
        }
    }

    /// Re-checks the channel subscription (on MPD's `subscription` event).
    pub fn check_channel(&mut self, conn: &mut Conn) {
        self.connected = conn.command("channels").is_ok_and(|ch| ch.iter().any(|(_, v)| v == CHANNEL));
    }

    pub fn song_changed(&mut self) {
        self.submitted = false;
    }

    /// Reads new log lines and the journal, at most every few seconds.
    pub fn poll(&mut self) {
        if self.polled.is_some_and(|t| t.elapsed() < POLL) {
            return;
        }
        let first = self.polled.is_none();
        self.polled = Some(Instant::now());
        self.backlog = self
            .journal
            .as_ref()
            .and_then(|j| fs::read_to_string(j).ok())
            .map_or(0, |t| t.lines().filter(|l| l.starts_with("a = ")).count());
        self.read_log(first);
    }

    fn read_log(&mut self, first: bool) {
        let Some(path) = &self.log else { return };
        let Ok(mut f) = File::open(path) else { return };
        let len = f.metadata().map_or(0, |m| m.len());
        if first {
            self.log_pos = len.saturating_sub(BACKFILL);
        } else if len < self.log_pos {
            self.log_pos = 0; // rotated
        }
        if f.seek(SeekFrom::Start(self.log_pos)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        // keep only whole lines; the rest is read next time
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return };
        self.log_pos += end as u64 + 1;
        let text = String::from_utf8_lossy(&buf[..end]);
        for line in text.lines() {
            // "Oct 31 00:19:24 sigil mpdscribble: [last.fm] ..." (syslog) or
            // a bare message (its own log file)
            let msg = match line.split_once(" mpdscribble: ").or_else(|| line.split_once(" mpdscribble[")) {
                Some((head, rest)) => {
                    let when = head.get(..15).unwrap_or(head);
                    let rest = rest.split_once("]: ").map_or(rest, |(_, r)| r);
                    format!("{when} {rest}")
                }
                None if !self.log.as_ref().is_some_and(|p| p.ends_with("syslog")) => line.to_owned(),
                None => continue,
            };
            if !first && is_success(&msg) {
                self.submitted = true;
            }
            self.recent.push_back(msg);
            if self.recent.len() > KEEP_LINES {
                self.recent.pop_front();
            }
        }
    }

    /// Absent if not connected; a warning if the latest message is a
    /// recent Last.fm problem (MPD connection errors are moot once it's
    /// connected again, and old ones are history).
    pub fn health(&self) -> Health {
        if !self.connected {
            return Health::Absent;
        }
        let latest = self.recent.iter().rev().find(|m| !m.to_ascii_lowercase().contains("mpd error"));
        match latest {
            Some(m) if is_problem(m) && age_secs(m).is_none_or(|a| a < RECENT.as_secs() as i64) => Health::Warning,
            _ => Health::Ok,
        }
    }

    /// Tells mpdscribble to love the playing song on Last.fm.
    pub fn love(conn: &mut Conn) -> anyhow::Result<()> {
        conn.command(&format!("sendmessage {CHANNEL} love")).map(drop)
    }
}

/// Seconds since a syslog timestamp ("Oct 31 00:19:24", local time, no
/// year). None if the line doesn't start with one.
fn age_secs(m: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mon = MONTHS.iter().position(|x| m.starts_with(x))? as i32;
    let day: i32 = m.get(4..6)?.trim().parse().ok()?;
    let mut hms = m.get(7..15)?.split(':').map(|x| x.parse::<i32>());
    let (h, mi, sec) = (hms.next()?.ok()?, hms.next()?.ok()?, hms.next()?.ok()?);
    // SAFETY: plain libc time calls on stack values.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        let year = tm.tm_year;
        let mut then = libc::tm { tm_mon: mon, tm_mday: day, tm_hour: h, tm_min: mi, tm_sec: sec, tm_isdst: -1, tm_year: year, ..std::mem::zeroed() };
        let mut t = libc::mktime(&mut then);
        if t > now {
            // December's lines read in January
            then.tm_year -= 1;
            t = libc::mktime(&mut then);
        }
        Some((now - t) as i64)
    }
}

fn is_problem(m: &str) -> bool {
    let m = m.to_ascii_lowercase();
    ["error", "failed", "invalid", "could not", "waiting", "refused"].iter().any(|w| m.contains(w))
}

fn is_success(m: &str) -> bool {
    let m = m.to_ascii_lowercase();
    !is_problem(&m) && (m.contains("submitting") || m.ends_with(" ok") || m.contains("scrobbled"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_messages() {
        assert!(is_problem("Oct 30 22:21:31 [last.fm] invalid session"));
        assert!(is_problem("Oct 29 21:53:56 [last.fm] handshake error: CURL failed"));
        assert!(!is_problem("Oct 31 01:00:00 [last.fm] submitting 1 song"));
        assert!(is_success("Oct 31 01:00:00 [last.fm] submitting 1 song"));
        assert!(is_success("Oct 31 01:00:01 [last.fm] OK"));
    }
}
