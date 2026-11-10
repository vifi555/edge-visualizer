//! Turns raw audio into bar heights (same maths as the Electron version).
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

use crate::settings::Settings;

pub const FFT_SIZE: usize = 4096;
const MIN_DB: f32 = -85.0;
const MAX_DB: f32 = -20.0;

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
    smooth: Vec<f32>,
    bands: Vec<(usize, usize)>,
    band_count: usize,
    sample_rate: u32,
    raw: Vec<f32>,
    pub cur: Vec<f32>,
    peak: f32,
}

impl Analyzer {
    pub fn new() -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        // Blackman window, like the browser's AnalyserNode
        let window = (0..FFT_SIZE)
            .map(|i| {
                let x = i as f32 / FFT_SIZE as f32 * std::f32::consts::TAU;
                0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos()
            })
            .collect();
        Self {
            fft,
            window,
            buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            smooth: vec![0.0; FFT_SIZE / 2],
            bands: Vec::new(),
            band_count: 0,
            sample_rate: 0,
            raw: Vec::new(),
            cur: Vec::new(),
            peak: 0.3,
        }
    }

    fn build_bands(&mut self, n: usize, sr: u32) {
        let bins = FFT_SIZE / 2;
        let bin_hz = sr as f32 / FFT_SIZE as f32;
        let f_min = 35.0f32;
        let f_max = 16000.0f32.min(sr as f32 / 2.0 - bin_hz);
        self.bands = (0..n)
            .map(|i| {
                let f0 = f_min * (f_max / f_min).powf(i as f32 / n as f32);
                let f1 = f_min * (f_max / f_min).powf((i + 1) as f32 / n as f32);
                let lo = ((f0 / bin_hz).floor() as usize).min(bins - 1);
                let hi = ((f1 / bin_hz).round() as usize).max(lo + 1).min(bins);
                (lo, hi)
            })
            .collect();
        self.raw = vec![0.0; n];
        self.cur = vec![0.0; n];
        self.band_count = n;
        self.sample_rate = sr;
    }

    /// `samples` = the latest FFT_SIZE mono samples. `dt` = seconds since last frame.
    pub fn update(&mut self, samples: &[f32], sr: u32, s: &Settings, dt: f32) {
        let n = s.bars.clamp(8, 400) as usize;
        if n != self.band_count || sr != self.sample_rate {
            self.build_bands(n, sr.max(8000));
        }
        let frames = (dt * 60.0).clamp(0.1, 10.0); // how many 60fps frames passed

        for i in 0..FFT_SIZE {
            self.buf[i] = Complex::new(samples.get(i).copied().unwrap_or(0.0) * self.window[i], 0.0);
        }
        self.fft.process(&mut self.buf);

        let keep = 0.5f32.powf(frames);
        for k in 0..FFT_SIZE / 2 {
            let mag = self.buf[k].norm() / FFT_SIZE as f32;
            self.smooth[k] = keep * self.smooth[k] + (1.0 - keep) * mag;
        }

        let mut mx = 0.0f32;
        for i in 0..n {
            let (lo, hi) = self.bands[i];
            let m = self.smooth[lo..hi].iter().fold(0.0f32, |a, &b| a.max(b));
            let db = 20.0 * m.max(1e-12).log10();
            let v = ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0) * (1.0 + 0.5 * i as f32 / n as f32);
            self.raw[i] = v;
            mx = mx.max(v);
        }
        // Auto-gain so it looks good at any volume
        self.peak = mx.max(self.peak * 0.997f32.powf(frames)).max(0.2);

        let fall = 0.25 - 0.22 * s.smoothness;
        let k_fall = 1.0 - (1.0 - fall).powf(frames);
        let k_rise = 1.0 - 0.4f32.powf(frames);
        for i in 0..n {
            let a = self.raw[i.saturating_sub(1)];
            let b = self.raw[i];
            let c = self.raw[(i + 1).min(n - 1)];
            let t = (((a * 0.25 + b * 0.5 + c * 0.25) / self.peak).powi(2) * s.intensity).min(1.0);
            let k = if t > self.cur[i] { k_rise } else { k_fall };
            self.cur[i] += (t - self.cur[i]) * k;
        }
    }

    /// Energy in a frequency range of the latest frame (raw, not smoothed).
    pub fn band_energy(&self, lo_hz: f32, hi_hz: f32) -> f32 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        let bin_hz = self.sample_rate as f32 / FFT_SIZE as f32;
        let lo = ((lo_hz / bin_hz) as usize).max(1);
        let hi = ((hi_hz / bin_hz) as usize).clamp(lo + 1, FFT_SIZE / 2);
        let n = FFT_SIZE as f32;
        self.buf[lo..hi].iter().map(|c| c.norm_sqr() / (n * n)).sum()
    }

    pub fn bass(&self) -> f32 {
        let k = ((self.cur.len() as f32 * 0.12).round() as usize).max(1).min(self.cur.len());
        if k == 0 {
            return 0.0;
        }
        self.cur[..k].iter().sum::<f32>() / k as f32
    }
}

/// Finds kick drums: a sudden jump in bass energy compared to the last second.
pub struct BeatDetector {
    avg: f32,
    cooldown: f32,
    /// 1.0 right after a beat, fading to 0.
    pub level: f32,
}

impl BeatDetector {
    pub fn new() -> Self {
        Self {
            avg: 0.0,
            cooldown: 0.0,
            level: 0.0,
        }
    }

    /// Returns true on a new beat. `sensitivity` 0..1 (higher = more beats).
    pub fn update(&mut self, energy: f32, dt: f32, sensitivity: f32) -> bool {
        let k = 1.9 - 0.7 * sensitivity; // how far above average counts as a hit
        self.cooldown -= dt;
        let beat = self.cooldown <= 0.0 && energy > self.avg * k && energy > 1e-7;
        // ~1 second running average of the bass energy
        let a = 1.0 - (-dt / 1.0).exp();
        self.avg += (energy - self.avg) * a;
        if beat {
            self.cooldown = 0.12;
            self.level = 1.0;
        } else {
            self.level *= (-dt / 0.22).exp();
            if self.level < 0.01 {
                self.level = 0.0;
            }
        }
        beat
    }
}
