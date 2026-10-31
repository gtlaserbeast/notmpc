//! What to show for a song. Tags win; untagged files get artist, album,
//! year, and title inferred from the path, flagged so the UI can style
//! them differently. For example
//! `Autechre/Albums/[WARP CD 051] Autechre - Radio Mix - 1997/01  Radio Mix.flac`
//! gives Autechre / Radio Mix / 1997 / track 1 "Radio Mix".

use crate::mpd::Song;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub year: String,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    /// Some fields came from the path rather than tags.
    pub inferred: bool,
}

impl Meta {
    pub fn of(song: &Song) -> Self {
        let mut parts: Vec<&str> = song.file.split('/').collect();
        let file = parts.pop().unwrap_or(&song.file);
        let dir = parts.last().copied().unwrap_or("");
        let guess = guess_from_dir(dir, parts.first().copied());
        let (file_track, file_title) = split_track(strip_ext(file));

        let mut inferred = false;
        let mut pick = |tag: &Option<String>, guess: Option<String>| match tag {
            Some(t) if !t.is_empty() => t.clone(),
            _ => guess.inspect(|_| inferred = true).unwrap_or_default(),
        };
        let title = pick(&song.title, Some(file_title.to_owned()));
        let artist = pick(&song.artist, guess.artist.clone());
        let album = pick(&song.album, guess.album);
        let year = pick(&song.date.as_ref().map(|d| d.chars().take(4).collect()), guess.year);
        let album_artist = song.album_artist.clone().filter(|a| !a.is_empty()).unwrap_or_else(|| artist.clone());
        Self {
            title,
            artist,
            album,
            album_artist,
            year,
            track: song.track.or(file_track),
            disc: song.disc,
            inferred,
        }
    }
}

#[derive(Default)]
struct DirGuess {
    artist: Option<String>,
    album: Option<String>,
    year: Option<String>,
}

/// `[WARP CD 051] Autechre - Radio Mix - 1997` → Autechre, Radio Mix, 1997.
/// Falls back to the top-level folder for the artist.
fn guess_from_dir(dir: &str, top: Option<&str>) -> DirGuess {
    let mut name = dir.trim();
    // drop leading [catalog numbers] / (tags)
    while let Some(rest) = name.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(_, r)| r) {
        name = rest.trim_start();
    }
    let mut segs: Vec<&str> = name.split(" - ").map(str::trim).filter(|s| !s.is_empty()).collect();

    let mut g = DirGuess::default();
    let is_year = |s: &str| {
        let s = s.trim_matches(|c| c == '(' || c == ')');
        s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit())
    };
    if segs.last().is_some_and(|s| is_year(s)) {
        g.year = segs.pop().map(|s| s.trim_matches(|c| c == '(' || c == ')').to_owned());
    }
    match segs.as_slice() {
        [] => {}
        [album] => g.album = Some((*album).to_owned()),
        [artist, rest @ ..] => {
            g.artist = Some((*artist).to_owned());
            g.album = Some(rest.join(" - "));
        }
    }
    if g.artist.is_none() {
        g.artist = top.filter(|t| *t != dir).map(str::to_owned);
    }
    g
}

fn strip_ext(file: &str) -> &str {
    match file.rsplit_once('.') {
        Some((stem, ext)) if ext.len() <= 5 && !stem.is_empty() => stem,
        _ => file,
    }
}

/// `01  Radio Mix` / `01 - Radio Mix` / `01. Radio Mix` → (1, "Radio Mix").
fn split_track(stem: &str) -> (Option<u32>, &str) {
    let digits = stem.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return (None, stem);
    }
    let rest = stem[digits..].trim_start_matches([' ', '-', '.', '_']);
    if rest.is_empty() {
        return (None, stem);
    }
    (stem[..digits].parse().ok(), rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(file: &str) -> Song {
        Song { file: file.into(), ..Default::default() }
    }

    #[test]
    fn infers_from_path() {
        let m = Meta::of(&song("Autechre/Albums/[WARP CD 051] Autechre - Radio Mix - 1997/01  Radio Mix.flac"));
        assert_eq!(
            (m.artist.as_str(), m.album.as_str(), m.year.as_str(), m.track, m.title.as_str()),
            ("Autechre", "Radio Mix", "1997", Some(1), "Radio Mix")
        );
        assert!(m.inferred);
    }

    #[test]
    fn falls_back_to_top_folder_for_artist() {
        let m = Meta::of(&song("Boards of Canada/Geogaddi/03 Music Is Math.flac"));
        assert_eq!((m.artist.as_str(), m.album.as_str()), ("Boards of Canada", "Geogaddi"));
    }

    #[test]
    fn tags_win() {
        let mut s = song("x/y/01 a.flac");
        s.title = Some("Kiska".into());
        s.artist = Some("Tatsuro Yamashita".into());
        s.album = Some("PACIFIC".into());
        s.date = Some("2013-07-24".into());
        let m = Meta::of(&s);
        assert_eq!((m.title.as_str(), m.year.as_str()), ("Kiska", "2013"));
        assert!(!m.inferred);
    }
}
