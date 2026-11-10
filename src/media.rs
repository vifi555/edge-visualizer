//! Reads what's playing (prefers Spotify) and where it is in the song,
//! straight from Windows' media controls.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Media::Control::*;
use windows::Win32::System::Com::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub pos_ms: f64,
    pub dur_ms: f64,
    pub upd_ms: f64,
    pub playing: bool,
}

impl Track {
    pub fn key(&self) -> String {
        format!("{}\n{}", self.artist, self.title)
    }
}

pub fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

pub fn start() -> Arc<Mutex<Option<Track>>> {
    let shared = Arc::new(Mutex::new(None));
    let s = shared.clone();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        loop {
            let mgr = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync().and_then(|op| op.get()) {
                Ok(m) => m,
                Err(e) => {
                    crate::sys::log(&format!("media controls unavailable: {e}"));
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };
            loop {
                let t = read(&mgr).unwrap_or(None);
                *s.lock().unwrap() = t;
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    });
    shared
}

fn pick_session(mgr: &GlobalSystemMediaTransportControlsSessionManager) -> Option<GlobalSystemMediaTransportControlsSession> {
    if let Ok(list) = mgr.GetSessions() {
        let n = list.Size().unwrap_or(0);
        for i in 0..n {
            if let Ok(s) = list.GetAt(i) {
                let id = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
                if id.to_lowercase().contains("spotify") {
                    return Some(s);
                }
            }
        }
    }
    mgr.GetCurrentSession().ok()
}

fn read(mgr: &GlobalSystemMediaTransportControlsSessionManager) -> windows::core::Result<Option<Track>> {
    let Some(s) = pick_session(mgr) else { return Ok(None) };
    let props = s.TryGetMediaPropertiesAsync()?.get()?;
    let title = props.Title()?.to_string();
    if title.trim().is_empty() {
        return Ok(None);
    }
    let artist = props.Artist()?.to_string();
    let tl = s.GetTimelineProperties()?;
    let pos_ms = tl.Position()?.Duration as f64 / 10_000.0;
    let dur_ms = tl.EndTime()?.Duration as f64 / 10_000.0;
    // Windows time (100ns since 1601) -> unix ms
    let mut upd_ms = (tl.LastUpdatedTime()?.UniversalTime - 116_444_736_000_000_000) as f64 / 10_000.0;
    let now = now_ms();
    if upd_ms < 1e12 || upd_ms > now + 5000.0 {
        upd_ms = now;
    }
    let playing = s.GetPlaybackInfo()?.PlaybackStatus()? == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
    Ok(Some(Track {
        title,
        artist,
        pos_ms,
        dur_ms,
        upd_ms,
        playing,
    }))
}
