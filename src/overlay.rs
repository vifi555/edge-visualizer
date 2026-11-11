//! The see-through, click-through layer over your screen, drawn on the GPU
//! (Direct2D + DirectComposition), plus the tray icon and hotkeys.
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use std::cell::Cell;
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

use crate::fx::{Fx, Tip};
use crate::lyrics::{Lyrics, LyricsState};
use crate::media::{self, Track};
use crate::settings::{self, Settings, Status};
use crate::sys;
use crate::viz::{Analyzer, BeatDetector, FFT_SIZE};

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

/// Colours for one part (edges or circle).
struct Palette {
    theme: String,
    a: (f32, f32, f32),
    b: (f32, f32, f32),
    speed: f32,
}

fn palette(s: &Settings, circle: bool) -> Palette {
    let (theme, a, b) = if circle {
        (&s.circle_theme, &s.circle_color_a, &s.circle_color_b)
    } else {
        (&s.edge_theme, &s.edge_color_a, &s.edge_color_b)
    };
    Palette {
        theme: theme.clone(),
        a: settings::parse_hex(a).unwrap_or((1.0, 1.0, 1.0)),
        b: settings::parse_hex(b).unwrap_or((1.0, 1.0, 1.0)),
        speed: s.color_speed,
    }
}

fn lerp3(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t)
}

/// Colour of a bar from its direction around the screen centre (rotates over time).
fn bar_color(p: &Palette, t: f32, dx: f32, dy: f32) -> D2D1_COLOR_F {
    let frac = ((dy.atan2(dx) - t * 0.6 * p.speed) / std::f32::consts::TAU).rem_euclid(1.0);
    let (r, g, b) = match p.theme.as_str() {
        "fire" => stops(&FIRE, frac),
        "ice" => stops(&ICE, frac),
        "white" => (1.0, 1.0, 1.0),
        "custom" => lerp3(p.a, p.b, if frac < 0.5 { frac * 2.0 } else { 2.0 - frac * 2.0 }),
        _ => hsl(frac * 360.0, 1.0, 0.6),
    };
    col(r, g, b, 1.0)
}

fn glow_color(p: &Palette, t: f32) -> D2D1_COLOR_F {
    let (r, g, b) = match p.theme.as_str() {
        "fire" => hex(0xff5a00),
        "ice" => hex(0x4aa8ff),
        "white" => (1.0, 1.0, 1.0),
        "custom" => lerp3(p.a, p.b, 0.5 + 0.5 * (t * p.speed).sin()),
        _ => hsl(t * 36.0 * p.speed, 1.0, 0.6),
    };
    col(r, g, b, 1.0)
}

/// Does the bottom edge have bars? (then the text sits above them)
fn bottom_bars(s: &Settings) -> bool {
    s.mode != "circle" && matches!(s.edges.as_str(), "all" | "bottom" | "topbottom")
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
    red: ID2D1Effect,  // RGB split: red channel only
    blue: ID2D1Effect, // RGB split: blue channel only
    round: ID2D1StrokeStyle1,
    trails: RefCell<[Option<ID2D1Bitmap1>; 2]>,
    trail_idx: Cell<usize>,
    trail_fresh: Cell<bool>,
    dwrite: IDWriteFactory,
    w: f32,
    h: f32,
}

/// Colour matrix (5 rows x 4) keeping one channel, for the RGB split glitch.
fn channel_matrix(channel: usize) -> Vec<u8> {
    let mut m = [0.0f32; 20];
    m[channel * 4 + channel] = 1.0; // keep r or b
    m[3 * 4 + 3] = 0.8; // keep most of the alpha
    m.iter().flat_map(|v| v.to_le_bytes()).collect()
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
            let red = dc.CreateEffect(&CLSID_D2D1ColorMatrix)?;
            red.SetValue(
                D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32,
                D2D1_PROPERTY_TYPE_MATRIX_5X4,
                &channel_matrix(0),
            )?;
            let blue = dc.CreateEffect(&CLSID_D2D1ColorMatrix)?;
            blue.SetValue(
                D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32,
                D2D1_PROPERTY_TYPE_MATRIX_5X4,
                &channel_matrix(2),
            )?;
            let round = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES1 {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 1.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                    transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
                },
                None,
            )?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let mut g = Gfx {
                swap,
                dc,
                target: None,
                _comp: (comp, ctarget, visual),
                brush,
                blur,
                red,
                blue,
                round,
                trails: RefCell::new([None, None]),
                trail_idx: Cell::new(0),
                trail_fresh: Cell::new(true),
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
            *self.trails.borrow_mut() = [None, None];
            self.trail_fresh.set(true);
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

    /// Bars around the screen edges, in the chosen style. Records each bar's tip for particles.
    fn draw_edge(&self, cur: &[f32], s: &Settings, t: f32, peaks: &[f32], tips: &mut Vec<Tip>) {
        let (w, h) = (self.w, self.h);
        // Path the bars follow: (start x, start y, direction x, direction y, length, side).
        // Bass sits where the path starts and ends, highs in the middle (mirrored).
        // Side: 0 bottom (grows up), 1 left (grows right), 2 top (grows down), 3 right (grows left).
        let path: Vec<(f32, f32, f32, f32, f32, u8)> = match s.edges.as_str() {
            "bottom" => vec![(w / 2.0, h, -1.0, 0.0, w / 2.0, 0), (w, h, -1.0, 0.0, w / 2.0, 0)],
            "sides" => vec![(0.0, h, 0.0, -1.0, h, 1), (w, 0.0, 0.0, 1.0, h, 3)],
            "topbottom" => vec![
                (w / 2.0, h, -1.0, 0.0, w / 2.0, 0),
                (0.0, 0.0, 1.0, 0.0, w, 2),
                (w, h, -1.0, 0.0, w / 2.0, 0),
            ],
            _ => vec![
                (w / 2.0, h, -1.0, 0.0, w / 2.0, 0),
                (0.0, h, 0.0, -1.0, h, 1),
                (0.0, 0.0, 1.0, 0.0, w, 2),
                (w, 0.0, 0.0, 1.0, h, 3),
                (w, h, -1.0, 0.0, w / 2.0, 0),
            ],
        };
        let total: f32 = path.iter().map(|p| p.4).sum();
        let n = cur.len();
        let m = n * 2;
        let step = total / m as f32;
        let bw = (step * 0.62).max(2.0);
        let max_len = w.min(h) * 0.11 * s.size;
        let line_w = (w.min(h) / 360.0).max(1.5);
        let pal = palette(s, false);
        let mut prev: Option<(f32, f32)> = None;
        for i in 0..m {
            let k = if i < n { i } else { m - 1 - i };
            let len = cur[k] * max_len;
            let mut d = (i as f32 + 0.5) * step;
            let mut seg = path[path.len() - 1];
            for p in &path {
                if d < p.4 {
                    seg = *p;
                    break;
                }
                d -= p.4;
            }
            let d = d.min(seg.4);
            let (x, y) = (seg.0 + seg.2 * d, seg.1 + seg.3 * d);
            // where the bar starts and which way it grows
            let (bx, by, dx, dy) = match seg.5 {
                0 => (x, h, 0.0, -1.0),
                1 => (0.0, y, 1.0, 0.0),
                2 => (x, 0.0, 0.0, 1.0),
                _ => (w, y, -1.0, 0.0),
            };
            let c = bar_color(&pal, t, x - w / 2.0, y - h / 2.0);
            let tip = (bx + dx * len, by + dy * len);
            tips.push(Tip {
                x: tip.0,
                y: tip.1,
                dx,
                dy,
                color: (c.r, c.g, c.b),
                level: cur[k],
            });
            // rectangle covering distance a..b along the bar
            let part = |a: f32, b: f32| {
                let (x0, y0, x1, y1) = (bx + dx * a, by + dy * a, bx + dx * b, by + dy * b);
                if dx == 0.0 {
                    rect(bx - bw / 2.0, y0.min(y1), bx + bw / 2.0, y0.max(y1))
                } else {
                    rect(x0.min(x1), by - bw / 2.0, x0.max(x1), by + bw / 2.0)
                }
            };
            unsafe {
                self.brush.SetColor(&c);
                match s.bar_style.as_str() {
                    "line" => {
                        if let Some(p) = prev {
                            self.dc.DrawLine(pt(p.0, p.1), pt(tip.0, tip.1), &self.brush, line_w, &self.round);
                        }
                        prev = Some(tip);
                    }
                    _ if len < 0.5 => {}
                    "rounded" => {
                        let rr = D2D1_ROUNDED_RECT {
                            rect: part(0.0, len),
                            radiusX: bw / 2.0,
                            radiusY: bw / 2.0,
                        };
                        self.dc.FillRoundedRectangle(&rr, &self.brush);
                    }
                    "dots" => {
                        let (dot, gap) = (bw, bw * 0.45);
                        let mut a = 0.0;
                        while a + dot <= len.max(dot) {
                            self.dc.FillRectangle(&part(a, a + dot), &self.brush);
                            a += dot + gap;
                        }
                    }
                    _ => self.dc.FillRectangle(&part(0.0, len), &self.brush),
                }
                if s.fx_peaks && s.bar_style != "line" {
                    let pk = peaks.get(k).copied().unwrap_or(0.0) * max_len;
                    if pk > 1.0 {
                        let th = (bw * 0.35).max(2.0);
                        self.dc.FillRectangle(&part(pk + bw * 0.4, pk + bw * 0.4 + th), &self.brush);
                    }
                }
            }
        }
    }

    /// The round visualizer in the middle.
    #[allow(clippy::too_many_arguments)]
    fn draw_circle(&self, cur: &[f32], bass: f32, s: &Settings, t: f32, peaks: &[f32], fx: (f32, f32), tips: &mut Vec<Tip>) {
        let (spin, punch) = fx;
        let n = cur.len();
        let m = n * 2;
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let min = self.w.min(self.h);
        let r = min * 0.17 * s.size * (1.0 + 0.06 * bass + 0.25 * punch);
        let max_len = min * 0.16 * s.size;
        let bw = (std::f32::consts::TAU * r / m as f32 * 0.6).max(1.5);
        let line_w = (min / 360.0).max(1.5);
        let pal = palette(s, true);
        let mut first: Option<(f32, f32)> = None;
        let mut prev: Option<(f32, f32)> = None;
        unsafe {
            for i in 0..m {
                let k = if i < n { i } else { m - 1 - i };
                let len = cur[k] * max_len;
                let a = std::f32::consts::FRAC_PI_2 + spin + (i as f32 + 0.5) / m as f32 * std::f32::consts::TAU;
                let (c, sn) = (a.cos(), a.sin());
                let at = |d: f32| pt(cx + c * (r + d), cy + sn * (r + d));
                let col_ = bar_color(&pal, t, c, sn);
                let tip = (cx + c * (r + len), cy + sn * (r + len));
                tips.push(Tip {
                    x: tip.0,
                    y: tip.1,
                    dx: c,
                    dy: sn,
                    color: (col_.r, col_.g, col_.b),
                    level: cur[k],
                });
                self.brush.SetColor(&col_);
                match s.bar_style.as_str() {
                    "line" => {
                        if let Some(p) = prev {
                            self.dc.DrawLine(pt(p.0, p.1), pt(tip.0, tip.1), &self.brush, line_w, &self.round);
                        }
                        first.get_or_insert(tip);
                        prev = Some(tip);
                    }
                    _ if len < 0.5 => {}
                    "rounded" => self.dc.DrawLine(at(0.0), at(len), &self.brush, bw, &self.round),
                    "dots" => {
                        let (dot, gap) = (bw, bw * 0.45);
                        let mut d = 0.0;
                        while d + dot <= len.max(dot) {
                            self.dc.DrawLine(at(d), at(d + dot), &self.brush, bw, None::<&ID2D1StrokeStyle>);
                            d += dot + gap;
                        }
                    }
                    _ => self.dc.DrawLine(at(0.0), at(len), &self.brush, bw, None::<&ID2D1StrokeStyle>),
                }
                if s.fx_peaks && s.bar_style != "line" {
                    let pk = peaks.get(k).copied().unwrap_or(0.0) * max_len;
                    if pk > 1.0 {
                        let th = (bw * 0.35).max(2.0);
                        self.dc.DrawLine(
                            at(pk + bw * 0.4),
                            at(pk + bw * 0.4 + th),
                            &self.brush,
                            bw,
                            None::<&ID2D1StrokeStyle>,
                        );
                    }
                }
            }
            if let (Some(a), Some(b)) = (prev, first) {
                self.dc.DrawLine(pt(a.0, a.1), pt(b.0, b.1), &self.brush, line_w, &self.round);
                // close the loop
            }
            let level = (bass * 2.5).min(1.0);
            if level > 0.02 {
                self.brush.SetOpacity(s.opacity * level);
                self.brush.SetColor(&glow_color(&pal, t));
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

    /// Hyperspace: stars streaking out from the centre of the screen.
    fn draw_hyperspace(&self, fx: &Fx, s: &Settings, t: f32) {
        if fx.stars.is_empty() || fx.hyper_vis <= 0.01 {
            return;
        }
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let f = self.h / 2.0; // y = ±1 reaches the top/bottom edge
        let thick = (self.w.min(self.h) / 540.0).max(1.0);
        // longer streaks the faster we go
        let streak = 0.02 + fx.hyper_speed * 0.06;
        let pal = palette(s, false);
        unsafe {
            self.brush.SetOpacity(1.0);
            for st in &fx.stars {
                let z1 = st.z.max(0.03);
                let z0 = (st.pz.max(z1) + streak).min(1.0);
                let (x1, y1) = (cx + st.x / z1 * f, cy + st.y / z1 * f);
                let (x0, y0) = (cx + st.x / z0 * f, cy + st.y / z0 * f);
                let near = (1.0 - z1).clamp(0.0, 1.0);
                let a = near.powf(1.5) * fx.hyper_vis * s.opacity;
                if a < 0.02 {
                    continue;
                }
                let c = if s.hyper_follow {
                    bar_color(&pal, t, st.x, st.y)
                } else {
                    // white, with a faint blue tint on far stars
                    col(0.85 + 0.15 * near, 0.9 + 0.1 * near, 1.0, 1.0)
                };
                self.brush.SetColor(&col(c.r, c.g, c.b, a));
                self.dc
                    .DrawLine(pt(x0, y0), pt(x1, y1), &self.brush, thick * (0.4 + 2.2 * near), &self.round);
            }
            self.brush.SetOpacity(s.opacity);
        }
    }

    /// Stars circling the middle, each a twinkling 4-point sparkle with a fading tail.
    fn draw_orbit(&self, fx: &Fx, s: &Settings, t: f32, bass: f32) {
        let n = s.orbit_stars as usize;
        if n == 0 || fx.orbit_vis <= 0.01 {
            return;
        }
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let min = self.w.min(self.h);
        // just outside the circle visualizer's bars
        let r = min * 0.17 * s.size * (1.0 + 0.06 * bass + 0.25 * fx.punch) + min * 0.16 * s.size * 0.8;
        let base = min / 160.0 * (1.0 + 1.3 * fx.orbit_kick); // star size, flares on kicks
        let pal = palette(s, true);
        let tau = std::f32::consts::TAU;
        unsafe {
            self.brush.SetOpacity(1.0);
            for i in 0..n {
                let a = fx.orbit_angle + i as f32 * tau / n as f32;
                let c = if s.orbit_follow {
                    bar_color(&pal, t, a.cos(), a.sin())
                } else {
                    col(1.0, 0.97, 0.9, 1.0)
                };
                let vis = fx.orbit_vis * s.opacity;
                // tail: fading dots behind the star (longer when it's whipping round)
                let tail_len = 0.25 + 0.6 * fx.orbit_kick;
                for k in 1..=20 {
                    let f = k as f32 / 20.0;
                    let ta = a - f * tail_len;
                    let alpha = (1.0 - f) * 0.4 * vis;
                    self.brush.SetColor(&col(c.r, c.g, c.b, alpha));
                    let rad = base * 0.55 * (1.0 - f * 0.7);
                    let e = D2D1_ELLIPSE {
                        point: pt(cx + ta.cos() * r, cy + ta.sin() * r),
                        radiusX: rad,
                        radiusY: rad,
                    };
                    self.dc.FillEllipse(&e, &self.brush);
                }
                // the star: a 4-point sparkle that slowly twinkles/rotates, with a bright core
                let (x, y) = (cx + a.cos() * r, cy + a.sin() * r);
                let tw = t * 2.0 + i as f32;
                let len = base * (2.6 + 0.4 * (t * 7.0 + i as f32).sin());
                self.brush.SetColor(&col(c.r, c.g, c.b, vis));
                for k in 0..2 {
                    let ang = tw + k as f32 * std::f32::consts::FRAC_PI_2;
                    let (dx, dy) = (ang.cos() * len, ang.sin() * len);
                    self.dc
                        .DrawLine(pt(x - dx, y - dy), pt(x + dx, y + dy), &self.brush, base * 0.45, &self.round);
                }
                self.brush.SetColor(&col(1.0, 1.0, 1.0, vis));
                let core = D2D1_ELLIPSE {
                    point: pt(x, y),
                    radiusX: base * 0.7,
                    radiusY: base * 0.7,
                };
                self.dc.FillEllipse(&core, &self.brush);
            }
            self.brush.SetOpacity(s.opacity);
        }
    }

    /// Sparks flying out from the bars.
    fn draw_particles(&self, fx: &Fx, s: &Settings) {
        unsafe {
            for p in &fx.particles {
                let a = (p.life / p.max_life).clamp(0.0, 1.0);
                self.brush.SetColor(&col(p.color.0, p.color.1, p.color.2, a * s.opacity));
                let e = D2D1_ELLIPSE {
                    point: pt(p.x, p.y),
                    radiusX: p.size * (0.5 + 0.5 * a),
                    radiusY: p.size * (0.5 + 0.5 * a),
                };
                self.dc.FillEllipse(&e, &self.brush);
            }
        }
    }

    /// Soft coloured glow around the whole screen edge, on the beat.
    fn draw_beat_glow(&self, strength: f32, c: D2D1_COLOR_F) -> Result<()> {
        unsafe {
            let rt: &ID2D1RenderTarget = &self.dc;
            let stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: col(c.r, c.g, c.b, 0.0),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.6,
                    color: col(c.r, c.g, c.b, 0.0),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: col(c.r, c.g, c.b, 0.55 * strength),
                },
            ];
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                center: pt(self.w / 2.0, self.h / 2.0),
                gradientOriginOffset: pt(0.0, 0.0),
                radiusX: self.w * 0.72,
                radiusY: self.h * 0.72,
            };
            let brush = rt.CreateRadialGradientBrush(&props, None, &coll)?;
            rt.FillRectangle(&rect(0.0, 0.0, self.w, self.h), &brush);
        }
        Ok(())
    }

    /// Full-screen bitmap for the afterglow trails (made the first time they're used).
    fn trail_bitmap(&self) -> Result<ID2D1Bitmap1> {
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                ..Default::default()
            };
            self.dc.CreateBitmap(
                D2D_SIZE_U {
                    width: self.w as u32,
                    height: self.h as u32,
                },
                None,
                0,
                &props,
            )
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

    let edge = if bottom_bars(s) { g.w.min(g.h) * 0.11 * s.size } else { 0.0 };
    let (tr, tg, tb) = settings::parse_hex(&s.text_color).unwrap_or((1.0, 1.0, 1.0));
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
            g.brush.SetColor(&col(tr, tg, tb, a));
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
            g.brush.SetColor(&col(tr, tg, tb, a));
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
#[allow(clippy::too_many_arguments)]
fn render(
    g: &Gfx,
    an: &Analyzer,
    s: &Settings,
    t: f32,
    dt: f32,
    dpi: f32,
    ts: &mut TextState,
    toast: &mut Toast,
    fx: &mut Fx,
    use_trails: bool,
) -> Result<()> {
    unsafe {
        let clear = col(0.0, 0.0, 0.0, 0.0);
        // Pass 1: bars + particles + text into a command list (so we can blur it for the glow)
        let scene = g.dc.CreateCommandList()?;
        g.dc.SetTarget(&scene);
        g.dc.BeginDraw();
        g.dc.Clear(Some(&clear));
        g.brush.SetOpacity(s.opacity);
        fx.tips.clear();
        g.draw_hyperspace(fx, s, t);
        g.draw_orbit(fx, s, t, an.bass());
        if !an.cur.is_empty() {
            if s.mode != "circle" {
                g.draw_edge(&an.cur, s, t, &fx.peaks, &mut fx.tips);
            }
            if s.mode != "edge" {
                g.draw_circle(&an.cur, an.bass(), s, t, &fx.peaks, (fx.spin, fx.punch), &mut fx.tips);
            }
        }
        g.brush.SetOpacity(1.0);
        g.draw_particles(fx, s);
        g.brush.SetOpacity(s.opacity);
        let text_res = draw_text(g, ts, s, an.bass(), dpi);
        g.dc.EndDraw(None, None)?;
        scene.Close()?;
        text_res?;

        // Afterglow trails: last frame (faded) + this frame, in a ping-pong bitmap.
        let content: ID2D1Image = if use_trails {
            {
                let mut bufs = g.trails.borrow_mut();
                if bufs[0].is_none() || bufs[1].is_none() {
                    *bufs = [Some(g.trail_bitmap()?), Some(g.trail_bitmap()?)];
                    g.trail_fresh.set(true);
                }
            }
            let bufs = g.trails.borrow();
            let i = g.trail_idx.get();
            let (prev, next) = (bufs[i].as_ref().unwrap(), bufs[1 - i].as_ref().unwrap());
            g.dc.SetTarget(next);
            g.dc.BeginDraw();
            g.dc.Clear(Some(&clear));
            if !g.trail_fresh.get() {
                let keep = s.fx_trails.powf((dt * 60.0).clamp(0.2, 5.0));
                let rt: &ID2D1RenderTarget = &g.dc;
                rt.DrawBitmap(prev, None, keep, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
            g.dc.DrawImage(&scene, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            g.dc.EndDraw(None, None)?;
            g.trail_idx.set(1 - i);
            g.trail_fresh.set(false);
            next.cast()?
        } else {
            g.trail_fresh.set(true);
            scene.cast()?
        };

        // Pass 2: glow + glitch + scene (shaken) onto the screen, then the beat glow
        let target = g.target.as_ref().ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        g.dc.SetTarget(target);
        g.dc.BeginDraw();
        g.dc.Clear(Some(&clear));
        let (sx, sy) = fx.shake();
        let off = pt(sx, sy);
        if s.glow > 0.5 {
            let sigma = s.glow * 0.5 * dpi;
            g.blur.SetValue(
                D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &sigma.to_le_bytes(),
            )?;
            g.blur.SetInput(0, &scene, true);
            if let Ok(out) = g.blur.GetOutput() {
                g.dc.DrawImage(
                    &out,
                    Some(&off),
                    None,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    D2D1_COMPOSITE_MODE_SOURCE_OVER,
                );
            }
        }
        if fx.glitch > 0.01 {
            let gx = fx.glitch * 14.0 * dpi;
            for (e, dx) in [(&g.red, -gx), (&g.blue, gx)] {
                e.SetInput(0, &content, true);
                if let Ok(out) = e.GetOutput() {
                    let o = pt(sx + dx, sy);
                    g.dc.DrawImage(&out, Some(&o), None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_PLUS);
                }
            }
        }
        g.dc.DrawImage(
            &content,
            Some(&off),
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_SOURCE_OVER,
        );
        let glow_res = if fx.glow > 0.01 {
            g.draw_beat_glow(fx.glow, glow_color(&palette(s, false), t))
        } else {
            Ok(())
        };
        let toast_res = draw_toast(g, toast, dpi);
        g.dc.EndDraw(None, None)?;
        glow_res?;
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
        add(20, "Rainbow", s.edge_theme == "rainbow");
        add(21, "Fire", s.edge_theme == "fire");
        add(22, "Ice", s.edge_theme == "ice");
        add(23, "White", s.edge_theme == "white");
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
pub fn run(updated_to: Option<String>) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let mut s = Settings::load();
    let mut s_mtime = settings::mtime(&settings::settings_path());

    let audio = crate::audio::start(&s.audio_app);
    let media_target = std::sync::Arc::new(std::sync::Mutex::new(s.audio_app.clone()));
    let media = media::start(media_target.clone());
    let mut lyrics = Lyrics::new();

    let (mut sw, mut sh, mut dpi) = screen();
    let hwnd = create_window(sw, sh)?;
    let mut gfx = Some(Gfx::new(hwnd, sw as u32, sh as u32)?);
    add_tray(hwnd);
    register_hotkeys(hwnd);
    apply_window(hwnd, &s, sw, sh);

    let rgb = crate::rgb::start();
    let mut beat = BeatDetector::new();
    let mut fx = Fx::new();
    let mut an = Analyzer::new();
    let mut samples = vec![0.0f32; FFT_SIZE];
    let mut ts = TextState::new();
    let restart_to = start_update_checker();
    let mut toast = Toast {
        text: match &updated_to {
            Some(v) => format!("Updated to {v} ✓"),
            None => "Edge Visualizer is running. Click the tray icon for settings.".into(),
        },
        until: Instant::now() + Duration::from_millis(3000),
        lay: None,
    };
    let start = Instant::now();
    let mut last = Instant::now();
    let mut last_poll = Instant::now();
    let mut status = Status::default();
    let mut gfx_error_logged = false;
    let mut idle_drawn = false; // drew the empty "nothing playing" frame already

    let mut restart: Option<String> = None;
    'main: loop {
        // ---- an update was installed: restart into the new version
        if let Some(tag) = restart_to.lock().unwrap().take() {
            restart = Some(tag);
            break 'main;
        }

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
                        s.edge_theme = "rainbow".into();
                        s.circle_theme = "rainbow".into();
                        changed = true;
                    }
                    21 => {
                        s.edge_theme = "fire".into();
                        s.circle_theme = "fire".into();
                        changed = true;
                    }
                    22 => {
                        s.edge_theme = "ice".into();
                        s.circle_theme = "ice".into();
                        changed = true;
                    }
                    23 => {
                        s.edge_theme = "white".into();
                        s.circle_theme = "white".into();
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
                            s.edge_theme = cycle(&["rainbow", "fire", "ice", "white"], &s.edge_theme);
                            s.circle_theme = s.edge_theme.clone();
                            msg = format!("Colors: {}", s.edge_theme);
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
        *audio.target.lock().unwrap() = s.audio_app.clone();
        *media_target.lock().unwrap() = s.audio_app.clone();
        if changed {
            idle_drawn = false;
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
                    idle_drawn = false;
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
            audio_mode: audio.mode.lock().unwrap().clone(),
            rgb: rgb.status.lock().unwrap().clone(),
        };
        if new_status != status {
            status = new_status;
            status.save();
        }

        if !s.visible {
            // Visualizer hidden: keep the RGB sync running if it's on.
            if s.rgb_enabled {
                let dt = last.elapsed().as_secs_f32().min(0.1);
                last = Instant::now();
                let sr = audio.sample_rate.load(Ordering::Relaxed);
                audio.ring.lock().unwrap().latest(&mut samples);
                an.update(&samples, sr, &s, dt);
                let hit = beat.update(an.band_energy(40.0, 130.0), dt, s.rgb_sensitivity);
                feed_rgb(&rgb, &beat, hit, &s, start.elapsed().as_secs_f32());
                std::thread::sleep(Duration::from_millis(16));
            } else {
                std::thread::sleep(Duration::from_millis(50));
                last = Instant::now();
            }
            continue;
        }

        // ---- audio -> bars
        let dt = last.elapsed().as_secs_f32().min(0.1);
        last = Instant::now();
        let sr = audio.sample_rate.load(Ordering::Relaxed);
        audio.ring.lock().unwrap().latest(&mut samples);
        an.update(&samples, sr, &s, dt);
        // kick drums drive the RGB sync and the screen effects
        let hit = beat.update(an.band_energy(40.0, 130.0), dt, s.rgb_sensitivity);
        if s.rgb_enabled {
            feed_rgb(&rgb, &beat, hit, &s, start.elapsed().as_secs_f32());
        }
        fx.update(dt, hit, &an.cur, &s, dpi);
        let playing = an.cur.iter().any(|&v| v > 0.01);
        fx.update_hyper(dt, hit, an.bass(), playing, sw as f32 / sh.max(1) as f32, &s);
        fx.update_orbit(dt, hit, playing, &s);

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
        // Sleep when silent: nothing on screen changes, so don't redraw it.
        let bars_max = an.cur.iter().fold(0.0f32, |a, &b| a.max(b));
        let text_steady = ts.vis == 0.0 || (ts.vis == 1.0 && ts.changed.elapsed() > Duration::from_millis(400));
        let idle = bars_max < 0.003 && text_steady && toast.until <= Instant::now() && !fx.active();
        let use_trails = s.fx_trails > 0.01 && !idle;
        if idle && idle_drawn {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let frame_start = Instant::now();
        if let Some(g) = gfx.as_ref() {
            if let Err(e) = render(
                g,
                &an,
                &s,
                start.elapsed().as_secs_f32(),
                dt,
                dpi,
                &mut ts,
                &mut toast,
                &mut fx,
                use_trails,
            ) {
                sys::log(&format!("draw failed, restarting graphics: {e}"));
                gfx = None;
                ts.main_lay = None;
                ts.sub_lay = None;
                toast.lay = None;
            }
        }
        idle_drawn = idle;
        // Present() waits for the screen refresh; the FPS setting can cap it lower.
        let budget = if s.fps > 0 {
            Duration::from_secs_f32(1.0 / s.fps as f32)
        } else {
            Duration::from_millis(4)
        };
        let spent = frame_start.elapsed();
        if spent < budget {
            std::thread::sleep(budget - spent);
        }
    }

    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd));
        let _ = DestroyWindow(hwnd);
    }
    let _ = std::fs::remove_file(settings::status_path());
    if let Some(tag) = restart {
        crate::update::launch_updated(&tag);
    }
    Ok(())
}

/// Kick detection -> tells the RGB thread how bright to flash and in which colour.
fn feed_rgb(rgb: &crate::rgb::Rgb, beat: &BeatDetector, hit: bool, s: &Settings, t: f32) {
    let color = if s.rgb_follow {
        let c = glow_color(&palette(s, false), t);
        (c.r, c.g, c.b)
    } else {
        settings::parse_hex(&s.rgb_color).unwrap_or((1.0, 1.0, 1.0))
    };
    let mut p = rgb.pulse.lock().unwrap();
    p.level = beat.level;
    p.color = color;
    if hit {
        p.beat_at = Some(Instant::now());
    }
}

/// Checks GitHub for a new version shortly after start, then every 6 hours.
fn start_update_checker() -> std::sync::Arc<std::sync::Mutex<Option<String>>> {
    let flag = std::sync::Arc::new(std::sync::Mutex::new(None));
    let f = flag.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(10));
        crate::update::cleanup_old();
        if crate::update::dev_build() {
            return;
        }
        loop {
            if Settings::load().auto_update {
                match crate::update::check_and_install() {
                    Ok(crate::update::Outcome::Installed(tag)) => {
                        sys::log(&format!("updated to {tag}"));
                        *f.lock().unwrap() = Some(tag);
                        return;
                    }
                    Ok(_) => {}
                    Err(e) => sys::log(&format!("update: {e}")),
                }
            }
            std::thread::sleep(Duration::from_secs(6 * 60 * 60));
        }
    });
    flag
}
