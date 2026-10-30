//! MarsScape walk grid: 2 m tiles over the site's heightfield, built lazily in 32x32 chunks.
//! A tile is walkable when the ground under it has data and its slope is at most `MAX_SLOPE_DEG`;
//! node tiles (rocks, fabricator, rifts) are blocked on top. A* over 8 neighbours (no corner
//! cutting), capped at `MAX_PATH` steps, falls back to the closest reachable tile.

use super::proto::{MAX_PATH, TILE_M};
use crate::terrain::Site;
use bevy::math::{IVec2, Vec3};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

pub const MAX_SLOPE_DEG: f32 = 30.0;
const CHUNK: i32 = 32;

#[derive(Clone, Copy)]
struct Cell {
    h: f32,
    walk: bool,
}

pub struct WalkGrid {
    site: Site,
    chunks: HashMap<IVec2, Box<[Cell]>>,
    pub blocked: HashSet<IVec2>,
    /// inclusive tile bounds of the site
    pub min: IVec2,
    pub max: IVec2,
}

pub fn tile_of(x: f32, z: f32) -> IVec2 {
    IVec2::new((x / TILE_M).floor() as i32, (z / TILE_M).floor() as i32)
}

pub fn tile_center(t: IVec2) -> (f32, f32) {
    ((t.x as f32 + 0.5) * TILE_M, (t.y as f32 + 0.5) * TILE_M)
}

impl WalkGrid {
    pub fn new(site: &Site) -> Self {
        let x0 = site.east0;
        let x1 = site.east0 + (site.w as f32 - 1.0) * site.mpp;
        let z0 = -site.north0;
        let z1 = -site.north0 + (site.h as f32 - 1.0) * site.mpp;
        WalkGrid { site: site.clone(), chunks: HashMap::new(), blocked: HashSet::new(), min: tile_of(x0, z0), max: tile_of(x1, z1) }
    }

    fn cell(&mut self, t: IVec2) -> Cell {
        if t.x < self.min.x || t.y < self.min.y || t.x > self.max.x || t.y > self.max.y {
            return Cell { h: f32::NAN, walk: false };
        }
        let c = IVec2::new(t.x.div_euclid(CHUNK), t.y.div_euclid(CHUNK));
        let site = &self.site;
        let chunk = self.chunks.entry(c).or_insert_with(|| {
            let mut v = Vec::with_capacity((CHUNK * CHUNK) as usize);
            for j in 0..CHUNK {
                for i in 0..CHUNK {
                    let (x, z) = tile_center(IVec2::new(c.x * CHUNK + i, c.y * CHUNK + j));
                    v.push(build_cell(site, x, z));
                }
            }
            v.into_boxed_slice()
        });
        chunk[(t.y.rem_euclid(CHUNK) * CHUNK + t.x.rem_euclid(CHUNK)) as usize]
    }

    /// Ground height at the tile centre (NaN = nodata).
    pub fn height(&mut self, t: IVec2) -> f32 {
        self.cell(t).h
    }

    pub fn walkable(&mut self, t: IVec2) -> bool {
        self.cell(t).walk && !self.blocked.contains(&t)
    }

    /// Terrain-only walkability (ignores node blocks).
    pub fn ground_ok(&mut self, t: IVec2) -> bool {
        self.cell(t).walk
    }

    /// World point on the ground at a tile centre.
    pub fn world(&mut self, t: IVec2) -> Option<Vec3> {
        let (x, z) = tile_center(t);
        self.site.height(x, z).map(|h| Vec3::new(x, h, z))
    }

    fn step_ok(&mut self, a: IVec2, d: IVec2) -> bool {
        let b = a + d;
        if !self.walkable(b) {
            return false;
        }
        if d.x != 0 && d.y != 0 {
            // no cutting corners past blocked tiles
            return self.walkable(IVec2::new(a.x + d.x, a.y)) && self.walkable(IVec2::new(a.x, a.y + d.y));
        }
        true
    }

    /// Path from `from` (excluded) to the first tile satisfying `goal`, searching within
    /// `MAX_PATH` tiles of `from`. If no goal tile is reachable, the path goes to the explored
    /// tile closest to `target`. Empty = already there (or nowhere better to go).
    pub fn find_path(&mut self, from: IVec2, target: IVec2, goal: impl Fn(IVec2) -> bool) -> Vec<IVec2> {
        if goal(from) {
            return Vec::new();
        }
        let h = |t: IVec2| {
            let d = (t - target).abs();
            (d.x.max(d.y) * 10 + d.x.min(d.y) * 4) as u32
        };
        let mut open = BinaryHeap::new();
        let mut came: HashMap<IVec2, IVec2> = HashMap::new();
        let mut cost: HashMap<IVec2, u32> = HashMap::new();
        cost.insert(from, 0);
        open.push(Reverse((h(from), 0u32, from.x, from.y)));
        let (mut best, mut best_h) = (from, h(from));
        let mut found = None;
        let mut expanded = 0;
        while let Some(Reverse((_, g, x, y))) = open.pop() {
            let t = IVec2::new(x, y);
            if cost.get(&t).is_some_and(|&c| c < g) {
                continue;
            }
            if goal(t) {
                found = Some(t);
                break;
            }
            expanded += 1;
            if expanded > 20_000 {
                break;
            }
            let ht = h(t);
            if ht < best_h || (ht == best_h && g < cost[&best]) {
                (best, best_h) = (t, ht);
            }
            for d in [IVec2::new(1, 0), IVec2::new(-1, 0), IVec2::new(0, 1), IVec2::new(0, -1), IVec2::new(1, 1), IVec2::new(1, -1), IVec2::new(-1, 1), IVec2::new(-1, -1)] {
                let n = t + d;
                let r = (n - from).abs();
                if r.x.max(r.y) > MAX_PATH as i32 {
                    continue;
                }
                // the goal tile itself may be blocked (a node): reaching its neighbour is the job of `goal`
                if !self.step_ok(t, d) {
                    continue;
                }
                let ng = g + if d.x != 0 && d.y != 0 { 14 } else { 10 };
                if cost.get(&n).is_none_or(|&c| ng < c) {
                    cost.insert(n, ng);
                    came.insert(n, t);
                    open.push(Reverse((ng + h(n), ng, n.x, n.y)));
                }
            }
        }
        let end = found.unwrap_or(best);
        let mut path = vec![end];
        let mut t = end;
        while let Some(&p) = came.get(&t) {
            if p == from {
                break;
            }
            path.push(p);
            t = p;
        }
        if end == from {
            return Vec::new();
        }
        path.reverse();
        path.truncate(MAX_PATH);
        path
    }

    /// Nearest walkable tile to `t` (spiral search), for spawns.
    pub fn nearest_walkable(&mut self, t: IVec2, radius: i32) -> Option<IVec2> {
        for r in 0..=radius {
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) != r {
                        continue;
                    }
                    let c = t + IVec2::new(dx, dz);
                    if self.walkable(c) {
                        return Some(c);
                    }
                }
            }
        }
        None
    }
}

fn build_cell(site: &Site, x: f32, z: f32) -> Cell {
    let Some(h) = site.height(x, z) else { return Cell { h: f32::NAN, walk: false } };
    let d = TILE_M * 0.5;
    let (Some(e), Some(w), Some(s), Some(n)) = (site.height(x + d, z), site.height(x - d, z), site.height(x, z + d), site.height(x, z - d)) else {
        return Cell { h, walk: false };
    };
    let g = ((e - w) / (2.0 * d)).hypot((s - n) / (2.0 * d));
    Cell { h, walk: g.atan().to_degrees() <= MAX_SLOPE_DEG }
}
