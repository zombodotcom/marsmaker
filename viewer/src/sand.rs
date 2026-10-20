//! REAL SAND mode (M cycles FUN -> REAL -> SAND; `--mode sand`, `?mode=sand`): the dunes behave
//! like sand instead of rock.
//!
//! * Moving dunes: a Werner (1995) slab automaton (`sand_sim.rs`) on a ~1 km square around the
//!   buggy, rotated into the wind, with a time-lapse from real time up to one Mars year per second.
//!   Its height changes are published into a sparse per-post delta layer that `Site::at` adds to
//!   the DTM, so physics, meshes and the HUD all see the moved sand.
//! * Tire tracks: a sparse 0.2 m "rut" layer on top (`Ruts`), stamped by the wheels (pressed down,
//!   pushed into side berms) and faded by the wind in proportion to the simulated sand transport.
//!   It's drawn as small overlay patches; the terrain chunk under an overlay leaves those quads out.
//! * Soft sand: per-wheel sinkage from load, sand thickness and slope; compaction resistance from
//!   the fresh sinkage; less grip in fresh / near-repose sand; wheelspin digs in (you can bog down);
//!   wheels on near-repose slopes trigger local avalanches.
//!
//! Hooks elsewhere (all marked "REAL SAND hook"): `Site::at` / `Site::height` / `chunk_mesh`
//! (terrain.rs), the tire loop in `Buggy::substep` (buggy.rs), the mode enum, the CLI and HUD
//! (main.rs). Everything else lives here and in sand_sim.rs.

use crate::buggy::{Buggy, Controls, Mode};
use crate::sand_sim::{self, MARS_YEAR_S, Params, Region, RunStats, SOL_S, Sim};
use crate::terrain::{self, Site, TerrainRender};
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::tasks::{AsyncComputeTaskPool, Task, futures::check_ready};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

// --- soft-sand tire constants ---
/// static sinkage in deep loose sand at the static wheel load (420 kg * 3.711 / 4 ~ 390 N)
const Z_REF: f32 = 0.05;
const LOAD_REF: f32 = 390.0;
const Z_MAX: f32 = 0.30;
const TIRE_R: f32 = 0.42;
const TIRE_HALF_W: f32 = 0.2;
const BERM_W: f32 = 0.3;
/// half length of the sunk tire's contact chord at sinkage z: sqrt(2 R z - z^2), at least 0.2 m
fn chord(z: f32) -> f32 {
    (2.0 * TIRE_R * z - z * z).max(0.0).sqrt().max(0.2)
}
/// compaction resistance: crr = C_COMPACT * sqrt(z / D) (rigid-wheel rule of thumb)
const C_COMPACT: f32 = 0.5;
/// bulldozing a bow wave once sunk past BULL_FROM
const C_BULL: f32 = 1.0;
const BULL_FROM: f32 = 0.12;
/// a wheel spinning in place (slipping, contact speed < DIG_BELOW) digs itself in at up to this
/// rate (m/s); rolling freely refills the hole under it at DIG_RECOVER
const DIG_RATE: f32 = 0.05;
const DIG_BELOW: f32 = 0.5;
const DIG_RECOVER: f32 = 0.2;
/// fraction of the pressed-out volume pushed into the side berms (the rest is compaction)
const BERM_FRAC: f32 = 0.7;
/// fine rut grid spacing target (m) and patch size (posts)
const RUT_SPACING: f32 = 0.2;
/// each simulated slab event per cell halves the rut relief
const RUT_DECAY_PER_EVENT: f64 = 0.5;

#[cfg(not(target_arch = "wasm32"))]
const SIM_BUDGET_MS: f32 = 12.0;
#[cfg(not(target_arch = "wasm32"))]
const PUBLISH_BUDGET_MS: f32 = 4.0;
#[cfg(target_arch = "wasm32")]
const SIM_BUDGET_MS: f32 = 2.0;
#[cfg(target_arch = "wasm32")]
const PUBLISH_BUDGET_MS: f32 = 1.0;
/// main-thread budget for chunk / overlay mesh rebuilds per frame (wasm builds chunks inline)
const MESH_BUDGET_MS: f32 = 2.5;
#[cfg(not(target_arch = "wasm32"))]
const MAX_CHUNK_TASKS: usize = 4;

/// `--sand-*` flags (browser: `?sand-rate=1&sand-wind=200`).
#[derive(Resource, Clone, Debug)]
pub struct SandArgs {
    /// initial time-lapse, Mars years per real second (default: real time)
    pub rate: Option<f64>,
    /// wind direction the sand moves *toward*, compass degrees (default: from the slip faces)
    pub wind: Option<f32>,
    /// saturated sand flux, m^3 / m / Earth year
    pub flux: f32,
    /// simulated square edge (m)
    pub region: f32,
    /// `NAME@T,...`: at T s write NAME.png (screenshot) and NAME_{before,after,diff}.png + stats
    pub shots: Vec<(String, f32)>,
    /// SAND log period (s)
    pub log: f32,
    /// test hook: press K (back to the real DTM) at these times (s)
    pub reset_at: Vec<f32>,
}

impl Default for SandArgs {
    fn default() -> Self {
        Self { rate: None, wind: None, flux: 5.0, region: 1024.0, shots: Vec::new(), log: 1.0, reset_at: Vec::new() }
    }
}

impl SandArgs {
    pub fn takes(flag: &str) -> bool {
        matches!(flag, "--sand-rate" | "--sand-wind" | "--sand-flux" | "--sand-region" | "--sand-shots" | "--sand-log" | "--sand-reset")
    }
    pub fn set(&mut self, flag: &str, v: String) {
        let num = |v: &str| v.trim().parse::<f64>().unwrap_or_else(|_| panic!("{flag} needs a number"));
        match flag {
            "--sand-rate" => self.rate = Some(num(&v)),
            "--sand-wind" => self.wind = Some(num(&v) as f32),
            "--sand-flux" => self.flux = num(&v) as f32,
            "--sand-region" => self.region = (num(&v) as f32).clamp(128.0, 4096.0),
            "--sand-log" => self.log = num(&v) as f32,
            "--sand-reset" => self.reset_at.extend(v.split(',').filter(|s| !s.is_empty()).map(|t| num(t) as f32)),
            "--sand-shots" => {
                for item in v.split(',').filter(|s| !s.is_empty()) {
                    let (name, t) = item.rsplit_once('@').unwrap_or_else(|| panic!("--sand-shots item {item:?} needs NAME@T"));
                    self.shots.push((name.to_string(), num(t) as f32));
                }
            }
            _ => unreachable!(),
        }
    }
}

pub struct SandPlugin(pub SandArgs);

impl Plugin for SandPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.0.clone())
            .insert_resource(SandState::new(&self.0))
            .init_resource::<SandRender>()
            .init_resource::<SandHud>()
            .add_systems(Update, (sand_control, sand_sim, sand_meshes, sand_report).chain());
    }
}

/// HUD text for main.rs (empty when the sand layer is off).
#[derive(Resource, Default)]
pub struct SandHud(pub String);

// ---------------------------------------------------------------------------------------------
// shared state (read by physics, mesh tasks and the background sim)

/// Post-space geometry of the site.
#[derive(Clone, Copy)]
pub struct Geom {
    east0: f32,
    north0: f32,
    mpp: f32,
    w: usize,
    h: usize,
    chunk: usize,
}

impl Geom {
    pub fn of(site: &Site) -> Self {
        Self { east0: site.east0, north0: site.north0, mpp: site.mpp, w: site.w, h: site.h, chunk: site.chunk }
    }
    /// world (x, z) -> fractional (row, col)
    fn rc(&self, p: Vec2) -> (f32, f32) {
        ((self.north0 + p.y) / self.mpp, (p.x - self.east0) / self.mpp)
    }
    fn world(&self, r: f32, c: f32) -> Vec2 {
        Vec2::new(self.east0 + c * self.mpp, r * self.mpp - self.north0)
    }
}

/// Fine rut layer: sparse patches of `pp` x `pp` posts, `s` fine vertices per post.
pub struct Ruts {
    s: usize,
    pp: usize,
    n: usize,
    patches: HashMap<(i32, i32), Vec<f32>>,
    /// patches drawn as overlays (rut patches + their 8 neighbours); terrain quads there are cut
    active: HashSet<(i32, i32)>,
    new_active: Vec<(i32, i32)>,
    dirty: HashSet<(i32, i32)>,
    /// fine-vertex bounding box of all patches (quick reject)
    bbox: (i64, i64, i64, i64),
}

impl Ruts {
    fn new(site: &Site) -> Self {
        let s = (site.mpp / RUT_SPACING).ceil().max(1.0) as usize;
        let pp = [8usize, 9, 7, 6, 12, 4, 3, 2, 1].into_iter().find(|p| site.chunk % p == 0).unwrap_or(1);
        Self { s, pp, n: pp * s, patches: HashMap::new(), active: HashSet::new(), new_active: Vec::new(), dirty: HashSet::new(), bbox: (i64::MAX, i64::MAX, i64::MIN, i64::MIN) }
    }
    fn key(&self, fr: i64, fc: i64) -> ((i32, i32), usize) {
        let n = self.n as i64;
        let k = (fr.div_euclid(n) as i32, fc.div_euclid(n) as i32);
        (k, (fr.rem_euclid(n) * n + fc.rem_euclid(n)) as usize)
    }
    #[inline]
    fn get(&self, fr: i64, fc: i64) -> f32 {
        let (k, i) = self.key(fr, fc);
        self.patches.get(&k).map_or(0.0, |p| p[i])
    }
    fn vert_mut(&mut self, fr: i64, fc: i64) -> &mut f32 {
        let (k, i) = self.key(fr, fc);
        if !self.patches.contains_key(&k) {
            self.patches.insert(k, vec![0.0; self.n * self.n]);
            for dr in -1..=1 {
                for dc in -1..=1 {
                    let q = (k.0 + dr, k.1 + dc);
                    if self.active.insert(q) {
                        self.new_active.push(q);
                    }
                }
            }
            let n = self.n as i64;
            let b = &mut self.bbox;
            *b = (b.0.min(k.0 as i64 * n), b.1.min(k.1 as i64 * n), b.2.max((k.0 as i64 + 1) * n), b.3.max((k.1 as i64 + 1) * n));
        }
        // the patch before / above shares this vertex as its far edge
        let (lr, lc) = (i / self.n, i % self.n);
        self.dirty.insert(k);
        if lr == 0 {
            self.dirty.insert((k.0 - 1, k.1));
        }
        if lc == 0 {
            self.dirty.insert((k.0, k.1 - 1));
        }
        if lr == 0 && lc == 0 {
            self.dirty.insert((k.0 - 1, k.1 - 1));
        }
        &mut self.patches.get_mut(&k).unwrap()[i]
    }
    /// bilinear rut value at fractional post (r, c)
    fn sample(&self, r: f32, c: f32) -> f32 {
        if self.patches.is_empty() {
            return 0.0;
        }
        let (fr, fc) = (r * self.s as f32, c * self.s as f32);
        let (i0, j0) = (fr.floor() as i64, fc.floor() as i64);
        let b = self.bbox;
        if i0 + 1 < b.0 || j0 + 1 < b.1 || i0 > b.2 || j0 > b.3 {
            return 0.0;
        }
        let (tr, tc) = (fr - i0 as f32, fc - j0 as f32);
        let (a, bb, c2, d) = (self.get(i0, j0), self.get(i0, j0 + 1), self.get(i0 + 1, j0), self.get(i0 + 1, j0 + 1));
        (a * (1.0 - tc) + bb * tc) * (1.0 - tr) + (c2 * (1.0 - tc) + d * tc) * tr
    }
    /// wind fades every rut by `f` (0..1)
    fn fade(&mut self, f: f32) {
        for (k, p) in self.patches.iter_mut() {
            for v in p.iter_mut() {
                *v *= f;
            }
            self.dirty.insert(*k);
        }
    }
}

/// Per-wheel soft-sand state (for the next substep, the HUD and the log).
#[derive(Clone, Copy, Default)]
pub struct WheelSand {
    pub sink: f32,
    pub fresh: f32,
    pub crr: f32,
    pub grip: f32,
    pub slip: f32,
    pub thick: f32,
    pub slope: f32,
    /// depth dug by wheelspin in place (m)
    pub dig: f32,
    dist_t: f32,
}

pub struct SandShared {
    bw: usize,
    blocks: Vec<OnceLock<Box<[AtomicU32]>>>,
    pub ruts: RwLock<Ruts>,
    region: RwLock<Option<Arc<Region>>>,
    disturb: Mutex<Vec<Vec2>>,
    pub wheels: Mutex<[WheelSand; 4]>,
    /// the surface differs from the NASA DTM somewhere
    pub changed: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SandShared {
    fn new(site: &Site) -> Self {
        let (bw, bh) = (site.w.div_ceil(64), site.h.div_ceil(64));
        Self {
            bw,
            blocks: (0..bw * bh).map(|_| OnceLock::new()).collect(),
            ruts: RwLock::new(Ruts::new(site)),
            region: RwLock::new(None),
            disturb: Mutex::new(Vec::new()),
            wheels: Mutex::new([WheelSand { grip: 1.0, ..default() }; 4]),
            changed: AtomicBool::new(false),
        }
    }

    /// Dune-layer height change at post (r, c) (REAL SAND hook in `Site::at`).
    #[inline]
    pub fn post_delta(&self, r: usize, c: usize) -> f32 {
        match self.blocks.get((r >> 6) * self.bw + (c >> 6)).and_then(|b| b.get()) {
            Some(b) => f32::from_bits(b[((r & 63) << 6) | (c & 63)].load(Relaxed)),
            None => 0.0,
        }
    }
    fn set_post(&self, r: usize, c: usize, v: f32) {
        let k = (r >> 6) * self.bw + (c >> 6);
        let b = self.blocks[k].get_or_init(|| (0..4096).map(|_| AtomicU32::new(0)).collect());
        b[((r & 63) << 6) | (c & 63)].store(v.to_bits(), Relaxed);
    }
    fn block_snapshot(&self, k: usize) -> Option<Box<[f32]>> {
        self.blocks.get(k)?.get().map(|b| b.iter().map(|v| f32::from_bits(v.load(Relaxed))).collect())
    }

    /// Rut height at fractional post (r, c) (REAL SAND hook in `Site::height`).
    #[inline]
    pub fn rut(&self, r: f32, c: f32) -> f32 {
        self.ruts.read().map_or(0.0, |ru| ru.sample(r, c))
    }
}

/// REAL SAND hook for `chunk_mesh`: full-res quad (r, c) is covered by a rut overlay patch.
pub fn quad_hidden(site: &Site, r: usize, c: usize) -> bool {
    let Some(s) = &site.sand else { return false };
    let Ok(ru) = s.ruts.read() else { return false };
    !ru.active.is_empty() && ru.active.contains(&((r / ru.pp) as i32, (c / ru.pp) as i32))
}

/// Triangle-interpolated height at fractional post (r, c), like `Site::height` but without the
/// rut layer (`raw`: also without the dune layer, i.e. the NASA DTM).
fn tri_rc(site: &Site, r: f32, c: f32, raw: bool) -> Option<f32> {
    if !(r >= 0.0 && c >= 0.0) {
        return None;
    }
    let (r0, c0) = (r as usize, c as usize);
    if r0 + 1 >= site.h || c0 + 1 >= site.w {
        return None;
    }
    let get = |r: usize, c: usize| if raw { site.z[r * site.w + c] } else { site.at(r, c) };
    let (fr, fc) = (r - r0 as f32, c - c0 as f32);
    let (a, b, cc, d) = (get(r0, c0), get(r0 + 1, c0), get(r0 + 1, c0 + 1), get(r0, c0 + 1));
    let v = if fr + fc <= 1.0 { a + (b - a) * fr + (d - a) * fc } else { cc + (d - cc) * (1.0 - fr) + (b - cc) * (1.0 - fc) };
    v.is_finite().then_some(v)
}

fn tri(site: &Site, p: Vec2, raw: bool) -> Option<f32> {
    let (r, c) = Geom::of(site).rc(p);
    tri_rc(site, r, c, raw)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// What the soft sand does to one tire this substep.
pub struct TireSand {
    /// added load-proportional rolling resistance coefficient
    pub crr: f32,
    /// multiplier on the friction ellipse
    pub grip: f32,
}
const NEUTRAL: TireSand = TireSand { crr: 0.0, grip: 1.0 };

/// REAL SAND hook in the tire loop: sinkage (the wheel presses a rut into the sand, which the
/// suspension ray then finds lower), compaction / bulldozing resistance from the *fresh* sinkage
/// ahead of the contact (rolling in an existing rut is easy), grip loss in fresh and
/// near-repose sand, and avalanches on slip faces. `n` = ground normal, `fwd` = wheel heading
/// in the ground plane, `v` = contact velocity, `h` = substep.
#[allow(clippy::too_many_arguments)]
pub fn wheel(site: &Site, i: usize, mode: Mode, contact: Vec3, n: Vec3, load: f32, fwd: Vec3, v: Vec3, h: f32) -> TireSand {
    if mode != Mode::Sand {
        return NEUTRAL;
    }
    let Some(s) = &site.sand else { return NEUTRAL };
    let p = Vec2::new(contact.x, contact.z);
    let Some(surf) = tri(site, p, false) else { return NEUTRAL };
    let thick = s
        .region
        .read()
        .ok()
        .and_then(|r| r.as_ref().and_then(|r| {
            let c = r.cell_of(p);
            r.sample(&r.base, c.x, c.y)
        }))
        .map_or(2.0, |b| surf - b);
    let sandy = smoothstep(0.05, 0.6, thick);
    let slope = n.y.clamp(-1.0, 1.0).acos();
    let repose = smoothstep(18f32.to_radians(), 30f32.to_radians(), slope);
    let mut ws = lock(&s.wheels)[i];
    // wheelspin in place digs in (progressively, so a stuck buggy bogs down)
    let spinning = ws.slip > 0.6 && v.length() < DIG_BELOW && load > 50.0;
    ws.dig = (ws.dig + if spinning { DIG_RATE * ws.slip * sandy * h } else { -DIG_RECOVER * h }).clamp(0.0, Z_MAX);
    // a fast tire has less time to sink (it planes over the surface): ~1/2 at 30 km/h
    let speed = 1.0 / (1.0 + v.length() / 8.0);
    let z_t = (Z_REF * (load / LOAD_REF).clamp(0.0, 2.5) * sandy * (1.0 + 0.8 * repose) * (1.0 + 0.5 * ws.slip) * speed + ws.dig).min(Z_MAX);
    let g = Geom::of(site);
    let f2 = Vec2::new(fwd.x, fwd.z).normalize_or(Vec2::X);
    let dir = if v.dot(fwd) < 0.0 { -1.0 } else { 1.0 };
    // existing rut just ahead of the contact patch (the leading edge meets fresh sand or a rut)
    let (ar, ac) = g.rc(p + f2 * dir * (chord(z_t) + 0.08));
    let depth_ahead = (-s.rut(ar, ac)).max(0.0);
    let fresh = (z_t - depth_ahead).max(0.0);
    if z_t > 0.003 {
        stamp(s, g, p, f2, z_t);
    }
    let crr = C_COMPACT * (fresh / (2.0 * TIRE_R)).sqrt() + C_BULL * (z_t - BULL_FROM).max(0.0) / TIRE_R * (fresh / z_t.max(1e-3));
    let grip = (1.0 - 0.25 * sandy * (fresh / z_t.max(0.02)).min(1.0) - 0.35 * repose * sandy).max(0.35);
    // slip faces: a wheel churning sand near the angle of repose sets it sliding
    ws.dist_t -= h;
    if repose > 0.5 && sandy > 0.5 && v.length() > 0.3 && ws.dist_t <= 0.0 {
        lock(&s.disturb).push(p);
        ws.dist_t = 0.15;
    }
    ws = WheelSand { sink: z_t, fresh, crr, grip, thick, slope: slope.to_degrees(), ..ws };
    lock(&s.wheels)[i] = ws;
    if z_t > 0.003 {
        s.changed.store(true, Relaxed);
    }
    TireSand { crr, grip }
}

/// REAL SAND hook after the friction ellipse: `k` > 1 = the tire is sliding / spinning, which
/// digs it in on the next substeps.
pub fn wheel_slip(site: &Site, i: usize, mode: Mode, k: f32) {
    if mode != Mode::Sand {
        return;
    }
    if let Some(s) = &site.sand {
        let w = &mut lock(&s.wheels)[i];
        // k ~ 3.5 is flat-out wheelspin from a standstill in REAL tuning
        let target = ((k - 1.0) / 2.5).clamp(0.0, 1.0);
        w.slip += (target - w.slip) * 0.02;
    }
}

/// Press the tire footprint to `z` below the dune surface; push the sand into side berms.
fn stamp(s: &SandShared, g: Geom, p: Vec2, fwd: Vec2, z: f32) {
    let Ok(mut ru) = s.ruts.write() else { return };
    let sf = ru.s as f32;
    let right = Vec2::new(-fwd.y, fwd.x);
    let half_l = chord(z) + 0.05;
    let reach = (TIRE_HALF_W + BERM_W + 0.1).max(half_l);
    let (r, c) = g.rc(p);
    let fs = g.mpp / sf; // fine spacing (m)
    let k = (reach / fs).ceil() as i64 + 1;
    let (fr0, fc0) = ((r * sf).round() as i64, (c * sf).round() as i64);
    let mut removed = 0.0f32;
    let mut berm: Vec<(i64, i64, f32)> = Vec::new();
    let mut wsum = 0.0;
    for fr in fr0 - k..=fr0 + k {
        for fc in fc0 - k..=fc0 + k {
            if fr < 0 || fc < 0 || fr as usize >= g.h * ru.s || fc as usize >= g.w * ru.s {
                continue;
            }
            let q = g.world(fr as f32 / sf, fc as f32 / sf) - p;
            let (l, f) = (q.dot(right).abs(), q.dot(fwd).abs());
            if f > half_l {
                continue;
            }
            if l <= TIRE_HALF_W {
                // the tire's round profile pressed z deep (matches the tread envelope in buggy.rs)
                let rise = TIRE_R - (TIRE_R * TIRE_R - f * f).max(0.0).sqrt();
                let target = (-z * (1.0 - 0.3 * (l / TIRE_HALF_W).powi(2)) + rise).min(0.0);
                // only touch (and create patches for) vertices the tire actually presses
                if ru.get(fr, fc) > target + 1e-4 {
                    let v = ru.vert_mut(fr, fc);
                    removed += *v - target;
                    *v = target;
                }
            } else if l <= TIRE_HALF_W + BERM_W {
                let w = (std::f32::consts::PI * (l - TIRE_HALF_W) / BERM_W).sin().max(0.05);
                berm.push((fr, fc, w));
                wsum += w;
            }
        }
    }
    if removed > 0.0 && wsum > 0.0 {
        let add = removed * BERM_FRAC / wsum;
        for (fr, fc, w) in berm {
            let v = ru.vert_mut(fr, fc);
            *v = (*v + add * w).min(0.25);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// region building, background sim, publishing

fn build_region(site: &Site, shared: &SandShared, centre: Vec2, u: Vec2, size_m: f32) -> Region {
    let dx = site.mpp.max(2.0);
    let n = (((size_m / dx) as usize) / sand_sim::BLOCK).max(4) * sand_sim::BLOCK;
    let u = u.normalize_or(Vec2::X);
    let v = Vec2::new(-u.y, u.x);
    let origin = centre - (u + v) * ((n - 1) as f32 * 0.5 * dx);
    let mut reg = Region { n, dx, origin, u, v, base: Vec::new(), h0: vec![f32::NAN; n * n], snap: HashMap::new() };
    let mut raw = vec![f32::NAN; n * n];
    for b in 0..n {
        for a in 0..n {
            let p = reg.world(a as f32, b as f32);
            raw[b * n + a] = tri(site, p, true).unwrap_or(f32::NAN);
            reg.h0[b * n + a] = tri(site, p, false).unwrap_or(f32::NAN);
        }
    }
    reg.base = sand_sim::base_from(&raw, n, (100.0 / dx) as usize);
    // never let the floor sit above the current surface
    for (b, h) in reg.base.iter_mut().zip(&reg.h0) {
        if h.is_finite() {
            *b = b.min(*h);
        }
    }
    // snapshot of the delta layer already there (earlier regions / wind directions)
    let g = Geom::of(site);
    let (rmin, rmax, cmin, cmax) = region_posts(&reg, g, 0.0, 0.0, (n - 1) as f32, (n - 1) as f32);
    for br in rmin / 64..=rmax / 64 {
        for bc in cmin / 64..=cmax / 64 {
            let k = br * shared.bw + bc;
            if let Some(s) = shared.block_snapshot(k) {
                reg.snap.insert(k, s);
            }
        }
    }
    reg
}

/// Post AABB (clamped) covering cells [a0, a1] x [b0, b1] of the region.
fn region_posts(reg: &Region, g: Geom, a0: f32, b0: f32, a1: f32, b1: f32) -> (usize, usize, usize, usize) {
    let (mut rmin, mut rmax, mut cmin, mut cmax) = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    for (a, b) in [(a0, b0), (a1, b0), (a0, b1), (a1, b1)] {
        let (r, c) = g.rc(reg.world(a, b));
        rmin = rmin.min(r);
        rmax = rmax.max(r);
        cmin = cmin.min(c);
        cmax = cmax.max(c);
    }
    let cl = |v: f32, hi: usize| (v.max(0.0) as usize).min(hi - 1);
    (cl(rmin.floor(), g.h), cl(rmax.ceil(), g.h), cl(cmin.floor(), g.w), cl(cmax.ceil(), g.w))
}

/// Write dirty sim blocks into the post delta layer (post = snapshot + interpolated change).
/// Returns the render chunks touched.
fn publish(sim: &mut Sim, shared: &SandShared, g: Geom, budget_ms: f32, stats: &mut RunStats) -> Vec<(usize, usize)> {
    let t0 = Instant::now();
    let reg = sim.reg.clone();
    let n = reg.n;
    let nb = sim.blocks_per_edge();
    let mut chunks = HashSet::new();
    let mut any = false;
    while t0.elapsed().as_secs_f32() * 1000.0 < budget_ms {
        let Some(k) = sim.take_block() else { break };
        stats.published_blocks += 1;
        let (kb, ka) = (k / nb, k % nb);
        let a0 = (ka * sand_sim::BLOCK) as f32 - 1.0;
        let b0 = (kb * sand_sim::BLOCK) as f32 - 1.0;
        let (rmin, rmax, cmin, cmax) = region_posts(&reg, g, a0, b0, a0 + sand_sim::BLOCK as f32 + 1.0, b0 + sand_sim::BLOCK as f32 + 1.0);
        for r in rmin..=rmax {
            for c in cmin..=cmax {
                let cell = reg.cell_of(g.world(r as f32, c as f32));
                if !(cell.x >= 0.0 && cell.y >= 0.0 && cell.x <= (n - 1) as f32 && cell.y <= (n - 1) as f32) {
                    continue;
                }
                let (Some(h), Some(h0)) = (reg.sample(&sim.h, cell.x, cell.y), reg.sample(&reg.h0, cell.x, cell.y)) else { continue };
                let pk = (r >> 6) * shared.bw + (c >> 6);
                let snap = reg.snap.get(&pk).map_or(0.0, |s| s[((r & 63) << 6) | (c & 63)]);
                let d = h - h0;
                if d.abs() > 1e-4 {
                    any = true;
                }
                shared.set_post(r, c, snap + d);
            }
        }
        for cr in rmin / g.chunk..=rmax / g.chunk {
            for cc in cmin / g.chunk..=cmax / g.chunk {
                chunks.insert((cr, cc));
            }
        }
    }
    if any {
        shared.changed.store(true, Relaxed);
    }
    stats.publish_ms += t0.elapsed().as_secs_f32() * 1000.0;
    chunks.into_iter().collect()
}

type SimOut = (Box<Sim>, RunStats, Vec<(usize, usize)>);

fn run_sim(mut sim: Box<Sim>, shared: Arc<SandShared>, g: Geom, picks: f64) -> SimOut {
    let mut st = RunStats::default();
    let dist: Vec<Vec2> = std::mem::take(&mut *lock(&shared.disturb));
    for p in dist {
        st.disturb_moves += sim.disturb(p);
    }
    sim.step(picks, SIM_BUDGET_MS, &mut st);
    let chunks = publish(&mut sim, &shared, g, PUBLISH_BUDGET_MS, &mut st);
    (sim, st, chunks)
}

/// Time-lapse settings (simulated seconds per real second).
fn levels(args: &SandArgs) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = vec![
        ("paused".into(), 0.0),
        ("real time".into(), 1.0),
        ("1 min/s".into(), 60.0),
        ("1 hour/s".into(), 3600.0),
        ("1 sol/s".into(), SOL_S),
        ("10 sols/s".into(), 10.0 * SOL_S),
        ("100 sols/s".into(), 100.0 * SOL_S),
        ("1 Mars yr/s".into(), MARS_YEAR_S),
    ];
    if let Some(r) = args.rate {
        let s = r * MARS_YEAR_S;
        if !v.iter().any(|x| (x.1 - s).abs() < 1e-6 * s.max(1.0)) {
            v.push((format!("{r} Mars yr/s"), s));
            v.sort_by(|a, b| a.1.total_cmp(&b.1));
        }
    }
    v
}

#[derive(Resource)]
pub struct SandState {
    levels: Vec<(String, f64)>,
    level: usize,
    /// downwind unit vector (world x, z); None = estimate from the slip faces
    wind: Option<Vec2>,
    wind_note: String,
    sim: Option<Box<Sim>>,
    task: Option<Task<SimOut>>,
    params: Option<Params>,
    /// simulated seconds over all regions since the last reset
    total_s: f64,
    last_launch: Option<Instant>,
    last_dt: f64,
    rate_actual: f64,
    stats: RunStats,
    sim_ms_avg: f32,
    publish_ms_avg: f32,
    main_ms_avg: f32,
    total_events: u64,
    total_avalanche: u64,
    total_disturb: u64,
    rebuild_at: Option<Vec2>,
    rut_decay_acc: f64,
    bog_t: f32,
    pub bogged: bool,
    bog_count: u32,
    start: Option<f32>,
    next_log: f32,
    shots_done: Vec<bool>,
    dumps_pending: Vec<String>,
    regions_built: u32,
}

impl SandState {
    fn new(args: &SandArgs) -> Self {
        let levels = levels(args);
        let level = args.rate.map_or(1, |r| levels.iter().position(|l| (l.1 - r * MARS_YEAR_S).abs() < 1e-6 * l.1.max(1.0)).unwrap_or(1));
        Self {
            levels,
            level,
            wind: args.wind.map(|deg| {
                let a = deg.to_radians();
                Vec2::new(a.sin(), -a.cos()) // compass: 0 = north = -z
            }),
            wind_note: if args.wind.is_some() { "set with --sand-wind".into() } else { String::new() },
            sim: None,
            task: None,
            params: None,
            total_s: 0.0,
            last_launch: None,
            last_dt: 0.0,
            rate_actual: 0.0,
            stats: RunStats::default(),
            sim_ms_avg: 0.0,
            publish_ms_avg: 0.0,
            main_ms_avg: 0.0,
            total_events: 0,
            total_avalanche: 0,
            total_disturb: 0,
            rebuild_at: None,
            rut_decay_acc: 0.0,
            bog_t: 0.0,
            bogged: false,
            bog_count: 0,
            start: None,
            next_log: 0.0,
            shots_done: vec![false; args.shots.len()],
            dumps_pending: Vec::new(),
            regions_built: 0,
        }
    }
    fn rate(&self) -> f64 {
        self.levels[self.level].1
    }
}

fn compass(u: Vec2) -> f32 {
    (f32::atan2(u.x, -u.y).to_degrees() + 360.0) % 360.0
}

/// Downwind direction from the slip faces in a square around `centre`.
fn estimate_wind(site: &Site, centre: Vec2, size: f32) -> (Vec2, String) {
    let g = Geom::of(site);
    let (r, c) = g.rc(centre);
    let half = (size * 0.5 / g.mpp) as i64;
    let d = (1.0 / g.mpp).round().max(1.0) as i64; // ~1 m half-baseline
    let st = ((2.0 / g.mpp).round() as i64).max(1);
    let mut grads = Vec::new();
    let at = |r: i64, c: i64| -> f32 {
        if r < 0 || c < 0 || r as usize >= g.h || c as usize >= g.w { f32::NAN } else { site.z[r as usize * g.w + c as usize] }
    };
    let (r, c) = (r as i64, c as i64);
    for rr in (r - half..r + half).step_by(st as usize) {
        for cc in (c - half..c + half).step_by(st as usize) {
            let (e, w, s, n) = (at(rr, cc + d), at(rr, cc - d), at(rr + d, cc), at(rr - d, cc));
            if e.is_finite() && w.is_finite() && s.is_finite() && n.is_finite() {
                // dh/dx (east), dh/dz (south = +r)
                grads.push(Vec2::new(e - w, s - n) / (2.0 * d as f32 * g.mpp));
            }
        }
    }
    let (u, coh, votes) = sand_sim::slipface_wind(grads.into_iter());
    if votes < 50 || coh < 0.1 {
        let u = Vec2::new(1.0, 0.0);
        return (u, format!("no clear slip faces ({votes} steep posts, coherence {coh:.2}); default toward the east"));
    }
    (u, format!("from {votes} slip-face posts (24-42 deg), coherence {coh:.2}"))
}

// ---------------------------------------------------------------------------------------------
// systems

#[allow(clippy::too_many_arguments)]
fn sand_control(
    keys: Res<ButtonInput<KeyCode>>,
    args: Res<SandArgs>,
    mut st: ResMut<SandState>,
    mut rnd: ResMut<SandRender>,
    site: Option<ResMut<Site>>,
    buggy: Option<Res<Buggy>>,
    ctl: Res<Controls>,
    time: Res<Time>,
    render: Option<Res<TerrainRender>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Some(mut site), Some(buggy)) = (site, buggy) else { return };
    let now = time.elapsed_secs();
    let t_run = now - *st.start.get_or_insert(now);
    let scripted_reset = args.reset_at.iter().any(|&t| t <= t_run && t > t_run - time.delta_secs());
    let sand_mode = buggy.tuning.mode == Mode::Sand;
    let pos = Vec2::new(buggy.pos.x, buggy.pos.z);

    if keys.just_pressed(KeyCode::KeyK) || scripted_reset {
        // back to the real DTM
        st.task = None;
        st.sim = None;
        st.total_s = 0.0;
        st.total_events = 0;
        st.total_avalanche = 0;
        st.total_disturb = 0;
        st.rebuild_at = None;
        let had_holes: Vec<(usize, usize)> = site.sand.as_ref().map_or(Vec::new(), |s| {
            let ru = s.ruts.read().unwrap();
            ru.active.iter().map(|&(pr, pc)| ((pr.max(0) as usize * ru.pp) / site.chunk, (pc.max(0) as usize * ru.pp) / site.chunk)).collect()
        });
        site.sand = None;
        #[cfg(not(target_arch = "wasm32"))]
        rnd.tasks.clear();
        rnd.overlay_chunk_dirty.clear();
        for (_, (e, h)) in rnd.overlays.drain() {
            commands.entity(e).despawn();
            meshes.remove(&h);
        }
        let touched: Vec<_> = rnd.touched.iter().copied().collect();
        rnd.dirty.extend(touched);
        // chunks that had holes are rebuilt right away, so no hole shows
        if let Some(r) = &render {
            let set: HashSet<_> = had_holes.into_iter().collect();
            for k in set {
                rebuild_now(&site, r, &mut meshes, k, &mut rnd);
            }
        }
        info!("SAND reset: back to the NASA DTM");
    }
    if !sand_mode {
        return;
    }
    // time-lapse and wind
    if keys.just_pressed(KeyCode::BracketRight) && st.level + 1 < st.levels.len() {
        st.level += 1;
        info!("SAND time-lapse -> {}", st.levels[st.level].0);
    }
    if keys.just_pressed(KeyCode::BracketLeft) && st.level > 0 {
        st.level -= 1;
        info!("SAND time-lapse -> {}", st.levels[st.level].0);
    }
    for (key, deg) in [(KeyCode::Comma, -15f32), (KeyCode::Period, 15.0)] {
        if keys.just_pressed(key) {
            let u = st.wind.unwrap_or(Vec2::X);
            let u = Vec2::from_angle(deg.to_radians()).rotate(u);
            st.wind = Some(u);
            st.wind_note = "set with , .".into();
            st.rebuild_at = Some(pos);
            info!("SAND wind -> toward {:.0} deg", compass(u));
        }
    }
    let shared = match &site.sand {
        Some(s) => s.clone(),
        None => {
            let s = Arc::new(SandShared::new(&site));
            site.sand = Some(s.clone());
            s
        }
    };
    // (re)build the region when there is none, the buggy nears its edge, or the wind changed
    if let Some(sim) = &st.sim {
        let c = sim.reg.cell_of(pos);
        let n = sim.reg.n as f32;
        if (c - Vec2::splat((n - 1.0) * 0.5)).abs().max_element() > 0.3 * n && st.rebuild_at.is_none() {
            st.rebuild_at = Some(pos);
        }
    }
    let need = st.sim.is_none() && st.task.is_none() || st.rebuild_at.is_some();
    if need && st.task.is_none() {
        let t = Instant::now();
        let centre = st.rebuild_at.take().unwrap_or(pos);
        // flush what the old region hasn't published yet
        if let Some(mut old) = st.sim.take() {
            let mut rs = RunStats::default();
            for k in publish(&mut old, &shared, Geom::of(&site), f32::INFINITY, &mut rs) {
                rnd.dirty.insert(k);
                rnd.touched.insert(k);
                rnd.overlay_chunk_dirty.insert(k);
            }
        }
        let u = match st.wind {
            Some(u) => u,
            None => {
                let (u, note) = estimate_wind(&site, centre, args.region);
                st.wind_note = note;
                st.wind = Some(u);
                u
            }
        };
        let reg = Arc::new(build_region(&site, &shared, centre, u, args.region));
        let p = Params::new(reg.dx, args.flux);
        st.params = Some(p);
        *shared.region.write().unwrap() = Some(reg.clone());
        let seed = 0x9E3779B97F4A7C15u64 ^ st.regions_built as u64;
        st.regions_built += 1;
        let sand_cells = reg.h0.iter().zip(&reg.base).filter(|(h, b)| (*h - *b) > 0.5 * p.hs).count();
        let mean_t = reg.h0.iter().zip(&reg.base).filter(|(h, b)| h.is_finite() && b.is_finite()).map(|(h, b)| (h - b) as f64).sum::<f64>() / (reg.n * reg.n) as f64;
        info!(
            "SAND region #{}: {} x {} cells of {:.1} m ({:.0} m) at ({:.0}, {:.0}), wind toward {:.0} deg (from {:.0}) {}; sand cells {:.0}%, mean sand {:.1} m; hop {} cells, slab {} m, flux {} m3/m/yr -> {:.2} picks/cell/Mars yr; built in {:.0} ms",
            st.regions_built, reg.n, reg.n, reg.dx, reg.n as f32 * reg.dx, centre.x, centre.y, compass(u), (compass(u) + 180.0) % 360.0, st.wind_note,
            100.0 * sand_cells as f32 / (reg.n * reg.n) as f32, mean_t, p.hop, p.hs, p.flux, p.picks_per_cell_per_s() * MARS_YEAR_S,
            t.elapsed().as_secs_f32() * 1000.0
        );
        st.sim = Some(Box::new(Sim::new(reg, p, seed)));
        st.last_launch = None;
    }
    // bogging: throttle held, nearly stopped, wheels sunk in
    let w = *lock(&shared.wheels);
    let sink = w.iter().map(|w| w.sink).sum::<f32>() / 4.0;
    let dt = time.delta_secs();
    if ctl.throttle.abs() > 0.5 && buggy.vel.length() < 1.0 && sink > 0.06 {
        st.bog_t += dt;
        if st.bog_t > 2.0 && !st.bogged {
            st.bogged = true;
            st.bog_count += 1;
            info!("SAND BOGGED DOWN #{} at ({:.1}, {:.1}): sinkage {:.0}/{:.0}/{:.0}/{:.0} cm, slip {:.2}", st.bog_count, pos.x, pos.y,
                w[0].sink * 100.0, w[1].sink * 100.0, w[2].sink * 100.0, w[3].sink * 100.0, w.iter().map(|w| w.slip).fold(0.0, f32::max));
        }
    } else if buggy.vel.length() > 1.5 {
        st.bog_t = 0.0;
        if st.bogged {
            info!("SAND unstuck at ({:.1}, {:.1})", pos.x, pos.y);
        }
        st.bogged = false;
    }
}

fn sand_sim(mut st: ResMut<SandState>, site: Option<Res<Site>>, buggy: Option<Res<Buggy>>, mut rnd: ResMut<SandRender>) {
    let (Some(site), Some(buggy)) = (site, buggy) else { return };
    let Some(shared) = site.sand.clone() else { return };
    // collect a finished run
    if let Some(task) = &mut st.task {
        if let Some(out) = check_ready(task) {
            st.task = None;
            absorb(&mut st, &shared, &mut rnd, out);
        }
    }
    if st.task.is_some() || st.sim.is_none() || st.rebuild_at.is_some() {
        return;
    }
    // test dumps need the sim at rest
    if !st.dumps_pending.is_empty() {
        let names = std::mem::take(&mut st.dumps_pending);
        for name in names {
            dump(&st, &name);
        }
    }
    let sand_mode = buggy.tuning.mode == Mode::Sand;
    let now = Instant::now();
    let dt = st.last_launch.map_or(0.0, |t| now.duration_since(t).as_secs_f64()).min(0.25);
    st.last_launch = Some(now);
    st.last_dt = dt;
    let p = st.params.unwrap();
    #[allow(unused_mut)]
    let mut sim = st.sim.take().unwrap();
    let n2 = (sim.reg.n * sim.reg.n) as f64;
    let picks = if sand_mode { st.rate() * dt * p.picks_per_cell_per_s() * n2 } else { 0.0 };
    let g = Geom::of(&site);
    let busy = picks > 0.0 || !sim.dirty_list.is_empty() || !lock(&shared.disturb).is_empty();
    if !busy {
        st.sim = Some(sim);
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let sh = shared.clone();
        st.task = Some(AsyncComputeTaskPool::get().spawn(async move { run_sim(sim, sh, g, picks) }));
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = AsyncComputeTaskPool::get;
        let out = run_sim(sim, shared.clone(), g, picks);
        absorb(&mut st, &shared, &mut rnd, out);
    }
}

fn absorb(st: &mut SandState, shared: &SandShared, rnd: &mut SandRender, (sim, stats, chunks): SimOut) {
    let n2 = (sim.reg.n * sim.reg.n) as f64;
    let p = st.params.unwrap();
    let ds = stats.picks as f64 / (p.picks_per_cell_per_s() * n2);
    st.total_s += ds;
    st.total_events += stats.events;
    st.total_avalanche += stats.avalanche_moves;
    st.total_disturb += stats.disturb_moves;
    let a = 0.1f32;
    st.sim_ms_avg += (stats.ms - st.sim_ms_avg) * a;
    st.publish_ms_avg += (stats.publish_ms - st.publish_ms_avg) * a;
    st.stats = stats;
    let rate_now = if st.last_dt > 0.0 { ds / st.last_dt } else { 0.0 };
    st.rate_actual += (rate_now.min(1e12) - st.rate_actual) * 0.1;
    // the wind wipes out tracks in proportion to the sand it moves
    st.rut_decay_acc += stats.picks as f64 / n2;
    if st.rut_decay_acc > 0.02 {
        let f = RUT_DECAY_PER_EVENT.powf(st.rut_decay_acc) as f32;
        st.rut_decay_acc = 0.0;
        if let Ok(mut ru) = shared.ruts.write() {
            if !ru.patches.is_empty() {
                ru.fade(f);
            }
        }
    }
    for k in chunks {
        rnd.dirty.insert(k);
        rnd.touched.insert(k);
        rnd.overlay_chunk_dirty.insert(k);
    }
    st.sim = Some(sim);
}

// ---------------------------------------------------------------------------------------------
// rendering: dirty chunk rebuilds and rut overlays

type ChunkOut = ((usize, usize), usize, Option<(u8, usize)>, Option<Mesh>);

#[derive(Resource, Default)]
pub struct SandRender {
    overlays: HashMap<(i32, i32), (Entity, Handle<Mesh>)>,
    dirty: HashSet<(usize, usize)>,
    touched: HashSet<(usize, usize)>,
    overlay_chunk_dirty: HashSet<(usize, usize)>,
    #[cfg(not(target_arch = "wasm32"))]
    tasks: Vec<Task<ChunkOut>>,
    pub chunk_rebuilds: u64,
    pub overlay_builds: u64,
}

/// What a chunk is currently drawn as: (step, snap) for `chunk_mesh`.
fn chunk_kind(render: &TerrainRender, k: (usize, usize)) -> Option<(usize, Option<(u8, usize)>)> {
    if let Some(f) = render.fine.get(&k) {
        return Some((1, Some((f.2, render.step))));
    }
    render.coarse.contains_key(&k).then_some((render.step, None))
}

fn install(render: &TerrainRender, meshes: &mut Assets<Mesh>, mesh3d: &Query<&Mesh3d>, out: ChunkOut) -> bool {
    let (k, step, snap, mesh) = out;
    let Some(mesh) = mesh else { return true };
    if chunk_kind(render, k) != Some((step, snap)) {
        return false; // the fine window moved meanwhile
    }
    let handle = match render.fine.get(&k) {
        Some(f) => f.1.clone(),
        None => match render.coarse.get(&k).and_then(|e| mesh3d.get(*e).ok()) {
            Some(m) => m.0.clone(),
            None => return true,
        },
    };
    let _ = meshes.insert(&handle, mesh);
    true
}

fn rebuild_now(site: &Site, render: &TerrainRender, meshes: &mut Assets<Mesh>, k: (usize, usize), rnd: &mut SandRender) {
    let Some((step, snap)) = chunk_kind(render, k) else { return };
    let mesh = terrain::chunk_mesh(site, k.0, k.1, step, render.tile_step_px, snap);
    if let Some(mesh) = mesh {
        let handle = match render.fine.get(&k) {
            Some(f) => Some(f.1.clone()),
            None => None,
        };
        if let Some(h) = handle {
            let _ = meshes.insert(&h, mesh);
            rnd.chunk_rebuilds += 1;
        } else {
            // coarse: handled by the async path (needs the Mesh3d query)
            rnd.dirty.insert(k);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sand_meshes(
    mut commands: Commands,
    site: Option<Res<Site>>,
    buggy: Option<Res<Buggy>>,
    render: Option<Res<TerrainRender>>,
    mut rnd: ResMut<SandRender>,
    mut st: ResMut<SandState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mesh3d: Query<&Mesh3d>,
    mut vis: Query<&mut Visibility>,
) {
    let (Some(site), Some(buggy), Some(render)) = (site, buggy, render) else { return };
    let t0 = Instant::now();
    let g = Geom::of(&site);
    let (br, bc) = g.rc(Vec2::new(buggy.pos.x, buggy.pos.z));
    let here = ((br / g.chunk as f32) as i64, (bc / g.chunk as f32) as i64);
    let dist = |k: &(usize, usize)| (k.0 as i64 - here.0).abs().max((k.1 as i64 - here.1).abs());

    // rut overlays: new ones right away (their terrain quads are about to be cut), then updates
    if let Some(shared) = &site.sand {
        let (new, mut dirty, pp) = {
            let mut ru = shared.ruts.write().unwrap();
            let new = std::mem::take(&mut ru.new_active);
            let dirty: Vec<(i32, i32)> = ru.dirty.drain().collect();
            (new, dirty, ru.pp)
        };
        // patches over chunks the dunes changed need new heights too
        if !rnd.overlay_chunk_dirty.is_empty() {
            let cd = std::mem::take(&mut rnd.overlay_chunk_dirty);
            for k in rnd.overlays.keys() {
                let ck = ((k.0.max(0) as usize * pp) / g.chunk, (k.1.max(0) as usize * pp) / g.chunk);
                if cd.contains(&ck) {
                    dirty.push(*k);
                }
            }
        }
        let ru = shared.ruts.read().unwrap();
        for k in &new {
            if rnd.overlays.contains_key(k) {
                continue;
            }
            if let Some(mesh) = overlay_mesh(&site, &ru, *k, render.tile_step_px) {
                let tile = ((k.0 as usize * pp) / render.tile_step_px, (k.1 as usize * pp) / render.tile_step_px);
                if let Some(mat) = render.materials.get(&tile) {
                    let h = meshes.add(mesh);
                    let e = commands.spawn((Mesh3d(h.clone()), MeshMaterial3d(mat.clone()), Transform::default(), Visibility::Hidden)).id();
                    rnd.overlays.insert(*k, (e, h));
                    rnd.overlay_builds += 1;
                }
            }
            let ck = ((k.0.max(0) as usize * pp) / g.chunk, (k.1.max(0) as usize * pp) / g.chunk);
            rnd.dirty.insert(ck);
            rnd.touched.insert(ck);
        }
        // nearest dirty overlays first, within the budget; the rest stay dirty
        let me = (br * ru.s as f32 / ru.n as f32, bc * ru.s as f32 / ru.n as f32);
        dirty.sort_by_key(|k| ((k.0 as f32 - me.0).abs() + (k.1 as f32 - me.1).abs()) as i64);
        dirty.dedup();
        let mut left = Vec::new();
        for k in dirty {
            if !rnd.overlays.contains_key(&k) {
                continue;
            }
            if t0.elapsed().as_secs_f32() * 1000.0 > MESH_BUDGET_MS {
                left.push(k);
                continue;
            }
            if let Some(mesh) = overlay_mesh(&site, &ru, k, render.tile_step_px) {
                let h = rnd.overlays[&k].1.clone();
                let _ = meshes.insert(&h, mesh);
                rnd.overlay_builds += 1;
            }
        }
        drop(ru);
        if !left.is_empty() {
            shared.ruts.write().unwrap().dirty.extend(left);
        }
        // an overlay shows where its terrain chunk is drawn at full resolution (holes cut)
        for (k, (e, _)) in &rnd.overlays {
            let ck = ((k.0.max(0) as usize * pp) / g.chunk, (k.1.max(0) as usize * pp) / g.chunk);
            let show = render.step == 1 || render.fine.contains_key(&ck);
            if let Ok(mut v) = vis.get_mut(*e) {
                let want = if show { Visibility::Inherited } else { Visibility::Hidden };
                if *v != want {
                    *v = want;
                }
            }
        }
    }

    // dirty terrain chunks
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut i = 0;
        while i < rnd.tasks.len() {
            if let Some(out) = check_ready(&mut rnd.tasks[i]) {
                drop(rnd.tasks.swap_remove(i));
                let k = out.0;
                if install(&render, &mut meshes, &mesh3d, out) {
                    rnd.chunk_rebuilds += 1;
                } else {
                    rnd.dirty.insert(k);
                }
            } else {
                i += 1;
            }
        }
        if rnd.tasks.len() < MAX_CHUNK_TASKS && !rnd.dirty.is_empty() {
            let mut list: Vec<_> = rnd.dirty.iter().copied().collect();
            list.sort_by_key(dist);
            let pool = AsyncComputeTaskPool::get();
            for k in list.into_iter().take(MAX_CHUNK_TASKS - rnd.tasks.len()) {
                rnd.dirty.remove(&k);
                let Some((step, snap)) = chunk_kind(&render, k) else { continue };
                let (site, tsp) = (site.clone(), render.tile_step_px);
                rnd.tasks.push(pool.spawn(async move { (k, step, snap, terrain::chunk_mesh(&site, k.0, k.1, step, tsp, snap)) }));
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut list: Vec<_> = rnd.dirty.iter().copied().collect();
        list.sort_by_key(dist);
        for k in list {
            if t0.elapsed().as_secs_f32() * 1000.0 > MESH_BUDGET_MS {
                break;
            }
            rnd.dirty.remove(&k);
            let Some((step, snap)) = chunk_kind(&render, k) else { continue };
            let out = (k, step, snap, terrain::chunk_mesh(&site, k.0, k.1, step, render.tile_step_px, snap));
            if install(&render, &mut meshes, &mesh3d, out) {
                rnd.chunk_rebuilds += 1;
            } else {
                rnd.dirty.insert(k);
            }
        }
    }
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    st.main_ms_avg += (ms - st.main_ms_avg) * 0.1;
}

/// Overlay mesh of rut patch `k`: the full-res surface plus ruts on the fine grid. Edges sit
/// exactly on the terrain's own triangles (the fine quads tile them), so there are no cracks.
fn overlay_mesh(site: &Site, ru: &Ruts, k: (i32, i32), tile_step_px: usize) -> Option<Mesh> {
    if k.0 < 0 || k.1 < 0 {
        return None;
    }
    let (s, n) = (ru.s, ru.n);
    let m = n + 1;
    let (r0, c0) = (k.0 as usize * ru.pp, k.1 as usize * ru.pp);
    let (fr0, fc0) = ((r0 * s) as i64, (c0 * s) as i64);
    let (tr0, tc0) = ((r0 / tile_step_px) * tile_step_px, (c0 / tile_step_px) * tile_step_px);
    let g = Geom::of(site);
    let sf = s as f32;
    let fs = g.mpp / sf;
    // heights with a 1-vertex margin for the rut gradient
    let mut pos = Vec::with_capacity(m * m);
    let mut nrm = Vec::with_capacity(m * m);
    let mut uv = Vec::with_capacity(m * m);
    let mut ok = Vec::with_capacity(m * m);
    for i in 0..m {
        for j in 0..m {
            let (rr, cc) = ((fr0 + i as i64) as f32 / sf, (fc0 + j as i64) as f32 / sf);
            let base = tri_rc(site, rr, cc, false);
            let (fr, fc) = (fr0 + i as i64, fc0 + j as i64);
            let rut = ru.get(fr, fc);
            let w = g.world(rr, cc);
            ok.push(base.is_some());
            let y = base.unwrap_or(0.0) + rut;
            pos.push([w.x, y, w.y]);
            // smooth dune-surface slope (1-post central differences, like the terrain mesh)
            // plus the rut's own slope on the fine grid
            let hs = |dr: f32, dc: f32| tri_rc(site, rr + dr, cc + dc, false);
            let (gx, gz) = match (hs(0.0, 1.0), hs(0.0, -1.0), hs(1.0, 0.0), hs(-1.0, 0.0)) {
                (Some(e), Some(w), Some(s2), Some(n2)) => ((e - w) / (2.0 * g.mpp), (s2 - n2) / (2.0 * g.mpp)),
                _ => (0.0, 0.0),
            };
            let rgx = (ru.get(fr, fc + 1) - ru.get(fr, fc - 1)) / (2.0 * fs);
            let rgz = (ru.get(fr + 1, fc) - ru.get(fr - 1, fc)) / (2.0 * fs);
            nrm.push(Vec3::new(-(gx + rgx), 1.0, -(gz + rgz)).normalize().to_array());
            uv.push([(cc - tc0 as f32) / tile_step_px as f32, (rr - tr0 as f32) / tile_step_px as f32]);
        }
    }
    let mut idx = Vec::new();
    for i in 0..m - 1 {
        for j in 0..m - 1 {
            let (a, b, c, d) = (i * m + j, (i + 1) * m + j, (i + 1) * m + j + 1, i * m + j + 1);
            if ok[a] && ok[b] && ok[c] && ok[d] {
                idx.extend_from_slice(&[a as u32, b as u32, d as u32, b as u32, c as u32, d as u32]);
            }
        }
    }
    if idx.is_empty() {
        return None;
    }
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};
    Some(
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
            .with_inserted_indices(Indices::U32(idx)),
    )
}

// ---------------------------------------------------------------------------------------------
// HUD, log, test dumps

fn fmt_span(s: f64) -> String {
    if s < 120.0 {
        format!("{s:.0} s")
    } else if s < 2.0 * 3600.0 {
        format!("{:.1} min", s / 60.0)
    } else if s < 2.0 * SOL_S {
        format!("{:.1} h", s / 3600.0)
    } else if s < 0.5 * MARS_YEAR_S {
        format!("{:.1} sols", s / SOL_S)
    } else {
        format!("{:.2} Mars yr", s / MARS_YEAR_S)
    }
}

#[allow(clippy::too_many_arguments)]
fn sand_report(
    mut commands: Commands,
    args: Res<SandArgs>,
    mut st: ResMut<SandState>,
    rnd: Res<SandRender>,
    site: Option<Res<Site>>,
    buggy: Option<Res<Buggy>>,
    time: Res<Time>,
    mut hud: ResMut<SandHud>,
) {
    let (Some(site), Some(buggy)) = (site, buggy) else { return };
    let sand_mode = buggy.tuning.mode == Mode::Sand;
    let Some(shared) = site.sand.clone() else {
        hud.0.clear();
        return;
    };
    let changed = shared.changed.load(Relaxed);
    let w = *lock(&shared.wheels);
    if !sand_mode {
        hud.0 = if changed { "SIMULATED - not NASA data: the sand was moved in REAL SAND mode (K = back to the real DTM)".into() } else { String::new() };
    } else {
        let wind = st.wind.map_or("?".into(), |u| format!("toward {:.0} deg (from {:.0})", compass(u), (compass(u) + 180.0) % 360.0));
        let mut s = format!(
            "time-lapse [ ]: {}  (actual {})   simulated {}   wind {} , .\n",
            st.levels[st.level].0,
            if st.rate_actual > 0.0 { format!("{}/s", fmt_span(st.rate_actual)) } else { "-".into() },
            fmt_span(st.total_s),
            wind
        );
        {
            s += &format!(
                "sinkage {:.0} {:.0} {:.0} {:.0} cm   rolling res +{:.2}   grip x{:.2}   sand {:.1} m",
                w[0].sink * 100.0,
                w[1].sink * 100.0,
                w[2].sink * 100.0,
                w[3].sink * 100.0,
                w.iter().map(|w| w.crr).sum::<f32>() / 4.0,
                w.iter().map(|w| w.grip).sum::<f32>() / 4.0,
                w.iter().map(|w| w.thick).sum::<f32>() / 4.0
            );
        }
        if st.bogged {
            s += "   BOGGED DOWN - ease off, then back out along your ruts";
        }
        if changed {
            s += "\nSIMULATED - not NASA data (K = back to the real DTM)";
        }
        hud.0 = s;
    }
    let now = time.elapsed_secs();
    let t = now - st.start.unwrap_or(now);
    if args.log > 0.0 && t >= st.next_log && sand_mode {
        st.next_log = t + args.log;
        let nan = st.sim.as_ref().map_or(0, |s| s.h.iter().zip(&s.reg.h0).filter(|(h, h0)| h0.is_finite() && !h.is_finite()).count());
        let (patches, active) = shared.ruts.read().map_or((0, 0), |r| (r.patches.len(), r.active.len()));
        info!(
            "SAND t={t:5.1} sim={} rate={} actual={}/s events={} avalanche_moves={} disturb_moves={} sim_ms={:.2} publish_ms={:.2} main_ms={:.2} chunk_rebuilds={} dirty={} overlays={} rut_patches={}/{} sink_cm=[{:.1} {:.1} {:.1} {:.1}] fresh_cm=[{:.1} {:.1} {:.1} {:.1}] crr=[{:.2} {:.2} {:.2} {:.2}] grip=[{:.2} {:.2} {:.2} {:.2}] slip=[{:.2} {:.2} {:.2} {:.2}] sand_m={:.1} slope={:.0} bogged={} bog_count={} nan={nan} changed={changed}",
            fmt_span(st.total_s).replace(' ', ""),
            st.levels[st.level].0.replace(' ', ""),
            fmt_span(st.rate_actual).replace(' ', ""),
            st.total_events,
            st.total_avalanche,
            st.total_disturb,
            st.sim_ms_avg,
            st.publish_ms_avg,
            st.main_ms_avg,
            rnd.chunk_rebuilds,
            rnd.dirty.len(),
            rnd.overlays.len(),
            patches,
            active,
            w[0].sink * 100.0, w[1].sink * 100.0, w[2].sink * 100.0, w[3].sink * 100.0,
            w[0].fresh * 100.0, w[1].fresh * 100.0, w[2].fresh * 100.0, w[3].fresh * 100.0,
            w[0].crr, w[1].crr, w[2].crr, w[3].crr,
            w[0].grip, w[1].grip, w[2].grip, w[3].grip,
            w[0].slip, w[1].slip, w[2].slip, w[3].slip,
            w.iter().map(|w| w.thick).sum::<f32>() / 4.0,
            w.iter().map(|w| w.slope).fold(0.0, f32::max),
            st.bogged,
            st.bog_count,
        );
    }
    // test dumps
    for i in 0..args.shots.len() {
        let (name, at) = &args.shots[i];
        if !st.shots_done[i] && t >= *at {
            st.shots_done[i] = true;
            let path = format!("{name}.png");
            let p2 = path.clone();
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path)).observe(move |_: On<bevy::render::view::screenshot::ScreenshotCaptured>| {
                info!("SAND screenshot saved to {p2}");
            });
            st.dumps_pending.push(name.clone());
        }
    }
}

/// Before/after stats of the current region, plus (natively) hillshade and difference images in
/// the wind frame (wind blows left -> right, north is wherever the wind says).
fn dump(st: &SandState, name: &str) {
    let Some(sim) = &st.sim else { return };
    let reg = &sim.reg;
    let n = reg.n;
    let m = sim.p.taper;
    let (mut changed, mut sum_abs, mut dmax, mut dmin, mut vol, mut nan, mut cnt) = (0usize, 0.0f64, 0.0f32, 0.0f32, 0.0f64, 0usize, 0usize);
    for i in 0..n * n {
        let (h, h0) = (sim.h[i], reg.h0[i]);
        if !h0.is_finite() {
            continue;
        }
        if !h.is_finite() {
            nan += 1;
            continue;
        }
        let d = h - h0;
        cnt += 1;
        if d.abs() > 0.05 {
            changed += 1;
        }
        sum_abs += d.abs() as f64;
        dmax = dmax.max(d);
        dmin = dmin.min(d);
        vol += d as f64 * (reg.dx * reg.dx) as f64;
    }
    let t0: Vec<f32> = reg.h0.iter().zip(&reg.base).map(|(h, b)| h - b).collect();
    let t1 = sim.thickness();
    let (shift, c_best, c0) = sand_sim::migration_shift(&t0, &t1, n, m + 8, 40);
    let years = sim.sim_s / MARS_YEAR_S;
    let mig = shift * reg.dx;
    info!(
        "SAND DUMP {name}: region sim time {} ({:.2} Mars yr), events {}, cells changed >5cm {:.1}% ({changed}/{cnt}), mean |dh| {:.3} m, dh min {:.2} max {:.2} m, net volume {:+.1} m3 (should be ~0), NaN {nan}; migration downwind {:+.2} m ({:+.2} m/Mars yr, {:+.2} m/Earth yr) by cross-correlation (r={:.4} vs {:.4} unshifted)",
        fmt_span(sim.sim_s), years, sim.events, 100.0 * changed as f32 / cnt.max(1) as f32, sum_abs / cnt.max(1) as f64, dmin, dmax, vol, mig,
        if years > 0.0 { mig as f64 / years } else { 0.0 },
        if years > 0.0 { mig as f64 / (sim.sim_s / sand_sim::EARTH_YEAR_S) } else { 0.0 },
        c_best, c0
    );
    #[cfg(not(target_arch = "wasm32"))]
    {
        let shade = |h: &[f32], i: usize| -> f32 {
            let (a, b) = (i % n, i / n);
            if a == 0 || b == 0 || a + 1 >= n || b + 1 >= n || !h[i].is_finite() {
                return 0.0;
            }
            let gx = (h[i + 1] - h[i - 1]) / (2.0 * reg.dx);
            let gy = (h[i + n] - h[i - n]) / (2.0 * reg.dx);
            let nn = Vec3::new(-gx, -gy, 1.0).normalize();
            let l = Vec3::new(-0.6, -0.5, 0.62).normalize();
            if !nn.is_finite() { 0.0 } else { (0.15 + 0.85 * nn.dot(l).max(0.0)).min(1.0) }
        };
        let write = |path: String, px: &dyn Fn(usize) -> [u8; 3]| {
            let mut data = Vec::with_capacity(n * n * 3);
            for i in 0..n * n {
                data.extend_from_slice(&px(i));
            }
            let res = std::fs::File::create(&path).map_err(|e| e.to_string()).and_then(|f| {
                let mut enc = png::Encoder::new(std::io::BufWriter::new(f), n as u32, n as u32);
                enc.set_color(png::ColorType::Rgb);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().and_then(|mut w| w.write_image_data(&data)).map_err(|e| e.to_string())
            });
            match res {
                Ok(()) => info!("SAND wrote {path}"),
                Err(e) => warn!("SAND {path}: {e}"),
            }
        };
        let g8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
        write(format!("{name}_before.png"), &|i| [g8(shade(&reg.h0, i)); 3]);
        write(format!("{name}_after.png"), &|i| [g8(shade(&sim.h, i)); 3]);
        write(format!("{name}_diff.png"), &|i| {
            let base = 0.55 + 0.45 * shade(&sim.h, i);
            let d = sim.h[i] - reg.h0[i];
            if !d.is_finite() {
                return [0, 0, 0];
            }
            let t = (d / 2.0).clamp(-1.0, 1.0);
            let (r, g, b) = if t > 0.0 { (1.0, 1.0 - t, 1.0 - t) } else { (1.0 + t, 1.0 + t, 1.0) };
            [g8(base * r), g8(base * g), g8(base * b)]
        });
    }
}
