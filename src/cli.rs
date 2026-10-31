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

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::enqueue::{Item, Where, enqueue, natural_cmp};
use crate::mpd::{Conn, Target};

const AUDIO: &[&str] = &[
    "flac", "mp3", "ogg", "oga", "opus", "m4a", "mp4", "aac", "wav", "aif", "aiff", "wv", "ape", "dsf", "dff",
    "mpc", "wma",
];

pub fn run(at: Where, paths: &[String]) -> Result<()> {
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
    // By database URI when under the music directory, falling back to
    // file:// (e.g. not scanned yet).
    let items: Vec<Item> = files
        .iter()
        .map(|f| {
            let rel = music_dir.as_deref().and_then(|d| f.strip_prefix(d).ok()).and_then(Path::to_str);
            rel.map(str::to_owned).into_iter().chain([format!("file://{}", f.display())]).collect()
        })
        .collect();
    let n = enqueue(&mut conn, &items, at)?;
    eprintln!("notmpc: queued {n} song{}", if n == 1 { "" } else { "s" });
    Ok(())
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
