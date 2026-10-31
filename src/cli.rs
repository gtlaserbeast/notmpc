//! `notmpc add PATH...` and `notmpc insert PATH...`: queue files or whole
//! directory trees from a file manager (yazi's `ma` / `mi`).
//!
//! Directories are walked in natural order (`Disc 2` before `Disc 10`,
//! folders and files alike). Files under MPD's music directory are added
//! by their database URI, anything else as `file://` (which MPD allows
//! over a local socket).
//!
//! - add: appends to the queue.
//! - insert: places the songs right after the current one.
//!
//! If nothing is playing, the first new song starts.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::mpd::{Ack, Conn, State, Status, Target, quote};

const AUDIO: &[&str] = &[
    "flac", "mp3", "ogg", "oga", "opus", "m4a", "mp4", "aac", "wav", "aif", "aiff", "wv", "ape", "dsf", "dff",
    "mpc", "wma",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Add,
    Insert,
}

pub fn run(mode: Mode, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        bail!("usage: notmpc add|insert PATH...");
    }
    let mut files = Vec::new();
    for p in paths {
        let p = fs::canonicalize(p).with_context(|| format!("{p}: no such file or directory"))?;
        collect(&p, &mut files)?;
    }
    if files.is_empty() {
        bail!("no audio files found");
    }

    let mut conn = Conn::connect(&Target::from_env())?;
    let music_dir = music_dir(&mut conn);
    let status = Status::parse(&conn.command("status")?);
    let insert_at = match mode {
        Mode::Add => None,
        Mode::Insert => Some(status.song_pos.map_or(0, |p| p + 1)),
    };

    let mut first_id = None;
    for (i, f) in files.iter().enumerate() {
        let pos = insert_at.map(|p| format!(" {}", p + i)).unwrap_or_default();
        let id = add_one(&mut conn, f, music_dir.as_deref(), &pos)?;
        first_id = first_id.or(id);
    }

    if status.state != State::Play
        && let Some(id) = first_id
    {
        conn.command(&format!("playid {id}"))?;
    }
    eprintln!("notmpc: queued {} song{}", files.len(), if files.len() == 1 { "" } else { "s" });
    Ok(())
}

/// Adds one file, by database URI when it's under the music directory,
/// falling back to `file://` (e.g. not scanned yet). Returns its queue id.
fn add_one(conn: &mut Conn, file: &Path, music_dir: Option<&Path>, pos: &str) -> Result<Option<u32>> {
    let rel = music_dir.and_then(|d| file.strip_prefix(d).ok()).and_then(Path::to_str);
    let abs = format!("file://{}", file.to_str().context("non-UTF-8 path")?);
    let mut last_err = None;
    for uri in rel.map(str::to_owned).into_iter().chain([abs]) {
        match conn.command(&format!("addid {}{pos}", quote(&uri))) {
            Ok(pairs) => return Ok(pairs.iter().find(|(k, _)| k == "Id").and_then(|(_, v)| v.parse().ok())),
            Err(e) if e.is::<Ack>() => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap()).with_context(|| format!("adding {}", file.display()))
}

/// MPD's music directory, canonicalized. Only reported to local clients.
fn music_dir(conn: &mut Conn) -> Option<PathBuf> {
    let pairs = conn.command("config").ok()?;
    let dir = pairs.iter().find(|(k, _)| k == "music_directory")?;
    fs::canonicalize(&dir.1).ok()
}

/// Audio files under `path`, in natural order.
fn collect(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort_by(|a, b| natural_cmp(&name(a), &name(b)));
        for e in entries {
            collect(&e, out)?;
        }
    } else if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
    {
        out.push(path.to_owned());
    }
    Ok(())
}

fn name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Compares runs of digits by value and everything else case-insensitively.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        s.push(c);
                    }
                    s
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["Disc 10", "Disc 2", "disc 1", "01 b.flac", "1 a.flac", "10 x.flac", "2 y.flac"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["1 a.flac", "01 b.flac", "2 y.flac", "10 x.flac", "disc 1", "Disc 2", "Disc 10"]);
    }
}
