//! Presets: saved "looks". Built-in ones plus your own (saved in presets.json).
//! A preset only changes how things look and react, never which app/screen,
//! RGB devices, autostart or update settings.
use serde::{Deserialize, Serialize};

use crate::settings::{self, Settings};

#[derive(Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub settings: Settings,
}

fn path() -> std::path::PathBuf {
    settings::dir().join("presets.json")
}

pub fn builtin() -> Vec<Preset> {
    let d = Settings::default();
    let phonk = Settings {
        mode: "both".into(),
        edge_theme: "fire".into(),
        circle_theme: "fire".into(),
        intensity: 1.3,
        fx_shake: 0.6,
        trig_shake: "kick".into(),
        fx_glitch: 0.7,
        trig_glitch: "highs".into(),
        fx_glow: 0.5,
        fx_particles: 0.4,
        circle_zoom: 0.6,
        fx_ripple: 0.6,
        trig_ripple: "drop".into(),
        text_font: "impact".into(),
        lyrics_style: "slide".into(),
        chorus_big: true,
        ..d.clone()
    };
    let chill = Settings {
        mode: "circle".into(),
        edge_theme: "ice".into(),
        circle_theme: "ice".into(),
        bar_style: "rounded".into(),
        fx_peaks: true,
        fx_trails: 0.6,
        circle_spin: 0.25,
        glow: 22.0,
        smoothness: 0.75,
        lyrics_style: "karaoke".into(),
        ..d.clone()
    };
    let space = Settings {
        mode: "circle".into(),
        edge_theme: "custom".into(),
        circle_theme: "custom".into(),
        edge_color_a: "#7b5cff".into(),
        edge_color_b: "#00e5ff".into(),
        circle_color_a: "#7b5cff".into(),
        circle_color_b: "#00e5ff".into(),
        bar_style: "dots".into(),
        hyperspace: 0.7,
        hyper_style: "wormhole".into(),
        worm_bend: 0.6,
        orbit_stars: 4,
        orbit_follow: true,
        trig_particles: "vocals".into(),
        fx_particles: 0.3,
        lyrics_style: "karaoke".into(),
        ..d.clone()
    };
    let neon = Settings {
        mode: "both".into(),
        edge_theme: "rainbow".into(),
        circle_theme: "rainbow".into(),
        bar_style: "line".into(),
        glow: 32.0,
        color_speed: 1.6,
        fx_ripple: 0.7,
        kaleido: 6,
        fx_glow: 0.4,
        trig_glow: "vocals".into(),
        show_bpm: true,
        ..d.clone()
    };
    vec![
        Preset {
            name: "Clean".into(),
            settings: d,
        },
        Preset {
            name: "Phonk".into(),
            settings: phonk,
        },
        Preset {
            name: "Chill".into(),
            settings: chill,
        },
        Preset {
            name: "Space".into(),
            settings: space,
        },
        Preset {
            name: "Neon".into(),
            settings: neon,
        },
    ]
}

pub fn load_user() -> Vec<Preset> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_user(list: &[Preset]) {
    let _ = std::fs::create_dir_all(settings::dir());
    let _ = std::fs::write(path(), serde_json::to_string_pretty(list).unwrap_or_default());
}

/// Built-ins first, then yours.
pub fn all() -> Vec<Preset> {
    let mut v = builtin();
    v.extend(load_user());
    v
}

/// The current settings with a preset's look applied.
pub fn apply(cur: &Settings, p: &Settings) -> Settings {
    let mut n = p.clone();
    // things a preset never changes
    n.visible = cur.visible;
    n.on_top = cur.on_top;
    n.audio_app = cur.audio_app.clone();
    n.auto_update = cur.auto_update;
    n.rgb_enabled = cur.rgb_enabled;
    n.rgb_corsair = cur.rgb_corsair;
    n.rgb_steelseries = cur.rgb_steelseries;
    n.display = cur.display.clone();
    n.wallpaper = cur.wallpaper;
    n.obs_window = cur.obs_window;
    n.obs_bg = cur.obs_bg.clone();
    n.fps = cur.fps;
    n.show_text = cur.show_text;
    n.text_shows = cur.text_shows.clone();
    n.lyrics_offset = cur.lyrics_offset;
    n.text_size = cur.text_size;
    n.text_offset = cur.text_offset;
    n.theme = cur.theme.clone();
    n
}
