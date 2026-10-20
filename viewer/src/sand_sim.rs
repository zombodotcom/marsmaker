//! REAL SAND mode, part 1: a Werner (1995) slab cellular automaton for moving dunes.
//!
//! Werner, B. T. (1995), "Eolian dunes: computer simulations and attractor interpretation",
//! Geology 23(12), 1107-1110. Sand is a stack of slabs of height `hs` on a grid. Repeatedly:
//! pick a random cell; if it has sand and is not in the wind shadow of an upwind crest, lift one
//! slab and carry it `hop` cells downwind; it settles with probability `p_sand` (0.6) on sand or
//! `p_bare` (0.4) on bare ground, always in a shadow zone, else it hops again. After every
//! erosion / deposition the neighbourhood is relaxed to the angle of repose (avalanching).
//! Shadow zones (15 deg behind crests) are what turn piles into dunes with slip faces.
//!
//! Here the slab heights ride on the real DTM: each cell's surface starts as the HiRISE height, a
//! "bedrock" floor is estimated under it (a smoothed lower envelope of the DTM, see `base_from`),
//! and only the sand between floor and surface can move. Heights are floats (slab = `hs` metres).
//! The grid is rotated so its +a axis points downwind, so the hop and the shadow test are along
//! grid rows exactly as in Werner's model, for any wind direction.
//!
//! Time: Werner's model has no clock. It's calibrated so that the bulk sand flux over saturated
//! flat sand is `flux` m^3 per metre width per Earth year: each pick moves `hs * dx^2` of sand a
//! mean distance `D = hop * dx / p_sand`, so with R picks per cell per second the flux is
//! `R * hs * D` (per unit width). A dune of height H then migrates at about `flux / H`.
//! Observed on Mars: dune migration ~0.1-1 m per Earth year and bulk sand fluxes of a few up to
//! ~10-20 m^3/m/yr (Bridges et al. 2012, Nature 485, 339, "Earth-like sand fluxes on Mars";
//! Chojnacki et al. 2019, Geology 47, 427, mean migration ~0.5 m/yr for active dune fields).
//! The default 5 m^3/m/yr moves a 10 m dune ~0.5 m per Earth year (~0.9 m per Mars year).
//!
//! Everything here is plain CPU code on flat arrays (no Bevy systems), so it runs in a native
//! background task or, in the browser (WebGL2, no compute shaders), inline with a time budget.

use bevy::math::Vec2;
use bevy::platform::time::Instant;
use std::sync::Arc;

pub const EARTH_YEAR_S: f64 = 365.25 * 86400.0;
/// one Mars year = 686.98 Earth days
pub const MARS_YEAR_S: f64 = 686.98 * 86400.0;
pub const SOL_S: f64 = 88775.244;
/// sim block edge in cells (dirty tracking / publishing unit)
pub const BLOCK: usize = 16;
/// furthest upwind distance checked for a shadow (cells)
const MAX_SHADOW: usize = 64;
const MAX_HOPS: usize = 200;
const MAX_RELAX: usize = 400;

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// cell size (m)
    pub dx: f32,
    /// slab thickness (m)
    pub hs: f32,
    /// saltation hop length (cells)
    pub hop: usize,
    pub p_sand: f32,
    pub p_bare: f32,
    /// tan of the shadow-zone angle (15 deg)
    pub tan_shadow: f32,
    /// tan of the angle of repose used for avalanching (30 deg; Mars sand ~32-34, lower = stabler)
    pub tan_repose: f32,
    /// slopes disturbed by a wheel fail at this lower angle (27 deg)
    pub tan_disturbed: f32,
    /// saturated sand flux, m^3 per m width per Earth year
    pub flux: f32,
    /// cells at the region edge over which erosion / deposition fade out (fixed boundary)
    pub taper: usize,
}

impl Params {
    pub fn new(dx: f32, flux: f32) -> Self {
        Self {
            dx,
            hs: 0.2,
            hop: 3,
            p_sand: 0.6,
            p_bare: 0.4,
            tan_shadow: 15f32.to_radians().tan(),
            tan_repose: 30f32.to_radians().tan(),
            tan_disturbed: 27f32.to_radians().tan(),
            flux,
            taper: 24,
        }
    }
    /// Werner picks per cell per simulated second for the requested flux (see the module doc).
    pub fn picks_per_cell_per_s(&self) -> f64 {
        let d = self.hop as f64 * self.dx as f64 / self.p_sand as f64;
        self.flux as f64 / (self.hs as f64 * d) / EARTH_YEAR_S
    }
}

/// The simulated square: geometry (rotated into the wind) and the fixed per-cell layers.
pub struct Region {
    pub n: usize,
    pub dx: f32,
    /// world (x, z) of cell (a=0, b=0); cell (a, b) is at origin + u*a*dx + v*b*dx
    pub origin: Vec2,
    /// unit downwind direction (world x, z) and the crosswind axis
    pub u: Vec2,
    pub v: Vec2,
    /// estimated sand floor (world Y), NaN = no data
    pub base: Vec<f32>,
    /// surface when the region was built (world Y)
    pub h0: Vec<f32>,
    /// post-space delta snapshot at build time: post-block index -> 64x64 values
    pub snap: std::collections::HashMap<usize, Box<[f32]>>,
}

impl Region {
    /// world (x, z) -> fractional cell (a, b)
    pub fn cell_of(&self, p: Vec2) -> Vec2 {
        let d = p - self.origin;
        Vec2::new(d.dot(self.u), d.dot(self.v)) / self.dx
    }
    pub fn world(&self, a: f32, b: f32) -> Vec2 {
        self.origin + self.u * (a * self.dx) + self.v * (b * self.dx)
    }
    /// Bilinear sample of a per-cell layer at fractional (a, b); None outside or over NaN.
    pub fn sample(&self, g: &[f32], a: f32, b: f32) -> Option<f32> {
        let n = self.n;
        if !(a >= 0.0 && b >= 0.0 && a <= (n - 1) as f32 && b <= (n - 1) as f32) {
            return None;
        }
        let (a0, b0) = ((a as usize).min(n - 2), (b as usize).min(n - 2));
        let (fa, fb) = (a - a0 as f32, b - b0 as f32);
        let i = b0 * n + a0;
        let (p, q, r, s) = (g[i], g[i + 1], g[i + n], g[i + n + 1]);
        let v = (p * (1.0 - fa) + q * fa) * (1.0 - fb) + (r * (1.0 - fa) + s * fa) * fb;
        v.is_finite().then_some(v)
    }
    pub fn centre(&self) -> Vec2 {
        self.world((self.n - 1) as f32 * 0.5, (self.n - 1) as f32 * 0.5)
    }
}

/// Sand floor from the raw DTM on the region grid: a lower envelope (moving minimum over
/// +-`r` cells along both grid axes) smoothed by two box blurs of +-`r/2`, never above the DTM.
/// Interdune troughs and the plain around a dune field become the floor; dunes are what sticks
/// up out of it. Hills of bedrock are mistaken for sand too (see the README caveats).
pub fn base_from(raw: &[f32], n: usize, r: usize) -> Vec<f32> {
    let inf = |v: f32| if v.is_finite() { v } else { f32::INFINITY };
    let mut a: Vec<f32> = raw.iter().map(|&v| inf(v)).collect();
    // separable moving minimum (monotone deque), rows then columns
    let mut line = vec![0.0f32; n];
    let mut out = vec![0.0f32; n];
    let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut movmin = |line: &[f32], out: &mut [f32]| {
        dq.clear();
        let mut next = 0usize;
        for i in 0..n {
            let hi = (i + r).min(n - 1);
            while next <= hi {
                while dq.back().is_some_and(|&j| line[j] >= line[next]) {
                    dq.pop_back();
                }
                dq.push_back(next);
                next += 1;
            }
            while dq.front().is_some_and(|&j| j + r < i) {
                dq.pop_front();
            }
            out[i] = line[*dq.front().unwrap()];
        }
    };
    for b in 0..n {
        line.copy_from_slice(&a[b * n..(b + 1) * n]);
        movmin(&line, &mut out);
        a[b * n..(b + 1) * n].copy_from_slice(&out);
    }
    for c in 0..n {
        for b in 0..n {
            line[b] = a[b * n + c];
        }
        movmin(&line, &mut out);
        for b in 0..n {
            a[b * n + c] = out[b];
        }
    }
    // box blur (finite values only), twice
    let rb = (r / 2).max(1);
    for _ in 0..2 {
        for pass in 0..2 {
            let mut res = vec![f32::INFINITY; n * n];
            for k in 0..n {
                let get = |i: usize| if pass == 0 { a[k * n + i] } else { a[i * n + k] };
                let (mut s, mut cnt) = (0.0f64, 0u32);
                let mut add = |v: f32, sign: f64, s: &mut f64, cnt: &mut u32| {
                    if v.is_finite() {
                        *s += v as f64 * sign;
                        if sign > 0.0 { *cnt += 1 } else { *cnt -= 1 }
                    }
                };
                for i in 0..rb.min(n) {
                    add(get(i), 1.0, &mut s, &mut cnt);
                }
                for i in 0..n {
                    if i + rb < n {
                        add(get(i + rb), 1.0, &mut s, &mut cnt);
                    }
                    if i > rb {
                        add(get(i - rb - 1), -1.0, &mut s, &mut cnt);
                    }
                    let v = if cnt > 0 { (s / cnt as f64) as f32 } else { f32::INFINITY };
                    if pass == 0 { res[k * n + i] = v } else { res[i * n + k] = v }
                }
            }
            a = res;
        }
    }
    a.iter().zip(raw).map(|(&b, &z)| if z.is_finite() && b.is_finite() { b.min(z) } else if z.is_finite() { z } else { f32::NAN }).collect()
}

/// Run statistics returned to the main thread.
#[derive(Default, Clone, Copy)]
pub struct RunStats {
    pub picks: u64,
    pub events: u64,
    pub avalanche_moves: u64,
    pub disturb_moves: u64,
    pub ms: f32,
    pub publish_ms: f32,
    pub published_blocks: u32,
}

pub struct Sim {
    pub reg: Arc<Region>,
    pub p: Params,
    /// current surface per cell (world Y), NaN = no data
    pub h: Vec<f32>,
    rng: u64,
    pub picks: u64,
    pub events: u64,
    pub avalanche_moves: u64,
    /// simulated seconds since this region was built (the caller keeps the grand total)
    pub sim_s: f64,
    acc: f64,
    hmax: f32,
    nb: usize,
    dirty: Vec<bool>,
    pub dirty_list: Vec<u32>,
    stack: Vec<u32>,
}

impl Sim {
    pub fn new(reg: Arc<Region>, p: Params, seed: u64) -> Self {
        let h = reg.h0.clone();
        let hmax = h.iter().copied().filter(|v| v.is_finite()).fold(f32::NEG_INFINITY, f32::max);
        let nb = reg.n.div_ceil(BLOCK);
        Self {
            reg,
            p,
            h,
            rng: seed | 1,
            picks: 0,
            events: 0,
            avalanche_moves: 0,
            sim_s: 0.0,
            acc: 0.0,
            hmax,
            nb,
            dirty: vec![false; nb * nb],
            dirty_list: Vec::new(),
            stack: Vec::new(),
        }
    }

    #[inline]
    fn rand(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    #[inline]
    fn unit(&mut self) -> f32 {
        (self.rand() >> 40) as f32 / (1u64 << 24) as f32
    }

    #[inline]
    fn mark(&mut self, i: usize) {
        let n = self.reg.n;
        let k = (i / n / BLOCK) * self.nb + (i % n) / BLOCK;
        if !self.dirty[k] {
            self.dirty[k] = true;
            self.dirty_list.push(k as u32);
        }
    }
    pub fn take_block(&mut self) -> Option<usize> {
        let k = self.dirty_list.pop()? as usize;
        self.dirty[k] = false;
        Some(k)
    }
    pub fn blocks_per_edge(&self) -> usize {
        self.nb
    }

    /// 1 inside, fading to 0 at the region edge (the edge stays the real DTM)
    #[inline]
    fn edge_w(&self, a: usize, b: usize) -> f32 {
        let n = self.reg.n;
        let d = a.min(b).min(n - 1 - a).min(n - 1 - b) as f32 / self.p.taper as f32;
        let t = d.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    #[inline]
    fn thick(&self, i: usize) -> f32 {
        self.h[i] - self.reg.base[i]
    }

    /// In the wind shadow of something upwind (higher than a 15 deg line back from this cell)?
    fn shadowed(&self, a: usize, b: usize) -> bool {
        let n = self.reg.n;
        let row = b * n;
        let h = self.h[row + a];
        if !h.is_finite() {
            return false;
        }
        let step = self.p.dx * self.p.tan_shadow;
        let reach = ((self.hmax - h) / step).ceil().max(0.0) as usize;
        let maxd = reach.min(MAX_SHADOW).min(a);
        let mut lim = 0.0;
        for d in 1..=maxd {
            lim += step;
            if self.h[row + a - d] - h > lim {
                return true;
            }
        }
        false
    }

    /// Avalanche: relax slopes steeper than `tan` around cell `start` (moving only sand, never
    /// the floor). Returns the number of slab moves.
    fn relax(&mut self, start: usize, tan: f32) -> u64 {
        let n = self.reg.n;
        let (lim_o, lim_d) = (tan * self.p.dx, tan * self.p.dx * std::f32::consts::SQRT_2);
        self.stack.clear();
        self.stack.push(start as u32);
        let push_nbrs = |stack: &mut Vec<u32>, i: usize| {
            let (a, b) = (i % n, i / n);
            for db in -1i32..=1 {
                for da in -1i32..=1 {
                    let (aa, bb) = (a as i32 + da, b as i32 + db);
                    if (da != 0 || db != 0) && aa >= 0 && bb >= 0 && (aa as usize) < n && (bb as usize) < n {
                        stack.push((bb as usize * n + aa as usize) as u32);
                    }
                }
            }
        };
        push_nbrs(&mut self.stack, start);
        let mut moves = 0;
        let mut pops = 0;
        while let Some(c) = self.stack.pop() {
            pops += 1;
            if pops > MAX_RELAX {
                break;
            }
            let c = c as usize;
            let hc = self.h[c];
            if !hc.is_finite() {
                continue;
            }
            let avail = self.thick(c);
            if avail <= 1e-4 {
                continue;
            }
            let (a, b) = (c % n, c / n);
            let mut best = (0.0f32, usize::MAX);
            for db in -1i32..=1 {
                for da in -1i32..=1 {
                    if da == 0 && db == 0 {
                        continue;
                    }
                    let (aa, bb) = (a as i32 + da, b as i32 + db);
                    if aa < 0 || bb < 0 || aa as usize >= n || bb as usize >= n {
                        continue;
                    }
                    let j = bb as usize * n + aa as usize;
                    let hj = self.h[j];
                    if !hj.is_finite() {
                        continue;
                    }
                    let lim = if da != 0 && db != 0 { lim_d } else { lim_o };
                    let ex = hc - hj - lim;
                    if ex > best.0 {
                        best = (ex, j);
                    }
                }
            }
            if best.1 == usize::MAX || best.0 < 0.01 {
                continue;
            }
            let m = (best.0 * 0.5).min(avail);
            if m <= 1e-4 {
                continue;
            }
            let j = best.1;
            self.h[c] -= m;
            self.h[j] += m;
            self.hmax = self.hmax.max(self.h[j]);
            self.mark(c);
            self.mark(j);
            moves += 1;
            self.stack.push(j as u32);
            push_nbrs(&mut self.stack, c);
        }
        moves
    }

    /// Wheel disturbance at world (x, z): let the slope around it fail at the lower angle.
    pub fn disturb(&mut self, p: Vec2) -> u64 {
        let c = self.reg.cell_of(p);
        let n = self.reg.n;
        let (a, b) = (c.x.round(), c.y.round());
        if a < 1.0 || b < 1.0 || a >= (n - 1) as f32 || b >= (n - 1) as f32 {
            return 0;
        }
        let i = b as usize * n + a as usize;
        let tan = self.p.tan_disturbed;
        self.relax(i, tan)
    }

    /// Advance by `picks_wanted` Werner picks (fractional ones carry over), within `budget_ms`.
    /// Returns the picks actually done; `sim_s` advances accordingly.
    pub fn step(&mut self, picks_wanted: f64, budget_ms: f32, stats: &mut RunStats) {
        let t0 = Instant::now();
        let n = self.reg.n;
        let nn = (n * n) as u64;
        self.acc += picks_wanted;
        let rate = self.p.picks_per_cell_per_s() * nn as f64;
        let mut done = 0u64;
        'outer: while self.acc >= 1.0 {
            let batch = (self.acc as u64).min(256);
            for _ in 0..batch {
                let i = (self.rand() % nn) as usize;
                done += 1;
                self.pick(i, n, stats);
            }
            self.acc -= batch as f64;
            if t0.elapsed().as_secs_f32() * 1000.0 > budget_ms {
                // out of time: drop the backlog (the sim runs slower than asked)
                self.acc = self.acc.fract();
                break 'outer;
            }
        }
        self.picks += done;
        stats.picks += done;
        self.sim_s += done as f64 / rate;
        stats.ms += t0.elapsed().as_secs_f32() * 1000.0;
    }

    fn pick(&mut self, i: usize, n: usize, stats: &mut RunStats) {
        let hi = self.h[i];
        if !hi.is_finite() {
            return;
        }
        let th = self.thick(i);
        if th < 0.5 * self.p.hs {
            return; // bare floor: nothing to blow away
        }
        let (a, b) = (i % n, i / n);
        let w = self.edge_w(a, b);
        if w < 1.0 && self.unit() >= w {
            return;
        }
        if self.shadowed(a, b) {
            return;
        }
        let e = self.p.hs.min(th);
        self.h[i] -= e;
        self.mark(i);
        let mv = self.relax(i, self.p.tan_repose);
        // carry it downwind (periodic along the wind, same crosswind row)
        let mut aa = a;
        let mut placed = false;
        for hop in 0..MAX_HOPS {
            aa = (aa + self.p.hop) % n;
            let j = b * n + aa;
            let hj = self.h[j];
            if !hj.is_finite() {
                continue;
            }
            let pd = if self.shadowed(aa, b) {
                1.0
            } else if hj - self.reg.base[j] > 0.5 * self.p.hs {
                self.p.p_sand
            } else {
                self.p.p_bare
            };
            let w2 = self.edge_w(aa, b);
            if hop + 1 == MAX_HOPS || self.unit() < pd * w2 {
                self.h[j] += e;
                self.hmax = self.hmax.max(self.h[j]);
                self.mark(j);
                let mv2 = self.relax(j, self.p.tan_repose);
                stats.avalanche_moves += mv2;
                self.avalanche_moves += mv2;
                placed = true;
                break;
            }
        }
        if !placed {
            self.h[i] += e; // nowhere to land (all nodata downwind): put it back
        }
        stats.avalanche_moves += mv;
        self.avalanche_moves += mv;
        stats.events += 1;
        self.events += 1;
    }

    /// Thickness of sand per cell (current), NaN where no data.
    pub fn thickness(&self) -> Vec<f32> {
        self.h.iter().zip(&self.reg.base).map(|(h, b)| h - b).collect()
    }
}

/// Dominant downwind direction from slip faces: posts sloping 24-42 deg (lee faces at or near
/// the angle of repose face downwind; stoss slopes are gentler), each voting with its unit
/// downslope direction. `grad` yields (dh/dx, dh/dz) samples. Returns (unit downwind (x, z),
/// coherence 0..1, votes).
pub fn slipface_wind(grads: impl Iterator<Item = Vec2>) -> (Vec2, f32, usize) {
    let (lo, hi) = (24f32.to_radians().tan(), 42f32.to_radians().tan());
    let mut sum = Vec2::ZERO;
    let mut n = 0usize;
    for g in grads {
        let s = g.length();
        if s > lo && s < hi {
            sum += -g / s;
            n += 1;
        }
    }
    if n == 0 {
        return (Vec2::X, 0.0, 0);
    }
    let coh = sum.length() / n as f32;
    (sum.normalize_or(Vec2::X), coh, n)
}

/// Migration estimate: shift (cells, + = downwind) that best aligns `before` with `after`
/// (thickness maps), by normalised cross-correlation over the interior, with a parabolic
/// sub-cell refinement. Returns (shift, correlation at best, correlation at 0).
pub fn migration_shift(before: &[f32], after: &[f32], n: usize, margin: usize, max_shift: i32) -> (f32, f32, f32) {
    let corr = |s: i32| -> f32 {
        let (mut sxy, mut sxx, mut syy, mut sx, mut sy, mut k) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for b in margin..n - margin {
            for a in margin..n - margin {
                let a2 = a as i32 + s;
                if a2 < margin as i32 || a2 >= (n - margin) as i32 {
                    continue;
                }
                let x = before[b * n + a];
                let y = after[b * n + a2 as usize];
                if !(x.is_finite() && y.is_finite()) {
                    continue;
                }
                let (x, y) = (x as f64, y as f64);
                sxy += x * y;
                sxx += x * x;
                syy += y * y;
                sx += x;
                sy += y;
                k += 1.0;
            }
        }
        if k < 10.0 {
            return 0.0;
        }
        let cov = sxy / k - sx / k * sy / k;
        let vx = sxx / k - (sx / k).powi(2);
        let vy = syy / k - (sy / k).powi(2);
        (cov / (vx * vy).sqrt().max(1e-12)) as f32
    };
    let mut best = (0i32, f32::NEG_INFINITY);
    let mut cs = Vec::new();
    for s in -max_shift..=max_shift {
        let c = corr(s);
        cs.push(c);
        if c > best.1 {
            best = (s, c);
        }
    }
    let i = (best.0 + max_shift) as usize;
    let mut sub = best.0 as f32;
    if i > 0 && i + 1 < cs.len() {
        let (l, m, r) = (cs[i - 1], cs[i], cs[i + 1]);
        let d = l - 2.0 * m + r;
        if d.abs() > 1e-9 {
            sub += (0.5 * (l - r) / d).clamp(-0.5, 0.5);
        }
    }
    (sub, best.1, cs[max_shift as usize])
}
