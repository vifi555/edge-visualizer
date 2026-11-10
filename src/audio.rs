//! Listens to everything your PC plays (WASAPI loopback) on a background thread.
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

use crate::viz::FFT_SIZE;

pub struct Ring {
    data: Vec<f32>,
    pos: usize,
}

impl Ring {
    fn push(&mut self, v: f32) {
        self.data[self.pos] = v;
        self.pos = (self.pos + 1) % self.data.len();
    }
    /// Copies the newest `out.len()` samples, oldest first.
    pub fn latest(&self, out: &mut [f32]) {
        let n = self.data.len();
        let k = out.len().min(n);
        for (i, o) in out.iter_mut().take(k).enumerate() {
            *o = self.data[(self.pos + n - k + i) % n];
        }
    }
}

pub struct Shared {
    pub ring: Mutex<Ring>,
    pub sample_rate: AtomicU32,
}

pub fn start() -> Arc<Shared> {
    let shared = Arc::new(Shared {
        ring: Mutex::new(Ring {
            data: vec![0.0; FFT_SIZE],
            pos: 0,
        }),
        sample_rate: AtomicU32::new(48000),
    });
    let s = shared.clone();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        loop {
            // Returns when the default output device changes or something fails; then restart.
            if let Err(e) = capture(&s) {
                crate::sys::log(&format!("audio: {e}"));
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
    });
    shared
}

fn default_id(en: &IMMDeviceEnumerator) -> Option<String> {
    unsafe {
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = dev.GetId().ok()?;
        let s = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        s
    }
}

fn capture(shared: &Shared) -> windows::core::Result<()> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let my_id = default_id(&en);
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

        let fmt = client.GetMixFormat()?;
        let channels = ((*fmt).nChannels as usize).max(1);
        let bits = (*fmt).wBitsPerSample;
        let sr = (*fmt).nSamplesPerSec;
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 2_000_000, 0, fmt, None);
        CoTaskMemFree(Some(fmt as *const _));
        init?;

        shared.sample_rate.store(sr, Ordering::Relaxed);
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let mut last_data = Instant::now();
        let mut last_tick = Instant::now();
        let mut last_check = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let mut got = false;
            loop {
                if cap.GetNextPacketSize()? == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                {
                    let mut ring = shared.ring.lock().unwrap();
                    for f in 0..frames as usize {
                        let mut sum = 0.0f32;
                        if !silent && !data.is_null() {
                            for c in 0..channels {
                                let i = f * channels + c;
                                sum += match bits {
                                    32 => *(data as *const f32).add(i),
                                    16 => *(data as *const i16).add(i) as f32 / 32768.0,
                                    _ => 0.0,
                                };
                            }
                        }
                        ring.push(sum / channels as f32);
                    }
                }
                cap.ReleaseBuffer(frames)?;
                got = true;
            }

            let now = Instant::now();
            if got {
                last_data = now;
            } else if now.duration_since(last_data) > Duration::from_millis(100) {
                // Windows sends nothing during silence, so feed zeros to let the bars drop.
                let zeros = (sr as f32 * now.duration_since(last_tick).as_secs_f32()) as usize;
                let mut ring = shared.ring.lock().unwrap();
                for _ in 0..zeros.min(FFT_SIZE) {
                    ring.push(0.0);
                }
            }
            last_tick = now;

            if now.duration_since(last_check) > Duration::from_secs(2) {
                last_check = now;
                if default_id(&en) != my_id {
                    let _ = client.Stop();
                    return Ok(()); // e.g. switched from speakers to headphones
                }
            }
        }
    }
}
