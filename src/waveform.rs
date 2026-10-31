//! Loudness envelopes for the waveform progress bar.
//!
//! Tracks are decoded once with symphonia on a low-priority thread and
//! reduced to `BUCKETS` (peak, RMS) pairs, cached on disk in
//! `~/.cache/notmpc/waveforms` keyed by path, size and mtime.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, anyhow};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub const BUCKETS: usize = 2048;

/// Per bucket, 0..=1: the loudest sample and the RMS.
#[derive(Debug)]
pub struct Waveform {
    pub peak: Vec<f32>,
    pub rms: Vec<f32>,
}

impl Waveform {
    /// (peak, rms) over the fraction range [a, b) of the track.
    pub fn span(&self, a: f32, b: f32) -> (f32, f32) {
        let n = self.peak.len();
        let i = ((a * n as f32) as usize).min(n - 1);
        let j = ((b * n as f32).ceil() as usize).clamp(i + 1, n);
        let peak = self.peak[i..j].iter().copied().fold(0.0, f32::max);
        let rms = self.rms[i..j].iter().copied().fold(0.0, f32::max);
        (peak, rms)
    }
}

#[derive(Clone)]
enum Slot {
    Pending,
    Ready(Arc<Waveform>),
    Failed,
}

/// Hands out waveforms, computing missing ones in the background.
pub struct Loader {
    slots: Arc<Mutex<HashMap<PathBuf, Slot>>>,
    tx: Sender<PathBuf>,
    requested: HashSet<PathBuf>,
}

impl Loader {
    pub fn new() -> Self {
        let slots: Arc<Mutex<HashMap<PathBuf, Slot>>> = Arc::default();
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let s = Arc::clone(&slots);
        thread::spawn(move || {
            // Decoding is the heaviest thing notmpc does; stay out of the way.
            // SAFETY: plain syscalls on this thread's id.
            unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 15) };
            for path in rx {
                let slot = match load_or_compute(&path) {
                    Ok(w) => Slot::Ready(Arc::new(w)),
                    Err(_) => Slot::Failed,
                };
                s.lock().unwrap().insert(path, slot);
            }
        });
        Self { slots, tx, requested: HashSet::new() }
    }

    /// The waveform for `path` if ready; otherwise queues it.
    pub fn get(&mut self, path: &Path) -> Option<Arc<Waveform>> {
        if let Some(Slot::Ready(w)) = self.slots.lock().unwrap().get(path) {
            return Some(Arc::clone(w));
        }
        if self.requested.insert(path.to_owned()) {
            self.slots.lock().unwrap().insert(path.to_owned(), Slot::Pending);
            let _ = self.tx.send(path.to_owned());
        }
        None
    }

    /// Whether `path` is still being decoded.
    pub fn pending(&self, path: &Path) -> bool {
        matches!(self.slots.lock().unwrap().get(path), Some(Slot::Pending))
    }
}

fn cache_path(path: &Path) -> Option<PathBuf> {
    let meta = fs::metadata(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, meta.len(), meta.modified().ok()).hash(&mut h);
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("notmpc/waveforms").join(format!("{:016x}", h.finish())))
}

fn load_or_compute(path: &Path) -> Result<Waveform> {
    let cache = cache_path(path);
    if let Some(bytes) = cache.as_ref().and_then(|c| fs::read(c).ok())
        && bytes.len() == BUCKETS * 2
    {
        let (p, r) = bytes.split_at(BUCKETS);
        let f = |b: &u8| *b as f32 / 255.0;
        return Ok(Waveform { peak: p.iter().map(f).collect(), rms: r.iter().map(f).collect() });
    }
    let w = compute(path)?;
    if let Some(c) = cache {
        let q = |x: &f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        let bytes: Vec<u8> = w.peak.iter().map(q).chain(w.rms.iter().map(q)).collect();
        if let Some(dir) = c.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(c, bytes);
    }
    Ok(w)
}

/// Decodes the whole track into fixed blocks of (peak, sum of squares),
/// then folds the blocks into `BUCKETS`.
pub fn compute(path: &Path) -> Result<Waveform> {
    let file = File::open(path).with_context(|| path.display().to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| anyhow!("{e}"))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| anyhow!("no audio track"))?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or_else(|| anyhow!("not audio"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| anyhow!("{e}"))?;

    const BLOCK: usize = 1024;
    let mut blocks: Vec<(f32, f32, usize)> = Vec::new(); // peak, sum sq, frames
    let (mut peak, mut sq, mut n) = (0f32, 0f32, 0usize);
    let mut buf: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(_)) => break,
            Err(e) => return Err(anyhow!("{e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        let audio = match decoder.decode(&packet) {
            Ok(a) => a,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(anyhow!("{e}")),
        };
        let ch = audio.spec().channels().count().max(1);
        buf.resize(audio.samples_interleaved(), 0.0);
        audio.copy_to_slice_interleaved(&mut buf);
        for frame in buf.chunks_exact(ch) {
            let x = frame.iter().sum::<f32>() / ch as f32;
            peak = peak.max(x.abs());
            sq += x * x;
            n += 1;
            if n == BLOCK {
                blocks.push((peak, sq, n));
                (peak, sq, n) = (0.0, 0.0, 0);
            }
        }
    }
    if n > 0 {
        blocks.push((peak, sq, n));
    }
    if blocks.is_empty() {
        return Err(anyhow!("no audio decoded"));
    }

    let mut out = Waveform { peak: vec![0.0; BUCKETS], rms: vec![0.0; BUCKETS] };
    for b in 0..BUCKETS {
        let i = b * blocks.len() / BUCKETS;
        let j = ((b + 1) * blocks.len() / BUCKETS).max(i + 1).min(blocks.len());
        let (mut p, mut s, mut c) = (0f32, 0f32, 0usize);
        for &(bp, bs, bn) in &blocks[i..j] {
            p = p.max(bp);
            s += bs;
            c += bn;
        }
        out.peak[b] = p.min(1.0);
        out.rms[b] = (s / c.max(1) as f32).sqrt().min(1.0);
    }
    Ok(out)
}
