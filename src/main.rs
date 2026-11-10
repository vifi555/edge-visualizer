#![windows_subsystem = "windows"]

mod audio;
mod lyrics;
mod media;
mod overlay;
mod rgb;
mod settings;
mod sys;
mod ui;
mod update;
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

    // Started by an update: the old version is still closing, so wait for it.
    let updated = args.windows(2).find(|w| w[0] == "--updated").map(|w| w[1].clone());
    let mut first = sys::claim("EdgeVisualizer.Overlay");
    if !first && updated.is_some() {
        for _ in 0..80 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if sys::claim("EdgeVisualizer.Overlay") {
                first = true;
                break;
            }
        }
    }
    // Already running? Just open settings.
    if !first {
        sys::open_settings();
        return;
    }

    sys::reset_log();
    if !has("--hidden") && updated.is_none() {
        sys::open_settings();
    }
    if let Err(e) = overlay::run(updated) {
        let msg = format!("Edge Visualizer stopped:\n\n{e}");
        sys::log(&msg);
        sys::error_box(&msg);
    }
}
