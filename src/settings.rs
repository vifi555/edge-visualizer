use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Same file and field names as the old Electron version, so settings carry over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub visible: bool,
    pub on_top: bool,
    pub mode: String,         // edge | circle | both
    pub theme: String,        // old single colour setting, only used to migrate
    pub edge_theme: String,   // rainbow | fire | ice | white | custom
    pub circle_theme: String, // rainbow | fire | ice | white | custom
    pub edge_color_a: String, // custom colours as "#rrggbb"
    pub edge_color_b: String,
    pub circle_color_a: String,
    pub circle_color_b: String,
    pub text_color: String,
    pub edges: String,        // all | bottom | sides | topbottom
    pub fps: u32,             // 0 = match the screen, else a cap
    pub audio_app: String,    // "" = all apps, else an exe name like "spotify.exe"
    pub rgb_enabled: bool,    // flash keyboard/mouse RGB on the beat
    pub rgb_effect: String,   // flash | ripple
    pub rgb_follow: bool,     // use the edge colours instead of rgb_color
    pub rgb_color: String,    // "#rrggbb"
    pub rgb_sensitivity: f32, // 0..1, higher = more beats
    pub rgb_corsair: bool,
    pub rgb_steelseries: bool,
    pub bar_style: String,  // bars | rounded | dots | line
    pub fx_peaks: bool,     // little caps that fall slowly
    pub fx_glow: f32,       // 0..1 beat glow around the screen
    pub fx_shake: f32,      // 0..1 bass shake
    pub fx_glitch: f32,     // 0..1 RGB split on the beat
    pub fx_particles: f32,  // 0..1 sparks from the bars
    pub fx_trails: f32,     // 0..1 afterglow trails
    pub circle_spin: f32,   // -1..1 rotation speed
    pub circle_zoom: f32,   // 0..1 bass punch
    pub hyperspace: f32,    // 0..1 star amount (0 = off)
    pub hyper_speed: f32,   // 0..1 warp speed
    pub hyper_follow: bool, // stars use the edge colours instead of white
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
            edge_theme: "rainbow".into(),
            circle_theme: "rainbow".into(),
            edge_color_a: "#ff7a1a".into(),
            edge_color_b: "#b45cff".into(),
            circle_color_a: "#00e5ff".into(),
            circle_color_b: "#ff3da0".into(),
            text_color: "#ffffff".into(),
            edges: "all".into(),
            fps: 0,
            audio_app: String::new(),
            rgb_enabled: false,
            rgb_effect: "flash".into(),
            rgb_follow: true,
            rgb_color: "#ffffff".into(),
            rgb_sensitivity: 0.5,
            rgb_corsair: true,
            rgb_steelseries: true,
            bar_style: "bars".into(),
            fx_peaks: false,
            fx_glow: 0.0,
            fx_shake: 0.0,
            fx_glitch: 0.0,
            fx_particles: 0.0,
            fx_trails: 0.0,
            circle_spin: 0.0,
            circle_zoom: 0.0,
            hyperspace: 0.0,
            hyper_speed: 0.5,
            hyper_follow: false,
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
        let Some(text) = std::fs::read_to_string(settings_path()).ok() else {
            return Self::default();
        };
        let Ok(mut s) = serde_json::from_str::<Settings>(&text) else {
            return Self::default();
        };
        // Older versions had one "theme" for everything: use it for both parts.
        if !text.contains("\"edgeTheme\"") {
            s.edge_theme = s.theme.clone();
            s.circle_theme = s.theme.clone();
        }
        s.clamped()
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
        const THEMES: &[&str] = &["rainbow", "fire", "ice", "white", "custom"];
        pick(&mut self.theme, THEMES, "rainbow");
        pick(&mut self.edge_theme, THEMES, "rainbow");
        pick(&mut self.circle_theme, THEMES, "rainbow");
        pick(&mut self.edges, &["all", "bottom", "sides", "topbottom"], "all");
        let d = Settings::default();
        for (v, def) in [
            (&mut self.edge_color_a, d.edge_color_a),
            (&mut self.edge_color_b, d.edge_color_b),
            (&mut self.circle_color_a, d.circle_color_a),
            (&mut self.circle_color_b, d.circle_color_b),
            (&mut self.text_color, d.text_color),
        ] {
            if parse_hex(v).is_none() {
                *v = def;
            }
        }
        if !matches!(self.fps, 0 | 30 | 60 | 120 | 144) {
            self.fps = 0;
        }
        self.audio_app = self.audio_app.trim().to_lowercase();
        pick(&mut self.rgb_effect, &["flash", "ripple"], "flash");
        if parse_hex(&self.rgb_color).is_none() {
            self.rgb_color = "#ffffff".into();
        }
        self.rgb_sensitivity = self.rgb_sensitivity.clamp(0.0, 1.0);
        pick(&mut self.bar_style, &["bars", "rounded", "dots", "line"], "bars");
        for v in [
            &mut self.fx_glow,
            &mut self.fx_shake,
            &mut self.fx_glitch,
            &mut self.fx_particles,
            &mut self.circle_zoom,
        ] {
            *v = v.clamp(0.0, 1.0);
        }
        self.fx_trails = self.fx_trails.clamp(0.0, 0.9);
        self.circle_spin = self.circle_spin.clamp(-1.0, 1.0);
        self.hyperspace = self.hyperspace.clamp(0.0, 1.0);
        self.hyper_speed = self.hyper_speed.clamp(0.0, 1.0);
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

/// "#ff7a1a" -> (1.0, 0.48, 0.1)
pub fn parse_hex(s: &str) -> Option<(f32, f32, f32)> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some((
        ((v >> 16) & 255) as f32 / 255.0,
        ((v >> 8) & 255) as f32 / 255.0,
        (v & 255) as f32 / 255.0,
    ))
}

/// What's playing, written by the visualizer so the settings window can show it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub title: String,
    pub artist: String,
    pub lyrics: String,
    pub audio_mode: String, // all | isolated | follow | waiting
    pub rgb: String,        // RGB sync status line
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
