//! Auto-update from GitHub Releases.
//! Downloads the newest .exe from the latest release, swaps it in place of the
//! running one (Windows allows renaming a running exe) and restarts.
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

pub const REPO: &str = "vifi555/edge-visualizer";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub enum Outcome {
    UpToDate(String),
    Installed(String),
}

fn agent() -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(180))
        .user_agent(&format!("EdgeVisualizer/{VERSION}"));
    if let Ok(tls) = native_tls::TlsConnector::new() {
        b = b.tls_connector(Arc::new(tls));
    }
    b.build()
}

fn parse(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

/// Is `remote` a higher version than `local`? ("v2.0.10" > "2.0.9")
pub fn newer(remote: &str, local: &str) -> bool {
    let (a, b) = (parse(remote), parse(local));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn exe() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// Builds run straight from cargo's target folder never update themselves.
pub fn dev_build() -> bool {
    exe()
        .map(|e| e.to_string_lossy().to_lowercase().contains("\\target\\"))
        .unwrap_or(true)
}

/// Deletes the previous version left behind by an update.
pub fn cleanup_old() {
    if let Some(e) = exe() {
        let _ = std::fs::remove_file(e.with_extension("old.exe"));
        let _ = std::fs::remove_file(e.with_extension("new.exe"));
    }
}

pub fn check_and_install() -> Result<Outcome, String> {
    let agent = agent();
    let resp = agent
        .get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("couldn't check for updates: {e}"))?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let tag = v.get("tag_name").and_then(Value::as_str).unwrap_or("").to_string();
    if tag.is_empty() || !newer(&tag, VERSION) {
        return Ok(Outcome::UpToDate(tag));
    }

    let asset = v
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter().find(|a| {
                a.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.to_lowercase().ends_with(".exe"))
            })
        })
        .ok_or("the newest release has no .exe")?;
    let url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .ok_or("no download link")?;
    let size = asset.get("size").and_then(Value::as_u64).unwrap_or(0);

    let mut bytes = Vec::new();
    agent
        .get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?
        .into_reader()
        .take(300_000_000)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("download failed: {e}"))?;
    if bytes.len() < 100_000 || !bytes.starts_with(b"MZ") || (size > 0 && bytes.len() as u64 != size) {
        return Err("the download looks broken, skipping it".into());
    }

    let exe = exe().ok_or("can't find my own exe")?;
    let new = exe.with_extension("new.exe");
    let old = exe.with_extension("old.exe");
    std::fs::write(&new, &bytes).map_err(|e| format!("can't save the update: {e}"))?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("can't replace the app: {e}"))?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe); // put the old one back
        return Err(format!("can't replace the app: {e}"));
    }
    Ok(Outcome::Installed(tag))
}

/// Starts the (new) exe. It waits for this process to exit before taking over.
pub fn launch_updated(tag: &str) {
    if let Some(e) = exe() {
        let _ = std::process::Command::new(e).args(["--updated", tag]).spawn();
    }
}
