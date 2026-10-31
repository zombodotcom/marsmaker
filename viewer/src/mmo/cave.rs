//! The lava tube under a site: its wall map (from the server's `welcome.cave` run lengths, or
//! generated offline with a port of the server's generator), the ground it gives the rest of the
//! mode, and its geometry.
//!
//! The rock is one heightfield over the whole tube, built from the same floor/wall grid the path
//! finder uses: flat floor on floor tiles, rock rising steeply across the tile edge into the wall
//! tiles. Each tile's wall value is blended with its neighbours before contouring, so straight walls
//! stay on the tile edges and corners come out round. Rock far from any floor is left out (it's
//! black down there anyway).

use super::proto::TILE_M;
use crate::terrain::Site;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

pub const CAVE_W: usize = 120;
pub const CAVE_H: usize = 120;
/// rock walls this tall (plus bumps on top)
pub const WALL_H: f32 = 2.6;
/// heightfield samples per tile side
const SUB: usize = 3;

pub struct CaveMap {
    pub w: usize,
    pub h: usize,
    /// 1 = floor, 0 = wall; index = tz * w + tx
    pub grid: Vec<u8>,
    /// wall-ness per tile, blended with its neighbours (0 floor .. 1 rock)
    field: Vec<f32>,
    /// floor height (the tube sits far below the site's terrain)
    pub y0: f32,
}

/// The ground under the mode: the site's terrain, or the tube's rock.
#[derive(Clone, Copy)]
pub enum Gnd<'a> {
    Site(&'a Site),
    Cave(&'a CaveMap),
}

impl Gnd<'_> {
    /// Solid surface height (cave: rock walls included), for clicks and the camera.
    pub fn h(&self, x: f32, z: f32) -> Option<f32> {
        match self {
            Gnd::Site(s) => s.height(x, z),
            Gnd::Cave(c) => Some(c.solid_h(x, z)),
        }
    }
    /// Where feet go.
    pub fn floor(&self, x: f32, z: f32) -> Option<f32> {
        match self {
            Gnd::Site(s) => s.height(x, z),
            Gnd::Cave(c) => Some(c.y0),
        }
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Cheap smooth noise in about -1..1 (rock bumps).
fn noise(x: f32, z: f32) -> f32 {
    0.5 * (x * 1.7 + (z * 1.3).cos()).sin() + 0.3 * (z * 2.3 + x * 0.7).sin() + 0.2 * ((x + z) * 3.1).sin()
}

impl CaveMap {
    pub fn from_grid(w: usize, h: usize, grid: Vec<u8>, y0: f32) -> Self {
        let mut field = vec![1.0f32; w * h];
        let wall = |x: i32, z: i32| -> f32 {
            if x < 0 || z < 0 || x >= w as i32 || z >= h as i32 { 1.0 } else if grid[z as usize * w + x as usize] == 1 { 0.0 } else { 1.0 }
        };
        for z in 0..h as i32 {
            for x in 0..w as i32 {
                let mut nb = 0.0;
                for (dx, dz) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    nb += wall(x + dx, z + dz);
                }
                field[z as usize * w + x as usize] = 0.5 * wall(x, z) + 0.5 * nb / 8.0;
            }
        }
        // a second, wider blur rounds the stair-steps of diagonal walls off; then floor centres are
        // kept low and wall centres high, so the contour can't cross a tile it doesn't belong to
        let f0 = field.clone();
        let at = |x: i32, z: i32| if x < 0 || z < 0 || x >= w as i32 || z >= h as i32 { 1.0 } else { f0[z as usize * w + x as usize] };
        for z in 0..h as i32 {
            for x in 0..w as i32 {
                let mut acc = 0.0;
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        let k = [0.25, 0.5, 0.25][(dx + 1) as usize] * [0.25, 0.5, 0.25][(dz + 1) as usize];
                        acc += k * at(x + dx, z + dz);
                    }
                }
                let i = z as usize * w + x as usize;
                field[i] = if grid[i] == 1 { acc.min(0.36) } else { acc.max(0.64) };
            }
        }
        CaveMap { w, h, grid, field, y0 }
    }

    /// From the server's run lengths (wall, floor, wall, ... starting with a wall run).
    pub fn from_rle(w: usize, h: usize, rle: &str, y0: f32) -> Option<Self> {
        let mut g = vec![0u8; w * h];
        let (mut i, mut v) = (0usize, 0u8);
        for n in rle.split(',') {
            let n: usize = n.trim().parse().ok()?;
            if i + n > w * h {
                return None;
            }
            g[i..i + n].fill(v);
            i += n;
            v ^= 1;
        }
        (i == w * h).then(|| Self::from_grid(w, h, g, y0))
    }

    pub fn floor_at(&self, tx: i32, tz: i32) -> bool {
        tx >= 0 && tz >= 0 && tx < self.w as i32 && tz < self.h as i32 && self.grid[tz as usize * self.w + tx as usize] == 1
    }

    /// Wall-ness at a world point (bilinear between tile centres).
    fn field_at(&self, x: f32, z: f32) -> f32 {
        let u = x / TILE_M - 0.5;
        let v = z / TILE_M - 0.5;
        let (i0, j0) = (u.floor() as i32, v.floor() as i32);
        let (fu, fv) = (u - i0 as f32, v - j0 as f32);
        let f = |i: i32, j: i32| if i < 0 || j < 0 || i >= self.w as i32 || j >= self.h as i32 { 1.0 } else { self.field[j as usize * self.w + i as usize] };
        let a = f(i0, j0) * (1.0 - fu) + f(i0 + 1, j0) * fu;
        let b = f(i0, j0 + 1) * (1.0 - fu) + f(i0 + 1, j0 + 1) * fu;
        a * (1.0 - fv) + b * fv
    }

    /// Height of the rock surface at a world point (floor = y0).
    pub fn solid_h(&self, x: f32, z: f32) -> f32 {
        let f = self.field_at(x, z);
        let rise = smoothstep(0.4, 0.6, f);
        let top = smoothstep(0.65, 1.0, f);
        self.y0 + rise * (WALL_H + 0.18 * noise(x * 1.3, z * 1.3)) + top * (0.9 + 0.7 * noise(x * 0.45, z * 0.45))
    }

    /// Tile centre on the floor.
    pub fn world(&self, t: IVec2) -> Vec3 {
        Vec3::new((t.x as f32 + 0.5) * TILE_M, self.y0, (t.y as f32 + 0.5) * TILE_M)
    }
}

// ------------------------------------------------------------------------------------ geometry

/// Marks the cave's geometry root (despawned when you climb out).
#[derive(Component)]
pub struct CaveGeo;

/// Floor mesh and rock mesh (two materials, same vertices) plus glowing lichen on the walls.
pub fn build_meshes(c: &CaveMap) -> (Mesh, Mesh, Mesh) {
    let (w, h) = (c.w, c.h);
    let (nx, nz) = (w * SUB + 1, h * SUB + 1);
    let step = TILE_M / SUB as f32;
    // which tiles get rock drawn: within 2 tiles of a floor tile
    let mut near = vec![false; w * h];
    for z in 0..h as i32 {
        for x in 0..w as i32 {
            if !c.floor_at(x, z) {
                continue;
            }
            for dz in -2..=2 {
                for dx in -2..=2 {
                    let (a, b) = (x + dx, z + dz);
                    if a >= 0 && b >= 0 && a < w as i32 && b < h as i32 {
                        near[b as usize * w + a as usize] = true;
                    }
                }
            }
        }
    }
    let mut pos = Vec::with_capacity(nx * nz);
    let mut hgt = Vec::with_capacity(nx * nz);
    for j in 0..nz {
        for i in 0..nx {
            let (x, z) = (i as f32 * step, j as f32 * step);
            let y = c.solid_h(x, z);
            hgt.push(y);
            pos.push([x, y, z]);
        }
    }
    let at = |i: usize, j: usize| hgt[j.min(nz - 1) * nx + i.min(nx - 1)];
    let mut nrm = Vec::with_capacity(nx * nz);
    for j in 0..nz {
        for i in 0..nx {
            let dx = at(i + 1, j) - at(i.saturating_sub(1), j);
            let dz = at(i, j + 1) - at(i, j.saturating_sub(1));
            nrm.push(Vec3::new(-dx, 2.0 * step, -dz).normalize().to_array());
        }
    }
    let uv: Vec<[f32; 2]> = pos.iter().map(|p| [p[0] * 0.1, p[2] * 0.1]).collect();
    let (mut floor_i, mut rock_i) = (Vec::new(), Vec::new());
    for j in 0..nz - 1 {
        for i in 0..nx - 1 {
            let (tx, tz) = (i / SUB, j / SUB);
            if !near[tz.min(h - 1) * w + tx.min(w - 1)] {
                continue;
            }
            let a = (j * nx + i) as u32;
            let (b, cc, d) = (a + 1, a + nx as u32, a + nx as u32 + 1);
            let flat = [a, b, cc, d].iter().all(|&k| hgt[k as usize] - c.y0 < 0.05);
            let list = if flat { &mut floor_i } else { &mut rock_i };
            list.extend_from_slice(&[a, cc, b, b, cc, d]);
        }
    }
    let mk = |idx: Vec<u32>| {
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos.clone())
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm.clone())
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv.clone())
            .with_inserted_indices(Indices::U32(idx))
    };
    // lichen: small glowing blobs low on the walls next to the floor
    let blob = Sphere::new(1.0).mesh().ico(0).unwrap();
    let bp: Vec<[f32; 3]> = match blob.attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(bevy::mesh::VertexAttributeValues::Float32x3(v)) => v.clone(),
        _ => Vec::new(),
    };
    let bi: Vec<u32> = match blob.indices() {
        Some(Indices::U32(v)) => v.clone(),
        Some(Indices::U16(v)) => v.iter().map(|&x| x as u32).collect(),
        None => Vec::new(),
    };
    let (mut lp, mut ln, mut li) = (Vec::new(), Vec::new(), Vec::new());
    let mut k = 0u32;
    for z in 0..h as i32 {
        for x in 0..w as i32 {
            if !c.floor_at(x, z) {
                continue;
            }
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                if c.floor_at(x + dx, z + dz) {
                    continue;
                }
                k = k.wrapping_mul(1103515245).wrapping_add(12345 + (x * 31 + z * 17) as u32);
                if k % 5 != 0 {
                    continue;
                }
                // on the rock face just past the tile edge
                let base = c.world(IVec2::new(x, z)) + Vec3::new(dx as f32, 0.0, dz as f32) * (TILE_M * 0.5 + 0.35);
                let y = 0.35 + (k >> 8) as f32 % 13.0 * 0.11;
                let r = 0.07 + ((k >> 4) % 5) as f32 * 0.025;
                let off = Vec3::new(((k >> 12) % 7) as f32 * 0.12 - 0.36, 0.0, ((k >> 16) % 7) as f32 * 0.12 - 0.36);
                let at = base + off + Vec3::Y * y;
                let start = lp.len() as u32;
                for p in &bp {
                    let v = Vec3::from(*p);
                    lp.push((at + v * r).to_array());
                    ln.push(v.to_array());
                }
                li.extend(bi.iter().map(|i| start + i));
            }
        }
    }
    let luv = vec![[0.0f32, 0.0]; lp.len()];
    let lichen = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, lp)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, ln)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, luv)
        .with_inserted_indices(Indices::U32(li));
    (mk(floor_i), mk(rock_i), lichen)
}

// ------------------------------------------------------------------------------------ offline generator

/// The server's string hash (FNV-1a over UTF-16 units; site ids are ASCII).
pub fn fnv1a(s: &str) -> u32 {
    let mut h = 0x811c9dc5u32;
    for c in s.encode_utf16() {
        h ^= c as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// The server's seeded generator (mulberry32), same sequence.
pub struct Mulberry(u32);

impl Mulberry {
    pub fn new(seed: u32) -> Self {
        Mulberry(seed)
    }
    pub fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        (t ^ (t >> 14)) as f64 / 4294967296.0
    }
}

/// JS Math.round
fn jround(x: f64) -> i32 {
    (x + 0.5).floor() as i32
}

/// What the offline world needs to know about a generated tube.
pub struct CaveGen {
    pub w: usize,
    pub h: usize,
    pub grid: Vec<u8>,
    pub spawn: IVec2,
    /// (kind, tile); index = node id (0 = ladder)
    pub nodes: Vec<(&'static str, IVec2)>,
    /// (kind, home)
    pub npcs: Vec<(&'static str, IVec2)>,
}

impl CaveGen {
    pub fn rle(&self) -> String {
        let mut runs = Vec::new();
        let (mut cur, mut n) = (0u8, 0u32);
        for &v in &self.grid {
            if v == cur {
                n += 1;
            } else {
                runs.push(n);
                cur = v;
                n = 1;
            }
        }
        runs.push(n);
        runs.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(",")
    }
}

/// Port of server/src/cave.ts `generateCave`: a meandering tube with branches and chambers.
pub fn generate(site: &str) -> CaveGen {
    let mut rng = Mulberry::new(fnv1a(&format!("marsscape-cave:{site}")));
    let (w, h) = (CAVE_W as i32, CAVE_H as i32);
    let mut g = vec![0u8; (w * h) as usize];
    let carve = |g: &mut Vec<u8>, cx: i32, cz: i32, r: i32| {
        let rr = (r * r) as f64 + 0.5;
        for dz in -r..=r {
            for dx in -r..=r {
                if (dx * dx + dz * dz) as f64 > rr {
                    continue;
                }
                let (x, z) = (cx + dx, cz + dz);
                if x >= 1 && z >= 1 && x < w - 1 && z < h - 1 {
                    g[(z * w + x) as usize] = 1;
                }
            }
        }
    };
    let mut tube: Vec<(i32, i32)> = Vec::new();
    let walk = |g: &mut Vec<u8>, tube: &mut Vec<(i32, i32)>, rng: &mut Mulberry, mut x: f64, mut z: f64, mut ang: f64, steps: i32| {
        for _ in 0..steps {
            ang += (rng.next() - 0.5) * 0.7;
            let (mut nx, mut nz) = (x + ang.cos(), z + ang.sin());
            if nx < 5.0 || nx > (w - 6) as f64 || nz < 5.0 || nz > (h - 6) as f64 {
                ang += std::f64::consts::PI * (0.5 + rng.next() * 0.5);
                nx = nx.clamp(5.0, (w - 6) as f64);
                nz = nz.clamp(5.0, (h - 6) as f64);
            }
            x = nx;
            z = nz;
            let (tx, tz) = (jround(x), jround(z));
            let r = if rng.next() < 0.3 { 2 } else { 1 };
            carve(g, tx, tz, r);
            tube.push((tx, tz));
        }
    };
    let entrance = (6, 50 + (rng.next() * 20.0).floor() as i32);
    carve(&mut g, entrance.0, entrance.1, 3);
    walk(&mut g, &mut tube, &mut rng, entrance.0 as f64, entrance.1 as f64, 0.0, 300);
    let branches = 3 + (rng.next() * 2.0).floor() as i32;
    for _ in 0..branches {
        let (bx, bz) = tube[(rng.next() * tube.len() as f64).floor() as usize];
        let ang = rng.next() * std::f64::consts::PI * 2.0;
        let steps = 60 + (rng.next() * 70.0).floor() as i32;
        walk(&mut g, &mut tube, &mut rng, bx as f64, bz as f64, ang, steps);
    }
    let hyp = |a: i32, b: i32| ((a * a + b * b) as f64).sqrt();
    let mut chambers: Vec<(i32, i32, i32)> = Vec::new();
    let mut tries = 0;
    while chambers.len() < 8 && tries < 200 {
        tries += 1;
        let (cx, cz) = tube[(rng.next() * tube.len() as f64).floor() as usize];
        if hyp(cx - entrance.0, cz - entrance.1) < 15.0 {
            continue;
        }
        if chambers.iter().any(|&(x, z, _)| hyp(cx - x, cz - z) < 16.0) {
            continue;
        }
        let r = 4 + (rng.next() * 3.0).floor() as i32;
        carve(&mut g, cx, cz, r);
        chambers.push((cx, cz, r));
    }
    // fill pockets, knock out lone pillars
    let mut s = g.clone();
    for z in 1..h - 1 {
        for x in 1..w - 1 {
            let mut nb = 0;
            for dz in -1..=1 {
                for dx in -1..=1 {
                    if (dx != 0 || dz != 0) && g[((z + dz) * w + x + dx) as usize] == 1 {
                        nb += 1;
                    }
                }
            }
            let i = (z * w + x) as usize;
            if g[i] == 0 && nb >= 6 {
                s[i] = 1;
            } else if g[i] == 1 && nb <= 1 {
                s[i] = 0;
            }
        }
    }
    // keep what's reachable from the entrance (no corner cutting)
    let floor_s = |x: i32, z: i32| x >= 0 && z >= 0 && x < w && z < h && s[(z * w + x) as usize] == 1;
    let open = |x: i32, z: i32, nx: i32, nz: i32| floor_s(nx, nz) && (nx == x || nz == z || (floor_s(nx, z) && floor_s(x, nz)));
    let mut grid = vec![0u8; (w * h) as usize];
    let mut q = vec![entrance.1 * w + entrance.0];
    grid[q[0] as usize] = 1;
    while let Some(i) = q.pop() {
        let (x, z) = (i % w, i / w);
        for dz in -1..=1 {
            for dx in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let j = (z + dz) * w + x + dx;
                if j < 0 || j >= w * h || grid[j as usize] == 1 || !open(x, z, x + dx, z + dz) {
                    continue;
                }
                grid[j as usize] = 1;
                q.push(j);
            }
        }
    }
    let floor_at = |x: i32, z: i32| x >= 0 && z >= 0 && x < w && z < h && grid[(z * w + x) as usize] == 1;
    let spawn = (entrance.0 + 1, entrance.1);
    let mut used: std::collections::HashSet<(i32, i32)> = [entrance, spawn].into_iter().collect();
    let mut nodes: Vec<(&'static str, IVec2)> = vec![("ladder", IVec2::new(entrance.0, entrance.1))];
    let spot = |rng: &mut Mulberry, used: &mut std::collections::HashSet<(i32, i32)>, cx: i32, cz: i32, r: i32| -> Option<(i32, i32)> {
        for _ in 0..40 {
            let x = cx + jround((rng.next() * 2.0 - 1.0) * r as f64);
            let z = cz + jround((rng.next() * 2.0 - 1.0) * r as f64);
            if floor_at(x, z) && !used.contains(&(x, z)) {
                used.insert((x, z));
                return Some((x, z));
            }
        }
        None
    };
    let mut far: Vec<(i32, i32, i32)> = chambers.iter().copied().filter(|&(x, z, _)| floor_at(x, z)).collect();
    far.sort_by(|a, b| hyp(a.0 - entrance.0, a.1 - entrance.1).total_cmp(&hyp(b.0 - entrance.0, b.1 - entrance.1)));
    let (mut veins, mut fossils) = (0, 0);
    for (k, &(cx, cz, r)) in far.iter().enumerate() {
        let mut i = 0;
        while i < 2 && veins < 12 {
            if let Some(p) = spot(&mut rng, &mut used, cx, cz, r - 1) {
                nodes.push(("void_vein", IVec2::new(p.0, p.1)));
                veins += 1;
            }
            i += 1;
        }
        let mut i = 0;
        while i < (if k % 2 == 1 { 2 } else { 1 }) && fossils < 8 {
            if let Some(p) = spot(&mut rng, &mut used, cx, cz, r - 1) {
                nodes.push(("fossil", IVec2::new(p.0, p.1)));
                fossils += 1;
            }
            i += 1;
        }
    }
    let mut npcs: Vec<(&'static str, IVec2)> = Vec::new();
    let tube_floor: Vec<(i32, i32)> = tube.iter().copied().filter(|&(x, z)| floor_at(x, z) && hyp(x - entrance.0, z - entrance.1) > 15.0).collect();
    for _ in 0..14 {
        if tube_floor.is_empty() {
            break;
        }
        let (x, z) = tube_floor[(rng.next() * tube_floor.len() as f64).floor() as usize];
        npcs.push(("glowworm", IVec2::new(x, z)));
    }
    for &(cx, cz, r) in &far[far.len() / 2..] {
        if let Some(p) = spot(&mut rng, &mut used, cx, cz, r - 1) {
            npcs.push(("crystal_horror", IVec2::new(p.0, p.1)));
        }
    }
    CaveGen { w: w as usize, h: h as usize, grid, spawn: IVec2::new(spawn.0, spawn.1), nodes, npcs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_matches_js() {
        // mulberry32(fnv1a("a")) first values, from node
        assert_eq!(fnv1a("a"), 0xe40c292c);
        let mut r = Mulberry::new(1);
        let v = r.next();
        assert!((v - 0.6270739405881613).abs() < 1e-12, "{v}");
        assert!((r.next() - 0.002735721180215478).abs() < 1e-12);
        assert!((Mulberry::new(0xdeadbeef).next() - 0.9413696140982211).abs() < 1e-12);
    }

    /// Same layout as the server's generator (signatures from node running server/src/cave.ts).
    #[test]
    fn matches_server() {
        for (site, hash, spawn, first) in [("PSP_007018_1255_PSP_007229_1255", 1544192268u32, (7, 63), "l6,63 v34,56 v35,61 f39,56"), ("ESP_011417_1755_ESP_011562_1755", 378832282, (7, 64), "l6,64 v30,67 v27,66 f29,67")] {
            let g = generate(site);
            assert_eq!(fnv1a(&g.rle()), hash, "{site} rle");
            assert_eq!((g.spawn.x, g.spawn.y), spawn);
            let nodes: Vec<String> = g.nodes.iter().take(4).map(|(k, t)| format!("{}{},{}", &k[..1], t.x, t.y)).collect();
            assert_eq!(nodes.join(" "), first);
        }
    }

    #[test]
    fn cave_roundtrip() {
        let g = generate("ESP_011417_1755_ESP_011562_1755");
        let floor = g.grid.iter().filter(|&&v| v == 1).count();
        assert!(floor > 800, "{floor}");
        assert_eq!(g.nodes[0].0, "ladder");
        let m = CaveMap::from_rle(g.w, g.h, &g.rle(), -100.0).unwrap();
        assert_eq!(m.grid, g.grid);
        assert!(m.floor_at(g.spawn.x, g.spawn.y));
        // floor tile centres are flat floor, wall tile centres are rock
        let f = m.world(g.spawn);
        assert!((m.solid_h(f.x, f.z) - m.y0).abs() < 0.05);
    }
}
