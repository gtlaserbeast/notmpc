//! Spectrum analyser over MPD's `fifo` output (44100:16:2 PCM).
//!
//! A reader thread keeps the latest samples; each frame takes a windowed
//! FFT and folds it into one log-spaced band per column (40 Hz–16 kHz),
//! with a tilt so treble isn't dwarfed by bass, fast attack, and slow
//! fall. When the FIFO goes quiet (pause, stop) the bars fall to rest.

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

struct Shared {
    /// Latest mono samples, oldest first, at most FFT_SIZE.
    samples: Vec<f32>,
    last_data: Option<Instant>,
}

pub struct Visualizer {
    shared: Arc<Mutex<Shared>>,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
    bars: Vec<f32>,
    last_frame: Instant,
}

impl Visualizer {
    /// Starts reading `fifo` in the background. A missing FIFO isn't an
    /// error: the reader keeps retrying (MPD creates it on start).
    pub fn new(fifo: PathBuf) -> Self {
        let shared = Arc::new(Mutex::new(Shared { samples: Vec::with_capacity(FFT_SIZE * 2), last_data: None }));
        let s = Arc::clone(&shared);
        thread::spawn(move || read_loop(&fifo, &s));
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        Self {
            shared,
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

    fn live(&self) -> bool {
        self.shared.lock().unwrap().last_data.is_some_and(|t| t.elapsed() < STALE)
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
            *bar = if target > *bar {
                *bar + (target - *bar) * 0.7
            } else {
                (*bar - FALL_PER_SEC * dt).max(target)
            };
        }
        &self.bars
    }

    fn analyse(&mut self, n: usize) -> Vec<f32> {
        {
            let s = self.shared.lock().unwrap();
            let pad = FFT_SIZE - s.samples.len().min(FFT_SIZE);
            for (i, c) in self.buf.iter_mut().enumerate() {
                let x = if i < pad { 0.0 } else { s.samples[i - pad] };
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
            let mut s = shared.lock().unwrap();
            for fr in carry[..frames * 4].chunks_exact(4) {
                let l = i16::from_le_bytes([fr[0], fr[1]]) as f32;
                let r = i16::from_le_bytes([fr[2], fr[3]]) as f32;
                s.samples.push((l + r) / (2.0 * 32768.0));
            }
            let excess = s.samples.len().saturating_sub(FFT_SIZE);
            s.samples.drain(..excess);
            s.last_data = Some(Instant::now());
            drop(s);
            carry.drain(..frames * 4);
        }
        thread::sleep(Duration::from_millis(200));
    }
}
