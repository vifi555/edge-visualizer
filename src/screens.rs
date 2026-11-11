//! The windows the visualizer draws into:
//! - one per chosen monitor: on top of your apps (click-through), or behind the
//!   desktop icons in wallpaper mode
//! - optionally a normal "OBS" window with a solid background for streaming.
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::overlay::{push_event, Ev};
use crate::settings::Settings;

const SURFACE_CLASS: PCWSTR = w!("EdgeVisualizerSurface");
const OBS_CLASS: PCWSTR = w!("EdgeVisualizerObs");

#[derive(Clone, Copy)]
pub struct Win {
    pub hwnd: HWND,
    pub w: u32,
    pub h: u32,
    pub obs: bool,
    pub wallpaper: bool,
}

extern "system" fn surface_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DPICHANGED {
        push_event(Ev::Display);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

extern "system" fn obs_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            push_event(Ev::ObsClosed);
            return LRESULT(0);
        }
        WM_SIZE => push_event(Ev::Resized(hwnd.0 as isize)),
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let a = WNDCLASSW {
            lpfnWndProc: Some(surface_proc),
            hInstance: hinst,
            lpszClassName: SURFACE_CLASS,
            ..Default::default()
        };
        RegisterClassW(&a);
        let b = WNDCLASSW {
            lpfnWndProc: Some(obs_proc),
            hInstance: hinst,
            lpszClassName: OBS_CLASS,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hIcon: crate::overlay::load_icon(32),
            ..Default::default()
        };
        RegisterClassW(&b);
    });
}

// ---------------------------------------------------------------- monitors
unsafe extern "system" fn monitor_cb(hmon: HMONITOR, _hdc: HDC, _r: *mut RECT, data: LPARAM) -> BOOL {
    let list = &mut *(data.0 as *mut Vec<(RECT, bool)>);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(hmon, &mut mi).as_bool() {
        list.push((mi.rcMonitor, mi.dwFlags & 1 != 0)); // 1 = MONITORINFOF_PRIMARY
    }
    TRUE
}

/// All monitors (in pixels), main screen first.
pub fn monitors() -> Vec<RECT> {
    let mut list: Vec<(RECT, bool)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(monitor_cb), LPARAM(&mut list as *mut _ as isize));
    }
    list.sort_by_key(|(r, primary)| (!*primary, r.left, r.top));
    let mons: Vec<RECT> = list.into_iter().map(|(r, _)| r).collect();
    if mons.is_empty() {
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        return vec![RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        }];
    }
    mons
}

// ---------------------------------------------------------------- wallpaper mode
unsafe extern "system" fn workerw_cb(top: HWND, data: LPARAM) -> BOOL {
    if let Ok(view) = FindWindowExW(top, HWND::default(), w!("SHELLDLL_DefView"), PCWSTR::null()) {
        if !view.is_invalid() {
            // the WorkerW right after the one holding the desktop icons draws the wallpaper
            if let Ok(ww) = FindWindowExW(HWND::default(), top, w!("WorkerW"), PCWSTR::null()) {
                *(data.0 as *mut HWND) = ww;
            }
        }
    }
    TRUE
}

/// The desktop window that sits between the wallpaper and the icons.
fn find_workerw() -> Option<HWND> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), PCWSTR::null()).ok()?;
        if progman.is_invalid() {
            return None;
        }
        // Ask Explorer to create that layer (the same trick live-wallpaper apps use).
        let mut res = 0usize;
        SendMessageTimeoutW(progman, 0x052C, WPARAM(0xD), LPARAM(1), SMTO_NORMAL, 1000, Some(&mut res));
        SendMessageTimeoutW(progman, 0x052C, WPARAM(0), LPARAM(0), SMTO_NORMAL, 1000, Some(&mut res));
        // Newer Windows 11: it's a child of Progman
        if let Ok(ww) = FindWindowExW(progman, HWND::default(), w!("WorkerW"), PCWSTR::null()) {
            if !ww.is_invalid() {
                return Some(ww);
            }
        }
        let mut found = HWND::default();
        let _ = EnumWindows(Some(workerw_cb), LPARAM(&mut found as *mut HWND as isize));
        (!found.is_invalid()).then_some(found)
    }
}

// ---------------------------------------------------------------- building the windows
/// Makes the windows for the current settings. Also returns a warning to show, if any.
pub fn build(s: &Settings) -> Result<(Vec<Win>, Option<String>)> {
    register();
    let mons = monitors();
    let chosen: Vec<RECT> = match s.display.as_str() {
        "all" => mons.clone(),
        "2" => vec![*mons.get(1).unwrap_or(&mons[0])],
        "3" => vec![*mons.get(2).unwrap_or(&mons[0])],
        _ => vec![mons[0]],
    };
    let mut warn = None;
    let worker = if s.wallpaper {
        let w = find_workerw();
        if w.is_none() {
            warn = Some("Wallpaper mode isn't available here, drawing on top instead".to_string());
        }
        w
    } else {
        None
    };
    let mut wins = Vec::new();
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        for r in chosen {
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let hwnd = if let Some(ww) = worker {
                // child of the desktop layer: behind icons and every app
                CreateWindowExW(
                    WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE,
                    SURFACE_CLASS,
                    w!("Edge Visualizer Wallpaper"),
                    WS_CHILD | WS_VISIBLE,
                    r.left - vx,
                    r.top - vy,
                    w,
                    h,
                    ww,
                    HMENU::default(),
                    hinst,
                    None,
                )?
            } else {
                let hwnd = CreateWindowExW(
                    WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    SURFACE_CLASS,
                    w!("Edge Visualizer Overlay"),
                    WS_POPUP,
                    r.left,
                    r.top,
                    w,
                    h,
                    HWND::default(),
                    HMENU::default(),
                    hinst,
                    None,
                )?;
                // Layered + transparent = clicks go straight through to the apps underneath.
                SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
                hwnd
            };
            wins.push(Win {
                hwnd,
                w: w.max(1) as u32,
                h: h.max(1) as u32,
                obs: false,
                wallpaper: worker.is_some(),
            });
        }
        if s.obs_window {
            // a normal window you can add to OBS as "Window Capture"
            let mut rc = RECT {
                left: 0,
                top: 0,
                right: 1280,
                bottom: 720,
            };
            let _ = AdjustWindowRect(&mut rc, WS_OVERLAPPEDWINDOW, false);
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                OBS_CLASS,
                w!("Edge Visualizer (OBS)"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rc.right - rc.left,
                rc.bottom - rc.top,
                HWND::default(),
                HMENU::default(),
                hinst,
                None,
            )?;
            let (w, h) = client_size(hwnd);
            wins.push(Win {
                hwnd,
                w,
                h,
                obs: true,
                wallpaper: false,
            });
        }
    }
    Ok((wins, warn))
}

pub fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    ((rc.right - rc.left).max(1) as u32, (rc.bottom - rc.top).max(1) as u32)
}

pub fn destroy(wins: &[Win]) {
    for w in wins {
        unsafe {
            let _ = DestroyWindow(w.hwnd);
        }
    }
}

/// Show/hide and keep on top (or not), per the settings.
pub fn apply(wins: &[Win], s: &Settings) {
    for w in wins {
        unsafe {
            if w.obs {
                continue;
            }
            if w.wallpaper {
                let _ = ShowWindow(w.hwnd, if s.visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
                continue;
            }
            let after = if s.on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
            let show = if s.visible { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
            let _ = SetWindowPos(w.hwnd, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | show);
        }
    }
}
