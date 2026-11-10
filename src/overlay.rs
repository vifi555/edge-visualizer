//! The see-through, click-through layer over your screen, drawn on the GPU
//! (Direct2D + DirectComposition), plus the tray icon and hotkeys.
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, Interface, Result, HSTRING, PCWSTR};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::lyrics::{Lyrics, LyricsState};
use crate::media::{self, Track};
use crate::settings::{self, Settings, Status};
use crate::sys;
use crate::viz::{Analyzer, FFT_SIZE};

const WM_APP_TRAY: u32 = WM_APP + 1;

// ---------------------------------------------------------------- window messages -> events
enum Ev {
    TrayLeft,
    TrayRight,
    Hotkey(i32),
    Display,
    TaskbarCreated,
    Close,
}

thread_local! {
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
}
static TASKBAR_MSG: AtomicU32 = AtomicU32::new(0);

fn push(e: Ev) {
    EVENTS.with(|v| v.borrow_mut().push(e));
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_APP_TRAY => match (lp.0 as u32) & 0xFFFF {
            WM_LBUTTONUP => push(Ev::TrayLeft),
            WM_RBUTTONUP => push(Ev::TrayRight),
            _ => {}
        },
        WM_HOTKEY => push(Ev::Hotkey(wp.0 as i32)),
        WM_DISPLAYCHANGE | WM_DPICHANGED => push(Ev::Display),
        WM_CLOSE => {
            push(Ev::Close);
            return LRESULT(0);
        }
        _ => {
            let tb = TASKBAR_MSG.load(Ordering::Relaxed);
            if tb != 0 && msg == tb {
                push(Ev::TaskbarCreated);
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

// ---------------------------------------------------------------- small helpers
fn col(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}
fn pt(x: f32, y: f32) -> D2D_POINT_2F {
    D2D_POINT_2F { x, y }
}
fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}
fn scale_around(s: f32, cx: f32, cy: f32) -> Matrix3x2 {
    Matrix3x2 {
        M11: s,
        M12: 0.0,
        M21: 0.0,
        M22: s,
        M31: cx - s * cx,
        M32: cy - s * cy,
    }
}
fn identity() -> Matrix3x2 {
    scale_around(1.0, 0.0, 0.0)
}

fn hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (r + m, g + m, b + m)
}

fn hex(c: u32) -> (f32, f32, f32) {
    (
        ((c >> 16) & 255) as f32 / 255.0,
        ((c >> 8) & 255) as f32 / 255.0,
        (c & 255) as f32 / 255.0,
    )
}

const FIRE: [u32; 5] = [0xff1e00, 0xff6a00, 0xffc400, 0xff6a00, 0xff1e00];
const ICE: [u32; 5] = [0x00e5ff, 0x3d7bff, 0xb45cff, 0x3d7bff, 0x00e5ff];

fn stops(list: &[u32], f: f32) -> (f32, f32, f32) {
    let x = f.clamp(0.0, 1.0) * (list.len() - 1) as f32;
    let i = (x.floor() as usize).min(list.len() - 2);
    let t = x - i as f32;
    let (a, b) = (hex(list[i]), hex(list[i + 1]));
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t)
}

/// Colour of a bar from its direction around the screen centre (rotates over time).
fn bar_color(s: &Settings, t: f32, dx: f32, dy: f32) -> D2D1_COLOR_F {
    let frac = ((dy.atan2(dx) - t * 0.6 * s.color_speed) / std::f32::consts::TAU).rem_euclid(1.0);
    let (r, g, b) = match s.theme.as_str() {
        "fire" => stops(&FIRE, frac),
        "ice" => stops(&ICE, frac),
        "white" => (1.0, 1.0, 1.0),
        _ => hsl(frac * 360.0, 1.0, 0.6),
    };
    col(r, g, b, 1.0)
}

fn glow_color(s: &Settings, t: f32) -> D2D1_COLOR_F {
    let (r, g, b) = match s.theme.as_str() {
        "fire" => hex(0xff5a00),
        "ice" => hex(0x4aa8ff),
        "white" => (1.0, 1.0, 1.0),
        _ => hsl(t * 36.0 * s.color_speed, 1.0, 0.6),
    };
    col(r, g, b, 1.0)
}

fn screen() -> (i32, i32, f32) {
    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN).max(320);
        let h = GetSystemMetrics(SM_CYSCREEN).max(240);
        let dpi = GetDpiForSystem().max(96) as f32 / 96.0;
        (w, h, dpi)
    }
}

// ---------------------------------------------------------------- GPU drawing
struct Gfx {
    swap: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    target: Option<ID2D1Bitmap1>,
    _comp: (IDCompositionDevice, IDCompositionTarget, IDCompositionVisual),
    brush: ID2D1SolidColorBrush,
    blur: ID2D1Effect,
    dwrite: IDWriteFactory,
    w: f32,
    h: f32,
}

impl Gfx {
    fn new(hwnd: HWND, w: u32, h: u32) -> Result<Self> {
        unsafe {
            let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
            let mut d3d: Option<ID3D11Device> = None;
            if D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )
            .is_err()
            {
                // No usable GPU driver: fall back to Windows' software renderer.
                D3D11CreateDevice(
                    None::<&IDXGIAdapter>,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut d3d),
                    None,
                    None,
                )?;
            }
            let d3d = d3d.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            if let Ok(d1) = dxgi.cast::<IDXGIDevice1>() {
                let _ = d1.SetMaximumFrameLatency(1);
            }

            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dev = factory.CreateDevice(&dxgi)?;
            let dc = dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w,
                Height: h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: FALSE,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };
            let swap = dxgi_factory.CreateSwapChainForComposition(&dxgi, &desc, None::<&IDXGIOutput>)?;

            let comp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let ctarget = comp.CreateTargetForHwnd(hwnd, true)?;
            let visual = comp.CreateVisual()?;
            visual.SetContent(&swap)?;
            ctarget.SetRoot(&visual)?;
            comp.Commit()?;

            let brush = dc.CreateSolidColorBrush(&col(1.0, 1.0, 1.0, 1.0), None)?;
            let blur = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
            // "speed" quality for the glow blur
            let _ = blur.SetValue(
                D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION.0 as u32,
                D2D1_PROPERTY_TYPE_ENUM,
                &0u32.to_le_bytes(),
            );
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let mut g = Gfx {
                swap,
                dc,
                target: None,
                _comp: (comp, ctarget, visual),
                brush,
                blur,
                dwrite,
                w: w as f32,
                h: h as f32,
            };
            g.make_target()?;
            Ok(g)
        }
    }

    fn make_target(&mut self) -> Result<()> {
        unsafe {
            let surface: IDXGISurface = self.swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            self.target = Some(self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?);
            Ok(())
        }
    }

    fn resize(&mut self, w: u32, h: u32) -> Result<()> {
        unsafe {
            self.target = None;
            self.dc.SetTarget(None::<&ID2D1Image>);
            self.swap
                .ResizeBuffers(2, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))?;
        }
        self.w = w as f32;
        self.h = h as f32;
        self.make_target()
    }

    fn text_layout(
        &self,
        text: &str,
        family: &str,
        weight: DWRITE_FONT_WEIGHT,
        size: f32,
        max_w: f32,
    ) -> Result<(IDWriteTextLayout, f32, f32)> {
        unsafe {
            let fmt = self.dwrite.CreateTextFormat(
                &HSTRING::from(family),
                None::<&IDWriteFontCollection>,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let wide: Vec<u16> = text.encode_utf16().collect();
            let layout = self.dwrite.CreateTextLayout(&wide, &fmt, max_w, 100_000.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok((layout, m.width, m.height))
        }
    }

    fn draw_edge(&self, cur: &[f32], s: &Settings, t: f32) {
        let (w, h) = (self.w, self.h);
        let n = cur.len();
        let m = n * 2;
        let step = 2.0 * (w + h) / m as f32;
        let bw = (step * 0.62).max(2.0);
        let max_len = w.min(h) * 0.11 * s.size;
        for i in 0..m {
            let k = if i < n { i } else { m - 1 - i };
            let len = cur[k] * max_len;
            if len < 0.5 {
                continue;
            }
            // distance along the border, starting bottom-centre going left
            let mut d = (i as f32 + 0.5) * step;
            let r = if d < w / 2.0 {
                rect(w / 2.0 - d - bw / 2.0, h - len, w / 2.0 - d + bw / 2.0, h)
            } else {
                d -= w / 2.0;
                if d < h {
                    rect(0.0, h - d - bw / 2.0, len, h - d + bw / 2.0)
                } else {
                    d -= h;
                    if d < w {
                        rect(d - bw / 2.0, 0.0, d + bw / 2.0, len)
                    } else {
                        d -= w;
                        if d < h {
                            rect(w - len, d - bw / 2.0, w, d + bw / 2.0)
                        } else {
                            d -= h;
                            rect(w - d - bw / 2.0, h - len, w - d + bw / 2.0, h)
                        }
                    }
                }
            };
            let cx = (r.left + r.right) / 2.0 - w / 2.0;
            let cy = (r.top + r.bottom) / 2.0 - h / 2.0;
            unsafe {
                self.brush.SetColor(&bar_color(s, t, cx, cy));
                self.dc.FillRectangle(&r, &self.brush);
            }
        }
    }

    fn draw_circle(&self, cur: &[f32], bass: f32, s: &Settings, t: f32) {
        let n = cur.len();
        let m = n * 2;
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let min = self.w.min(self.h);
        let r = min * 0.17 * s.size * (1.0 + 0.06 * bass);
        let max_len = min * 0.16 * s.size;
        let bw = (std::f32::consts::TAU * r / m as f32 * 0.6).max(1.5);
        unsafe {
            for i in 0..m {
                let k = if i < n { i } else { m - 1 - i };
                let len = cur[k] * max_len;
                if len < 0.5 {
                    continue;
                }
                let a = std::f32::consts::FRAC_PI_2 + (i as f32 + 0.5) / m as f32 * std::f32::consts::TAU;
                let (c, sn) = (a.cos(), a.sin());
                self.brush.SetColor(&bar_color(s, t, c, sn));
                self.dc.DrawLine(
                    pt(cx + c * r, cy + sn * r),
                    pt(cx + c * (r + len), cy + sn * (r + len)),
                    &self.brush,
                    bw,
                    None::<&ID2D1StrokeStyle>,
                );
            }
            let level = (bass * 2.5).min(1.0);
            if level > 0.02 {
                self.brush.SetOpacity(s.opacity * level);
                self.brush.SetColor(&glow_color(s, t));
                let e = D2D1_ELLIPSE {
                    point: pt(cx, cy),
                    radiusX: r - 6.0,
                    radiusY: r - 6.0,
                };
                self.dc.DrawEllipse(&e, &self.brush, 3.0, None::<&ID2D1StrokeStyle>);
                self.brush.SetOpacity(s.opacity);
            }
        }
    }
}

// ---------------------------------------------------------------- song text / lyrics
struct TextState {
    main: String,
    lyric: bool,
    shown_main: String,
    shown_sub: String,
    shown_lyric: bool,
    changed: Instant,
    vis: f32,
    main_lay: Option<(String, IDWriteTextLayout, f32)>,
    sub_lay: Option<(String, IDWriteTextLayout, f32)>,
}

impl TextState {
    fn new() -> Self {
        Self {
            main: String::new(),
            lyric: false,
            shown_main: String::new(),
            shown_sub: String::new(),
            shown_lyric: false,
            changed: Instant::now(),
            vis: 0.0,
            main_lay: None,
            sub_lay: None,
        }
    }
}

/// What to show: the line being sung right now, or the song name.
fn pick_text(track: Option<&Track>, ly: &LyricsState, s: &Settings) -> (String, String, bool) {
    let Some(tr) = track else {
        return (String::new(), String::new(), false);
    };
    match s.text_shows.as_str() {
        "title" => return (tr.title.clone(), String::new(), false),
        "both" => return (tr.title.clone(), tr.artist.clone(), false),
        _ => {}
    }
    let Some(lines) = ly.lines.as_ref().filter(|l| !l.is_empty()) else {
        return (tr.title.clone(), tr.artist.clone(), false);
    };
    let mut pos = tr.pos_ms + if tr.playing { media::now_ms() - tr.upd_ms } else { 0.0 } + s.lyrics_offset as f64 * 1000.0;
    if tr.dur_ms > 0.0 {
        pos = pos.min(tr.dur_ms + 1000.0);
    }
    let i = match lines.iter().rposition(|l| l.t <= pos) {
        Some(i) => i,
        None => return (tr.title.clone(), tr.artist.clone(), false), // before the singing starts
    };
    let line = &lines[i];
    if line.text.is_empty() || (i == lines.len() - 1 && pos - line.t > 10_000.0) {
        return (String::new(), String::new(), true); // nobody singing right now
    }
    let next = lines.get(i + 1).map(|l| l.text.clone()).unwrap_or_default();
    (line.text.clone(), next, true)
}

fn font(s: &Settings) -> (&'static str, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT) {
    match s.text_font.as_str() {
        "impact" => ("Impact", DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_NORMAL),
        "mono" => ("Consolas", DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL),
        "script" => ("Segoe Script", DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL),
        _ => ("Segoe UI", DWRITE_FONT_WEIGHT_BLACK, DWRITE_FONT_WEIGHT_SEMI_BOLD),
    }
}

fn draw_text(g: &Gfx, ts: &mut TextState, s: &Settings, bass: f32, dpi: f32) -> Result<()> {
    if ts.vis <= 0.01 || ts.shown_main.is_empty() {
        return Ok(());
    }
    let (family, w_main, w_sub) = font(s);
    let size = s.text_size * dpi;
    let sub_size = (size * 0.42).max(10.0 * dpi);
    let max_w = g.w * 0.8;
    let sub_text = if ts.shown_lyric {
        ts.shown_sub.clone()
    } else {
        ts.shown_sub.to_uppercase()
    };

    let key_main = format!("{}|{}|{}|{}", ts.shown_main, family, size, max_w);
    if ts.main_lay.as_ref().map(|x| &x.0) != Some(&key_main) {
        let (l, _, h) = g.text_layout(&ts.shown_main, family, w_main, size, max_w)?;
        ts.main_lay = Some((key_main, l, h));
    }
    let key_sub = format!("{}|{}|{}|{}", sub_text, family, sub_size, max_w);
    if sub_text.is_empty() {
        ts.sub_lay = None;
    } else if ts.sub_lay.as_ref().map(|x| &x.0) != Some(&key_sub) {
        let (l, _, h) = g.text_layout(&sub_text, family, w_sub, sub_size, max_w)?;
        ts.sub_lay = Some((key_sub, l, h));
    }

    let edge = if s.mode != "circle" { g.w.min(g.h) * 0.11 * s.size } else { 0.0 };
    let bottom = (edge + (30.0 + s.text_offset) * dpi).max(0.0);
    let pop = (ts.changed.elapsed().as_secs_f32() / 0.28).min(1.0);
    let y_off = 14.0 * dpi * (1.0 - pop);
    let main_h = ts.main_lay.as_ref().map(|x| x.2).unwrap_or(0.0);
    let sub_h = ts.sub_lay.as_ref().map(|x| x.2 + sub_size * 0.3).unwrap_or(0.0);
    let top = g.h - bottom - main_h - sub_h;
    let x0 = (g.w - max_w) / 2.0;
    let alpha = ts.vis * s.opacity;
    let pulse = 1.0 + 0.15 * s.text_pulse * bass;

    unsafe {
        g.dc.SetTransform(&scale_around(pulse, g.w / 2.0, g.h - bottom));
        g.brush.SetOpacity(1.0);
        if let Some((_, l, _)) = &ts.main_lay {
            let a = alpha * pop;
            g.brush.SetColor(&col(0.0, 0.0, 0.0, a * 0.6));
            g.dc.DrawTextLayout(
                pt(x0 + 2.0 * dpi, top + y_off + 2.0 * dpi),
                l,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
            g.brush.SetColor(&col(1.0, 1.0, 1.0, a));
            g.dc.DrawTextLayout(pt(x0, top + y_off), l, &g.brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
        if let Some((_, l, _)) = &ts.sub_lay {
            let y = top + main_h + sub_size * 0.3;
            let a = alpha * if ts.shown_lyric { 0.55 } else { 0.8 };
            g.brush.SetColor(&col(0.0, 0.0, 0.0, a * 0.6));
            g.dc.DrawTextLayout(
                pt(x0 + 1.5 * dpi, y + 1.5 * dpi),
                l,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
            g.brush.SetColor(&col(1.0, 1.0, 1.0, a));
            g.dc.DrawTextLayout(pt(x0, y), l, &g.brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
        g.dc.SetTransform(&identity());
    }
    Ok(())
}

// ---------------------------------------------------------------- toast (small message at the top)
struct Toast {
    text: String,
    until: Instant,
    lay: Option<(String, IDWriteTextLayout, f32, f32)>,
}

fn draw_toast(g: &Gfx, toast: &mut Toast, dpi: f32) -> Result<()> {
    let left = toast.until.saturating_duration_since(Instant::now()).as_secs_f32();
    if left <= 0.0 || toast.text.is_empty() {
        return Ok(());
    }
    let a = (left / 0.35).min(1.0);
    if toast.lay.as_ref().map(|x| &x.0) != Some(&toast.text) {
        let (l, w, h) = g.text_layout(&toast.text, "Segoe UI", DWRITE_FONT_WEIGHT_SEMI_BOLD, 15.0 * dpi, g.w * 0.9)?;
        toast.lay = Some((toast.text.clone(), l, w, h));
    }
    let Some((_, l, tw, th)) = &toast.lay else { return Ok(()) };
    let (pad_x, pad_y) = (16.0 * dpi, 10.0 * dpi);
    let y = 48.0 * dpi;
    let bg = D2D1_ROUNDED_RECT {
        rect: rect(g.w / 2.0 - tw / 2.0 - pad_x, y, g.w / 2.0 + tw / 2.0 + pad_x, y + th + pad_y * 2.0),
        radiusX: 10.0 * dpi,
        radiusY: 10.0 * dpi,
    };
    unsafe {
        g.brush.SetOpacity(1.0);
        g.brush.SetColor(&col(0.04, 0.04, 0.055, 0.78 * a));
        g.dc.FillRoundedRectangle(&bg, &g.brush);
        g.brush.SetColor(&col(1.0, 1.0, 1.0, a));
        g.dc.DrawTextLayout(pt(g.w * 0.05, y + pad_y), l, &g.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    Ok(())
}

// ---------------------------------------------------------------- one frame
fn render(g: &Gfx, an: &Analyzer, s: &Settings, t: f32, dpi: f32, ts: &mut TextState, toast: &mut Toast) -> Result<()> {
    unsafe {
        let clear = col(0.0, 0.0, 0.0, 0.0);
        // Pass 1: bars + text into a command list (so we can blur it for the glow)
        let scene = g.dc.CreateCommandList()?;
        g.dc.SetTarget(&scene);
        g.dc.BeginDraw();
        g.dc.Clear(Some(&clear));
        g.brush.SetOpacity(s.opacity);
        if !an.cur.is_empty() {
            if s.mode != "circle" {
                g.draw_edge(&an.cur, s, t);
            }
            if s.mode != "edge" {
                g.draw_circle(&an.cur, an.bass(), s, t);
            }
        }
        let text_res = draw_text(g, ts, s, an.bass(), dpi);
        g.dc.EndDraw(None, None)?;
        scene.Close()?;
        text_res?;

        // Pass 2: glow + scene onto the screen
        let target = g.target.as_ref().ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        g.dc.SetTarget(target);
        g.dc.BeginDraw();
        g.dc.Clear(Some(&clear));
        if s.glow > 0.5 {
            let sigma = s.glow * 0.5 * dpi;
            g.blur.SetValue(
                D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &sigma.to_le_bytes(),
            )?;
            g.blur.SetInput(0, &scene, true);
            if let Ok(out) = g.blur.GetOutput() {
                g.dc.DrawImage(&out, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            }
        }
        g.dc.DrawImage(&scene, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
        let toast_res = draw_toast(g, toast, dpi);
        g.dc.EndDraw(None, None)?;
        toast_res?;
        g.swap.Present(1, DXGI_PRESENT(0)).ok()?;
    }
    Ok(())
}

// ---------------------------------------------------------------- tray + hotkeys + window
fn load_icon(size: i32) -> HICON {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        match LoadImageW(hinst, PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR) {
            Ok(h) if !h.is_invalid() => HICON(h.0),
            _ => LoadIconW(HINSTANCE::default(), IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: load_icon(unsafe { GetSystemMetrics(SM_CXSMICON) }),
        ..Default::default()
    };
    let tip: Vec<u16> = "Edge Visualizer".encode_utf16().collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    nid
}

fn add_tray(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd));
    }
}

const HK: [(i32, u32); 7] = [(1, 0x56), (2, 0x53), (3, 0x4D), (4, 0x43), (5, 0x54), (6, 0x26), (7, 0x28)]; // V S M C T Up Down

fn register_hotkeys(hwnd: HWND) {
    unsafe {
        for (id, vk) in HK {
            let _ = RegisterHotKey(hwnd, id, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, vk);
        }
    }
}

fn tray_menu(hwnd: HWND, s: &Settings) -> u32 {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        let add = |id: usize, text: &str, checked: bool| {
            let flags = MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(m, flags, id, &HSTRING::from(text));
        };
        let sep = || {
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        };
        add(1, "Settings…", false);
        sep();
        add(2, "Show visualizer", s.visible);
        add(3, "On top of apps", s.on_top);
        add(4, "Song text / lyrics", s.show_text);
        sep();
        add(10, "Screen edges", s.mode == "edge");
        add(11, "Circle", s.mode == "circle");
        add(12, "Both", s.mode == "both");
        sep();
        add(20, "Rainbow", s.theme == "rainbow");
        add(21, "Fire", s.theme == "fire");
        add(22, "Ice", s.theme == "ice");
        add(23, "White", s.theme == "white");
        sep();
        add(99, "Quit", false);

        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, hwnd, None);
        let _ = DestroyMenu(m);
        cmd.0 as u32
    }
}

fn create_window(w: i32, h: i32) -> Result<HWND> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: sys::OVERLAY_CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            sys::OVERLAY_CLASS,
            w!("Edge Visualizer Overlay"),
            WS_POPUP,
            0,
            0,
            w,
            h,
            HWND::default(),
            HMENU::default(),
            hinst,
            None,
        )?;
        // Layered + transparent = clicks go straight through to the apps underneath.
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
        TASKBAR_MSG.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        Ok(hwnd)
    }
}

fn apply_window(hwnd: HWND, s: &Settings, w: i32, h: i32) {
    unsafe {
        let after = if s.on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
        let show = if s.visible { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
        let _ = SetWindowPos(hwnd, after, 0, 0, w, h, SWP_NOACTIVATE | show);
    }
}

fn cycle(list: &[&str], cur: &str) -> String {
    let i = list.iter().position(|x| *x == cur).unwrap_or(0);
    list[(i + 1) % list.len()].to_string()
}

// ---------------------------------------------------------------- main loop
pub fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let mut s = Settings::load();
    let mut s_mtime = settings::mtime(&settings::settings_path());

    let audio = crate::audio::start();
    let media = media::start();
    let mut lyrics = Lyrics::new();

    let (mut sw, mut sh, mut dpi) = screen();
    let hwnd = create_window(sw, sh)?;
    let mut gfx = Some(Gfx::new(hwnd, sw as u32, sh as u32)?);
    add_tray(hwnd);
    register_hotkeys(hwnd);
    apply_window(hwnd, &s, sw, sh);

    let mut an = Analyzer::new();
    let mut samples = vec![0.0f32; FFT_SIZE];
    let mut ts = TextState::new();
    let mut toast = Toast {
        text: "Edge Visualizer is running. Click the tray icon for settings.".into(),
        until: Instant::now() + Duration::from_millis(3000),
        lay: None,
    };
    let start = Instant::now();
    let mut last = Instant::now();
    let mut last_poll = Instant::now();
    let mut status = Status::default();
    let mut gfx_error_logged = false;

    'main: loop {
        // ---- window messages
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'main;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // ---- tray / hotkeys
        let events: Vec<Ev> = EVENTS.with(|v| std::mem::take(&mut *v.borrow_mut()));
        let mut changed = false;
        for ev in events {
            match ev {
                Ev::Close => break 'main,
                Ev::TrayLeft => sys::open_settings(),
                Ev::TaskbarCreated => add_tray(hwnd),
                Ev::Display => {
                    let (w, h, d) = screen();
                    if (w, h) != (sw, sh) {
                        sw = w;
                        sh = h;
                        if let Some(g) = gfx.as_mut() {
                            if g.resize(sw as u32, sh as u32).is_err() {
                                gfx = None;
                            }
                        }
                        apply_window(hwnd, &s, sw, sh);
                    }
                    dpi = d;
                }
                Ev::TrayRight => match tray_menu(hwnd, &s) {
                    1 => sys::open_settings(),
                    2 => {
                        s.visible = !s.visible;
                        changed = true;
                    }
                    3 => {
                        s.on_top = !s.on_top;
                        changed = true;
                    }
                    4 => {
                        s.show_text = !s.show_text;
                        changed = true;
                    }
                    10 => {
                        s.mode = "edge".into();
                        changed = true;
                    }
                    11 => {
                        s.mode = "circle".into();
                        changed = true;
                    }
                    12 => {
                        s.mode = "both".into();
                        changed = true;
                    }
                    20 => {
                        s.theme = "rainbow".into();
                        changed = true;
                    }
                    21 => {
                        s.theme = "fire".into();
                        changed = true;
                    }
                    22 => {
                        s.theme = "ice".into();
                        changed = true;
                    }
                    23 => {
                        s.theme = "white".into();
                        changed = true;
                    }
                    99 => break 'main,
                    _ => {}
                },
                Ev::Hotkey(id) => {
                    let mut msg = String::new();
                    match id {
                        1 => s.visible = !s.visible,
                        2 => sys::open_settings(),
                        3 => {
                            s.mode = cycle(&["edge", "circle", "both"], &s.mode);
                            msg = format!("Style: {}", s.mode);
                        }
                        4 => {
                            s.theme = cycle(&["rainbow", "fire", "ice", "white"], &s.theme);
                            msg = format!("Colors: {}", s.theme);
                        }
                        5 => s.show_text = !s.show_text,
                        6 | 7 => {
                            let d = if id == 6 { 0.1 } else { -0.1 };
                            s.intensity = ((s.intensity + d).clamp(0.3, 3.0) * 100.0).round() / 100.0;
                            msg = format!("Intensity {}%", (s.intensity * 100.0).round());
                        }
                        _ => {}
                    }
                    if id != 2 {
                        changed = true;
                    }
                    if !msg.is_empty() {
                        toast.text = msg;
                        toast.until = Instant::now() + Duration::from_millis(2200);
                    }
                }
            }
        }
        if changed {
            let _ = s.save();
            s_mtime = settings::mtime(&settings::settings_path());
            apply_window(hwnd, &s, sw, sh);
        }

        // ---- settings changed in the settings window? + status for it
        if last_poll.elapsed() > Duration::from_millis(200) {
            last_poll = Instant::now();
            let m = settings::mtime(&settings::settings_path());
            if m != s_mtime {
                s_mtime = m;
                let fresh = Settings::load();
                if fresh != s {
                    s = fresh;
                    apply_window(hwnd, &s, sw, sh);
                }
            }
        }

        let track = media.lock().unwrap().clone();
        lyrics.update(track.as_ref());
        let ly = lyrics.snapshot();
        let new_status = Status {
            title: track.as_ref().map(|t| t.title.clone()).unwrap_or_default(),
            artist: track.as_ref().map(|t| t.artist.clone()).unwrap_or_default(),
            lyrics: ly.status.clone(),
        };
        if new_status != status {
            status = new_status;
            status.save();
        }

        if !s.visible {
            std::thread::sleep(Duration::from_millis(50));
            last = Instant::now();
            continue;
        }

        // ---- audio -> bars
        let dt = last.elapsed().as_secs_f32().min(0.1);
        last = Instant::now();
        let sr = audio.sample_rate.load(Ordering::Relaxed);
        audio.ring.lock().unwrap().latest(&mut samples);
        an.update(&samples, sr, &s, dt);

        // ---- text
        let (main, sub, lyric) = pick_text(track.as_ref(), &ly, &s);
        if main != ts.main || lyric != ts.lyric {
            if !main.is_empty() {
                ts.changed = Instant::now();
            }
            ts.main = main.clone();
            ts.lyric = lyric;
        }
        if !main.is_empty() {
            ts.shown_main = main;
            ts.shown_sub = sub;
            ts.shown_lyric = lyric;
        }
        let target_vis = if s.show_text && !ts.main.is_empty() { 1.0 } else { 0.0 };
        let step = dt / 0.6;
        ts.vis = if ts.vis < target_vis {
            (ts.vis + step).min(target_vis)
        } else {
            (ts.vis - step).max(target_vis)
        };

        // ---- draw
        if gfx.is_none() {
            match Gfx::new(hwnd, sw as u32, sh as u32) {
                Ok(g) => gfx = Some(g),
                Err(e) => {
                    if !gfx_error_logged {
                        sys::log(&format!("graphics init failed: {e}"));
                        gfx_error_logged = true;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }
        let frame_start = Instant::now();
        if let Some(g) = gfx.as_ref() {
            if let Err(e) = render(g, &an, &s, start.elapsed().as_secs_f32(), dpi, &mut ts, &mut toast) {
                sys::log(&format!("draw failed, restarting graphics: {e}"));
                gfx = None;
                ts.main_lay = None;
                ts.sub_lay = None;
                toast.lay = None;
            }
        }
        // Present() normally waits for the screen refresh; this is just a safety cap.
        let spent = frame_start.elapsed();
        if spent < Duration::from_millis(4) {
            std::thread::sleep(Duration::from_millis(4) - spent);
        }
    }

    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd));
        let _ = DestroyWindow(hwnd);
    }
    let _ = std::fs::remove_file(settings::status_path());
    Ok(())
}
