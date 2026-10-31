//! The command line besides the UI (see `USAGE`).
//!
//! `add` / `insert` queue files or whole directory trees from a file
//! manager (yazi's `ma` / `mi`). Directories are walked in natural order (`Disc 2` before `Disc 10`,
//! folders and files alike). Files under MPD's music directory are added
//! by their database URI, anything else as `file://` (which MPD allows
//! over a local socket).
//!
//! - add: appends to the queue.
//! - insert: places the songs right after the current one.
//!
//! If nothing is playing, the first new song starts.

use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::enqueue::{Item, Where, enqueue, natural_cmp};
use crate::mpd::{Conn, Song, Status, Target, quote};

pub const USAGE: &str = "\
usage: notmpc                      run the player
       notmpc add PATH...          append files / directory trees to the queue
       notmpc insert PATH...       queue them right after the current song
       notmpc update [DIR] [--wait]
                                   scan the music directory (or DIR) for changes
       notmpc rescan [DIR] [--wait]
                                   same, re-reading unchanged files too
       notmpc albumart [FILE|-]    save the playing song's cover (default: stdout)
                                   exit 2: no cover, 3: nothing playing
       notmpc config               print an example config with every setting
       notmpc --help | --version

MPD is found as mpc finds it: MPD_HOST, MPD_PORT, else
$XDG_RUNTIME_DIR/mpd/socket, else localhost:6600.";

/// Handles `args` if they name a command; None means run the UI.
pub fn dispatch(args: &[String]) -> Option<Result<ExitCode>> {
    let ok = |r: Result<()>| r.map(|()| ExitCode::SUCCESS);
    let rest = args.get(1..).unwrap_or_default();
    Some(match args.first()?.as_str() {
        "add" => ok(run(Where::End, rest)),
        "insert" => ok(run(Where::Next, rest)),
        "update" => ok(update("update", rest)),
        "rescan" => ok(update("rescan", rest)),
        "albumart" => albumart(rest),
        "config" => {
            print!("{}", include_str!("../config.example.toml"));
            Ok(ExitCode::SUCCESS)
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        "-V" | "--version" | "version" => {
            println!("notmpc {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        other => Err(anyhow::anyhow!("unknown command {other:?}\n\n{USAGE}")),
    })
}

/// `update` / `rescan`: starts a database scan, of everything or one
/// directory (relative to the music directory); `--wait` blocks until
/// MPD has finished it.
fn update(cmd: &str, args: &[String]) -> Result<()> {
    let wait = args.iter().any(|a| a == "--wait");
    let dirs: Vec<&String> = args.iter().filter(|a| *a != "--wait").collect();
    let cmd = match dirs.as_slice() {
        [] => cmd.to_owned(),
        [dir] => format!("{cmd} {}", quote(dir.trim_end_matches('/'))),
        _ => bail!("usage: notmpc {cmd} [DIR] [--wait]"),
    };
    let mut conn = Conn::connect(&Target::from_env())?;
    let job = conn.command(&cmd)?.into_iter().find(|(k, _)| k == "updating_db").map(|(_, v)| v);
    let job = job.unwrap_or_else(|| "?".into());
    if !wait {
        eprintln!("notmpc: database update #{job} started");
        return Ok(());
    }
    // MPD runs one scan at a time and queues the rest; ours is done once
    // no scan at or before it is running.
    let ours: u64 = job.parse().unwrap_or(u64::MAX);
    loop {
        let running = conn.command("status")?.into_iter().find(|(k, _)| k == "updating_db");
        match running.and_then(|(_, v)| v.parse::<u64>().ok()) {
            Some(n) if n <= ours => thread::sleep(Duration::from_millis(250)),
            _ => break,
        }
    }
    eprintln!("notmpc: database update #{job} done");
    Ok(())
}

/// `albumart [FILE|-]`: the playing song's cover, embedded or `cover.*`,
/// with rmpc's exit codes.
fn albumart(args: &[String]) -> Result<ExitCode> {
    let out = match args {
        [] => None,
        [f] if f == "-" => None,
        [f] => Some(f),
        _ => bail!("usage: notmpc albumart [FILE|-]"),
    };
    if out.is_none() && std::io::stdout().is_terminal() {
        bail!("not writing an image to the terminal; give a FILE or redirect stdout");
    }
    let mut conn = Conn::connect(&Target::from_env())?;
    let status = Status::parse(&conn.command("status")?);
    let song = Song::parse(&conn.command("currentsong")?).filter(|_| status.state != crate::mpd::State::Stop);
    let Some(song) = song else {
        eprintln!("notmpc: nothing playing");
        return Ok(ExitCode::from(3));
    };
    let Some(picture) = conn.picture(&song.file)? else {
        eprintln!("notmpc: no cover for {}", song.file);
        return Ok(ExitCode::from(2));
    };
    match out {
        Some(f) => fs::write(f, &picture).with_context(|| format!("writing {f}"))?,
        None => std::io::stdout().write_all(&picture)?,
    }
    Ok(ExitCode::SUCCESS)
}

const AUDIO: &[&str] = &[
    "flac", "mp3", "ogg", "oga", "opus", "m4a", "mp4", "aac", "wav", "aif", "aiff", "wv", "ape", "dsf", "dff", "mpc",
    "wma",
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
    let n = enqueue(&mut conn, &items, at)?.len();
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
