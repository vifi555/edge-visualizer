use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Same file and field names as the old Electron version, so settings carry over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub visible: bool,
    pub on_top: bool,
    pub mode: String,  // edge | circle | both
    pub theme: String, // rainbow | fire | ice | white
    pub intensity: f32,
    pub size: f32,
    pub bars: u32,
    pub smoothness: f32,
    pub glow: f32,
    pub opacity: f32,
    pub color_speed: f32,
    pub show_text: bool,
    pub text_shows: String, // lyrics | both | title
    pub lyrics_offset: f32,
    pub text_size: f32,
    pub text_offset: f32,
    pub text_font: String, // clean | impact | mono | script
    pub text_pulse: f32,
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            visible: true,
            on_top: true,
            mode: "edge".into(),
            theme: "rainbow".into(),
            intensity: 1.0,
            size: 1.0,
            bars: 96,
            smoothness: 0.5,
            glow: 16.0,
            opacity: 0.9,
            color_speed: 1.0,
            show_text: true,
            text_shows: "lyrics".into(),
            lyrics_offset: 0.0,
            text_size: 42.0,
            text_offset: 0.0,
            text_font: "clean".into(),
            text_pulse: 0.5,
            auto_update: true,
        }
    }
}

pub fn dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Edge Visualizer")
}
pub fn settings_path() -> PathBuf {
    dir().join("settings.json")
}
pub fn status_path() -> PathBuf {
    dir().join("status.json")
}

pub fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .unwrap_or_default()
            .clamped()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        write_atomic(&settings_path(), &text)
    }

    fn clamped(mut self) -> Self {
        let pick = |v: &mut String, ok: &[&str], def: &str| {
            if !ok.contains(&v.as_str()) {
                *v = def.into();
            }
        };
        pick(&mut self.mode, &["edge", "circle", "both"], "edge");
        pick(&mut self.theme, &["rainbow", "fire", "ice", "white"], "rainbow");
        pick(&mut self.text_shows, &["lyrics", "both", "title"], "lyrics");
        pick(&mut self.text_font, &["clean", "impact", "mono", "script"], "clean");
        self.intensity = self.intensity.clamp(0.3, 3.0);
        self.size = self.size.clamp(0.4, 2.5);
        self.bars = self.bars.clamp(32, 160);
        self.smoothness = self.smoothness.clamp(0.0, 1.0);
        self.glow = self.glow.clamp(0.0, 40.0);
        self.opacity = self.opacity.clamp(0.2, 1.0);
        self.color_speed = self.color_speed.clamp(0.0, 3.0);
        self.lyrics_offset = self.lyrics_offset.clamp(-3.0, 3.0);
        self.text_size = self.text_size.clamp(16.0, 120.0);
        self.text_offset = self.text_offset.clamp(-100.0, 400.0);
        self.text_pulse = self.text_pulse.clamp(0.0, 1.0);
        self
    }
}

/// What's playing, written by the visualizer so the settings window can show it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub title: String,
    pub artist: String,
    pub lyrics: String,
}

impl Status {
    pub fn load() -> Self {
        std::fs::read_to_string(status_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) {
        let _ = write_atomic(&status_path(), &serde_json::to_string(self).unwrap_or_default());
    }
}
