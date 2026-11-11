//! Screen effects: state and physics (drawing is in overlay.rs).
use std::f32::consts::TAU;

use crate::settings::Settings;

/// The end of one bar this frame (where particles fly out from).
#[derive(Clone, Copy)]
pub struct Tip {
    pub x: f32,
    pub y: f32,
    pub dx: f32, // direction the bar grows
    pub dy: f32,
    pub color: (f32, f32, f32),
    pub level: f32,
}

pub struct Particle {
    pub x: f32,
    pub y: f32,
    vx: f32,
    vy: f32,
    pub life: f32,
    pub max_life: f32,
    pub size: f32,
    pub color: (f32, f32, f32),
}

/// One star of the hyperspace field: x/y in screen units around the centre, z = depth (1 far, 0 at you).
pub struct Star {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub pz: f32, // depth last frame (for the streak)
}

pub struct Fx {
    rng: u32,
    pub tips: Vec<Tip>,
    pub particles: Vec<Particle>,
    pub peaks: Vec<f32>,
    pub spin: f32,
    shake_dir: (f32, f32),
    shake_amp: f32,
    shake_t: f32,
    pub glitch: f32, // 0..1, RGB split strength right now
    pub glow: f32,   // 0..1, beat glow strength right now
    pub punch: f32,  // 0..1, circle bass zoom right now
    pub stars: Vec<Star>,
    pub warp: f32,      // 0..1, extra speed right after a kick
    pub hyper_vis: f32, // 0..1, stars fade in with music and out in silence
    pub hyper_speed: f32,
    pub orbit_angle: f32, // where the orbiting stars are
    pub orbit_kick: f32,  // 0..1, flare + speed burst right after a kick
    pub orbit_vis: f32,   // 0..1, fade with the music
    // wormhole
    pub worm_off: (f32, f32), // where the tunnel bends to (screen-height units)
    worm_jolt: (f32, f32),    // a kick threw the tunnel this way
    worm_t: f32,
    pub roll: f32,       // starfield twist angle
    pub rings: Vec<f32>, // tunnel ring depths
}

const MAX_PARTICLES: usize = 600;

impl Fx {
    pub fn new() -> Self {
        Self {
            rng: 0x9E37_79B9,
            tips: Vec::new(),
            particles: Vec::new(),
            peaks: Vec::new(),
            spin: 0.0,
            shake_dir: (0.0, 0.0),
            shake_amp: 0.0,
            shake_t: 10.0,
            glitch: 0.0,
            glow: 0.0,
            punch: 0.0,
            stars: Vec::new(),
            warp: 0.0,
            hyper_vis: 0.0,
            hyper_speed: 0.0,
            orbit_angle: 0.0,
            orbit_kick: 0.0,
            orbit_vis: 0.0,
            worm_off: (0.0, 0.0),
            worm_jolt: (0.0, 0.0),
            worm_t: 0.0,
            roll: 0.0,
            rings: Vec::new(),
        }
    }

    fn rand(&mut self) -> f32 {
        // xorshift32: plenty for sparks
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / 16_777_216.0
    }

    pub fn update(&mut self, dt: f32, hit: bool, cur: &[f32], s: &Settings, dpi: f32) {
        // peak caps: jump up with the bar, then fall slowly
        if self.peaks.len() != cur.len() {
            self.peaks = cur.to_vec();
        }
        for (p, &c) in self.peaks.iter_mut().zip(cur) {
            *p = (*p - dt * 0.35).max(c);
        }
        self.spin = (self.spin + s.circle_spin * dt * 1.2).rem_euclid(TAU);

        if hit {
            if s.fx_shake > 0.0 {
                let a = self.rand() * TAU;
                self.shake_dir = (a.cos(), a.sin());
                self.shake_amp = s.fx_shake * 18.0 * dpi;
                self.shake_t = 0.0;
            }
            if s.fx_glitch > 0.0 {
                self.glitch = s.fx_glitch;
            }
            if s.fx_glow > 0.0 {
                self.glow = s.fx_glow;
            }
            if s.circle_zoom > 0.0 {
                self.punch = s.circle_zoom;
            }
            if s.fx_particles > 0.0 && !self.tips.is_empty() {
                let count = (s.fx_particles * 36.0).round() as usize;
                for _ in 0..count {
                    if self.particles.len() >= MAX_PARTICLES {
                        break;
                    }
                    // favour the tallest of a few random bars
                    let n = self.tips.len();
                    let pick = self.rand();
                    let mut tip = self.tips[(pick * n as f32) as usize % n];
                    for _ in 0..2 {
                        let pick = self.rand();
                        let t = self.tips[(pick * n as f32) as usize % n];
                        if t.level > tip.level {
                            tip = t;
                        }
                    }
                    if tip.level < 0.12 {
                        continue;
                    }
                    let speed = (180.0 + self.rand() * 420.0) * dpi * (0.5 + tip.level);
                    let spread = (self.rand() - 0.5) * 0.9;
                    let (c, sn) = (spread.cos(), spread.sin());
                    let (dx, dy) = (tip.dx * c - tip.dy * sn, tip.dx * sn + tip.dy * c);
                    let life = 0.5 + self.rand() * 0.8;
                    let size = (1.5 + self.rand() * 3.0) * dpi;
                    self.particles.push(Particle {
                        x: tip.x,
                        y: tip.y,
                        vx: dx * speed,
                        vy: dy * speed,
                        life,
                        max_life: life,
                        size,
                        color: tip.color,
                    });
                }
            }
        }

        self.shake_t += dt;
        self.glitch *= (-dt / 0.10).exp();
        self.glow *= (-dt / 0.25).exp();
        self.punch *= (-dt / 0.15).exp();
        let drag = (-dt * 1.8).exp();
        for p in &mut self.particles {
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.vx *= drag;
            p.vy *= drag;
            p.life -= dt;
        }
        self.particles.retain(|p| p.life > 0.0);
        if s.fx_particles <= 0.0 {
            self.particles.clear();
        }
    }

    /// Screen offset for the bass shake right now (a quick, fading wobble).
    pub fn shake(&self) -> (f32, f32) {
        let fade = (-self.shake_t / 0.12).exp();
        if fade < 0.02 {
            return (0.0, 0.0);
        }
        let wobble = (self.shake_t * 55.0).sin();
        let k = self.shake_amp * fade * wobble;
        (self.shake_dir.0 * k, self.shake_dir.1 * k)
    }

    fn new_star(&mut self, aspect: f32, z: f32) -> Star {
        loop {
            let x = (self.rand() * 2.0 - 1.0) * aspect;
            let y = self.rand() * 2.0 - 1.0;
            if x.abs() + y.abs() > 0.06 {
                // not dead centre (those would sit still)
                return Star { x, y, z, pz: z };
            }
        }
    }

    /// Hyperspace: stars fly towards you; bass speeds them up and kicks punch into warp.
    /// `playing` = is there sound right now; `aspect` = screen width / height.
    pub fn update_hyper(&mut self, dt: f32, hit: bool, bass: f32, playing: bool, aspect: f32, s: &Settings) {
        let want = (s.hyperspace * 900.0).round() as usize;
        if want == 0 {
            self.stars.clear();
            self.hyper_vis = 0.0;
            return;
        }
        while self.stars.len() < want {
            let z = 0.05 + self.rand() * 0.95; // spread out so it doesn't start as a wall
            let st = self.new_star(aspect, z);
            self.stars.push(st);
        }
        self.stars.truncate(want);

        let target = if playing { 1.0 } else { 0.0 };
        let step = dt / 0.8;
        self.hyper_vis = if self.hyper_vis < target {
            (self.hyper_vis + step).min(1.0)
        } else {
            (self.hyper_vis - step).max(0.0)
        };
        if hit {
            self.warp = 1.0;
        }
        self.warp *= (-dt / 0.35).exp();

        // depth units per second
        self.hyper_speed = (0.12 + 0.9 * s.hyper_speed) * (0.35 + 1.4 * bass + 2.6 * self.warp) * self.hyper_vis;

        let worm = s.hyper_style == "wormhole";
        if worm {
            // The tunnel wanders around on its own; some kicks throw it somewhere new.
            self.worm_t += dt * (0.4 + self.warp);
            if hit && self.rand() < 0.35 {
                let a = self.rand() * TAU;
                let r = 0.4 + 0.5 * self.rand();
                self.worm_jolt = (a.cos() * r, a.sin() * r);
            }
            let fade = (-dt / 1.5).exp();
            self.worm_jolt = (self.worm_jolt.0 * fade, self.worm_jolt.1 * fade);
            let wt = self.worm_t;
            let wander = (
                (wt * 0.53).sin() * 0.7 + (wt * 0.21).cos() * 0.3,
                (wt * 0.41).cos() * 0.6 + (wt * 0.17).sin() * 0.4,
            );
            let target = (
                (wander.0 + self.worm_jolt.0) * s.worm_bend * aspect * 0.55,
                (wander.1 + self.worm_jolt.1) * s.worm_bend * 0.55,
            );
            let k = 1.0 - (-dt / 0.5).exp();
            self.worm_off.0 += (target.0 - self.worm_off.0) * k;
            self.worm_off.1 += (target.1 - self.worm_off.1) * k;
            // the whole field twists, faster in warp
            self.roll = (self.roll + (0.1 + 0.8 * self.warp) * dt * self.hyper_vis).rem_euclid(TAU);
            if s.worm_rings {
                if self.rings.is_empty() {
                    self.rings = (0..12).map(|i| (i as f32 + 0.5) / 12.0).collect();
                }
                for z in &mut self.rings {
                    *z -= self.hyper_speed * dt;
                    if *z < 0.04 {
                        *z += 1.0;
                    }
                }
            } else {
                self.rings.clear();
            }
        } else {
            let k = (-dt / 0.4).exp();
            self.worm_off = (self.worm_off.0 * k, self.worm_off.1 * k);
            self.roll *= k;
            self.rings.clear();
        }
        // bent/twisted stars can come back on screen, so keep them a bit longer
        let edge = if worm { 1.6 } else { 1.05 };

        for i in 0..self.stars.len() {
            let st = &mut self.stars[i];
            st.pz = st.z;
            st.z -= self.hyper_speed * dt;
            let gone = st.z <= 0.03 || (st.x / st.z).abs() > aspect * edge || (st.y / st.z).abs() > edge;
            if gone {
                self.stars[i] = self.new_star(aspect, 1.0);
            }
        }
    }

    /// Orbiting stars: circle around the middle, speed up and flare on every kick.
    pub fn update_orbit(&mut self, dt: f32, hit: bool, playing: bool, s: &Settings) {
        if s.orbit_stars == 0 {
            self.orbit_vis = 0.0;
            return;
        }
        let target = if playing { 1.0 } else { 0.0 };
        let step = dt / 0.8;
        self.orbit_vis = if self.orbit_vis < target {
            (self.orbit_vis + step).min(1.0)
        } else {
            (self.orbit_vis - step).max(0.0)
        };
        if hit {
            self.orbit_kick = 1.0;
        }
        self.orbit_kick *= (-dt / 0.3).exp();
        // radians per second: a calm drift, whipping round on each kick
        let speed = (0.5 + 0.5 * s.orbit_speed) * (0.6 + 5.0 * self.orbit_kick) * self.orbit_vis;
        self.orbit_angle = (self.orbit_angle + speed * dt).rem_euclid(TAU);
    }

    /// Is anything still moving? (so "sleep when silent" waits for it)
    pub fn active(&self) -> bool {
        let (x, y) = self.shake();
        !self.particles.is_empty()
            || self.glitch > 0.01
            || self.glow > 0.01
            || self.punch > 0.01
            || x.abs() + y.abs() > 0.3
            || (self.hyper_vis > 0.01 && !self.stars.is_empty())
            || self.orbit_vis > 0.01
    }
}
