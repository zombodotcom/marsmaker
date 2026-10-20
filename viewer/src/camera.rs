//! Terrain-aware chase camera: sits behind the buggy, tilts with the slope it is driving on (low
//! and looking up a dune face it climbs, high and looking down one it descends), climbs over
//! (or, past a limit, pulls in in front of) terrain that would hide it, and never goes into the
//! ground. Plain code with no
//! ECS so `--bench` can score it on the same drives as the physics.

use crate::terrain::Site;
use bevy::prelude::*;

/// The camera aims this far above the buggy's centre.
pub const LOOK_Y: f32 = 1.0;
/// clearance of the view line over the terrain near the buggy (m) ...
const LINE_MARGIN: f32 = 0.5;
/// ... growing to this at the camera end
const CAM_MARGIN: f32 = 1.2;
/// steepest view from above (rise / run, ~58 deg) before the camera pulls in instead of climbing
const MAX_ELEV: f32 = 1.6;
/// closest the camera pulls in (m)
const MIN_DIST: f32 = 3.0;
/// hard floor: camera height over the highest ground around it, so the near plane stays clear (m)
const FLOOR: f32 = 0.9;
/// the view line is not checked this close to the buggy (its own body / wheels)
const SKIP: f32 = 1.5;
/// smoothing rates (1/s): pulling in / climbing is fast so the view never clips, easing back slow
const FAST: f32 = 14.0;
const SLOW: f32 = 1.5;
/// how quickly the camera swings round behind the buggy (1/s)
const FOLLOW: f32 = 5.0;
/// share of the ground pitch along the heading the camera tilts with, and how quickly (1/s)
const TILT: f32 = 0.7;
const TILT_RATE: f32 = 3.0;
/// the ground pitch is measured between points this far ahead of / behind the buggy (m)
const PITCH_BASE: f32 = 6.0;

#[derive(Default)]
pub struct ChaseCam {
    pos: Option<Vec3>,
    last_look: Vec3,
    /// smoothed fraction of the wanted distance in use (< 1 = pulled in)
    reach: f32,
    /// smoothed camera elevation over the look point (rise / run)
    elev: f32,
    /// smoothed ground pitch along the heading (radians, + = uphill ahead)
    pitch: f32,
}

impl ChaseCam {
    /// Next camera position for a buggy at `buggy_pos` facing `forward`; `dist` / `height` are
    /// the wanted distance behind and height above the buggy (zoom already applied).
    pub fn update(&mut self, site: &Site, buggy_pos: Vec3, forward: Vec3, dist: f32, height: f32, dt: f32) -> Vec3 {
        let look = buggy_pos + Vec3::Y * LOOK_Y;
        let back = -Vec3::new(forward.x, 0.0, forward.z).normalize_or(Vec3::NEG_Z);
        let dist = dist.max(0.5);
        // first frame, respawn or teleport: no smoothing
        let snap = self.pos.is_none_or(|p| p.distance(look) > dist * 3.0 + 50.0) || look.distance(self.last_look) > 20.0;
        self.last_look = look;
        // tilt the wanted view with the ground pitch (less for the far / high camera)
        let g = |s: f32| site.height(buggy_pos.x - back.x * s, buggy_pos.z - back.z * s);
        let pitch = match (g(PITCH_BASE), g(-PITCH_BASE)) {
            (Some(a), Some(b)) => ((a - b) / (2.0 * PITCH_BASE)).atan(),
            _ => 0.0,
        };
        self.pitch = if snap { pitch } else { self.pitch + (pitch - self.pitch) * (1.0 - (-dt * TILT_RATE).exp()) };
        let tilt = TILT * self.pitch * (1.0 - (dist - 15.0) / 45.0).clamp(0.3, 1.0);
        let theta = ((height - LOOK_Y).atan2(dist) - tilt).clamp(-0.35, 1.2);
        let dist = (height - LOOK_Y).hypot(dist) * theta.cos();
        let (reach, elev) = clear_view(site, look, back, dist, theta.tan());
        if snap {
            (self.reach, self.elev) = (reach, elev);
        } else {
            let k = |fast: bool| 1.0 - (-dt * if fast { FAST } else { SLOW }).exp();
            self.reach += (reach - self.reach) * k(reach < self.reach);
            self.elev += (elev - self.elev) * k(elev > self.elev);
        }
        let d = dist * self.reach;
        let want = look + back * d + Vec3::Y * (self.elev * d);
        let mut p = match self.pos {
            Some(p) if !snap => p.lerp(want, 1.0 - (-dt * FOLLOW).exp()),
            _ => want,
        };
        // hard limits on where the camera actually is (it lags `want` while swinging round):
        // above the ground around it, and a view line that clears the terrain
        p.y = p.y.max(ground_around(site, p) + FLOOR);
        let h = Vec3::new(p.x - look.x, 0.0, p.z - look.z);
        let run = h.length();
        if run > SKIP {
            let need = need_elev(site, look, h / run, run, run, 0.25, 0.25, f32::INFINITY).1;
            p.y = p.y.max(look.y + need.min(3.0) * run);
        }
        self.pos = Some(p);
        p
    }
}

/// Wanted (reach, elevation) for a camera `dist` behind the look point along `back`: the
/// lowest elevation >= `base` whose view line clears the terrain; if that is steeper than
/// MAX_ELEV, the furthest distance (>= MIN_DIST) that can keep a view at MAX_ELEV.
fn clear_view(site: &Site, look: Vec3, back: Vec3, dist: f32, base: f32) -> (f32, f32) {
    let (s_ok, need) = need_elev(site, look, back, dist, dist, LINE_MARGIN, CAM_MARGIN, MAX_ELEV);
    if s_ok >= dist {
        return (1.0, base.max(need));
    }
    let min_d = MIN_DIST.min(dist);
    if s_ok >= min_d {
        return (s_ok / dist, base.max(need));
    }
    // blocked even close in: stay at MIN_DIST and look down steeply enough to clear it
    let need = need_elev(site, look, back, min_d, min_d, LINE_MARGIN, CAM_MARGIN, f32::INFINITY).1;
    (min_d / dist, base.max(need))
}

/// Walks from the look point along the horizontal unit `dir` out to `len` and returns
/// (furthest distance reached, elevation needed so far): the elevation is the steepest
/// rise / run from `look` to (ground + margin) over the samples, the margin growing from
/// `m0` to `m1` over `span`. Stops early when the need passes `cap`.
#[allow(clippy::too_many_arguments)]
fn need_elev(site: &Site, look: Vec3, dir: Vec3, len: f32, span: f32, m0: f32, m1: f32, cap: f32) -> (f32, f32) {
    let n = ((len / 0.75) as usize).clamp(8, 64);
    let mut need = f32::NEG_INFINITY;
    for i in 1..=n {
        let s = len * i as f32 / n as f32;
        if s < SKIP {
            continue;
        }
        let p = look + dir * s;
        let Some(g) = site.height(p.x, p.z) else { continue };
        let e = (g + m0 + (m1 - m0) * (s / span).min(1.0) - look.y) / s;
        if e > cap {
            return (len * (i - 1) as f32 / n as f32, need);
        }
        need = need.max(e);
    }
    (len, need)
}

/// Highest ground under and around a camera at `p` (the near-plane footprint).
fn ground_around(site: &Site, p: Vec3) -> f32 {
    const R: f32 = 0.8;
    [(0.0, 0.0), (R, 0.0), (-R, 0.0), (0.0, R), (0.0, -R)]
        .iter()
        .filter_map(|(dx, dz)| site.height(p.x + dx, p.z + dz))
        .fold(f32::NEG_INFINITY, f32::max)
}

/// Lowest height of the straight line `a`→`b` above the terrain over `n` samples, skipping the
/// first 1.5 m at `a` (the buggy itself); negative = the terrain blocks the view.
pub fn line_clearance(site: &Site, a: Vec3, b: Vec3, n: usize) -> f32 {
    let skip = (SKIP / a.distance(b).max(1e-3)).min(1.0);
    (0..=n)
        .filter_map(|i| {
            let p = a.lerp(b, skip + (1.0 - skip) * i as f32 / n as f32);
            site.height(p.x, p.z).map(|g| p.y - g)
        })
        .fold(f32::INFINITY, f32::min)
}
