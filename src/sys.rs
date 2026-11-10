//! Small Windows helpers: single instance, opening settings, autostart, logging.
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings;

const NO_WINDOW: u32 = 0x0800_0000;
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Edge Visualizer";
pub const OVERLAY_CLASS: PCWSTR = w!("EdgeVisualizerOverlay");
pub const SETTINGS_TITLE: &str = "Edge Visualizer Settings";

/// True if we're the first process with this name. The mutex stays open until exit.
pub fn claim(name: &str) -> bool {
    unsafe {
        let handle = CreateMutexW(None, true, &HSTRING::from(name));
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        match handle {
            Ok(h) if exists => {
                let _ = CloseHandle(h);
                false
            }
            _ => true,
        }
    }
}

pub fn open_settings() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).arg("--settings").spawn();
    }
}

pub fn focus_settings() {
    unsafe {
        if let Ok(h) = FindWindowW(PCWSTR::null(), &HSTRING::from(SETTINGS_TITLE)) {
            if !h.is_invalid() {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            }
        }
    }
}

pub fn quit_overlay() {
    unsafe {
        if let Ok(h) = FindWindowW(OVERLAY_CLASS, PCWSTR::null()) {
            if !h.is_invalid() {
                let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

pub fn error_box(msg: &str) {
    unsafe {
        MessageBoxW(HWND::default(), &HSTRING::from(msg), w!("Edge Visualizer"), MB_ICONERROR | MB_OK);
    }
}

pub fn reset_log() {
    let _ = std::fs::create_dir_all(settings::dir());
    let _ = std::fs::write(settings::dir().join("log.txt"), "");
}

pub fn log(msg: &str) {
    let _ = std::fs::create_dir_all(settings::dir());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(settings::dir().join("log.txt"))
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

pub fn autostart_on() -> bool {
    Command::new("reg")
        .args(["query", RUN_KEY, "/v", RUN_NAME])
        .creation_flags(NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn set_autostart(on: bool) {
    let mut cmd = Command::new("reg");
    if on {
        let Ok(exe) = std::env::current_exe() else { return };
        let data = format!("\"{}\" --hidden", exe.display());
        cmd.args(["add", RUN_KEY, "/v", RUN_NAME, "/t", "REG_SZ", "/d", &data, "/f"]);
    } else {
        cmd.args(["delete", RUN_KEY, "/v", RUN_NAME, "/f"]);
    }
    let _ = cmd.creation_flags(NO_WINDOW).output();
}
