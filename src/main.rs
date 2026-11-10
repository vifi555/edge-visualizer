#![windows_subsystem = "windows"]

mod audio;
mod lyrics;
mod media;
mod overlay;
mod settings;
mod sys;
mod ui;
mod viz;

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("Edge Visualizer crashed:\n\n{info}");
        sys::log(&msg);
        sys::error_box(&msg);
    }));

    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    // Settings window runs as its own small process.
    if has("--settings") {
        if sys::claim("EdgeVisualizer.Settings") {
            if let Err(e) = ui::run() {
                sys::log(&format!("settings window error: {e}"));
            }
        } else {
            sys::focus_settings();
        }
        return;
    }

    // Already running? Just open settings.
    if !sys::claim("EdgeVisualizer.Overlay") {
        sys::open_settings();
        return;
    }

    sys::reset_log();
    if !has("--hidden") {
        sys::open_settings();
    }
    if let Err(e) = overlay::run() {
        let msg = format!("Edge Visualizer stopped:\n\n{e}");
        sys::log(&msg);
        sys::error_box(&msg);
    }
}
