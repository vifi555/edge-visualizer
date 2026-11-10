//! RGB sync: flashes keyboard/mouse lighting on the beat.
//!
//! - Corsair: the official iCUE SDK. Its license doesn't allow bundling the DLL,
//!   so it's downloaded from Corsair's own GitHub releases the first time.
//!   Uses shared access with see-through colours, so your normal iCUE lighting
//!   shows between flashes. Needs "third-party control / SDK" on in iCUE.
//! - SteelSeries: the GameSense service built into SteelSeries GG.
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use windows::core::{HSTRING, PCSTR};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::settings::{self, Settings};

/// What the visualizer tells the RGB thread every frame.
#[derive(Clone, Copy)]
pub struct Pulse {
    pub level: f32, // 1 right after a beat, fading to 0
    pub beat_at: Option<Instant>,
    pub color: (f32, f32, f32),
}

impl Default for Pulse {
    fn default() -> Self {
        Self {
            level: 0.0,
            beat_at: None,
            color: (1.0, 1.0, 1.0),
        }
    }
}

pub struct Rgb {
    pub pulse: Arc<Mutex<Pulse>>,
    pub status: Arc<Mutex<String>>,
}

pub fn start() -> Rgb {
    let pulse = Arc::new(Mutex::new(Pulse::default()));
    let status = Arc::new(Mutex::new(String::new()));
    let (p, st) = (pulse.clone(), status.clone());
    std::thread::spawn(move || run(p, st));
    Rgb { pulse, status }
}

fn run(pulse: Arc<Mutex<Pulse>>, status: Arc<Mutex<String>>) {
    let mut s = Settings::load();
    let mut last_load = Instant::now();
    let mut corsair: Option<Corsair> = None;
    let mut corsair_msg = String::new();
    let mut corsair_retry = Instant::now();
    let mut gs: Option<GameSense> = None;
    let mut gs_msg = String::new();
    let mut gs_retry = Instant::now();

    loop {
        if last_load.elapsed() > Duration::from_millis(500) {
            s = Settings::load();
            last_load = Instant::now();
        }
        if !s.rgb_enabled {
            corsair = None;
            gs = None; // dropping it hands the lights back to GG
            *status.lock().unwrap() = String::new();
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        let p = *pulse.lock().unwrap();

        // ---- Corsair
        if !s.rgb_corsair {
            corsair = None;
            corsair_msg.clear();
        } else if corsair.is_none() && Instant::now() >= corsair_retry {
            match Corsair::connect() {
                Ok(c) => {
                    corsair_msg = format!("Corsair: {} ✓", c.names());
                    corsair = Some(c);
                }
                Err(e) => {
                    corsair_msg = format!("Corsair: {e}");
                    corsair_retry = Instant::now() + Duration::from_secs(15);
                }
            }
        }
        if let Some(c) = corsair.as_mut() {
            if let Err(e) = c.frame(&p, &s) {
                corsair_msg = format!("Corsair: {e}");
                corsair = None;
                corsair_retry = Instant::now() + Duration::from_secs(5);
            }
        }

        // ---- SteelSeries
        if !s.rgb_steelseries {
            gs = None;
            gs_msg.clear();
        } else if gs.is_none() && Instant::now() >= gs_retry {
            match GameSense::connect(p.color) {
                Ok(g) => {
                    gs_msg = format!("SteelSeries: {} ✓", g.summary);
                    gs = Some(g);
                }
                Err(e) => {
                    gs_msg = format!("SteelSeries: {e}");
                    gs_retry = Instant::now() + Duration::from_secs(15);
                }
            }
        }
        if let Some(g) = gs.as_mut() {
            if let Err(e) = g.frame(&p) {
                gs_msg = format!("SteelSeries: {e}");
                gs = None;
                gs_retry = Instant::now() + Duration::from_secs(5);
            }
        }

        let msg = [corsair_msg.as_str(), gs_msg.as_str()]
            .iter()
            .filter(|m| !m.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        *status.lock().unwrap() = msg;
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn agent(timeout_ms: u64) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(timeout_ms))
        .user_agent("EdgeVisualizer");
    if let Ok(tls) = native_tls::TlsConnector::new() {
        b = b.tls_connector(Arc::new(tls));
    }
    b.build()
}

// ================================================================ Corsair (iCUE SDK v4)
const CE_SUCCESS: i32 = 0;
const CSS_CONNECTED: i32 = 6;
const CSS_REFUSED: i32 = 4;
const CDT_ALL: i32 = -1; // 0xFFFFFFFF
const CAL_SHARED: i32 = 0;
const ID_LEN: usize = 128;

#[repr(C)]
struct SdkVersion {
    major: i32,
    minor: i32,
    patch: i32,
}
#[repr(C)]
struct SessionStateChanged {
    state: i32,
    details: [SdkVersion; 3],
}
#[repr(C)]
struct DeviceFilter {
    mask: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct DeviceInfo {
    kind: i32,
    id: [u8; ID_LEN],
    serial: [u8; ID_LEN],
    model: [u8; ID_LEN],
    led_count: i32,
    channel_count: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct LedPosition {
    id: u32,
    cx: f64,
    cy: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct LedColor {
    id: u32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

type StateFn = unsafe extern "C" fn(*mut c_void, *const SessionStateChanged);
type ConnectFn = unsafe extern "C" fn(Option<StateFn>, *mut c_void) -> i32;
type GetDevicesFn = unsafe extern "C" fn(*const DeviceFilter, i32, *mut DeviceInfo, *mut i32) -> i32;
type GetLedPositionsFn = unsafe extern "C" fn(*const u8, i32, *mut LedPosition, *mut i32) -> i32;
type SetLedColorsFn = unsafe extern "C" fn(*const u8, i32, *const LedColor) -> i32;
type RequestControlFn = unsafe extern "C" fn(*const u8, i32) -> i32;

static SESSION: AtomicI32 = AtomicI32::new(0);
unsafe extern "C" fn on_state(_ctx: *mut c_void, ev: *const SessionStateChanged) {
    if !ev.is_null() {
        SESSION.store((*ev).state, Ordering::SeqCst);
    }
}

struct Dev {
    id: [u8; ID_LEN],
    model: String,
    leds: Vec<LedPosition>,
    center: (f64, f64),
    radius: f64,
    sent: Vec<LedColor>,
}

struct Corsair {
    set: SetLedColorsFn,
    devs: Vec<Dev>,
}

fn c_str(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).to_string()
}

fn sdk_path() -> PathBuf {
    settings::dir().join("iCUESDK.x64_2019.dll")
}

/// Downloads Corsair's official SDK DLL from their GitHub releases (once).
fn ensure_sdk() -> Result<PathBuf, String> {
    let dll = sdk_path();
    if dll.exists() {
        return Ok(dll);
    }
    let agent = agent(120_000);
    let text = agent
        .get("https://api.github.com/repos/CorsairOfficial/cue-sdk/releases/latest")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("couldn't reach Corsair's SDK download ({e})"))?
        .into_string()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let url = v["assets"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|x| x["name"].as_str().is_some_and(|n| n.to_lowercase().ends_with(".zip")))
        })
        .and_then(|x| x["browser_download_url"].as_str())
        .ok_or("Corsair's SDK release has no zip")?
        .to_string();
    let mut bytes = Vec::new();
    use std::io::Read;
    agent
        .get(&url)
        .call()
        .map_err(|e| format!("SDK download failed ({e})"))?
        .into_reader()
        .take(100_000_000)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;

    let work = settings::dir().join("icue-sdk");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let zip = work.join("sdk.zip");
    std::fs::write(&zip, &bytes).map_err(|e| e.to_string())?;
    // Windows' own unzip (no extra libraries needed)
    use std::os::windows::process::CommandExt;
    let ok = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            zip.display(),
            work.join("x").display()
        ))
        .creation_flags(0x0800_0000)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Err("couldn't unzip Corsair's SDK".into());
    }
    let found = find_file(&work.join("x"), &|name: &str| {
        let n = name.to_lowercase();
        n.starts_with("icuesdk") && n.contains("x64") && n.ends_with(".dll")
    })
    .ok_or("couldn't find the 64-bit SDK file in Corsair's download")?;
    std::fs::copy(&found, &dll).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(dll)
}

fn find_file(dir: &std::path::Path, want: &dyn Fn(&str) -> bool) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, want) {
                return Some(f);
            }
        } else if p.file_name().and_then(|n| n.to_str()).is_some_and(want) {
            return Some(p);
        }
    }
    None
}

impl Corsair {
    fn connect() -> Result<Self, String> {
        let dll = ensure_sdk()?;
        unsafe {
            let lib = LoadLibraryW(&HSTRING::from(dll.to_string_lossy().as_ref())).map_err(|e| format!("couldn't load the SDK ({e})"))?;
            macro_rules! proc {
                ($name:literal, $t:ty) => {{
                    let f = GetProcAddress(lib, PCSTR(concat!($name, "\0").as_ptr())).ok_or(concat!("SDK is missing ", $name))?;
                    std::mem::transmute::<_, $t>(f)
                }};
            }
            let connect: ConnectFn = proc!("CorsairConnect", ConnectFn);
            let get_devices: GetDevicesFn = proc!("CorsairGetDevices", GetDevicesFn);
            let get_leds: GetLedPositionsFn = proc!("CorsairGetLedPositions", GetLedPositionsFn);
            let set: SetLedColorsFn = proc!("CorsairSetLedColors", SetLedColorsFn);
            let request: RequestControlFn = proc!("CorsairRequestControl", RequestControlFn);

            SESSION.store(0, Ordering::SeqCst);
            if connect(Some(on_state), std::ptr::null_mut()) != CE_SUCCESS {
                return Err("iCUE isn't running".into());
            }
            let t = Instant::now();
            while SESSION.load(Ordering::SeqCst) != CSS_CONNECTED {
                if SESSION.load(Ordering::SeqCst) == CSS_REFUSED {
                    return Err("iCUE refused. Turn on third-party control (SDK) in iCUE's settings".into());
                }
                if t.elapsed() > Duration::from_secs(6) {
                    return Err("iCUE didn't answer. Is it running, with the SDK turned on in its settings?".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }

            let filter = DeviceFilter { mask: CDT_ALL };
            let mut infos = vec![std::mem::zeroed::<DeviceInfo>(); 64];
            let mut n = 0i32;
            if get_devices(&filter, infos.len() as i32, infos.as_mut_ptr(), &mut n) != CE_SUCCESS {
                return Err("couldn't list devices".into());
            }
            let mut devs = Vec::new();
            for info in infos.iter().take(n.max(0) as usize) {
                if info.led_count <= 0 {
                    continue;
                }
                let mut leds = vec![LedPosition::default(); info.led_count as usize + 8];
                let mut count = 0i32;
                if get_leds(info.id.as_ptr(), leds.len() as i32, leds.as_mut_ptr(), &mut count) != CE_SUCCESS || count <= 0 {
                    continue;
                }
                leds.truncate(count as usize);
                let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for l in &leds {
                    x0 = x0.min(l.cx);
                    y0 = y0.min(l.cy);
                    x1 = x1.max(l.cx);
                    y1 = y1.max(l.cy);
                }
                let _ = request(info.id.as_ptr(), CAL_SHARED);
                devs.push(Dev {
                    id: info.id,
                    model: c_str(&info.model),
                    center: ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
                    radius: (((x1 - x0) / 2.0).hypot((y1 - y0) / 2.0)).max(1.0),
                    sent: Vec::new(),
                    leds,
                });
            }
            if devs.is_empty() {
                return Err("no RGB devices found in iCUE".into());
            }
            Ok(Corsair { set, devs })
        }
    }

    fn names(&self) -> String {
        self.devs.iter().map(|d| d.model.clone()).collect::<Vec<_>>().join(", ")
    }

    fn frame(&mut self, p: &Pulse, s: &Settings) -> Result<(), String> {
        let since = p.beat_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(99.0);
        let (r, g, b) = ((p.color.0 * 255.0) as u8, (p.color.1 * 255.0) as u8, (p.color.2 * 255.0) as u8);
        for d in &mut self.devs {
            let colors: Vec<LedColor> = d
                .leds
                .iter()
                .map(|l| {
                    let k = if s.rgb_effect == "ripple" {
                        // a ring that travels from the middle of the device outwards
                        let dist = (l.cx - d.center.0).hypot(l.cy - d.center.1) / d.radius; // 0..1
                        let front = since / 0.35;
                        let x = (dist - front) / 0.18;
                        ((-x * x).exp() * (1.0 - since / 0.7).max(0.0)) as f32
                    } else {
                        p.level
                    };
                    // alpha = how much the flash covers your normal iCUE lighting
                    LedColor {
                        id: l.id,
                        r,
                        g,
                        b,
                        a: (k.clamp(0.0, 1.0) * 255.0) as u8,
                    }
                })
                .collect();
            if colors == d.sent {
                continue; // nothing changed (e.g. silence): don't spam iCUE
            }
            let rc = unsafe { (self.set)(d.id.as_ptr(), colors.len() as i32, colors.as_ptr()) };
            if rc != CE_SUCCESS {
                return Err(format!("lost connection to iCUE (error {rc})"));
            }
            d.sent = colors;
        }
        Ok(())
    }
}

// ================================================================ SteelSeries (GameSense)
const GAME: &str = "EDGE_VISUALIZER";

struct GameSense {
    agent: ureq::Agent,
    base: String,
    color: (u8, u8, u8),
    last_value: i64,
    last_send: Instant,
    last_rebind: Instant,
    handlers: Vec<(String, String)>,
    summary: String,
}

fn gamesense_address() -> Option<String> {
    let pd = std::env::var("PROGRAMDATA").unwrap_or_else(|_| "C:\\ProgramData".into());
    let text = std::fs::read_to_string(format!("{pd}\\SteelSeries\\SteelSeries Engine 3\\coreProps.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["address"].as_str().map(|s| format!("http://{s}"))
}

/// Zones to try. Unsupported ones are rejected by GG and skipped.
const ZONES: &[(&str, &str)] = &[
    ("mouse", "wheel"),
    ("mouse", "logo"),
    ("rgb-per-key-zones", "all"),
    ("rgb-8-zone", "one"),
    ("rgb-8-zone", "two"),
    ("rgb-8-zone", "three"),
    ("rgb-8-zone", "four"),
    ("rgb-8-zone", "five"),
    ("rgb-8-zone", "six"),
    ("rgb-8-zone", "seven"),
    ("rgb-8-zone", "eight"),
    ("rgb-2-zone", "one"),
    ("rgb-2-zone", "two"),
    ("rgb-1-zone", "one"),
];

fn rgb8(c: (f32, f32, f32)) -> (u8, u8, u8) {
    ((c.0 * 255.0) as u8, (c.1 * 255.0) as u8, (c.2 * 255.0) as u8)
}

impl GameSense {
    fn post(&self, path: &str, body: Value) -> Result<(), String> {
        match self.agent.post(&format!("{}/{path}", self.base)).send_json(body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, r)) => Err(format!("{code} {}", r.into_string().unwrap_or_default())),
            Err(e) => Err(e.to_string()),
        }
    }

    fn handler(dev: &str, zone: &str, c: (u8, u8, u8)) -> Value {
        // value 0 = a dim glow, value 100 = full flash colour
        json!({
            "device-type": dev,
            "zone": zone,
            "mode": "color",
            "color": { "gradient": {
                "zero": { "red": c.0 / 10, "green": c.1 / 10, "blue": c.2 / 10 },
                "hundred": { "red": c.0, "green": c.1, "blue": c.2 }
            }}
        })
    }

    fn bind(&self, event: &str, handlers: Vec<Value>) -> Result<(), String> {
        self.post(
            "bind_game_event",
            json!({ "game": GAME, "event": event, "min_value": 0, "max_value": 100, "icon_id": 0, "handlers": handlers }),
        )
    }

    fn connect(color: (f32, f32, f32)) -> Result<Self, String> {
        let base = gamesense_address().ok_or("SteelSeries GG isn't running")?;
        let mut g = GameSense {
            agent: agent(800),
            base,
            color: rgb8(color),
            last_value: -1,
            last_send: Instant::now(),
            last_rebind: Instant::now(),
            handlers: Vec::new(),
            summary: String::new(),
        };
        g.post(
            "game_metadata",
            json!({ "game": GAME, "game_display_name": "Edge Visualizer", "developer": "Edge Visualizer", "deinitialize_timer_length_ms": 2000 }),
        )
        .map_err(|e| format!("GG didn't accept the app ({e})"))?;

        // Find which zones this setup supports.
        let c = g.color;
        let all: Vec<Value> = ZONES.iter().map(|(d, z)| Self::handler(d, z, c)).collect();
        if g.bind("BEAT", all).is_ok() {
            g.handlers = ZONES.iter().map(|(d, z)| (d.to_string(), z.to_string())).collect();
        } else {
            for (d, z) in ZONES {
                if g.bind("PROBE", vec![Self::handler(d, z, c)]).is_ok() {
                    g.handlers.push((d.to_string(), z.to_string()));
                }
            }
            let _ = g.post("remove_game_event", json!({ "game": GAME, "event": "PROBE" }));
            if g.handlers.is_empty() {
                return Err("GG didn't accept any lighting zones".into());
            }
            g.rebind()?;
        }
        let mice = g.handlers.iter().any(|(d, _)| d == "mouse" || d.starts_with("rgb-"));
        let keys = g.handlers.iter().any(|(d, _)| d == "rgb-per-key-zones");
        g.summary = match (mice, keys) {
            (true, true) => "mouse + keyboard".into(),
            (true, false) => "mouse".into(),
            _ => "keyboard".into(),
        };
        Ok(g)
    }

    fn rebind(&mut self) -> Result<(), String> {
        let c = self.color;
        let hs = self.handlers.iter().map(|(d, z)| Self::handler(d, z, c)).collect();
        self.last_rebind = Instant::now();
        self.bind("BEAT", hs)
    }

    fn frame(&mut self, p: &Pulse) -> Result<(), String> {
        // New colour (e.g. rainbow edges)? Re-bind, but not more than every 1.5 s.
        let c = rgb8(p.color);
        let diff =
            (c.0 as i32 - self.color.0 as i32).abs() + (c.1 as i32 - self.color.1 as i32).abs() + (c.2 as i32 - self.color.2 as i32).abs();
        if diff > 40 && self.last_rebind.elapsed() > Duration::from_millis(1500) {
            self.color = c;
            self.rebind()?;
        }
        let value = (p.level * 100.0).round() as i64;
        // Send changes ~25 times a second; once at 0 we go quiet and GG takes the lights back.
        let due = self.last_send.elapsed() > Duration::from_millis(40);
        if value != self.last_value && (due || value == 0 || value == 100) {
            self.post("game_event", json!({ "game": GAME, "event": "BEAT", "data": { "value": value } }))?;
            self.last_value = value;
            self.last_send = Instant::now();
        }
        Ok(())
    }
}

impl Drop for GameSense {
    fn drop(&mut self) {
        let _ = self.post("stop_game", json!({ "game": GAME }));
    }
}
