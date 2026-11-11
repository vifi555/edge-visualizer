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

/// Album cover for a song: (song key, image bytes).
pub type Art = Arc<Mutex<Option<(String, Arc<Vec<u8>>)>>>;

pub struct Media {
    pub track: Arc<Mutex<Option<Track>>>,
    pub art: Art,
}

/// `target` = exe of the app to follow ("" = prefer Spotify, else whatever is playing).
pub fn start(target: Arc<Mutex<String>>) -> Media {
    let shared = Arc::new(Mutex::new(None));
    let art: Art = Arc::new(Mutex::new(None));
    let s = shared.clone();
    let art_out = art.clone();
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
            let mut art_key = String::new();
            let mut art_tries = 0;
            loop {
                let want = target.lock().unwrap().clone();
                let t = read(&mgr, &want).unwrap_or(None);
                // Cover art: fetch when the song changes (Spotify sometimes adds it a moment later).
                if let Some(tr) = &t {
                    let key = tr.key();
                    if key != art_key {
                        art_key = key.clone();
                        art_tries = 0;
                        *art_out.lock().unwrap() = None;
                    }
                    let have = art_out.lock().unwrap().is_some();
                    if !have && art_tries < 6 {
                        art_tries += 1;
                        if let Ok(bytes) = read_art(&mgr, &want) {
                            *art_out.lock().unwrap() = Some((key, Arc::new(bytes)));
                        }
                    }
                }
                *s.lock().unwrap() = t;
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    });
    Media { track: shared, art }
}

fn read_art(mgr: &GlobalSystemMediaTransportControlsSessionManager, target: &str) -> windows::core::Result<Vec<u8>> {
    use windows::Storage::Streams::DataReader;
    let fail = || windows::core::Error::from(windows::Win32::Foundation::E_FAIL);
    let session = pick_session(mgr, target).ok_or_else(fail)?;
    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let stream = props.Thumbnail()?.OpenReadAsync()?.get()?;
    let size = stream.Size()? as u32;
    if size == 0 || size > 20_000_000 {
        return Err(fail());
    }
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.get()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf)?;
    Ok(buf)
}

fn pick_session(mgr: &GlobalSystemMediaTransportControlsSessionManager, target: &str) -> Option<GlobalSystemMediaTransportControlsSession> {
    // "spotify.exe" -> "spotify"; matched against the media session's app id.
    let want = target.trim_end_matches(".exe").to_string();
    let prefer = if want.is_empty() { "spotify".to_string() } else { want.clone() };
    if let Ok(list) = mgr.GetSessions() {
        let n = list.Size().unwrap_or(0);
        for i in 0..n {
            if let Ok(s) = list.GetAt(i) {
                let id = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
                if id.to_lowercase().contains(&prefer) {
                    return Some(s);
                }
            }
        }
    }
    if !want.is_empty() {
        return None; // a specific app was chosen: don't show another app's song
    }
    mgr.GetCurrentSession().ok()
}

fn read(mgr: &GlobalSystemMediaTransportControlsSessionManager, target: &str) -> windows::core::Result<Option<Track>> {
    let Some(s) = pick_session(mgr, target) else { return Ok(None) };
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
