//! Spectrum analyser over MPD's `fifo` output (44100:16:2 PCM).
//!
//! A reader thread keeps the latest samples; each frame takes a windowed
//! FFT and folds it into one log-spaced band per column (40 Hz–16 kHz),
//! with a tilt so treble isn't dwarfed by bass, fast attack, and slow
//! fall. When the FIFO goes quiet (pause, stop) the bars fall to rest.
//!
//! MPD writes the FIFO as it plays, but PipeWire's buffering means the
//! speakers lag behind it, so samples are timestamped on arrival and each
//! frame analyses what arrived `delay` ago.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
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
    /// Recent mono samples, oldest first.
    samples: VecDeque<f32>,
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
    delay: Duration,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
    bars: Vec<f32>,
    last_frame: Instant,
}

impl Visualizer {
    /// Starts reading `fifo` in the background. A missing FIFO isn't an
    /// error: the reader keeps retrying (MPD creates it on start).
    pub fn new(fifo: PathBuf, delay: Duration) -> Self {
        let shared =
            Arc::new(Mutex::new(Shared { samples: VecDeque::with_capacity(KEEP), written: 0, marks: VecDeque::new() }));
        let s = Arc::clone(&shared);
        thread::spawn(move || read_loop(&fifo, &s));
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        Self {
            shared,
            delay,
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            window,
            buf: vec![Complex::default(); FFT_SIZE],
            bars: Vec::new(),
            last_frame: Instant::now(),
        }
    }

    /// Whether anything is (still) moving: audio arriving or bars falling.
    pub fn active(&self) -> bool {
        self.live() || self.bars.iter().any(|&b| b > 0.0)
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

    fn live(&self) -> bool {
        let Some(t) = self.heard() else { return false };
        let s = self.shared.lock().unwrap();
        // the latest read before `t`, recent enough to still be playing
        s.marks.iter().rev().find(|(at, _)| *at <= t).is_some_and(|(at, _)| t - *at < STALE)
    }

    /// Advances one frame; returns `n` bar heights in 0..=1.
    pub fn frame(&mut self, n: usize) -> &[f32] {
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.25);
        self.last_frame = Instant::now();
        if self.bars.len() != n {
            self.bars = vec![0.0; n];
        }
        let targets = if self.live() { self.analyse(n) } else { vec![0.0; n] };
        for (bar, target) in self.bars.iter_mut().zip(targets) {
            *bar = if target > *bar { *bar + (target - *bar) * 0.7 } else { (*bar - FALL_PER_SEC * dt).max(target) };
        }
        &self.bars
    }

    fn analyse(&mut self, n: usize) -> Vec<f32> {
        {
            let s = self.shared.lock().unwrap();
            let end_n = self.heard().and_then(|t| s.written_at(t)).unwrap_or(0);
            // index just past the window's last sample, within `samples`
            let first_n = s.written - s.samples.len() as u64;
            let end = end_n.saturating_sub(first_n).min(s.samples.len() as u64) as usize;
            let start = end.saturating_sub(FFT_SIZE);
            let pad = FFT_SIZE - (end - start);
            for (i, c) in self.buf.iter_mut().enumerate() {
                let x = if i < pad { 0.0 } else { s.samples[start + i - pad] };
                *c = Complex::new(x * self.window[i], 0.0);
            }
        }
        self.fft.process(&mut self.buf);

        // Magnitude in dBFS per bin (Hann window halves the amplitude).
        let scale = 4.0 / FFT_SIZE as f32;
        let mag: Vec<f32> = self.buf[..FFT_SIZE / 2].iter().map(|c| c.norm() * scale).collect();
        let hz_per_bin = RATE / FFT_SIZE as f32;
        let edge = |i: usize| LOW_HZ * (HIGH_HZ / LOW_HZ).powf(i as f32 / n as f32);

        (0..n)
            .map(|i| {
                let (lo, hi) = (edge(i), edge(i + 1));
                let (a, b) = ((lo / hz_per_bin) as usize, (hi / hz_per_bin).ceil() as usize);
                // Narrow low bands can fall between bins: take the nearest.
                let peak = mag[a.min(mag.len() - 1)..b.clamp(a + 1, mag.len())].iter().copied().fold(0.0, f32::max);
                let centre = (lo * hi).sqrt();
                let db = 20.0 * peak.max(1e-9).log10() + TILT_DB_PER_OCTAVE * (centre / 1000.0).log2();
                ((db - FLOOR_DB) / RANGE_DB).clamp(0.0, 1.0)
            })
            .collect()
    }
}

/// Reads s16le stereo frames forever, reopening when MPD closes the FIFO.
fn read_loop(fifo: &PathBuf, shared: &Mutex<Shared>) {
    let mut raw = vec![0u8; 4096];
    loop {
        // Blocks until MPD opens its end.
        let Ok(mut f) = File::open(fifo) else {
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        let mut carry: Vec<u8> = Vec::new();
        loop {
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
                s.samples.push_back((l + r) / (2.0 * 32768.0));
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
