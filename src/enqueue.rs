//! Adding songs to the queue, shared by `notmpc add/insert` and the
//! Directories tab: natural order, appended or placed after the current
//! song, and playback started on the first new song if nothing is playing.

use std::cmp::Ordering;

use anyhow::{Result, anyhow};

use crate::mpd::{Ack, Conn, State, Status, quote};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    End,
    /// Right after the current song.
    Next,
}

/// One song to queue: URIs tried in order (a database URI, then `file://`).
pub type Item = Vec<String>;

/// Queues `items` and returns how many were added.
pub fn enqueue(conn: &mut Conn, items: &[Item], at: Where) -> Result<usize> {
    let status = Status::parse(&conn.command("status")?);
    let insert_at = match at {
        Where::End => None,
        Where::Next => Some(status.song_pos.map_or(0, |p| p + 1)),
    };
    let mut first_id = None;
    for (i, item) in items.iter().enumerate() {
        let pos = insert_at.map(|p| format!(" {}", p + i)).unwrap_or_default();
        let id = add_one(conn, item, &pos)?;
        first_id = first_id.or(id);
    }
    if status.state != State::Play
        && let Some(id) = first_id
    {
        conn.command(&format!("playid {id}"))?;
    }
    Ok(items.len())
}

/// Adds the first URI of `item` MPD accepts; returns its queue id.
fn add_one(conn: &mut Conn, item: &Item, pos: &str) -> Result<Option<u32>> {
    let mut last_err = None;
    for uri in item {
        match conn.command(&format!("addid {}{pos}", quote(uri))) {
            Ok(pairs) => return Ok(pairs.iter().find(|(k, _)| k == "Id").and_then(|(_, v)| v.parse().ok())),
            Err(e) if e.is::<Ack>() => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("nothing to add")))
}

/// Every song under a database directory (or the song itself), in natural
/// order.
pub fn db_songs(conn: &mut Conn, uri: &str) -> Result<Vec<String>> {
    let mut files: Vec<String> =
        conn.command(&format!("listall {}", quote(uri)))?.into_iter().filter(|(k, _)| k == "file").map(|(_, v)| v).collect();
    files.sort_by(|a, b| natural_cmp(a, b));
    Ok(files)
}

/// Compares runs of digits by value and everything else case-insensitively.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
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
