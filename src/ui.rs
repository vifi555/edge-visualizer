//! The settings window. Every change is saved to settings.json right away;
//! the visualizer picks it up within a fraction of a second.
use std::ops::RangeInclusive;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32, RichText};

use crate::settings::{self, Settings, Status};
use crate::sys;

const ORANGE: Color32 = Color32::from_rgb(255, 122, 26);
const MUTED: Color32 = Color32::from_rgb(154, 154, 171);
const BG: Color32 = Color32::from_rgb(18, 18, 24);
const CARD: Color32 = Color32::from_rgb(27, 27, 36);
const FIELD: Color32 = Color32::from_rgb(36, 36, 47);
const LINE: Color32 = Color32::from_rgb(42, 42, 54);
const GREEN: Color32 = Color32::from_rgb(95, 211, 141);
const WARN: Color32 = Color32::from_rgb(232, 176, 74);

pub fn run() -> eframe::Result<()> {
    let icon = egui::IconData {
        rgba: include_bytes!("../assets/icon64.rgba").to_vec(),
        width: 64,
        height: 64,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(sys::SETTINGS_TITLE)
            .with_inner_size([440.0, 800.0])
            .with_min_inner_size([360.0, 400.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(sys::SETTINGS_TITLE, options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

struct App {
    s: Settings,
    written: Settings,
    seen: Option<SystemTime>,
    saved_at: Option<Instant>,
    status: Status,
    status_seen: Option<SystemTime>,
    autostart: bool,
    confirm_reset: bool,
    update_msg: std::sync::Arc<std::sync::Mutex<String>>,
    apps: Vec<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        add_system_fonts(&cc.egui_ctx);
        style(&cc.egui_ctx);
        let s = Settings::load();
        Self {
            written: s.clone(),
            s,
            seen: settings::mtime(&settings::settings_path()),
            saved_at: None,
            status: Status::load(),
            status_seen: settings::mtime(&settings::status_path()),
            autostart: sys::autostart_on(),
            confirm_reset: false,
            update_msg: Default::default(),
            apps: crate::audio::list_apps(),
        }
    }

    /// Pick up changes made elsewhere (hotkeys, tray menu) and the now-playing status.
    fn poll(&mut self) {
        let m = settings::mtime(&settings::settings_path());
        if m != self.seen {
            self.seen = m;
            let fresh = Settings::load();
            if fresh != self.written {
                self.s = fresh.clone();
                self.written = fresh;
            }
        }
        let sm = settings::mtime(&settings::status_path());
        if sm != self.status_seen {
            self.status_seen = sm;
            self.status = Status::load();
        }
    }

    fn save_if_changed(&mut self) {
        if self.s != self.written && self.s.save().is_ok() {
            self.written = self.s.clone();
            self.seen = settings::mtime(&settings::settings_path());
            self.saved_at = Some(Instant::now());
        }
    }
}

/// egui's built-in font has no Japanese/Korean/Chinese characters, so borrow Windows' own fonts.
fn add_system_fonts(ctx: &egui::Context) {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()) + "\\Fonts\\";
    let groups: [(&str, &[&str]); 3] = [
        ("cjk-jp", &["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"]),
        ("cjk-kr", &["malgun.ttf"]),
        ("cjk-zh", &["msyh.ttc", "simsun.ttc"]),
    ];
    let mut fonts = egui::FontDefinitions::default();
    for (name, files) in groups {
        if let Some(bytes) = files.iter().find_map(|f| std::fs::read(format!("{dir}{f}")).ok()) {
            fonts.font_data.insert(name.to_string(), egui::FontData::from_owned(bytes));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.to_string());
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = FIELD;
    v.selection.bg_fill = ORANGE;
    v.selection.stroke = egui::Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.inactive.bg_fill = FIELD;
    v.widgets.inactive.weak_bg_fill = FIELD;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ORANGE);
    v.widgets.active.bg_fill = ORANGE;
    ctx.set_visuals(v);
    ctx.style_mut(|st| {
        st.spacing.item_spacing = egui::vec2(8.0, 7.0);
        st.spacing.interact_size.y = 24.0;
    });
}

fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0_f32, LINE))
        .rounding(12.0)
        .inner_margin(egui::Margin::same(14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(11.5).color(MUTED).strong());
            ui.add_space(2.0);
            add(ui);
        });
    ui.add_space(10.0);
}

fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
    });
}

fn toggle(ui: &mut egui::Ui, label: &str, v: &mut bool) {
    row(ui, label, |ui| {
        ui.checkbox(v, "");
    });
}

fn combo(ui: &mut egui::Ui, label: &str, value: &mut String, opts: &[(&str, &str)]) {
    row(ui, label, |ui| {
        let cur = opts.iter().find(|o| o.0 == value.as_str()).map(|o| o.1).unwrap_or("?");
        egui::ComboBox::new(label, "").selected_text(cur).width(180.0).show_ui(ui, |ui| {
            for (key, text) in opts {
                ui.selectable_value(value, key.to_string(), *text);
            }
        });
    });
}

fn slider<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut N,
    range: RangeInclusive<N>,
    fmt: impl Fn(f64) -> String + 'static,
) {
    ui.label(label);
    ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(80.0);
    ui.add(egui::Slider::new(v, range).trailing_fill(true).custom_formatter(move |x, _| fmt(x)));
    ui.add_space(2.0);
}

/// "Check for updates" button: installs a newer version and restarts the visualizer.
fn check_now(msg: std::sync::Arc<std::sync::Mutex<String>>, ctx: egui::Context) {
    if crate::update::dev_build() {
        *msg.lock().unwrap() = "Updates are off for test builds (run from the target folder).".into();
        return;
    }
    *msg.lock().unwrap() = "Checking…".into();
    std::thread::spawn(move || {
        let text = match crate::update::check_and_install() {
            Ok(crate::update::Outcome::UpToDate(_)) => "You have the newest version ✓".to_string(),
            Ok(crate::update::Outcome::Installed(tag)) => {
                sys::quit_overlay();
                std::thread::sleep(Duration::from_millis(800));
                crate::update::launch_updated(&tag);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                format!("Updated to {tag}, restarting…")
            }
            Err(e) => e,
        };
        *msg.lock().unwrap() = text;
        ctx.request_repaint();
    });
}

const THEMES: &[(&str, &str)] = &[
    ("rainbow", "Rainbow"),
    ("fire", "Fire"),
    ("ice", "Ice"),
    ("white", "White"),
    ("custom", "Custom"),
];

fn color_row(ui: &mut egui::Ui, label: &str, hex: &mut String) {
    row(ui, label, |ui| {
        let (r, g, b) = settings::parse_hex(hex).unwrap_or((1.0, 1.0, 1.0));
        let mut c = [(r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8];
        if ui.color_edit_button_srgb(&mut c).changed() {
            *hex = format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        }
    });
}

/// A colour theme picker; "Custom" shows two colour buttons it blends between.
fn theme_rows(ui: &mut egui::Ui, label: &str, theme: &mut String, a: &mut String, b: &mut String) {
    combo(ui, label, theme, THEMES);
    if theme == "custom" {
        color_row(ui, "      Color 1", a);
        color_row(ui, "      Color 2", b);
    }
}

/// "spotify.exe" -> "Spotify"
fn pretty_app(exe: &str) -> String {
    let stem = exe.trim_end_matches(".exe");
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => exe.to_string(),
    }
}

fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Edge Visualizer").size(20.0).strong().color(ORANGE));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.saved_at.is_some_and(|t| t.elapsed() < Duration::from_millis(1200)) {
                            ui.label(RichText::new("Saved ✓").color(GREEN).strong());
                        }
                    });
                });
                ui.add_space(8.0);

                let s = &mut self.s;
                let apps = &mut self.apps;
                let mode = self.status.audio_mode.clone();
                card(ui, "LISTEN TO", |ui| {
                    let mut opts: Vec<(String, String)> = vec![(String::new(), "All apps".into())];
                    for a in apps.iter() {
                        opts.push((a.clone(), pretty_app(a)));
                    }
                    if !s.audio_app.is_empty() && !apps.contains(&s.audio_app) {
                        opts.push((s.audio_app.clone(), pretty_app(&s.audio_app)));
                    }
                    let refs: Vec<(&str, &str)> = opts.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                    combo(ui, "App", &mut s.audio_app, &refs);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Only apps making sound right now are listed.").color(MUTED).size(12.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Refresh").clicked() {
                                *apps = crate::audio::list_apps();
                            }
                        });
                    });
                    if !s.audio_app.is_empty() {
                        let app = pretty_app(&s.audio_app);
                        let (text, color) = match mode.as_str() {
                            "isolated" => (format!("Only {app}'s sound moves the bars ✓"), GREEN),
                            "follow" => (
                                format!("This Windows version can't capture one app on its own, so the bars follow {app}'s volume. Other sounds playing at the same time still mix in."),
                                WARN,
                            ),
                            "waiting" => (format!("{app} isn't running. The bars start when it does."), MUTED),
                            _ => (String::new(), MUTED),
                        };
                        if !text.is_empty() {
                            ui.label(RichText::new(text).color(color).size(12.0));
                        }
                    }
                });

                card(ui, "VISUALIZER", |ui| {
                    toggle(ui, "Show visualizer", &mut s.visible);
                    toggle(ui, "On top of apps", &mut s.on_top);
                    combo(
                        ui,
                        "Style",
                        &mut s.mode,
                        &[("edge", "Screen edges"), ("circle", "Circle"), ("both", "Both")],
                    );
                    combo(
                        ui,
                        "Edges",
                        &mut s.edges,
                        &[("all", "All edges"), ("bottom", "Bottom only"), ("sides", "Sides only"), ("topbottom", "Top + bottom")],
                    );
                    theme_rows(ui, "Edge colors", &mut s.edge_theme, &mut s.edge_color_a, &mut s.edge_color_b);
                    theme_rows(ui, "Circle colors", &mut s.circle_theme, &mut s.circle_color_a, &mut s.circle_color_b);
                    row(ui, "FPS limit", |ui| {
                        let name = |f: u32| if f == 0 { "Match screen".to_string() } else { format!("{f} FPS") };
                        egui::ComboBox::new("fps", "").selected_text(name(s.fps)).width(180.0).show_ui(ui, |ui| {
                            for f in [0u32, 144, 120, 60, 30] {
                                ui.selectable_value(&mut s.fps, f, name(f));
                            }
                        });
                    });
                    ui.add_space(4.0);
                    slider(ui, "Intensity", &mut s.intensity, 0.3..=3.0, pct);
                    slider(ui, "Bar length", &mut s.size, 0.4..=2.5, pct);
                    slider(ui, "Bar count", &mut s.bars, 32..=160, |x| format!("{x:.0}"));
                    slider(ui, "Smoothness", &mut s.smoothness, 0.0..=1.0, pct);
                    slider(ui, "Glow", &mut s.glow, 0.0..=40.0, |x| {
                        if x < 0.5 {
                            "Off".into()
                        } else {
                            format!("{x:.0}px")
                        }
                    });
                    slider(ui, "Opacity", &mut s.opacity, 0.2..=1.0, pct);
                    slider(ui, "Color speed", &mut s.color_speed, 0.0..=3.0, |x| {
                        if x < 0.05 {
                            "Still".into()
                        } else {
                            format!("{x:.1}x")
                        }
                    });
                });

                let status = &self.status;
                card(ui, "SONG TEXT (BOTTOM)", |ui| {
                    toggle(ui, "Show text", &mut s.show_text);
                    combo(
                        ui,
                        "Text shows",
                        &mut s.text_shows,
                        &[
                            ("lyrics", "Lyrics (when singing)"),
                            ("both", "Song + artist"),
                            ("title", "Song only"),
                        ],
                    );

                    egui::Frame::none()
                        .fill(FIELD)
                        .rounding(8.0)
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if status.title.is_empty() {
                                ui.label(RichText::new("Nothing playing").color(MUTED));
                            } else {
                                ui.label(RichText::new(format!("{}  ·  {}", status.title, status.artist)).strong());
                                let (text, color) = match status.lyrics.as_str() {
                                    "searching" => ("Looking for lyrics…", MUTED),
                                    "found" => ("Lyrics found ✓", GREEN),
                                    "instrumental" => ("No singing in this song, showing the name", MUTED),
                                    "nosync" => ("Only untimed lyrics exist, showing the name", WARN),
                                    "notfound" => ("No lyrics found, showing the name", WARN),
                                    "error" => ("Could not reach the lyrics site", WARN),
                                    _ => ("", MUTED),
                                };
                                if !text.is_empty() {
                                    ui.label(RichText::new(text).color(color).size(12.5));
                                }
                            }
                        });
                    ui.add_space(4.0);

                    color_row(ui, "Text color", &mut s.text_color);
                    combo(
                        ui,
                        "Font",
                        &mut s.text_font,
                        &[("clean", "Clean"), ("impact", "Impact"), ("mono", "Mono"), ("script", "Script")],
                    );
                    slider(ui, "Text size", &mut s.text_size, 16.0..=120.0, |x| format!("{x:.0}px"));
                    slider(ui, "Text height", &mut s.text_offset, -100.0..=400.0, |x| {
                        format!("{}{x:.0}px", if x > 0.0 { "+" } else { "" })
                    });
                    slider(ui, "Beat bounce", &mut s.text_pulse, 0.0..=1.0, |x| {
                        if x < 0.01 {
                            "Off".into()
                        } else {
                            pct(x)
                        }
                    });
                    slider(ui, "Lyrics timing", &mut s.lyrics_offset, -3.0..=3.0, |x| {
                        if x.abs() < 0.05 {
                            "Normal".into()
                        } else {
                            format!("{}{x:.1}s", if x > 0.0 { "+" } else { "" })
                        }
                    });
                    ui.label(
                        RichText::new("Lyrics come from lrclib.net. Lyrics late? Drag the timing slider right. Early? Drag it left.")
                            .color(MUTED)
                            .size(12.0),
                    );
                });

                card(ui, "EFFECTS", |ui| {
                    combo(
                        ui,
                        "Bar style",
                        &mut s.bar_style,
                        &[("bars", "Bars"), ("rounded", "Rounded"), ("dots", "Dots (LED meter)"), ("line", "Line (wave)")],
                    );
                    toggle(ui, "Peak caps", &mut s.fx_peaks);
                    let off_pct = |x: f64| if x < 0.01 { "Off".to_string() } else { pct(x) };
                    slider(ui, "Beat glow (screen edges light up)", &mut s.fx_glow, 0.0..=1.0, off_pct);
                    slider(ui, "Bass shake", &mut s.fx_shake, 0.0..=1.0, off_pct);
                    slider(ui, "RGB split glitch", &mut s.fx_glitch, 0.0..=1.0, off_pct);
                    slider(ui, "Particles", &mut s.fx_particles, 0.0..=1.0, off_pct);
                    slider(ui, "Afterglow trails", &mut s.fx_trails, 0.0..=0.9, off_pct);
                    slider(ui, "Circle spin", &mut s.circle_spin, -1.0..=1.0, |x| {
                        if x.abs() < 0.02 { "Off".into() } else if x > 0.0 { format!("→ {:.0}%", x * 100.0) } else { format!("← {:.0}%", -x * 100.0) }
                    });
                    slider(ui, "Circle bass zoom", &mut s.circle_zoom, 0.0..=1.0, off_pct);
                    slider(ui, "Hyperspace (stars)", &mut s.hyperspace, 0.0..=1.0, off_pct);
                    if s.hyperspace > 0.0 {
                        slider(ui, "Warp speed", &mut s.hyper_speed, 0.0..=1.0, pct);
                        toggle(ui, "Stars match edge colors", &mut s.hyper_follow);
                    }
                    slider(ui, "Beat sensitivity", &mut s.rgb_sensitivity, 0.0..=1.0, pct);
                    ui.label(
                        RichText::new("Beat effects react to kick drums. Beat glow and glitch flash on the beat: turn them down if flashing bothers you.")
                            .color(MUTED)
                            .size(12.0),
                    );
                });

                let rgb_status = self.status.rgb.clone();
                card(ui, "RGB SYNC (KEYBOARD & MOUSE)", |ui| {
                    toggle(ui, "Flash RGB on the beat", &mut s.rgb_enabled);
                    if s.rgb_enabled {
                        combo(ui, "Effect", &mut s.rgb_effect, &[("flash", "Flash"), ("ripple", "Ripple (per-key keyboards)")]);
                        toggle(ui, "Match edge colors", &mut s.rgb_follow);
                        if !s.rgb_follow {
                            color_row(ui, "Flash color", &mut s.rgb_color);
                        }
                        toggle(ui, "Corsair (iCUE)", &mut s.rgb_corsair);
                        toggle(ui, "SteelSeries (GG)", &mut s.rgb_steelseries);
                        let text = if rgb_status.is_empty() { "Connecting…".to_string() } else { rgb_status.clone() };
                        for line in text.lines() {
                            let color = if line.ends_with('✓') { GREEN } else if line.contains("Connecting") { MUTED } else { WARN };
                            ui.label(RichText::new(line).color(color).size(12.0));
                        }
                        ui.label(
                            RichText::new(
                                "Corsair needs iCUE open with third-party control (SDK) turned on in its settings. \
                                 SteelSeries needs GG open. Your normal lighting comes back between flashes.",
                            )
                            .color(MUTED)
                            .size(12.0),
                        );
                    }
                });

                let autostart = &mut self.autostart;
                let confirm_reset = &mut self.confirm_reset;
                let update_msg = self.update_msg.clone();
                card(ui, "GENERAL", |ui| {
                    row(ui, "Start with Windows", |ui| {
                        if ui.checkbox(autostart, "").changed() {
                            sys::set_autostart(*autostart);
                        }
                    });
                    toggle(ui, "Auto-update", &mut s.auto_update);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Version {}", crate::update::VERSION)).color(MUTED).size(12.5));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let busy = update_msg.lock().unwrap().ends_with('…');
                            if ui.add_enabled(!busy, egui::Button::new("Check for updates")).clicked() {
                                check_now(update_msg.clone(), ctx.clone());
                            }
                        });
                    });
                    let msg = update_msg.lock().unwrap().clone();
                    if !msg.is_empty() {
                        ui.label(RichText::new(msg).color(MUTED).size(12.0));
                    }
                    ui.add_space(4.0);
                    egui::Grid::new("keys").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
                        for (k, d) in [
                            ("Ctrl+Alt+V", "Show / hide"),
                            ("Ctrl+Alt+S", "Open settings"),
                            ("Ctrl+Alt+M", "Switch style"),
                            ("Ctrl+Alt+C", "Switch colors"),
                            ("Ctrl+Alt+T", "Song text on / off"),
                            ("Ctrl+Alt+Up / Down", "Intensity up / down"),
                        ] {
                            ui.label(RichText::new(k).monospace().size(12.0));
                            ui.label(RichText::new(d).color(MUTED).size(12.5));
                            ui.end_row();
                        }
                    });
                    ui.add_space(8.0);
                    let full = egui::vec2(ui.available_width(), 30.0);
                    if *confirm_reset {
                        ui.label("Reset every setting to default?");
                        ui.horizontal(|ui| {
                            if ui.button("Yes, reset").clicked() {
                                *s = Settings::default();
                                *confirm_reset = false;
                            }
                            if ui.button("Cancel").clicked() {
                                *confirm_reset = false;
                            }
                        });
                    } else if ui.add_sized(full, egui::Button::new("Reset to defaults")).clicked() {
                        *confirm_reset = true;
                    }
                    if ui.add_sized(full, egui::Button::new("Quit Edge Visualizer")).clicked() {
                        sys::quit_overlay();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    ui.label(
                        RichText::new("Closing this window keeps the visualizer running in the tray. Settings save automatically.")
                            .color(MUTED)
                            .size(12.0),
                    );
                });
            });
        });

        self.save_if_changed();
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}
