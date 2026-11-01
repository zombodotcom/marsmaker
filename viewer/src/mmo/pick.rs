//! What's under the cursor in MARSSCAPE, and the `clicktest` hook that measures it.
//!
//! Every node / creature / astronaut gets a screen rectangle each frame: the bounds of its meshes
//! (cached in the root's local space; astronauts have a fixed `PickBox`) projected at its render
//! position. Targets hidden behind
//! terrain are dropped. `clicktest` clicks (virtually) at the centre, inside and around each
//! visible target and logs how often the left-click default picks the thing you clicked on.

use super::cave::Gnd;
use super::grid::tile_of;
use super::{Scape, anim};
use crate::terrain::Site;
use bevy::camera::primitives::Aabb;
use bevy::prelude::*;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Target {
    Npc(u32),
    Player(u32),
    Node(u32),
}

/// A target's screen rectangle this frame.
#[derive(Clone, Copy)]
pub struct Shown {
    pub target: Target,
    pub min: Vec2,
    pub max: Vec2,
    /// distance from the camera to the target's centre, and its bounds' half-diagonal
    pub depth: f32,
    pub reach: f32,
    /// where it stands, and how far it reaches sideways (hover ring size)
    pub base: Vec3,
    pub foot: f32,
}

impl Shown {
    pub fn centre(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }
    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }
    /// Behind a dune from here: the ground is hit well before the thing's middle. Only checked for
    /// what's near the cursor (a ray march per thing per frame would be too slow in the browser).
    pub fn hidden(&self, camera: &Camera, cgt: &GlobalTransform, g: Gnd) -> bool {
        camera.viewport_to_world(cgt, self.centre()).is_ok_and(|ray| ray_ground_t(g, ray).is_some_and(|t| t < self.depth - self.reach - 1.0))
    }
    pub fn contains(&self, p: Vec2, pad: f32) -> bool {
        p.x >= self.min.x - pad && p.y >= self.min.y - pad && p.x <= self.max.x + pad && p.y <= self.max.y + pad
    }
}

/// Screen rectangles of everything clickable, rebuilt every frame by `rects`.
#[derive(Resource, Default)]
pub struct PickRects {
    pub shown: Vec<Shown>,
    /// local-space bounds per root entity (+ whether a node was up when measured)
    boxes: HashMap<Entity, (Vec3, Vec3, bool)>,
}

fn local_box(root: Entity, kids: &Query<&Children>, parts: &Query<(&GlobalTransform, &Aabb)>, gts: &Query<&GlobalTransform>) -> Option<(Vec3, Vec3)> {
    let inv = gts.get(root).ok()?.affine().inverse();
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut any = false;
    for d in std::iter::once(root).chain(kids.iter_descendants(root)) {
        let Ok((gt, b)) = parts.get(d) else { continue };
        let (c, h) = (Vec3::from(b.center), Vec3::from(b.half_extents));
        for i in 0..8 {
            let s = Vec3::new(if i & 1 == 0 { -1.0 } else { 1.0 }, if i & 2 == 0 { -1.0 } else { 1.0 }, if i & 4 == 0 { -1.0 } else { 1.0 });
            let p = inv.transform_point3(gt.transform_point(c + h * s));
            lo = lo.min(p);
            hi = hi.max(p);
            any = true;
        }
    }
    any.then_some((lo, hi))
}

/// Ray-march to the ground: first crossing below the terrain, refined by bisection.
pub fn ray_ground(g: Gnd, ray: Ray3d) -> Option<Vec3> {
    ray_ground_t(g, ray).map(|t| ray.get_point(t))
}

pub fn ray_ground_t(g: Gnd, ray: Ray3d) -> Option<f32> {
    let mut t = 0.0f32;
    let mut prev = 0.0;
    loop {
        // fine steps near the camera, coarser far away (still well under a tile up close)
        t += 0.2 + t * 0.004;
        if t > 1500.0 {
            return None;
        }
        let p = ray.get_point(t);
        if g.h(p.x, p.z).is_some_and(|h| p.y <= h) {
            let (mut a, mut b) = (prev, t);
            for _ in 0..14 {
                let m = 0.5 * (a + b);
                let q = ray.get_point(m);
                if g.h(q.x, q.z).is_some_and(|h| q.y <= h) {
                    b = m;
                } else {
                    a = m;
                }
            }
            return Some(b);
        }
        prev = t;
    }
}

/// Rebuild the screen rectangles (runs before input each frame).
#[allow(clippy::too_many_arguments)]
pub fn rects(
    scape: Option<Res<Scape>>,
    mut pr: ResMut<PickRects>,
    cam: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    kids: Query<&Children>,
    parts: Query<(&GlobalTransform, &Aabb)>,
    gts: Query<&GlobalTransform>,
    fixed: Query<&super::look::PickBox>,
) {
    pr.shown.clear();
    let (Some(sc), Ok((camera, cgt))) = (scape, cam.single()) else { return };
    let eye = cgt.translation();
    let mut list: Vec<(Target, Entity, bool)> = Vec::with_capacity(sc.nodes.len() + sc.npcs.len() + sc.players.len());
    for (id, a) in &sc.npcs {
        if a.anim != anim::DEAD {
            list.push((Target::Npc(*id), a.e, true));
        }
    }
    for (id, a) in &sc.players {
        if Some(*id) != sc.me {
            list.push((Target::Player(*id), a.e, true));
        }
    }
    for (id, n) in &sc.nodes {
        list.push((Target::Node(*id), n.e, n.up));
    }
    let PickRects { shown, boxes } = &mut *pr;
    // forget despawned things now and then
    if boxes.len() > list.len() * 2 + 64 {
        let live: std::collections::HashSet<Entity> = list.iter().map(|l| l.1).collect();
        boxes.retain(|e, _| live.contains(e));
    }
    for (target, e, up) in list {
        let Ok(gt) = gts.get(e) else { continue };
        // far beyond what anyone clicks on: skip the projection work
        if gt.translation().distance_squared(eye) > 400.0 * 400.0 {
            continue;
        }
        // characters have a fixed box; their skinned meshes' bounds move with every pose
        let cached = fixed.get(e).ok().map(|b| (b.0, b.1)).or_else(|| boxes.get(&e).filter(|b| b.2 == up).map(|b| (b.0, b.1)));
        let Some((lo, hi)) = cached.or_else(|| {
            let b = local_box(e, &kids, &parts, &gts)?;
            boxes.insert(e, (b.0, b.1, up));
            Some(b)
        }) else {
            continue;
        };
        let (mut smin, mut smax) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        let mut ok = true;
        for i in 0..8 {
            let p = Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
            match camera.world_to_viewport(cgt, gt.transform_point(p)) {
                Ok(s) => {
                    smin = smin.min(s);
                    smax = smax.max(s);
                }
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        // entirely off screen
        if let Some(vp) = camera.logical_viewport_rect() {
            if smax.x < vp.min.x || smax.y < vp.min.y || smin.x > vp.max.x || smin.y > vp.max.y {
                continue;
            }
        }
        let depth = gt.transform_point((lo + hi) * 0.5).distance(eye);
        let ext = (hi - lo) * 0.5 * gt.scale();
        shown.push(Shown { target, min: smin, max: smax, depth, reach: ext.length(), base: gt.translation(), foot: ext.x.max(ext.z) });
    }
}

/// Everything under the cursor, best first. Being inside a thing's drawn rectangle beats being
/// near it; then creatures beat astronauts beat rocks / stations (rifts last, they're mostly
/// empty space); among equals the one in front wins. Small or far things get a few pixels of
/// slack so they stay clickable.
pub fn under(pr: &PickRects, sc: &Scape, p: Vec2, cam: (&Camera, &GlobalTransform), g: Gnd) -> Vec<Target> {
    let mut v: Vec<(bool, u8, f32, Target)> = Vec::new();
    for s in &pr.shown {
        let pad = (12.0 - s.size().max_element() * 0.2).clamp(2.0, 8.0);
        if !s.contains(p, pad) || s.hidden(cam.0, cam.1, g) {
            continue;
        }
        let inside = s.contains(p, 0.0);
        let class = match s.target {
            Target::Npc(_) => 0,
            Target::Player(_) => 1,
            Target::Node(id) if sc.nodes.get(&id).is_some_and(|n| n.kind == "rift") => 3,
            Target::Node(_) => 2,
        };
        let key = if inside { s.depth } else { (s.min - p).max(p - s.max).max(Vec2::ZERO).length() };
        v.push((!inside, class, key, s.target));
    }
    v.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then(a.2.total_cmp(&b.2)));
    v.into_iter().map(|x| x.3).collect()
}

/// Put the ring under what a left click would act on.
pub fn hover_ring(
    mut commands: Commands,
    st: Res<super::ui::UiState>,
    pr: Res<PickRects>,
    time: Res<Time>,
    mut ring: Query<(Entity, &super::look::HoverRing, &mut Transform, &mut Visibility, &mut MeshMaterial3d<StandardMaterial>)>,
    mut cx_parts: (ResMut<Assets<Mesh>>, ResMut<Assets<StandardMaterial>>, ResMut<super::look::Kit>),
) {
    let Ok((_, r, mut t, mut vis, mut mat)) = ring.single_mut() else {
        let (meshes, mats, kit) = &mut cx_parts;
        super::look::hover_ring(&mut super::look::Ctx { commands: &mut commands, meshes, mats, kit });
        return;
    };
    let shown = st.hover_target.and_then(|h| pr.shown.iter().find(|s| s.target == h));
    let Some(s) = shown else {
        *vis = Visibility::Hidden;
        return;
    };
    *vis = Visibility::Visible;
    let k = match s.target {
        Target::Node(_) => 0,
        Target::Npc(_) => 1,
        Target::Player(_) => 2,
    };
    if mat.0 != r.mats[k] {
        mat.0 = r.mats[k].clone();
    }
    let pulse = 1.0 + 0.06 * (time.elapsed_secs() * 6.0).sin();
    let rad = (s.foot * 1.15 + 0.25).clamp(0.5, 4.0) * pulse;
    t.translation = s.base + Vec3::Y * 0.12;
    t.scale = Vec3::new(rad, 0.6, rad);
}

// ------------------------------------------------------------------------------------ clicktest

/// Set by the script step `clicktest:LABEL`; `click_test` runs it next frame.
#[derive(Resource, Default)]
pub struct ClickTest(pub Option<String>);

/// What a left click at `p` does: a target, the ground, or nothing.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Picked {
    T(Target),
    Ground,
    Nothing,
}

fn class(sc: &Scape, t: Target) -> &'static str {
    match t {
        Target::Npc(_) => "creature",
        Target::Player(id) => {
            if sc.players.get(&id).is_some_and(|a| a.role.is_some()) {
                "colonist"
            } else {
                "player"
            }
        }
        Target::Node(id) => match sc.nodes.get(&id).map(|n| n.kind.as_str()) {
            Some("fabricator") => "fabricator",
            Some("rift") => "rift",
            Some("locker" | "pit" | "ladder") => "station",
            _ => "rock",
        },
    }
}

#[derive(Default, Clone, Copy)]
struct Tally {
    n: u32,
    hit: u32,
}

impl Tally {
    fn add(&mut self, ok: bool) {
        self.n += 1;
        self.hit += ok as u32;
    }
    fn pct(&self) -> String {
        if self.n == 0 { "-".into() } else { format!("{:.0}% ({}/{})", 100.0 * self.hit as f32 / self.n as f32, self.hit, self.n) }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn click_test(
    mut req: ResMut<ClickTest>,
    scape: Option<Res<Scape>>,
    site: Option<Res<Site>>,
    pr: Res<PickRects>,
    cam: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    win: Query<&Window, With<bevy::window::PrimaryWindow>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &InheritedVisibility), With<super::ui::Blocker>>,
) {
    let Some(label) = req.0.take() else { return };
    let (Some(sc), Some(site), Ok((camera, cgt)), Ok(w)) = (scape, site, cam.single(), win.single()) else { return };
    let g = sc.gnd(&site);
    let scale = w.scale_factor();
    let (ww, wh) = (w.width(), w.height());
    let panels: Vec<(ComputedNode, UiGlobalTransform)> = blockers.iter().filter(|b| b.2.get()).map(|b| (*b.0, *b.1)).collect();
    let usable = |p: Vec2| p.x > 2.0 && p.y > 2.0 && p.x < ww - 2.0 && p.y < wh - 2.0 && !panels.iter().any(|(n, t)| n.contains_point(*t, p * scale));
    let pick = |p: Vec2| super::left_pick(&sc, (camera, cgt), p, g, &pr);
    let mut by: HashMap<&'static str, [Tally; 4]> = HashMap::new(); // centre, inside, ±10 px, ±20 px
    let mut overlap: HashMap<String, u32> = HashMap::new();
    let (mut near_grab, mut near_walk, mut near_n) = (0u32, 0u32, 0u32);
    let mut sizes: HashMap<&'static str, Vec<f32>> = HashMap::new();
    let shown: Vec<&Shown> = pr.shown.iter().filter(|s| !s.hidden(camera, cgt, g)).collect();
    for s in shown.iter().copied() {
        let c = s.centre();
        if !usable(c) {
            continue;
        }
        let k = class(&sc, s.target);
        sizes.entry(k).or_default().push(s.size().max_element());
        let others = |p: Vec2| shown.iter().any(|o| o.target != s.target && o.contains(p, 0.0));
        let mut test = |slot: usize, p: Vec2| {
            if !usable(p) || !s.contains(p, 0.0) {
                return;
            }
            let got = pick(p);
            if others(p) {
                let what = match got {
                    Picked::T(t) if t == s.target => "self".to_string(),
                    Picked::T(t) => class(&sc, t).to_string(),
                    Picked::Ground => "ground".into(),
                    Picked::Nothing => "nothing".into(),
                };
                *overlap.entry(format!("{k}->{what}")).or_default() += 1;
                return;
            }
            // astronauts aren't a left-click target (a click there walks up to them): they count
            // as hit when the right-click menu offers them
            let ok = match s.target {
                Target::Player(_) => super::options_at(&sc, (camera, cgt), p, g, &pr).iter().any(|o| o.target == Some(s.target)),
                t => got == Picked::T(t),
            };
            by.entry(k).or_default()[slot].add(ok);
        };
        test(0, c);
        let h = s.size() * 0.25;
        for (dx, dy) in [(-1.0, -1.0), (0.0, -1.0), (1.0, -1.0), (-1.0, 0.0), (1.0, 0.0), (-1.0, 1.0), (0.0, 1.0), (1.0, 1.0)] {
            test(1, c + Vec2::new(dx * h.x, dy * h.y));
        }
        for (slot, r) in [(2usize, 10.0f32), (3, 20.0)] {
            for d in [Vec2::X, -Vec2::X, Vec2::Y, -Vec2::Y] {
                test(slot, c + d * r);
            }
        }
        // just outside the drawn thing: should walk there (unless something else is there)
        for d in [Vec2::X, -Vec2::X, Vec2::Y, -Vec2::Y] {
            let p = c + d * (s.size() * 0.5 + Vec2::splat(10.0));
            if !usable(p) || shown.iter().any(|o| o.contains(p, 4.0)) {
                continue;
            }
            near_n += 1;
            match pick(p) {
                Picked::T(_) => near_grab += 1,
                Picked::Ground => near_walk += 1,
                Picked::Nothing => {}
            }
        }
    }
    // walking: does the clicked ground point land where the cursor is?
    let mut px_err: Vec<f32> = Vec::new();
    let mut tile_wrong = 0u32;
    for j in 0..12 {
        for i in 0..16 {
            let p = Vec2::new((i as f32 + 0.5) * ww / 16.0, (j as f32 + 0.5) * wh / 12.0);
            if !usable(p) || shown.iter().any(|o| o.contains(p, 30.0)) {
                continue;
            }
            let Ok(ray) = camera.viewport_to_world(cgt, p) else { continue };
            let Some(gp) = super::ground_pick(g, ray) else { continue };
            if let Ok(back) = camera.world_to_viewport(cgt, gp) {
                px_err.push(back.distance(p));
            }
            // reference: very fine march
            let mut t = 0.0f32;
            while t < 1500.0 {
                t += 0.02;
                let q = ray.get_point(t);
                if g.h(q.x, q.z).is_some_and(|h| q.y <= h) {
                    if tile_of(q.x, q.z) != tile_of(gp.x, gp.z) {
                        tile_wrong += 1;
                    }
                    break;
                }
            }
        }
    }
    px_err.sort_by(|a, b| a.total_cmp(b));
    let q = |f: f32| px_err.get(((px_err.len() as f32 - 1.0) * f).round() as usize).copied().unwrap_or(0.0);
    info!("mmo-clicktest [{label}] targets on screen: {} ({} behind terrain)", shown.len(), pr.shown.len() - shown.len());
    let mut keys: Vec<_> = by.keys().copied().collect();
    keys.sort();
    for k in keys {
        let t = by[k];
        let sz = sizes.get(k).map(|v| {
            let mut v = v.clone();
            v.sort_by(|a, b| a.total_cmp(b));
            format!("{:.0}..{:.0}px", v[0], v[v.len() - 1])
        });
        info!("mmo-clicktest [{label}] {k:10} size {} centre {} inside {} off10 {} off20 {}", sz.unwrap_or_default(), t[0].pct(), t[1].pct(), t[2].pct(), t[3].pct());
    }
    let mut ov: Vec<_> = overlap.into_iter().collect();
    ov.sort();
    info!("mmo-clicktest [{label}] overlaps (drawn under X -> picked): {ov:?}");
    info!("mmo-clicktest [{label}] just outside a target: {near_n} clicks, {near_walk} walk, {near_grab} grabbed a target");
    info!("mmo-clicktest [{label}] ground clicks: {} samples, reprojection error p50 {:.1}px p95 {:.1}px max {:.1}px, wrong tile {}", px_err.len(), q(0.5), q(0.95), q(1.0), tile_wrong);
}

