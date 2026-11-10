//! Listens to what your PC plays on a background thread.
//!
//! - "All apps": WASAPI loopback of the speakers/headphones.
//! - One app on Windows 11 (and new Windows 10 builds): process loopback, so
//!   only that app's sound is captured.
//! - One app where that isn't supported: the full mix, but scaled by how loud
//!   the chosen app is right now (bars only move while that app makes sound).
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, HRESULT};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::System::Com::*;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

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
    /// Which app to listen to: "" = all, else an exe name like "spotify.exe".
    pub target: Mutex<String>,
    /// all | isolated | follow | waiting (shown in settings)
    pub mode: Mutex<String>,
}

pub fn start(target: &str) -> Arc<Shared> {
    let shared = Arc::new(Shared {
        ring: Mutex::new(Ring {
            data: vec![0.0; FFT_SIZE],
            pos: 0,
        }),
        sample_rate: AtomicU32::new(48000),
        target: Mutex::new(target.to_lowercase()),
        mode: Mutex::new("all".into()),
    });
    let s = shared.clone();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut isolate_broken = false; // process loopback failed once: don't retry it
        loop {
            let target = s.target.lock().unwrap().clone();
            let result = if target.is_empty() {
                set_mode(&s, "all");
                capture_system(&s, None)
            } else if !isolate_broken {
                match find_app_pid(&target) {
                    Some(pid) => match capture_process(&s, pid, &target) {
                        Err(e) if e.code() == HRESULT(-1) => Ok(()), // app closed / target changed
                        Err(e) => {
                            crate::sys::log(&format!("per-app capture not available, following volume instead: {e}"));
                            isolate_broken = true;
                            Ok(())
                        }
                        ok => ok,
                    },
                    None => {
                        // Chosen app isn't running yet: keep the bars still and wait.
                        set_mode(&s, "waiting");
                        idle_until_changed(&s, &target);
                        Ok(())
                    }
                }
            } else {
                set_mode(&s, "follow");
                capture_system(&s, Some(&target))
            };
            if let Err(e) = result {
                crate::sys::log(&format!("audio: {e}"));
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
    });
    shared
}

fn set_mode(s: &Shared, m: &str) {
    *s.mode.lock().unwrap() = m.to_string();
}

fn push_zeros(s: &Shared, n: usize) {
    let mut ring = s.ring.lock().unwrap();
    for _ in 0..n.min(FFT_SIZE) {
        ring.push(0.0);
    }
}

/// Feeds silence until the target changes or the app starts.
fn idle_until_changed(s: &Shared, target: &str) {
    for _ in 0..20 {
        push_zeros(s, FFT_SIZE);
        std::thread::sleep(Duration::from_millis(100));
        if *s.target.lock().unwrap() != target {
            return;
        }
    }
}

// ---------------------------------------------------------------- processes and audio sessions
/// pid -> (parent pid, lowercase exe name)
fn processes() -> HashMap<u32, (u32, String)> {
    let mut map = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return map;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
                map.insert(e.th32ProcessID, (e.th32ParentProcessID, name));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    map
}

/// The top-most running process with this exe name (e.g. Chrome's main process),
/// so capturing its process tree covers all of the app's sound.
fn find_app_pid(exe: &str) -> Option<u32> {
    let procs = processes();
    let mut pid = procs.iter().find(|(_, (_, n))| n == exe).map(|(p, _)| *p)?;
    for _ in 0..16 {
        match procs.get(&pid) {
            Some((parent, _)) if procs.get(parent).is_some_and(|(_, n)| n == exe) => pid = *parent,
            _ => break,
        }
    }
    Some(pid)
}

fn default_device() -> windows::core::Result<(IMMDeviceEnumerator, IMMDevice)> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        Ok((en, dev))
    }
}

fn device_id(en: &IMMDeviceEnumerator) -> Option<String> {
    unsafe {
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = dev.GetId().ok()?;
        let s = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        s
    }
}

/// Audio sessions on the speakers: (exe name, peak meter).
fn sessions(dev: &IMMDevice, procs: &HashMap<u32, (u32, String)>) -> Vec<(String, IAudioMeterInformation)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
            return out;
        };
        let Ok(list) = mgr.GetSessionEnumerator() else { return out };
        let n = list.GetCount().unwrap_or(0);
        for i in 0..n {
            let Ok(ctl) = list.GetSession(i) else { continue };
            let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
            let pid = ctl2.GetProcessId().unwrap_or(0);
            if pid == 0 {
                continue; // "System sounds"
            }
            let Some((_, name)) = procs.get(&pid) else { continue };
            if let Ok(meter) = ctl.cast::<IAudioMeterInformation>() {
                out.push((name.clone(), meter));
            }
        }
    }
    out
}

/// Apps that currently have sound open, for the "Listen to" list in settings.
pub fn list_apps() -> Vec<String> {
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let Ok((_, dev)) = default_device() else { return Vec::new() };
        let mut names: Vec<String> = sessions(&dev, &processes()).into_iter().map(|(n, _)| n).collect();
        names.retain(|n| n != "edge visualizer.exe");
        names.sort();
        names.dedup();
        names
    })
    .join()
    .unwrap_or_default()
}

// ---------------------------------------------------------------- reading samples
unsafe fn drain(cap: &IAudioCaptureClient, s: &Shared, channels: usize, bits: u16, gain: f32) -> windows::core::Result<bool> {
    let mut got = false;
    loop {
        if cap.GetNextPacketSize()? == 0 {
            return Ok(got);
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
        let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
        {
            let mut ring = s.ring.lock().unwrap();
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
                ring.push(sum / channels as f32 * gain);
            }
        }
        cap.ReleaseBuffer(frames)?;
        got = true;
    }
}

/// Speakers loopback. With `follow`, scales the sound by that app's share of the volume.
fn capture_system(s: &Shared, follow: Option<&str>) -> windows::core::Result<()> {
    unsafe {
        let (en, device) = default_device()?;
        let my_id = device_id(&en);
        let my_target = s.target.lock().unwrap().clone();
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let fmt = client.GetMixFormat()?;
        let channels = ((*fmt).nChannels as usize).max(1);
        let bits = (*fmt).wBitsPerSample;
        let sr = (*fmt).nSamplesPerSec;
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 2_000_000, 0, fmt, None);
        CoTaskMemFree(Some(fmt as *const _));
        init?;
        s.sample_rate.store(sr, Ordering::Relaxed);
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let master: Option<IAudioMeterInformation> = follow.and_then(|_| device.Activate(CLSCTX_ALL, None).ok());
        let mut meters: Vec<IAudioMeterInformation> = Vec::new();
        let mut gain = if follow.is_some() { 0.0f32 } else { 1.0 };
        let mut last_data = Instant::now();
        let mut last_tick = Instant::now();
        let mut last_check = Instant::now() - Duration::from_secs(10);

        loop {
            std::thread::sleep(Duration::from_millis(10));
            let now = Instant::now();

            if now.duration_since(last_check) > Duration::from_secs(2) {
                last_check = now;
                if device_id(&en) != my_id || *s.target.lock().unwrap() != my_target {
                    let _ = client.Stop();
                    return Ok(()); // device switched or a different app was chosen
                }
                if let Some(app) = follow {
                    let procs = processes();
                    meters = sessions(&device, &procs)
                        .into_iter()
                        .filter(|(n, _)| n == app)
                        .map(|(_, m)| m)
                        .collect();
                }
            }
            if let Some(master) = &master {
                let total = master.GetPeakValue().unwrap_or(0.0);
                let app = meters.iter().map(|m| m.GetPeakValue().unwrap_or(0.0)).fold(0.0f32, f32::max);
                let want = if total < 1e-4 { 0.0 } else { (app / total).clamp(0.0, 1.0) };
                gain += (want - gain) * 0.3;
            }

            if drain(&cap, s, channels, bits, gain)? {
                last_data = now;
            } else if now.duration_since(last_data) > Duration::from_millis(100) {
                // Windows sends nothing during silence, so feed zeros to let the bars drop.
                push_zeros(s, (sr as f32 * now.duration_since(last_tick).as_secs_f32()) as usize);
            }
            last_tick = now;
        }
    }
}

// ---------------------------------------------------------------- one app only (process loopback)
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Done(Arc<(Mutex<bool>, Condvar)>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Done_Impl {
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let (lock, cv) = &*self.0;
        *lock.lock().unwrap() = true;
        cv.notify_all();
        Ok(())
    }
}

/// Same memory layout as a PROPVARIANT holding a VT_BLOB.
#[repr(C)]
struct BlobVariant {
    vt: u16,
    reserved: [u16; 3],
    size: u32,
    _pad: u32,
    data: *const u8,
}

fn activate_process_client(pid: u32) -> windows::core::Result<IAudioClient> {
    unsafe {
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        let var = BlobVariant {
            vt: 65, // VT_BLOB
            reserved: [0; 3],
            size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            _pad: 0,
            data: &params as *const _ as *const u8,
        };
        let signal = Arc::new((Mutex::new(false), Condvar::new()));
        let handler: IActivateAudioInterfaceCompletionHandler = Done(signal.clone()).into();
        let op = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&var as *const BlobVariant as *const _),
            &handler,
        )?;
        let (lock, cv) = &*signal;
        let mut done = lock.lock().unwrap();
        while !*done {
            let (d, timeout) = cv.wait_timeout(done, Duration::from_secs(5)).unwrap();
            done = d;
            if timeout.timed_out() {
                return Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL));
            }
        }
        let mut hr = HRESULT(0);
        let mut unk: Option<windows::core::IUnknown> = None;
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok()?;
        unk.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?
            .cast()
    }
}

/// Error with code -1 = stopped on purpose (app closed or target changed).
fn capture_process(s: &Shared, pid: u32, target: &str) -> windows::core::Result<()> {
    unsafe {
        let client = activate_process_client(pid)?;
        // Process loopback has no mix format of its own: ask for 48 kHz float stereo.
        let fmt = WAVEFORMATEX {
            wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
            nChannels: 2,
            nSamplesPerSec: 48000,
            nAvgBytesPerSec: 48000 * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            2_000_000,
            0,
            &fmt,
            None,
        )?;
        let event: HANDLE = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(event)?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        s.sample_rate.store(48000, Ordering::Relaxed);
        set_mode(s, "isolated");

        let mut last_check = Instant::now();
        let mut last_data = Instant::now();
        let result = loop {
            let woke = WaitForSingleObject(event, 100) == WAIT_OBJECT_0;
            if woke {
                match drain(&cap, s, 2, 32, 1.0) {
                    Ok(true) => last_data = Instant::now(),
                    Ok(false) => {}
                    Err(e) => break Err(e),
                }
            }
            if last_data.elapsed() > Duration::from_millis(100) {
                push_zeros(s, 4800); // the app is quiet: let the bars drop
            }
            if last_check.elapsed() > Duration::from_secs(2) {
                last_check = Instant::now();
                let gone = !processes().contains_key(&pid);
                if gone || *s.target.lock().unwrap() != target {
                    break Err(windows::core::Error::from(HRESULT(-1)));
                }
            }
        };
        let _ = client.Stop();
        let _ = CloseHandle(event);
        result
    }
}
