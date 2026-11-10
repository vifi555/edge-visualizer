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
                        "Colors",
                        &mut s.theme,
                        &[("rainbow", "Rainbow"), ("fire", "Fire"), ("ice", "Ice"), ("white", "White")],
                    );
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

                let autostart = &mut self.autostart;
                let confirm_reset = &mut self.confirm_reset;
                card(ui, "GENERAL", |ui| {
                    row(ui, "Start with Windows", |ui| {
                        if ui.checkbox(autostart, "").changed() {
                            sys::set_autostart(*autostart);
                        }
                    });
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
