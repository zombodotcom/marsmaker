//! Loads a MarsMaker site (meta.json + h_/t_/a_ tiles) into a full-resolution heightfield for
//! physics and textured chunk meshes for rendering.
//!
//! World axes: X = east, Y = up, Z = south (so -Z = north). Heights are relative to the site's
//! lowest point (`Site::base_m` holds the absolute Mars height of Y = 0) to keep f32 precision.
//!
//! Loading runs as an async task (files on native, HTTP fetch on wasm) and hands back the
//! heightfield, decoded textures and the decimated chunk meshes in one `Loaded`.
//!
//! Rendering: every tile is cut into `Site::chunk`-post square chunks. Far away the chunks are
//! drawn decimated (every `step`-th post) to keep the vertex count sane; a window of chunks around
//! the buggy is drawn at full resolution (step 1), rebuilt on background tasks as the buggy moves,
//! so the ground the wheels touch is exactly the ground that is drawn. `Site::height`
//! interpolates with the same two-triangles-per-quad split as the meshes, so physics and the
//! full-res mesh agree exactly.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{AsyncComputeTaskPool, Task, futures::check_ready};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Full-resolution window radius in chunks around the buggy (2 -> 5x5 chunks).
const FINE_RADIUS: i64 = 2;
/// Vertex budget for the decimated far terrain (browsers get much less memory).
#[cfg(not(target_arch = "wasm32"))]
const VERTEX_BUDGET: usize = 16_000_000;
#[cfg(target_arch = "wasm32")]
const VERTEX_BUDGET: usize = 4_000_000;

#[derive(Deserialize)]
struct Meta {
    id: String,
    description: String,
    metres_per_pixel: f32,
    tile_px: usize,
    grid_px: [usize; 2],
    grid_top_left_m: EastNorth,
    height_min_m: f64,
    height_range_m: f64,
    center_lat: f64,
    center_lon_east: f64,
    tiles: Vec<TileMeta>,
    sun: Option<SunMeta>,
}

#[derive(Deserialize, Clone, Copy)]
struct SunMeta {
    azimuth_deg: f32,
    elevation_deg: f32,
}

#[derive(Deserialize, Clone, Copy)]
struct EastNorth {
    east: f32,
    north: f32,
}

#[derive(Deserialize, Clone)]
struct TileMeta {
    x: usize,
    y: usize,
    height: String,
    texture: Option<String>,
    albedo: Option<String>,
}

/// Full-resolution heightfield of a site. NaN = no data. Cheap to clone (heights are shared).
#[derive(Resource, Clone)]
pub struct Site {
    pub name: String,
    pub rationale: String,
    pub lat: f64,
    pub lon: f64,
    pub base_m: f64,
    pub mpp: f32,
    pub(crate) east0: f32,
    pub(crate) north0: f32,
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) z: Arc<Vec<f32>>,
    /// posts per render chunk edge (divides the tile step and every decimation step)
    pub(crate) chunk: usize,
    pub spawn: Vec3,
    /// lowest valid height in the site (world Y); anything far below this is falling forever
    pub min_y: f32,
    /// image sun (azimuth clockwise from north towards the sun, elevation), degrees
    pub sun: Option<(f32, f32)>,
    /// texture set actually used ("ortho", "albedo", "mixed" or "none")
    pub texture_kind: &'static str,
    /// REAL SAND mode: moved sand + tire ruts on top of the DTM (see sand.rs); None = the DTM
    pub sand: Option<Arc<crate::sand::SandShared>>,
}

impl Site {
    pub(crate) fn at(&self, r: usize, c: usize) -> f32 {
        let v = self.z[r * self.w + c];
        // REAL SAND hook: dune-layer change at this post
        match &self.sand {
            Some(s) => v + s.post_delta(r, c),
            None => v,
        }
    }

    /// World (x, z) -> fractional (row, col) in posts.
    fn rc(&self, x: f32, z: f32) -> (f32, f32) {
        ((self.north0 + z) / self.mpp, (x - self.east0) / self.mpp) // north = -z
    }

    /// Ground height at world (x, z); None off the map or over a nodata hole.
    /// Interpolates on the same triangles the meshes draw (quad split along the
    /// (r+1,c)-(r,c+1) diagonal), so at full resolution physics == visuals exactly.
    pub fn height(&self, x: f32, z: f32) -> Option<f32> {
        let (r, c) = self.rc(x, z);
        if !(c >= 0.0 && r >= 0.0) {
            return None;
        }
        let (c0, r0) = (c as usize, r as usize);
        if c0 + 1 >= self.w || r0 + 1 >= self.h {
            return None;
        }
        let (fc, fr) = (c - c0 as f32, r - r0 as f32);
        let a = self.at(r0, c0);
        let b = self.at(r0 + 1, c0);
        let cc = self.at(r0 + 1, c0 + 1);
        let d = self.at(r0, c0 + 1);
        if !(a.is_finite() && b.is_finite() && cc.is_finite() && d.is_finite()) {
            return None;
        }
        let y = if fr + fc <= 1.0 {
            a + (b - a) * fr + (d - a) * fc
        } else {
            cc + (d - cc) * (1.0 - fr) + (b - cc) * (1.0 - fc)
        };
        // REAL SAND hook: tire ruts (fine layer) on top
        Some(match &self.sand {
            Some(s) => y + s.rut(r, c),
            None => y,
        })
    }

    /// Surface normal from central differences at spacing `d` metres (used for mesh shading).
    pub fn normal_at(&self, x: f32, z: f32, d: f32) -> Vec3 {
        let (Some(e), Some(w), Some(s), Some(n)) =
            (self.height(x + d, z), self.height(x - d, z), self.height(x, z + d), self.height(x, z - d))
        else {
            return Vec3::Y;
        };
        Vec3::new(w - e, 2.0 * d, n - s).normalize()
    }

    /// One-post central-difference normal (HUD slope readout).
    pub fn normal(&self, x: f32, z: f32) -> Vec3 {
        self.normal_at(x, z, self.mpp)
    }

    /// Smoothed contact normal for the tire model: 3x3 Sobel over samples 1.5 posts apart
    /// (~3x3 m footprint, about a wheel's contact-patch neighbourhood), so single-post DTM
    /// noise and the triangle facets don't make the tire plane flicker. Falls back to plain
    /// central differences near nodata.
    pub fn contact_normal(&self, x: f32, z: f32) -> Vec3 {
        let d = 1.5 * self.mpp;
        let mut s = [[0.0f32; 3]; 3];
        for (i, row) in s.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                match self.height(x + (j as f32 - 1.0) * d, z + (i as f32 - 1.0) * d) {
                    Some(h) => *v = h,
                    None => return self.normal_at(x, z, self.mpp),
                }
            }
        }
        // s[i][j]: i = z index (south +), j = x index (east +)
        let gx = ((s[0][2] + 2.0 * s[1][2] + s[2][2]) - (s[0][0] + 2.0 * s[1][0] + s[2][0])) / (8.0 * d);
        let gz = ((s[2][0] + 2.0 * s[2][1] + s[2][2]) - (s[0][0] + 2.0 * s[0][1] + s[0][2])) / (8.0 * d);
        Vec3::new(-gx, 1.0, -gz).normalize()
    }

    fn world(&self, r: usize, c: usize) -> Vec3 {
        Vec3::new(self.east0 + c as f32 * self.mpp, self.at(r, c), -(self.north0 - r as f32 * self.mpp))
    }

    /// Chunk (row, col) containing world (x, z).
    fn chunk_of(&self, x: f32, z: f32) -> (i64, i64) {
        let (r, c) = self.rc(x, z);
        ((r / self.chunk as f32).floor() as i64, (c / self.chunk as f32).floor() as i64)
    }
}

/// Where site files come from: a directory on native, a URL prefix (e.g. `sites/<name>/`) on wasm.
/// Live mode (`live` set): `base` is the live index file / URL instead.
#[derive(Clone)]
pub struct Source {
    base: String,
    live: Option<String>,
}

impl Source {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into(), live: None }
    }

    /// Stream site `id` from NASA; `index` = path (native) or URL (wasm) of live_index.json.
    pub fn live(id: impl Into<String>, index: impl Into<String>) -> Self {
        Self { base: index.into(), live: Some(id.into()) }
    }

    /// The live index (base is the index path / URL itself).
    pub async fn read_index(&self) -> Result<Vec<u8>, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::fs::read(&self.base).map_err(|e| format!("{}: {e}", self.base))
        }
        #[cfg(target_arch = "wasm32")]
        {
            crate::live::get(&self.base, None).await
        }
    }

    /// Last path component (the site name), or the live site id.
    pub fn name(&self) -> String {
        if let Some(id) = &self.live {
            return id.clone();
        }
        self.base.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("site").to_string()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn read(&self, file: &str) -> Result<Vec<u8>, String> {
        let p = std::path::Path::new(&self.base).join(file);
        std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn read(&self, file: &str) -> Result<Vec<u8>, String> {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;
        let url = format!("{}/{}", self.base.trim_end_matches('/'), file);
        let win = web_sys::window().ok_or("no window")?;
        let resp = JsFuture::from(win.fetch_with_str(&url)).await.map_err(|e| format!("{url}: {e:?}"))?;
        let resp: web_sys::Response = resp.dyn_into().map_err(|_| format!("{url}: not a Response"))?;
        if !resp.ok() {
            return Err(format!("{url}: HTTP {}", resp.status()));
        }
        let buf = JsFuture::from(resp.array_buffer().map_err(|e| format!("{url}: {e:?}"))?)
            .await
            .map_err(|e| format!("{url}: {e:?}"))?;
        Ok(js_sys::Uint8Array::new(&buf).to_vec())
    }
}

fn decode_png(bytes: &[u8], name: &str) -> Result<(Vec<u16>, usize, usize), String> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().map_err(|e| format!("{name}: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{name}: {e}"))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = match info.bit_depth {
        png::BitDepth::Sixteen => buf[..w * h * 2].chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect(),
        _ => buf[..w * h].iter().map(|&v| v as u16).collect(),
    };
    Ok((px, w, h))
}

/// Greyscale ortho/albedo -> RGBA8 sRGB image with a full mip chain.
///
/// The converter already applied its DN stretch (1..255), so values are used as-is. 0 is nodata:
/// level 0 nodata pixels next to data are first filled from their valid neighbours (2 px dilation,
/// so bilinear filtering at the data edge doesn't bleed black), and each mip texel is the mean of
/// only the *valid* texels beneath it (0 only if all four are nodata), so holes don't darken the
/// surrounding terrain at a distance. Texels still invalid after that are written as the tile's
/// median grey so holes in the ortho read as neutral sand instead of black. Returns None if the
/// tile has no valid texels at all.
fn texture_image(px: Vec<u16>, w: usize, h: usize, name: &str) -> Option<(Image, u8)> {
    let mut hist = [0usize; 256];
    for &v in &px {
        hist[(v as usize).min(255)] += 1;
    }
    let n_valid: usize = hist[1..].iter().sum();
    if n_valid == 0 {
        warn!("{name}: texture has no valid texels, drawing untextured");
        return None;
    }
    let fill = {
        let mut acc = 0;
        (1..256).find(|&i| {
            acc += hist[i];
            acc * 2 >= n_valid
        }).unwrap_or(128) as u8
    };
    let mut level: Vec<f32> = px.iter().map(|&v| v as f32).collect();
    drop(px);
    for _ in 0..2 {
        let src = level.clone();
        for r in 0..h {
            for c in 0..w {
                if src[r * w + c] > 0.0 {
                    continue;
                }
                let (mut sum, mut n) = (0.0, 0);
                for (dr, dc) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1), (-1, -1), (-1, 1), (1, -1), (1, 1)] {
                    let (rr, cc) = (r as i64 + dr, c as i64 + dc);
                    if rr >= 0 && cc >= 0 && (rr as usize) < h && (cc as usize) < w {
                        let v = src[rr as usize * w + cc as usize];
                        if v > 0.0 {
                            sum += v;
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    level[r * w + c] = sum / n as f32;
                }
            }
        }
    }
    let (mut lw, mut lh) = (w, h);
    let mut data = Vec::with_capacity(w * h * 4 * 4 / 3 + 64);
    let mut levels = 0;
    loop {
        for &v in &level {
            let g = if v > 0.0 { v.round().clamp(1.0, 255.0) as u8 } else { fill };
            data.extend_from_slice(&[g, g, g, 255]);
        }
        levels += 1;
        if lw == 1 && lh == 1 {
            break;
        }
        let (nw, nh) = ((lw / 2).max(1), (lh / 2).max(1));
        let mut next = vec![0.0; nw * nh];
        for r in 0..nh {
            for c in 0..nw {
                let (r0, c0) = ((r * 2).min(lh - 1), (c * 2).min(lw - 1));
                let (r1, c1) = ((r0 + 1).min(lh - 1), (c0 + 1).min(lw - 1));
                let (mut sum, mut n) = (0.0, 0.0);
                for v in [level[r0 * lw + c0], level[r0 * lw + c1], level[r1 * lw + c0], level[r1 * lw + c1]] {
                    if v > 0.0 {
                        sum += v;
                        n += 1.0;
                    }
                }
                next[r * nw + c] = if n > 0.0 { sum / n } else { 0.0 };
            }
        }
        level = next;
        (lw, lh) = (nw, nh);
    }
    let mut img = Image::new(
        Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.texture_descriptor.mip_level_count = levels;
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor { anisotropy_clamp: 16, ..ImageSamplerDescriptor::linear() });
    Some((img, fill))
}

#[derive(Clone, Copy, Default)]
pub struct LoadOptions {
    pub step: Option<usize>,
    /// use the approximate de-lit a_ tiles instead of t_ where present
    pub albedo: bool,
    /// live mode: finest post spacing to stream (metres; default ~2)
    pub live_mpp: Option<f32>,
    /// contact-surface low-pass sigma in metres (None = SMOOTH_M, 0 = raw DTM); see
    /// `condition_heights`
    pub smooth_m: Option<f32>,
}

/// Default Gaussian sigma of the terrain low-pass (m): about the DTM's true ~3 m resolution.
pub const SMOOTH_M: f32 = 2.0;

pub struct LoadedTile {
    pub row: usize,
    pub col: usize,
    pub texture: Option<(Image, u8)>,
    /// decimated chunk meshes of this tile, keyed by global chunk (row, col)
    pub chunks: Vec<((usize, usize), Mesh)>,
}

pub struct Loaded {
    pub site: Site,
    pub step: usize,
    pub tile_step_px: usize,
    pub tiles: Vec<LoadedTile>,
}

/// Shared, human-readable loading progress (shown in the HUD while loading).
pub type Progress = Arc<Mutex<String>>;

pub fn say(p: &Progress, s: String) {
    if let Ok(mut g) = p.lock() {
        *g = s;
    }
}

/// Largest chunk size from a list of "nice" sizes that divides the tile step.
fn pick_chunk(tile_step: usize) -> usize {
    [144usize, 126, 112, 96, 84, 72, 63, 56, 48, 42, 36, 32, 28, 24, 21, 16].into_iter().find(|c| tile_step % c == 0).unwrap_or(tile_step)
}

/// Texture source of one grid tile.
pub enum Tex {
    None,
    /// a PNG in the site folder (decoded lazily, one tile at a time)
    File(String),
    /// already decoded greyscale (0 = nodata, 1..255), width, height
    Px(Vec<u16>, usize, usize),
}

pub struct GridTile {
    pub x: usize,
    pub y: usize,
    pub tex: Tex,
}

/// A site's heightfield and texture sources, from a converted site folder or streamed live.
pub struct Grid {
    pub name: String,
    pub rationale: String,
    pub lat: f64,
    pub lon: f64,
    pub base_m: f64,
    pub mpp: f32,
    pub east0: f32,
    pub north0: f32,
    pub w: usize,
    pub h: usize,
    /// heights above base_m, NaN = nodata
    pub z: Vec<f32>,
    /// height quantum (planarity test of the spawn picker)
    pub quantum: f32,
    pub sun: Option<(f32, f32)>,
    /// tile step in posts (tiles are tile_step + 1 posts with a 1-post overlap)
    pub tile_step: usize,
    pub tiles: Vec<GridTile>,
    pub texture_kind: &'static str,
}

/// Converted site folder (meta.json + h_/t_/a_ PNG tiles) -> Grid.
async fn png_grid(src: &Source, opts: LoadOptions, progress: &Progress) -> Result<Grid, String> {
    say(progress, "reading meta.json".into());
    let meta: Meta = serde_json::from_slice(&src.read("meta.json").await?).map_err(|e| format!("meta.json: {e}"))?;
    let [w, h] = meta.grid_px;
    let step_px = meta.tile_px - 1;
    let mut z = vec![f32::NAN; w * h];
    let scale = meta.height_range_m / 65534.0;
    let n_tiles = meta.tiles.len();
    for (i, t) in meta.tiles.iter().enumerate() {
        say(progress, format!("heights {}/{n_tiles}", i + 1));
        let (px, tw, th) = decode_png(&src.read(&t.height).await?, &t.height)?;
        let (r0, c0) = (t.y * step_px, t.x * step_px);
        for r in 0..th.min(h - r0) {
            for c in 0..tw.min(w - c0) {
                let v = px[r * tw + c];
                if v > 0 {
                    z[(r0 + r) * w + c0 + c] = ((v - 1) as f64 * scale) as f32;
                }
            }
        }
    }
    let (mut n_albedo, mut n_ortho) = (0, 0);
    let tiles = meta
        .tiles
        .iter()
        .map(|t| {
            let tex = match (&t.albedo, &t.texture) {
                (Some(a), _) if opts.albedo => {
                    n_albedo += 1;
                    Tex::File(a.clone())
                }
                (_, Some(tx)) => {
                    n_ortho += 1;
                    Tex::File(tx.clone())
                }
                _ => Tex::None,
            };
            GridTile { x: t.x, y: t.y, tex }
        })
        .collect();
    if opts.albedo && n_albedo == 0 {
        warn!("--albedo: this site has no a_ tiles, using the ortho texture");
    }
    Ok(Grid {
        name: src.name(),
        rationale: meta.description.split("rationale for creation was: ").nth(1).unwrap_or(&meta.id)
            .split(". This DTM").next().unwrap_or("").to_string(),
        lat: meta.center_lat,
        lon: meta.center_lon_east,
        base_m: meta.height_min_m,
        mpp: meta.metres_per_pixel,
        east0: meta.grid_top_left_m.east,
        north0: meta.grid_top_left_m.north,
        w,
        h,
        z,
        quantum: scale as f32,
        sun: meta.sun.map(|s| (s.azimuth_deg, s.elevation_deg)),
        tile_step: step_px,
        tiles,
        texture_kind: match (n_albedo, n_ortho) {
            (0, 0) => "none",
            (_, 0) => "albedo",
            (0, _) => "ortho",
            _ => "mixed",
        },
    })
}

/// Read the site's heightfield and texture sources: a converted folder, or live from NASA.
pub async fn load_grid(src: &Source, opts: LoadOptions, progress: &Progress) -> Result<Grid, String> {
    match &src.live {
        Some(id) => {
            say(progress, "reading the live site index".into());
            let index = src.read_index().await?;
            Ok(crate::live::load_grid(index, id, opts.live_mpp, progress).await?.0)
        }
        None => png_grid(src, opts, progress).await,
    }
}

/// Heightfield of a grid -> physics `Site` (with the automatic spawn). Returns the site, the
/// grid's tiles (texture sources) and the tile step in posts.
pub fn site_from_grid(g: Grid, opts: LoadOptions, progress: &Progress) -> (Site, Vec<GridTile>, usize) {
    let (w, h, step_px) = (g.w, g.h, g.tile_step);
    let min_y = g.z.iter().copied().filter(|v| v.is_finite()).fold(f32::INFINITY, f32::min);
    let chunk = pick_chunk(step_px);
    let mut site = Site {
        name: g.name,
        rationale: g.rationale,
        lat: g.lat,
        lon: g.lon,
        base_m: g.base_m,
        mpp: g.mpp,
        east0: g.east0,
        north0: g.north0,
        w,
        h,
        z: Arc::new(g.z),
        chunk,
        spawn: Vec3::ZERO,
        min_y: if min_y.is_finite() { min_y } else { 0.0 },
        sun: g.sun,
        texture_kind: g.texture_kind,
        sand: None,
    };
    say(progress, "choosing a spawn in the dunes".into());
    // the spawn picker's artefact filters (steep noise, planar TIN facets) need the raw DTM
    site.spawn = pick_spawn(&site, g.quantum);
    let sigma = opts.smooth_m.unwrap_or(SMOOTH_M).max(0.0);
    if sigma > 0.0 {
        say(progress, "smoothing the DTM noise".into());
        let t0 = bevy::platform::time::Instant::now();
        let z = Arc::get_mut(&mut site.z).expect("site heights are not shared yet");
        let spikes = condition_heights(z, w, h, site.mpp, sigma);
        info!("terrain: despiked {spikes} posts, Gaussian low-pass sigma {sigma:.1} m in {:.1}s", t0.elapsed().as_secs_f32());
        if let Some(y) = site.height(site.spawn.x, site.spawn.z) {
            site.spawn.y = y;
        }
    }
    (site, g.tiles, step_px)
}

/// Heightfield only (no textures or meshes): for the headless `--bench`.
#[cfg(not(target_arch = "wasm32"))]
pub async fn load_site(src: &Source, opts: LoadOptions, progress: &Progress) -> Result<Site, String> {
    let g = load_grid(src, opts, progress).await?;
    Ok(site_from_grid(g, opts, progress).0)
}

/// Load a whole site (heights, textures, decimated meshes). Runs as an async task.
pub async fn load(src: Source, opts: LoadOptions, progress: Progress) -> Result<Loaded, String> {
    let t0 = bevy::platform::time::Instant::now();
    let g = load_grid(&src, opts, &progress).await?;
    let (site, grid_tiles, step_px) = site_from_grid(g, opts, &progress);
    let (n_tiles, chunk) = (grid_tiles.len(), site.chunk);

    // mesh step: keep the whole site under the vertex budget; physics always uses full
    // resolution (and the chunks near the buggy are drawn at full resolution too)
    let steps = [1usize, 2, 3, 4, 6, 8, 12, 16].into_iter().filter(|s| chunk % s == 0);
    let step = opts.step.filter(|s| *s >= 1 && chunk % s == 0).unwrap_or_else(|| {
        steps.clone().find(|s| n_tiles * (step_px / s + 1).pow(2) <= VERTEX_BUDGET).unwrap_or(steps.last().unwrap_or(1))
    });
    let per_tile = step_px / chunk;
    let mut tiles = Vec::with_capacity(n_tiles);
    for (i, t) in grid_tiles.into_iter().enumerate() {
        say(&progress, format!("textures + meshes {}/{n_tiles}", i + 1));
        let texture = match t.tex {
            Tex::File(f) => match src.read(&f).await {
                Ok(bytes) => decode_png(&bytes, &f).ok().and_then(|(px, tw, th)| texture_image(px, tw, th, &f)),
                Err(e) => {
                    warn!("{e}");
                    None
                }
            },
            Tex::Px(px, tw, th) => texture_image(px, tw, th, &site.name),
            Tex::None => None,
        };
        let mut chunks = Vec::new();
        for ci in 0..per_tile {
            for cj in 0..per_tile {
                let key = (t.y * per_tile + ci, t.x * per_tile + cj);
                if let Some(m) = chunk_mesh(&site, key.0, key.1, step, step_px, None) {
                    chunks.push((key, m));
                }
            }
        }
        tiles.push(LoadedTile { row: t.y, col: t.x, texture, chunks });
    }
    info!("loaded {} ({n_tiles} tiles, far mesh step {step}, chunk {chunk}) in {:.1}s", site.name, t0.elapsed().as_secs_f32());
    Ok(Loaded { site, step, tile_step_px: step_px, tiles })
}


/// Contact-surface conditioning of a raw DTM grid (heights in place, NaN = nodata).
///
/// HiRISE DTMs are posted at ~1 m but their true resolution is ~3 m (stereo correlation window),
/// so the top octave is mostly matching noise, isolated spikes and the creases of flat TIN facets.
/// Driven at 1:1 every one of those is a hard kick into a point-contact wheel. Two steps:
///  1. despike: a post more than `SPIKE_M` above the highest (or below the lowest) of its 8
///     neighbours is replaced with their mean (real terrain can't make a 1-post peak or pit);
///  2. a Gaussian low-pass with sigma `sigma_m` metres (normalised over valid posts, so nodata
///     edges don't sag), i.e. a low-pass at about the DTM's true resolution.
/// Render meshes and physics both use the result, so the wheels sit exactly on what's drawn.
/// Returns the number of despiked posts.
pub fn condition_heights(z: &mut Vec<f32>, w: usize, h: usize, mpp: f32, sigma_m: f32) -> usize {
    const SPIKE_M: f32 = 0.35;
    let sigma = sigma_m / mpp; // posts
    let rad = if sigma >= 0.25 { (sigma * 2.5).ceil() as usize } else { 0 };
    let kernel: Vec<f32> = (0..=rad).map(|i| (-(i as f32).powi(2) / (2.0 * sigma * sigma).max(1e-6)).exp()).collect();
    let src = std::mem::take(z);
    let mut out = vec![f32::NAN; w * h];
    let threads = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16)
        }
        #[cfg(target_arch = "wasm32")]
        {
            1
        }
    };
    let band = 128usize; // rows per work item (keeps per-thread buffers small)
    let spikes = std::sync::atomic::AtomicUsize::new(0);
    let work = |r0: usize, dst: &mut [f32]| {
        let r1 = (r0 + band).min(h);
        let halo = rad + 1;
        let (a0, a1) = (r0.saturating_sub(halo), (r1 + halo).min(h));
        let rows = a1 - a0;
        // despiked copy of the band (+ halo)
        let mut d = vec![f32::NAN; rows * w];
        let mut n_sp = 0;
        for r in a0..a1 {
            for c in 0..w {
                let v = src[r * w + c];
                let mut val = v;
                if v.is_finite() && r > 0 && c > 0 && r + 1 < h && c + 1 < w {
                    let (mut mx, mut mn, mut sum, mut n) = (f32::NEG_INFINITY, f32::INFINITY, 0.0, 0);
                    for (dr, dc) in [(-1i64, -1i64), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)] {
                        let q = src[(r as i64 + dr) as usize * w + (c as i64 + dc) as usize];
                        if q.is_finite() {
                            mx = mx.max(q);
                            mn = mn.min(q);
                            sum += q;
                            n += 1;
                        }
                    }
                    if n >= 6 && (v > mx + SPIKE_M || v < mn - SPIKE_M) {
                        val = sum / n as f32;
                        if r >= r0 && r < r1 {
                            n_sp += 1;
                        }
                    }
                }
                d[(r - a0) * w + c] = val;
            }
        }
        spikes.fetch_add(n_sp, std::sync::atomic::Ordering::Relaxed);
        if rad == 0 {
            for r in r0..r1 {
                dst[(r - r0) * w..(r - r0 + 1) * w].copy_from_slice(&d[(r - a0) * w..(r - a0 + 1) * w]);
            }
            return;
        }
        // horizontal pass: numerator (weighted heights) and denominator (weights of valid posts)
        let mut num = vec![0.0f32; rows * w];
        let mut den = vec![0.0f32; rows * w];
        for rr in 0..rows {
            let row = &d[rr * w..(rr + 1) * w];
            for c in 0..w {
                let (mut s, mut k) = (0.0, 0.0);
                let lo = c.saturating_sub(rad);
                let hi = (c + rad).min(w - 1);
                for cc in lo..=hi {
                    let q = row[cc];
                    if q.is_finite() {
                        let wt = kernel[cc.abs_diff(c)];
                        s += wt * q;
                        k += wt;
                    }
                }
                num[rr * w + c] = s;
                den[rr * w + c] = k;
            }
        }
        // vertical pass over the band's own rows
        for r in r0..r1 {
            let rr = r - a0;
            let lo = rr.saturating_sub(rad);
            let hi = (rr + rad).min(rows - 1);
            for c in 0..w {
                if !d[rr * w + c].is_finite() {
                    continue; // nodata stays nodata
                }
                let (mut s, mut k) = (0.0, 0.0);
                for q in lo..=hi {
                    let wt = kernel[q.abs_diff(rr)];
                    s += wt * num[q * w + c];
                    k += wt * den[q * w + c];
                }
                dst[(r - r0) * w + c] = s / k;
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let queue = std::sync::Mutex::new(out.chunks_mut(band * w).enumerate().collect::<Vec<_>>());
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(|| {
                    loop {
                        let Some((i, dst)) = queue.lock().unwrap().pop() else { break };
                        work(i * band, dst);
                    }
                });
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = threads;
    #[cfg(target_arch = "wasm32")]
    for (i, dst) in out.chunks_mut(band * w).enumerate() {
        work(i * band, dst);
    }
    *z = out;
    spikes.into_inner()
}

/// Pick a spawn in the dune field. The site is cut into ~128 m blocks, each scored by the
/// variance of the surface gradient measured over an 8 m baseline (dunes = slopes alternating in
/// direction at the 10-100 m scale; a uniform crater wall or a flat plain scores low, and the 8 m
/// baseline ignores 1-post DTM noise). Blocks are rejected when
///  * any block in their 3x3 neighbourhood has nodata or is off the map (>= 1 block from edges),
///  * their mean tilt is over ~14 degrees (sitting on a big wall),
///  * > 1% of posts are steeper than 40 degrees at 1 m (stereo-matching noise / knobby artefacts),
///  * too many posts are planar (zero Laplacian; > max(15%, 1.5x the site median)): TIN-interpolated
///    facets where the DTM was filled in, which look like crisp ridges but aren't real terrain.
/// Within the winning block, spawn on the flattest post near its centre.
fn pick_spawn(site: &Site, quantum: f32) -> Vec3 {
    let bp = ((128.0 / site.mpp) as usize).max(16); // block size in posts
    let k = ((4.0 / site.mpp).round() as usize).max(1); // half-baseline in posts
    let st = if site.mpp < 1.5 { 2 } else { 1 }; // sample stride in posts
    let (nbr, nbc) = (site.h / bp, site.w / bp);
    let ok = |r: usize, c: usize, d: usize| r >= d && c >= d && r + d < site.h && c + d < site.w;
    let grad = |r: usize, c: usize, d: usize| -> Option<Vec2> {
        if !ok(r, c, d) {
            return None;
        }
        let (e, w, s, n) = (site.at(r, c + d), site.at(r, c - d), site.at(r + d, c), site.at(r - d, c));
        (e.is_finite() && w.is_finite() && s.is_finite() && n.is_finite())
            .then(|| Vec2::new(e - w, s - n) / (2.0 * d as f32 * site.mpp))
    };
    let planar = |r: usize, c: usize| -> bool {
        let l = site.at(r - 1, c) + site.at(r + 1, c) + site.at(r, c - 1) + site.at(r, c + 1) - 4.0 * site.at(r, c);
        l.abs() < 2.4 * quantum
    };
    struct Block {
        valid: bool,
        score: f32,
        tilt: f32,
        steep: f32,
        planar: f32,
    }
    let mut blocks = Vec::with_capacity(nbr * nbc);
    for br in 0..nbr {
        for bc in 0..nbc {
            let (mut sum, mut sum2, mut n, mut n_steep, mut n_planar, mut bad) = (Vec2::ZERO, 0.0f32, 0.0f32, 0.0, 0.0, false);
            'b: for r in (br * bp..(br + 1) * bp).step_by(st) {
                for c in (bc * bp..(bc + 1) * bp).step_by(st) {
                    let (Some(g), Some(f)) = (grad(r, c, k), grad(r, c, 1)) else {
                        bad = true;
                        break 'b;
                    };
                    sum += g;
                    sum2 += g.length_squared();
                    n += 1.0;
                    n_steep += (f.length() > 0.84) as u8 as f32;
                    n_planar += planar(r, c) as u8 as f32;
                }
            }
            let valid = !bad && n > 0.0;
            let mean = if valid { sum / n } else { Vec2::ZERO };
            blocks.push(Block {
                valid,
                score: if valid { sum2 / n - mean.length_squared() } else { 0.0 },
                tilt: mean.length(),
                steep: if valid { n_steep / n } else { 1.0 },
                planar: if valid { n_planar / n } else { 1.0 },
            });
        }
    }
    // TIN fill is an outlier relative to the site: smooth real dunes can be fairly planar at the
    // DTM's height quantum too, so the planarity cut is relative to the site's median block
    let mut pl: Vec<f32> = blocks.iter().filter(|b| b.valid).map(|b| b.planar).collect();
    pl.sort_by(f32::total_cmp);
    let planar_max = pl.get(pl.len() / 2).map_or(0.15, |m| (1.5 * m).max(0.15));
    let pick_block = |strict: bool| -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, f32)> = None;
        for br in 1..nbr.saturating_sub(1) {
            for bc in 1..nbc.saturating_sub(1) {
                let b = &blocks[br * nbc + bc];
                let ring_ok = (br - 1..=br + 1).all(|r| (bc - 1..=bc + 1).all(|c| blocks[r * nbc + c].valid));
                let clean = !strict || (b.steep < 0.01 && b.planar < planar_max);
                if ring_ok && clean && b.tilt < 0.25 && best.is_none_or(|x| b.score > x.2) {
                    best = Some((br, bc, b.score));
                }
            }
        }
        best.map(|b| (b.0, b.1))
    };
    // strict, then without the artefact filters, then any fully valid block
    let best = pick_block(true).or_else(|| pick_block(false)).or_else(|| {
        (0..nbr * nbc).filter(|&i| blocks[i].valid).max_by(|&a, &b| blocks[a].score.total_cmp(&blocks[b].score)).map(|i| (i / nbc, i % nbc))
    });
    if let Some((br, bc)) = best {
        let (cr, cc) = (br * bp + bp / 2, bc * bp + bp / 2);
        let mut pick = None;
        for r in (br * bp + bp / 4..br * bp + 3 * bp / 4).step_by(st) {
            for c in (bc * bp + bp / 4..bc * bp + 3 * bp / 4).step_by(st) {
                if let Some(g) = grad(r, c, 2) {
                    let d = ((r as f32 - cr as f32).powi(2) + (c as f32 - cc as f32).powi(2)).sqrt() / bp as f32;
                    let cost = g.length() + 0.2 * d;
                    if pick.is_none_or(|(_, _, k)| cost < k) {
                        pick = Some((r, c, cost));
                    }
                }
            }
        }
        if let Some((r, c, _)) = pick {
            let b = &blocks[br * nbc + bc];
            info!(
                "spawn: dune block ({br},{bc}) score {:.4} tilt {:.3} steep {:.4} planar {:.3}",
                b.score, b.tilt, b.steep, b.planar
            );
            return site.world(r, c);
        }
    }
    warn!("spawn: no fully valid block, using first valid post");
    let i = site.z.iter().position(|v| v.is_finite()).expect("site has no valid heights");
    site.world(i / site.w, i % site.w)
}

/// Build the mesh for chunk (cr, cc) at the given step (quads touching nodata are left out).
/// `snap` = (side mask N=1,S=2,W=4,E=8, coarse step): edge vertices on those sides are put onto the
/// coarse mesh's straight edges, so a full-res chunk next to a decimated chunk has no cracks.
pub(crate) fn chunk_mesh(site: &Site, cr: usize, cc: usize, step: usize, tile_step_px: usize, snap: Option<(u8, usize)>) -> Option<Mesh> {
    let chunk = site.chunk;
    let (r0, c0) = (cr * chunk, cc * chunk);
    if r0 + 1 >= site.h || c0 + 1 >= site.w {
        return None;
    }
    let (tr0, tc0) = ((r0 / tile_step_px) * tile_step_px, (c0 / tile_step_px) * tile_step_px);
    let n = chunk / step + 1;
    let mut pos = Vec::with_capacity(n * n);
    let mut nrm = Vec::with_capacity(n * n);
    let mut uv = Vec::with_capacity(n * n);
    let mut ok = Vec::with_capacity(n * n);
    let nd = site.mpp * step as f32;
    for i in 0..n {
        for j in 0..n {
            let (li, lj) = (i * step, j * step);
            let (r, c) = ((r0 + li).min(site.h - 1), (c0 + lj).min(site.w - 1));
            let mut p = site.world(r, c);
            if let Some((mask, cs)) = snap {
                // interpolate along a coarse edge between coarse posts a and b (t in 0..1)
                let lerp = |a: f32, b: f32, t: f32| if a.is_finite() && b.is_finite() { Some(a + (b - a) * t) } else { None };
                let on_h = (li == 0 && mask & 1 != 0) || (li == chunk && mask & 2 != 0);
                let on_v = (lj == 0 && mask & 4 != 0) || (lj == chunk && mask & 8 != 0);
                if on_h && lj % cs != 0 {
                    let j0 = lj / cs * cs;
                    let (ca, cb) = ((c0 + j0).min(site.w - 1), (c0 + j0 + cs).min(site.w - 1));
                    if let Some(y) = lerp(site.at(r, ca), site.at(r, cb), (lj - j0) as f32 / cs as f32) {
                        p.y = y;
                    }
                } else if on_v && li % cs != 0 {
                    let i0 = li / cs * cs;
                    let (ra, rb) = ((r0 + i0).min(site.h - 1), (r0 + i0 + cs).min(site.h - 1));
                    if let Some(y) = lerp(site.at(ra, c), site.at(rb, c), (li - i0) as f32 / cs as f32) {
                        p.y = y;
                    }
                }
            }
            ok.push(p.y.is_finite());
            let p = if p.y.is_finite() { p } else { Vec3::new(p.x, 0.0, p.z) };
            pos.push(p.to_array());
            nrm.push(site.normal_at(p.x, p.z, nd).to_array());
            uv.push([(r0 + li - tr0) as f32, (c0 + lj - tc0) as f32].map(|v| v / tile_step_px as f32));
        }
    }
    let uv: Vec<[f32; 2]> = uv.into_iter().map(|[v, u]| [u, v]).collect();
    let mut idx = Vec::new();
    for i in 0..n - 1 {
        for j in 0..n - 1 {
            let (a, b, c, d) = (i * n + j, (i + 1) * n + j, (i + 1) * n + j + 1, i * n + j + 1);
            // REAL SAND hook: full-res quads under a tire-rut overlay patch are drawn by the overlay
            if ok[a] && ok[b] && ok[c] && ok[d] && !(step == 1 && crate::sand::quad_hidden(site, r0 + i, c0 + j)) {
                // counter-clockwise seen from above (+Y); diagonal b-d (matches Site::height)
                idx.extend_from_slice(&[a as u32, b as u32, d as u32, b as u32, c as u32, d as u32]);
            }
        }
    }
    if idx.is_empty() {
        return None;
    }
    Some(
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
            .with_inserted_indices(Indices::U32(idx)),
    )
}

/// A full-res window rebuild running on the async compute pool.
struct Pending {
    want: HashMap<(usize, usize), u8>,
    tasks: Vec<Task<((usize, usize), u8, Option<Mesh>)>>,
    done: Vec<((usize, usize), u8, Option<Mesh>)>,
    started: bevy::platform::time::Instant,
}

/// Render bookkeeping: decimated chunks everywhere, a full-res window around the buggy.
#[derive(Resource)]
pub struct TerrainRender {
    pub(crate) step: usize,
    pub(crate) tile_step_px: usize,
    pub(crate) materials: HashMap<(usize, usize), Handle<StandardMaterial>>, // per tile (row, col)
    pub(crate) coarse: HashMap<(usize, usize), Entity>,                        // per chunk (row, col)
    pub(crate) fine: HashMap<(usize, usize), (Entity, Handle<Mesh>, u8)>,
    /// window centre the current `fine` set (or the pending rebuild) is for
    center: Option<(i64, i64)>,
    pending: Option<Pending>,
    pub fine_chunks: usize,
    /// main-thread time of the last swap (ms), background build time of the last rebuild (ms)
    pub swap_ms: f32,
    pub build_ms: f32,
    pub rebuilds: u32,
}

/// Turn the loaded tiles into materials and decimated chunk entities.
pub fn spawn_terrain(
    commands: &mut Commands,
    tiles: Vec<LoadedTile>,
    step: usize,
    tile_step_px: usize,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    materials: &mut Assets<StandardMaterial>,
) -> TerrainRender {
    let mut render = TerrainRender {
        step,
        tile_step_px,
        materials: HashMap::new(),
        coarse: HashMap::new(),
        fine: HashMap::new(),
        center: None,
        pending: None,
        fine_chunks: 0,
        swap_ms: 0.0,
        build_ms: 0.0,
        rebuilds: 0,
    };
    let mut greys: Vec<u8> = tiles.iter().filter_map(|t| t.texture.as_ref().map(|x| x.1)).collect();
    greys.sort_unstable();
    let tint = Color::srgb(1.0, 0.66, 0.46); // HiRISE RED is greyscale: tint toward Mars dust
    // untextured tiles: tint times the site's median ortho grey (plain tint if no tile has a texture)
    let untextured = greys.get(greys.len() / 2).map_or(tint, |&g| {
        let l = Color::srgb_u8(g, g, g).to_linear().red;
        let t = tint.to_linear();
        Color::linear_rgb(t.red * l, t.green * l, t.blue * l)
    });
    for tile in tiles {
        let material = materials.add(StandardMaterial {
            base_color: if tile.texture.is_some() { tint } else { untextured },
            base_color_texture: tile.texture.map(|(img, _)| images.add(img)),
            perceptual_roughness: 0.97,
            reflectance: 0.2,
            ..default()
        });
        for (key, mesh) in tile.chunks {
            let e = commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.clone()))).id();
            render.coarse.insert(key, e);
        }
        render.materials.insert((tile.row, tile.col), material);
    }
    render
}

fn set_vis(vis: &mut Query<&mut Visibility>, e: Option<&Entity>, v: Visibility) {
    if let Some(Ok(mut x)) = e.map(|e| vis.get_mut(*e)) {
        *x = v;
    }
}

/// Keep a (2*FINE_RADIUS+1)^2 window of full-resolution chunks centred on the buggy's chunk.
///
/// Meshes are built on the async compute pool. Until every chunk of a rebuild is ready, the old
/// full-res chunks stay on screen untouched (the old window still covers +-1 chunk around the
/// buggy, since it only moves one chunk at a time), then old and new are swapped in one frame.
/// Physics never depends on this: it always samples the full-res heightfield.
pub fn update_fine_window(
    commands: &mut Commands,
    site: &Site,
    render: &mut TerrainRender,
    meshes: &mut Assets<Mesh>,
    vis: &mut Query<&mut Visibility>,
    at: Vec3,
) {
    if render.step == 1 {
        return; // everything is already full resolution
    }
    if let Some(p) = &mut render.pending {
        let mut i = 0;
        while i < p.tasks.len() {
            if let Some(r) = check_ready(&mut p.tasks[i]) {
                p.done.push(r);
                drop(p.tasks.swap_remove(i));
            } else {
                i += 1;
            }
        }
        if !p.tasks.is_empty() {
            return; // still building; keep showing the old window
        }
        let p = render.pending.take().unwrap();
        let t0 = bevy::platform::time::Instant::now();
        render.build_ms = p.started.elapsed().as_secs_f32() * 1000.0;
        // drop chunks that left the window or whose seam sides changed
        let old: Vec<_> = render.fine.keys().copied().collect();
        for k in old {
            if p.want.get(&k) != render.fine.get(&k).map(|f| &f.2) {
                let (e, mesh, _) = render.fine.remove(&k).unwrap();
                commands.entity(e).despawn();
                meshes.remove(&mesh);
                if !p.want.contains_key(&k) {
                    set_vis(vis, render.coarse.get(&k), Visibility::Inherited);
                }
            }
        }
        let per_tile = render.tile_step_px / site.chunk;
        for ((cr, cc), mask, mesh) in p.done {
            let (Some(mesh), Some(material)) = (mesh, render.materials.get(&(cr / per_tile, cc / per_tile)).cloned()) else {
                continue;
            };
            let h = meshes.add(mesh);
            let e = commands.spawn((Mesh3d(h.clone()), MeshMaterial3d(material))).id();
            render.fine.insert((cr, cc), (e, h, mask));
            set_vis(vis, render.coarse.get(&(cr, cc)), Visibility::Hidden);
        }
        render.fine_chunks = render.fine.len();
        render.swap_ms = t0.elapsed().as_secs_f32() * 1000.0;
        render.rebuilds += 1;
    }

    let center = site.chunk_of(at.x, at.z);
    if render.center == Some(center) {
        return;
    }
    render.center = Some(center);
    let inside = |r: i64, c: i64| (r - center.0).abs() <= FINE_RADIUS && (c - center.1).abs() <= FINE_RADIUS;
    let mut want: HashMap<(usize, usize), u8> = HashMap::new();
    for r in center.0 - FINE_RADIUS..=center.0 + FINE_RADIUS {
        for c in center.1 - FINE_RADIUS..=center.1 + FINE_RADIUS {
            if r < 0 || c < 0 || !render.coarse.contains_key(&(r as usize, c as usize)) {
                continue;
            }
            let mask = (!inside(r - 1, c)) as u8 | ((!inside(r + 1, c)) as u8) << 1 | ((!inside(r, c - 1)) as u8) << 2 | ((!inside(r, c + 1)) as u8) << 3;
            want.insert((r as usize, c as usize), mask);
        }
    }
    let pool = AsyncComputeTaskPool::get();
    let (coarse_step, tile_step_px) = (render.step, render.tile_step_px);
    let tasks = want
        .iter()
        .filter(|(k, m)| render.fine.get(*k).map(|f| f.2) != Some(**m))
        .map(|(&(cr, cc), &mask)| {
            let site = site.clone();
            pool.spawn(async move { ((cr, cc), mask, chunk_mesh(&site, cr, cc, 1, tile_step_px, Some((mask, coarse_step)))) })
        })
        .collect();
    render.pending = Some(Pending { want, tasks, done: Vec::new(), started: bevy::platform::time::Instant::now() });
}
