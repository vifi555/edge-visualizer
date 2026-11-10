//! Finds timed lyrics on lrclib.net.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::media::Track;

const UA: &str = "EdgeVisualizer/2.0 (personal desktop music visualizer)";

#[derive(Clone, Debug)]
pub struct Line {
    pub t: f64,
    pub text: String,
}

pub type Lines = Option<Arc<Vec<Line>>>;

#[derive(Clone, Debug, Default)]
pub struct LyricsState {
    pub key: String,
    pub lines: Lines,
    pub status: String, // none | searching | found | instrumental | nosync | notfound | error
}

pub struct Lyrics {
    pub state: Arc<Mutex<LyricsState>>,
    cache: Arc<Mutex<HashMap<String, (Lines, String)>>>,
    key: String,
}

impl Lyrics {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LyricsState {
                status: "none".into(),
                ..Default::default()
            })),
            cache: Arc::new(Mutex::new(HashMap::new())),
            key: String::new(),
        }
    }

    pub fn snapshot(&self) -> LyricsState {
        self.state.lock().unwrap().clone()
    }

    /// Call every frame; starts a lookup when the song changes.
    pub fn update(&mut self, track: Option<&Track>) {
        let key = track.map(|t| t.key()).unwrap_or_default();
        if key == self.key {
            return;
        }
        self.key = key.clone();
        let Some(track) = track else {
            self.set(&key, None, "none");
            return;
        };
        let cached = self.cache.lock().unwrap().get(&key).cloned();
        if let Some((lines, status)) = cached {
            self.set(&key, lines, &status);
            return;
        }
        self.set(&key, None, "searching");

        let (state, cache, track) = (self.state.clone(), self.cache.clone(), track.clone());
        std::thread::spawn(move || {
            let (lines, status) = match fetch(&track) {
                Ok((lines, status)) => {
                    cache.lock().unwrap().insert(key.clone(), (lines.clone(), status.to_string()));
                    (lines, status.to_string())
                }
                Err(()) => (None, "error".to_string()), // not cached, so it retries next time
            };
            let mut st = state.lock().unwrap();
            if st.key == key {
                st.lines = lines;
                st.status = status;
            }
        });
    }

    fn set(&self, key: &str, lines: Lines, status: &str) {
        *self.state.lock().unwrap() = LyricsState {
            key: key.to_string(),
            lines,
            status: status.to_string(),
        };
    }
}

fn agent() -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).user_agent(UA);
    if let Ok(tls) = native_tls::TlsConnector::new() {
        b = b.tls_connector(Arc::new(tls));
    }
    b.build()
}

fn api(agent: &ureq::Agent, endpoint: &str, params: &[(&str, String)]) -> Result<Option<Value>, ()> {
    let mut req = agent.get(&format!("https://lrclib.net/api/{endpoint}"));
    for (k, v) in params {
        req = req.query(k, v);
    }
    match req.call() {
        Ok(r) => r
            .into_string()
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .map(Some)
            .ok_or(()),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => {
            crate::sys::log(&format!("lyrics request failed: {e}"));
            Err(())
        }
    }
}

fn pick(v: &Value) -> Option<(Lines, &'static str)> {
    if v.get("instrumental").and_then(Value::as_bool) == Some(true) {
        return Some((Some(Arc::new(Vec::new())), "instrumental"));
    }
    let synced = v.get("syncedLyrics").and_then(Value::as_str)?;
    Some((Some(Arc::new(parse_lrc(synced))), "found"))
}

fn fetch(t: &Track) -> Result<(Lines, &'static str), ()> {
    let agent = agent();
    let artist = first_artist(&t.artist);
    let mut titles = vec![t.title.clone()];
    let cleaned = clean_title(&t.title);
    if !cleaned.is_empty() && cleaned != t.title {
        titles.push(cleaned);
    }
    let secs = (t.dur_ms / 1000.0).round() as i64;

    for title in &titles {
        let mut p = vec![("track_name", title.clone()), ("artist_name", artist.clone())];
        if secs > 0 {
            p.push(("duration", secs.to_string()));
        }
        if let Some(v) = api(&agent, "get", &p)? {
            if let Some(hit) = pick(&v) {
                return Ok(hit);
            }
        }
    }

    for title in &titles {
        let p = [("track_name", title.clone()), ("artist_name", artist.clone())];
        let Some(Value::Array(list)) = api(&agent, "search", &p)? else {
            continue;
        };
        if list.is_empty() {
            continue;
        }
        let mut synced: Vec<&Value> = list
            .iter()
            .filter(|x| x.get("syncedLyrics").and_then(Value::as_str).is_some())
            .collect();
        if secs > 0 {
            let diff = |x: &Value| (x.get("duration").and_then(Value::as_f64).unwrap_or(0.0) - secs as f64).abs();
            synced.sort_by(|a, b| diff(a).partial_cmp(&diff(b)).unwrap_or(std::cmp::Ordering::Equal));
        }
        if let Some(best) = synced.first() {
            if let Some(hit) = pick(best) {
                return Ok(hit);
            }
        }
        if list.iter().any(|x| x.get("instrumental").and_then(Value::as_bool) == Some(true)) {
            return Ok((Some(Arc::new(Vec::new())), "instrumental"));
        }
        if list.iter().any(|x| x.get("plainLyrics").and_then(Value::as_str).is_some()) {
            return Ok((None, "nosync"));
        }
    }
    Ok((None, "notfound"))
}

pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim_start();
        let mut stamps = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            match parse_stamp(&rest[1..end]) {
                Some(ms) => stamps.push(ms),
                None => break,
            }
            rest = &rest[end + 1..];
        }
        let words = rest.trim().to_string();
        for t in stamps {
            out.push(Line { t, text: words.clone() });
        }
    }
    out.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn parse_stamp(tag: &str) -> Option<f64> {
    let (m, s) = tag.split_once(':')?;
    let m: f64 = m.trim().parse().ok()?;
    let s: f64 = s.trim().parse().ok()?;
    Some((m * 60.0 + s) * 1000.0)
}

fn first_artist(a: &str) -> String {
    let lower = a.to_ascii_lowercase();
    let cut = [", ", " & ", " feat", " ft.", " x ", ";"]
        .iter()
        .filter_map(|p| lower.find(p))
        .min()
        .unwrap_or(a.len());
    let first = a[..cut].trim();
    if first.is_empty() {
        a.trim().to_string()
    } else {
        first.to_string()
    }
}

fn clean_title(s: &str) -> String {
    let mut t = s.to_string();
    if let Some(i) = t.rfind(" - ") {
        let suffix = t[i + 3..].to_ascii_lowercase();
        if ["remaster", "version", "edit", "mix", "live", "mono", "stereo", "from "]
            .iter()
            .any(|k| suffix.contains(k))
        {
            t.truncate(i);
        }
    }
    // remove "(feat. X)", "[with Y]" ...
    loop {
        let lower = t.to_ascii_lowercase();
        let mut found = None;
        for (open, close) in [('(', ')'), ('[', ']')] {
            for (a, _) in lower.match_indices(open) {
                if let Some(b) = lower[a..].find(close) {
                    let inner = lower[a + 1..a + b].trim_start();
                    if inner.starts_with("feat") || inner.starts_with("ft") || inner.starts_with("with ") {
                        found = Some((a, a + b + 1));
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        match found {
            Some((a, b)) => t.replace_range(a..b, ""),
            None => break,
        }
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}
