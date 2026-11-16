//! Spectrum analyser over MPD's `fifo` output (44100:16:2 PCM).
//!
//! A reader thread keeps the latest samples; each frame takes a windowed
//! FFT and folds it into log-spaced bands (40 Hz–16 kHz), with a tilt so
//! treble isn't dwarfed by bass; `Bars` gives them fast attack, slow fall
//! and peak caps. When the FIFO goes quiet (pause, stop) they fall to rest.
//!
//! MPD writes the FIFO as it plays, but PipeWire's buffering means the
//! speakers lag behind it, so samples are timestamped on arrival and each
//! frame analyses what arrived `delay` ago.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

const RATE: f32 = 44100.0;
const FFT_SIZE: usize = 4096;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16000.0;
/// dB below full scale that maps to an empty bar, and the span to full.
const FLOOR_DB: f32 = -78.0;
const RANGE_DB: f32 = 60.0;
/// Pink-noise compensation: dB added per octave above 1 kHz.
const TILT_DB_PER_OCTAVE: f32 = 3.0;
/// Bar heights (0..1) fallen per second.
const FALL_PER_SEC: f32 = 1.6;
/// Samples older than this mean the FIFO went quiet.
const STALE: Duration = Duration::from_millis(150);

/// Samples kept: enough for the longest delay plus one FFT window.
const KEEP: usize = 3 * RATE as usize;

struct Shared {
    /// Recent stereo samples (left, right), oldest first.
    samples: VecDeque<[f32; 2]>,
    /// Samples ever received; `samples.back()` is number `written - 1`.
    written: u64,
    /// (arrival time, `written` after that read), oldest first.
    marks: VecDeque<(Instant, u64)>,
}

impl Shared {
    /// How many samples had arrived by time `t`, interpolating within a
    /// read (MPD paces the FIFO in real time). None if nothing had yet.
    fn written_at(&self, t: Instant) -> Option<u64> {
        let i = self.marks.iter().rposition(|(at, _)| *at <= t)?;
        let (at, n) = self.marks[i];
        let next = self.marks.get(i + 1).map_or(n, |m| m.1);
        let since = (t - at).as_secs_f32() * RATE;
        Some((n + since as u64).min(next))
    }
}

pub struct Visualizer {
    shared: Arc<Mutex<Shared>>,
    /// Whether the reader should read; it closes the FIFO and sleeps
    /// while this is off.
    reading: Arc<AtomicBool>,
    reader: Thread,
    delay: Duration,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
    /// The latest spectrum (magnitude per bin), and when it was taken:
    /// everything drawn in one frame shares it.
    mag: Vec<f32>,
    mag_at: Option<Instant>,
}

impl Visualizer {
    /// Starts reading `fifo` in the background. A missing FIFO isn't an
    /// error: the reader keeps retrying (MPD creates it on start).
    pub fn new(fifo: PathBuf, delay: Duration) -> Self {
        let shared =
            Arc::new(Mutex::new(Shared { samples: VecDeque::with_capacity(KEEP), written: 0, marks: VecDeque::new() }));
        let reading = Arc::new(AtomicBool::new(true));
        let (s, r) = (Arc::clone(&shared), Arc::clone(&reading));
        let reader = thread::spawn(move || read_loop(&fifo, &s, &r)).thread().clone();
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        Self {
            shared,
            reading,
            reader,
            delay,
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            window,
            buf: vec![Complex::default(); FFT_SIZE],
            mag: vec![0.0; FFT_SIZE / 2],
            mag_at: None,
        }
    }

    /// Starts or stops reading the FIFO. Stopped, the reader holds no
    /// file open and doesn't wake (MPD keeps a reader of its own, so its
    /// output never blocks).
    pub fn set_reading(&self, on: bool) {
        if self.reading.swap(on, Ordering::Relaxed) != on && on {
            self.reader.unpark();
        }
    }

    pub fn delay(&self) -> Duration {
        self.delay
    }

    pub fn set_delay(&mut self, d: Duration) {
        self.delay = d.min(Duration::from_secs(2));
    }

    /// The moment whose audio is on the speakers now.
    fn heard(&self) -> Option<Instant> {
        Instant::now().checked_sub(self.delay)
    }

    /// Whether audio is playing (arriving, `delay` ago).
    pub fn live(&self) -> bool {
        let Some(t) = self.heard() else { return false };
        let s = self.shared.lock().unwrap();
        // the latest read before `t`, recent enough to still be playing
        s.marks.iter().rev().find(|(at, _)| *at <= t).is_some_and(|(at, _)| t - *at < STALE)
    }

    /// The last `n` stereo samples heard, oldest first (fewer at first).
    pub fn recent(&self, n: usize) -> Vec<[f32; 2]> {
        let s = self.shared.lock().unwrap();
        let end = self.end_index(&s);
        s.samples.range(end.saturating_sub(n)..end).copied().collect()
    }

    /// Index just past the last sample heard, within `samples`.
    fn end_index(&self, s: &Shared) -> usize {
        let end_n = self.heard().and_then(|t| s.written_at(t)).unwrap_or(0);
        let first_n = s.written - s.samples.len() as u64;
        end_n.saturating_sub(first_n).min(s.samples.len() as u64) as usize
    }

    /// `n` log-spaced band levels (0..=1, 40 Hz–16 kHz) of what's heard
    /// now; silence when nothing plays. The FFT runs at most once per
    /// few milliseconds however many callers ask.
    pub fn levels(&mut self, n: usize) -> Vec<f32> {
        if !self.live() {
            return vec![0.0; n];
        }
        if self.mag_at.is_none_or(|t| t.elapsed() > Duration::from_millis(8)) {
            self.analyse();
            self.mag_at = Some(Instant::now());
        }
        bands(&self.mag, n)
    }

    fn analyse(&mut self) {
        {
            let s = self.shared.lock().unwrap();
            let end = self.end_index(&s);
            let start = end.saturating_sub(FFT_SIZE);
            let pad = FFT_SIZE - (end - start);
            for (i, c) in self.buf.iter_mut().enumerate() {
                let x = if i < pad {
                    0.0
                } else {
                    let [l, r] = s.samples[start + i - pad];
                    (l + r) / 2.0
                };
                *c = Complex::new(x * self.window[i], 0.0);
            }
        }
        self.fft.process(&mut self.buf);
        // Hann window halves the amplitude.
        let scale = 4.0 / FFT_SIZE as f32;
        for (m, c) in self.mag.iter_mut().zip(&self.buf) {
            *m = c.norm() * scale;
        }
    }
}

/// `n` log-spaced band levels in 0..=1 from a magnitude spectrum.
fn bands(mag: &[f32], n: usize) -> Vec<f32> {
    let hz_per_bin = RATE / FFT_SIZE as f32;
    let edge = |i: usize| LOW_HZ * (HIGH_HZ / LOW_HZ).powf(i as f32 / n as f32);
    (0..n)
        .map(|i| {
            let (lo, hi) = (edge(i), edge(i + 1));
            let centre = (lo * hi).sqrt();
            let peak = band_level(mag, lo / hz_per_bin, hi / hz_per_bin, centre / hz_per_bin);
            let db = 20.0 * peak.max(1e-9).log10() + TILT_DB_PER_OCTAVE * (centre / 1000.0).log2();
            ((db - FLOOR_DB) / RANGE_DB).clamp(0.0, 1.0)
        })
        .collect()
}

/// Seconds a peak cap holds before it falls, and how fast it then falls.
const PEAK_HOLD: f32 = 0.5;
const PEAK_FALL_PER_SEC: f32 = 0.6;

/// Bars that rise with the music and fall back slowly, each with a peak
/// cap that holds a moment above it. One per thing drawn, since each
/// smooths at its own frame rate and bar count.
#[derive(Default)]
pub struct Bars {
    pub bars: Vec<f32>,
    pub peaks: Vec<f32>,
    hold: Vec<f32>,
    last: Option<Instant>,
}

impl Bars {
    /// Moves toward `targets` (resetting if their count changed).
    pub fn update(&mut self, targets: &[f32]) {
        let dt = self.last.map_or(0.0, |t| t.elapsed().as_secs_f32().min(0.25));
        self.last = Some(Instant::now());
        if self.bars.len() != targets.len() {
            let n = targets.len();
            (self.bars, self.peaks, self.hold) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
        }
        for (i, &t) in targets.iter().enumerate() {
            let bar = &mut self.bars[i];
            *bar = if t > *bar { *bar + (t - *bar) * 0.7 } else { (*bar - FALL_PER_SEC * dt).max(t) };
            if *bar >= self.peaks[i] {
                self.peaks[i] = *bar;
                self.hold[i] = PEAK_HOLD;
            } else if self.hold[i] > 0.0 {
                self.hold[i] -= dt;
            } else {
                self.peaks[i] = (self.peaks[i] - PEAK_FALL_PER_SEC * dt).max(*bar);
            }
        }
    }

    /// Whether anything is still moving (falling bars or caps).
    pub fn moving(&self) -> bool {
        self.peaks.iter().any(|&p| p > 0.0)
    }
}

/// A band's magnitude from `mag`, with its edges and centre in bins: the
/// loudest bin in it, or, for a band narrower than two bins (the bass,
/// with many bars), the spectrum read between bins at its centre, so
/// neighbouring bars don't all copy the same bin.
fn band_level(mag: &[f32], lo: f32, hi: f32, centre: f32) -> f32 {
    let last = mag.len() - 1;
    if hi - lo < 2.0 {
        let f = centre.min(last as f32);
        let (i, k) = (f as usize, f.fract());
        return mag[i] + (mag[(i + 1).min(last)] - mag[i]) * k;
    }
    mag[(lo as usize).min(last)..(hi.ceil() as usize).min(mag.len())].iter().copied().fold(0.0, f32::max)
}

/// Reads s16le stereo frames forever, reopening when MPD closes the FIFO,
/// and sleeping while `reading` is off.
fn read_loop(fifo: &PathBuf, shared: &Mutex<Shared>, reading: &AtomicBool) {
    let mut raw = vec![0u8; 4096];
    loop {
        while !reading.load(Ordering::Relaxed) {
            thread::park();
        }
        // Blocks until MPD opens its end.
        let Ok(mut f) = File::open(fifo) else {
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        let mut carry: Vec<u8> = Vec::new();
        while reading.load(Ordering::Relaxed) {
            let n = match f.read(&mut raw) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            carry.extend_from_slice(&raw[..n]);
            let frames = carry.len() / 4;
            let now = Instant::now();
            let mut s = shared.lock().unwrap();
            for fr in carry[..frames * 4].as_chunks::<4>().0 {
                let l = i16::from_le_bytes([fr[0], fr[1]]) as f32;
                let r = i16::from_le_bytes([fr[2], fr[3]]) as f32;
                s.samples.push_back([l / 32768.0, r / 32768.0]);
            }
            s.written += frames as u64;
            let written = s.written;
            // This read's samples arrived over the last read's span; stamp
            // its start so interpolation runs forward through it.
            let began = now.checked_sub(Duration::from_secs_f32(frames as f32 / RATE)).unwrap_or(now);
            s.marks.push_back((began, written - frames as u64));
            s.marks.push_back((now, written));
            let excess = s.samples.len().saturating_sub(KEEP);
            s.samples.drain(..excess);
            while s.marks.front().is_some_and(|(at, _)| now.duration_since(*at) > Duration::from_secs(3)) {
                s.marks.pop_front();
            }
            drop(s);
            carry.drain(..frames * 4);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_bands_read_between_bins() {
        let mag = [0.0, 1.0, 3.0, 2.0, 0.5];
        // three bands inside bin 1..2: each gets its own level
        let l: Vec<f32> = [1.2, 1.5, 1.8].iter().map(|&c| band_level(&mag, c - 0.1, c + 0.1, c)).collect();
        assert!(l[0] < l[1] && l[1] < l[2], "{l:?}");
        assert!((l[1] - 2.0).abs() < 1e-6);
        // a wide band takes its loudest bin
        assert_eq!(band_level(&mag, 0.5, 4.0, 2.0), 3.0);
        // past the end stays in range
        assert_eq!(band_level(&mag, 9.0, 9.5, 9.2), 0.5);
    }
}
