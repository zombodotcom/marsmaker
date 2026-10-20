//! Live mode: stream any HiRISE DTM (and its ortho) straight from the public astrogeo-ard bucket
//! (NASA/USGS, `Access-Control-Allow-Origin: *`) with HTTP Range requests, no server of ours.
//!
//! The DTMs and orthos are Cloud-Optimized GeoTIFFs: BigTIFF, 512x512 tiles, DEFLATE with the
//! horizontal-differencing predictor (2), float32 heights / uint16 DN, 5-7 internal overviews,
//! all IFDs in the first 64 KB. This module has a minimal COG reader for exactly that family
//! (plus LZW, no compression, predictor 3, 8/16/32-bit int and float samples, ClassicTIFF), picks
//! the overview giving ~2 m posts, fetches only the tiles it needs (coalesced into few requests),
//! and resamples onto the same kind of grid the converter writes: true metres, world origin at the
//! site centre, X east / north up, heights in metres, NaN = nodata.
//!
//! No reprojection: the grid is the file's own map grid scaled to true metres at the centre
//! (`k` in the index, from the projection). For the equirectangular files (standard parallel 0)
//! that is a cos(lat) stretch of x; it differs from the converter's transverse Mercator by
//! ~0.1-0.3 % towards the far ends of a 10 km strip. Polar stereographic is conformal and its
//! scale changes by < 0.2 % across a site; there the grid is the map grid, so "up" is grid north
//! (`conv` in the index rotates the sun into it).
//!
//! The site list (`live_index.json`, from tools/make_live_index.py) also carries what the browser
//! cannot fetch itself: the sun geometry (HiRISE RDR labels; that host sends no CORS header) and
//! the ortho northing fix (orthos placed on the label's local radius but tagged with the IAU
//! sphere; see converter/hirise2engine.ortho_yscale_candidate).

use crate::terrain::{Grid, GridTile, Progress, Tex, say};
use bevy::prelude::*;

/// Tile step of the generated grid (tiles are STEP+1 posts with a 1-post overlap, like web_pack).
const STEP: usize = 504;
/// Post budgets: posts in tiles that hold data, and the whole (dense) grid.
const MAX_DATA_POSTS: usize = 16_000_000;
const MAX_GRID_POSTS: usize = 48_000_000;
/// Range-request coalescing: merge tiles closer than GAP bytes, up to MAX_REQ per request.
const GAP: u64 = 256 * 1024;
const MAX_REQ: u64 = 3 * 1024 * 1024;
const HEAD: u64 = 65536;
/// Prefer the next overview (if it is still <= 6 m) over a DTM level bigger than this.
const MAX_DTM_BYTES: u64 = 64_000_000;

#[derive(Clone, Debug)]
pub struct Spec {
    pub id: String,
    pub title: String,
    pub lat: f64,
    pub lon: f64,
    pub dtm: String,
    pub ortho: Option<String>,
    /// map coordinates of the centre, map metres per true metre (x, y), grid bearing of true north
    pub c: [f64; 2],
    pub k: [f64; 2],
    pub conv: f64,
    pub sun: Option<(f32, f32)>,
    /// ortho georeference fix: y_true = oys * y_file + osh.1, x_true = x_file + osh.0
    pub oys: f64,
    pub osh: [f64; 2],
}

/// Find a site in the index (`{"base", "fields", "sites": [[...]]}`).
pub fn spec_from_index(index: &[u8], id: &str) -> Result<Spec, String> {
    let v: serde_json::Value = serde_json::from_slice(index).map_err(|e| format!("live_index.json: {e}"))?;
    let base = v["base"].as_str().ok_or("live_index.json: no base")?;
    let fields: Vec<&str> = v["fields"].as_array().ok_or("live_index.json: no fields")?.iter().filter_map(|f| f.as_str()).collect();
    let col = |name: &str| fields.iter().position(|f| *f == name).ok_or(format!("live_index.json: no field {name}"));
    let (ci, ct, clat, clon, cd, co) = (col("id")?, col("title")?, col("lat")?, col("lon")?, col("dtm")?, col("ortho")?);
    let (cc, ck, ccv, cs, coy, cosh) = (col("c")?, col("k")?, col("conv")?, col("sun")?, col("oys")?, col("osh")?);
    let row = v["sites"]
        .as_array()
        .ok_or("live_index.json: no sites")?
        .iter()
        .find(|r| r[ci].as_str() == Some(id))
        .ok_or(format!("{id} is not in live_index.json"))?;
    let url = |p: &str| if p.contains('/') { format!("{base}{p}") } else { format!("{base}{id}/{p}") };
    let f = |x: &serde_json::Value| x.as_f64().unwrap_or(0.0);
    let pair = |x: &serde_json::Value| [f(&x[0]), f(&x[1])];
    Ok(Spec {
        id: id.to_string(),
        title: row[ct].as_str().unwrap_or("").to_string(),
        lat: f(&row[clat]),
        lon: f(&row[clon]),
        dtm: url(row[cd].as_str().ok_or("no dtm")?),
        ortho: row[co].as_str().map(url),
        c: pair(&row[cc]),
        k: pair(&row[ck]),
        conv: f(&row[ccv]),
        sun: row[cs].as_array().map(|s| (f(&s[0]) as f32, f(&s[1]) as f32)),
        oys: row[coy].as_f64().unwrap_or(1.0),
        osh: pair(&row[cosh]),
    })
}

// ------------------------------------------------------------------------------------ HTTP

/// Downloads byte ranges of one URL concurrently and hands them back as they complete.
struct Batch {
    #[cfg(target_arch = "wasm32")]
    promises: std::collections::VecDeque<(usize, js_sys::Promise)>,
    #[cfg(not(target_arch = "wasm32"))]
    rx: std::sync::mpsc::Receiver<(usize, Result<Vec<u8>, String>)>,
    left: usize,
}

#[cfg(target_arch = "wasm32")]
async fn fetch_buf(url: String, range: Option<(u64, u64)>) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_mode(web_sys::RequestMode::Cors);
    let headers = web_sys::Headers::new()?;
    if let Some((a, b)) = range {
        headers.set("Range", &format!("bytes={a}-{b}"))?;
    }
    init.set_headers(&headers);
    let req = web_sys::Request::new_with_str_and_init(&url, &init)?;
    let win = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response = JsFuture::from(win.fetch_with_request(&req)).await?.dyn_into()?;
    if !resp.ok() {
        return Err(format!("{url}: HTTP {}", resp.status()).into());
    }
    let buf = JsFuture::from(resp.array_buffer()?).await?;
    // a server that ignores Range answers 200 with the whole file
    if let (Some((a, b)), 200) = (range, resp.status()) {
        let arr = js_sys::Uint8Array::new(&buf);
        return Ok(arr.subarray(a as u32, (b + 1).min(arr.length() as u64) as u32).into());
    }
    Ok(buf)
}

#[cfg(target_arch = "wasm32")]
pub async fn get(url: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>, String> {
    let v = fetch_buf(url.to_string(), range).await.map_err(|e| format!("{url}: {}", e.as_string().unwrap_or_else(|| format!("{e:?}"))))?;
    Ok(js_sys::Uint8Array::new(&v).to_vec())
}

#[cfg(not(target_arch = "wasm32"))]
fn agent() -> ureq::Agent {
    use std::sync::OnceLock;
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(120))).build().into()).clone()
}

#[cfg(not(target_arch = "wasm32"))]
fn get_blocking(url: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        let mut req = agent().get(url);
        if let Some((a, b)) = range {
            req = req.header("Range", &format!("bytes={a}-{b}"));
        }
        match req.call() {
            Ok(mut r) => {
                let full = r.status() == 200;
                match r.body_mut().with_config().limit(4_000_000_000).read_to_vec() {
                    Ok(v) => {
                        return Ok(match (range, full) {
                            (Some((a, b)), true) => v[(a as usize).min(v.len())..((b + 1) as usize).min(v.len())].to_vec(),
                            _ => v,
                        });
                    }
                    Err(e) => last = format!("{url}: {e}"),
                }
            }
            Err(ureq::Error::StatusCode(c)) => return Err(format!("{url}: HTTP {c}")),
            Err(e) => last = format!("{url}: {e}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(500 << attempt));
    }
    Err(last)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn get(url: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>, String> {
    get_blocking(url, range)
}

impl Batch {
    #[cfg(target_arch = "wasm32")]
    fn start(url: &str, ranges: &[(u64, u64)]) -> Self {
        // the browser runs the requests concurrently (6 per host); we await them in order
        let promises = ranges
            .iter()
            .enumerate()
            .map(|(i, &r)| (i, wasm_bindgen_futures::future_to_promise(fetch_buf(url.to_string(), Some(r)))))
            .collect();
        Batch { promises, left: ranges.len() }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start(url: &str, ranges: &[(u64, u64)]) -> Self {
        use std::sync::{Arc, Mutex};
        let (tx, rx) = std::sync::mpsc::channel();
        let queue = Arc::new(Mutex::new(ranges.iter().copied().enumerate().collect::<std::collections::VecDeque<_>>()));
        for _ in 0..ranges.len().min(6) {
            let (tx, queue, url) = (tx.clone(), queue.clone(), url.to_string());
            std::thread::spawn(move || {
                loop {
                    let Some((i, r)) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { break };
                    if tx.send((i, get_blocking(&url, Some(r)))).is_err() {
                        break;
                    }
                }
            });
        }
        Batch { rx, left: ranges.len() }
    }

    #[cfg(target_arch = "wasm32")]
    async fn next(&mut self) -> Option<Result<(usize, Vec<u8>), String>> {
        let (i, p) = self.promises.pop_front()?;
        self.left -= 1;
        Some(
            wasm_bindgen_futures::JsFuture::from(p)
                .await
                .map(|v| (i, js_sys::Uint8Array::new(&v).to_vec()))
                .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}"))),
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn next(&mut self) -> Option<Result<(usize, Vec<u8>), String>> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        Some(match self.rx.recv() {
            Ok((i, r)) => r.map(|v| (i, v)),
            Err(_) => Err("download threads ended".into()),
        })
    }
}

// ------------------------------------------------------------------------------------ TIFF

#[derive(Clone, Debug)]
pub struct Level {
    pub w: usize,
    pub h: usize,
    tw: usize,
    th: usize,
    offsets: Vec<u64>,
    counts: Vec<u64>,
}

impl Level {
    fn tiles_across(&self) -> usize {
        self.w.div_ceil(self.tw)
    }
    fn tiles_down(&self) -> usize {
        self.h.div_ceil(self.th)
    }
}

#[derive(Clone, Debug)]
pub struct Cog {
    url: String,
    le: bool,
    pub levels: Vec<Level>,
    bps: u16,
    sample_format: u16,
    compression: u16,
    predictor: u16,
    nodata: Option<f64>,
    /// full-resolution pixel size and upper-left corner (map metres)
    sx: f64,
    sy: f64,
    x0: f64,
    y0: f64,
    pub size_hint: u64,
}

struct Entry {
    tag: u16,
    typ: u16,
    count: u64,
    /// inline value bytes, or the offset of the value
    inline: Option<Vec<u8>>,
    offset: u64,
}

fn type_size(t: u16) -> u64 {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => 1,
    }
}

struct Head {
    url: String,
    buf: Vec<u8>,
    le: bool,
    big: bool,
}

impl Head {
    fn u16(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.le { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) }
    }
    fn u32(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
    }
    fn u64(&self, b: &[u8]) -> u64 {
        let a: [u8; 8] = b[..8].try_into().unwrap();
        if self.le { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) }
    }
    fn f64(&self, b: &[u8]) -> f64 {
        f64::from_bits(self.u64(b))
    }

    /// `len` bytes at `off`, from the head buffer or a new range request.
    async fn bytes(&self, off: u64, len: u64) -> Result<Vec<u8>, String> {
        if off + len <= self.buf.len() as u64 {
            return Ok(self.buf[off as usize..(off + len) as usize].to_vec());
        }
        let v = get(&self.url, Some((off, off + len.max(1) - 1))).await?;
        if (v.len() as u64) < len {
            return Err(format!("{}: short read at {off}", self.url));
        }
        Ok(v)
    }

    async fn ifd(&self, off: u64) -> Result<(Vec<Entry>, u64), String> {
        let (cnt_sz, esz, inl) = if self.big { (8u64, 20u64, 8usize) } else { (2, 12, 4) };
        let n_raw = self.bytes(off, cnt_sz).await?;
        let n = if self.big { self.u64(&n_raw) } else { self.u16(&n_raw) as u64 };
        let raw = self.bytes(off + cnt_sz, n * esz + cnt_sz.max(4)).await?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            let e = &raw[i * esz as usize..(i + 1) * esz as usize];
            let (tag, typ) = (self.u16(&e[0..2]), self.u16(&e[2..4]));
            let count = if self.big { self.u64(&e[4..12]) } else { self.u32(&e[4..8]) as u64 };
            let v = &e[esz as usize - inl..];
            let nbytes = count * type_size(typ);
            if nbytes <= inl as u64 {
                out.push(Entry { tag, typ, count, inline: Some(v[..nbytes as usize].to_vec()), offset: 0 });
            } else {
                let offset = if self.big { self.u64(v) } else { self.u32(v) as u64 };
                out.push(Entry { tag, typ, count, inline: None, offset });
            }
        }
        let q = &raw[(n * esz) as usize..];
        let next = if self.big { self.u64(q) } else { self.u32(q) as u64 };
        Ok((out, next))
    }

    async fn raw(&self, e: &Entry) -> Result<Vec<u8>, String> {
        match &e.inline {
            Some(v) => Ok(v.clone()),
            None => self.bytes(e.offset, e.count * type_size(e.typ)).await,
        }
    }

    async fn uints(&self, e: &Entry) -> Result<Vec<u64>, String> {
        let b = self.raw(e).await?;
        let sz = type_size(e.typ) as usize;
        Ok(b.chunks_exact(sz)
            .map(|c| match sz {
                1 => c[0] as u64,
                2 => self.u16(c) as u64,
                4 => self.u32(c) as u64,
                _ => self.u64(c),
            })
            .collect())
    }

    async fn doubles(&self, e: &Entry) -> Result<Vec<f64>, String> {
        let b = self.raw(e).await?;
        Ok(b.chunks_exact(8).map(|c| self.f64(c)).collect())
    }
}

impl Cog {
    pub async fn open(url: &str) -> Result<Cog, String> {
        let buf = get(url, Some((0, HEAD - 1))).await?;
        if buf.len() < 16 {
            return Err(format!("{url}: not a TIFF"));
        }
        let le = match &buf[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return Err(format!("{url}: not a TIFF")),
        };
        let mut h = Head { url: url.to_string(), buf, le, big: false };
        let ver = h.u16(&h.buf[2..4]);
        h.big = ver == 43;
        let mut off = if h.big { h.u64(&h.buf[8..16]) } else { h.u32(&h.buf[4..8]) as u64 };
        let mut cog = Cog {
            url: url.to_string(),
            le,
            levels: Vec::new(),
            bps: 8,
            sample_format: 1,
            compression: 1,
            predictor: 1,
            nodata: None,
            sx: 1.0,
            sy: 1.0,
            x0: 0.0,
            y0: 0.0,
            size_hint: 0,
        };
        let mut first = true;
        while off != 0 && cog.levels.len() < 32 {
            let (entries, next) = h.ifd(off).await?;
            off = next;
            let get1 = |tag: u16| entries.iter().find(|e| e.tag == tag);
            let one = async |tag: u16, dflt: u64| -> Result<u64, String> {
                match get1(tag) {
                    Some(e) => Ok(h.uints(e).await?.first().copied().unwrap_or(dflt)),
                    None => Ok(dflt),
                }
            };
            if one(254, 0).await? & 4 != 0 {
                continue; // transparency mask
            }
            let (w, ht) = (one(256, 0).await? as usize, one(257, 0).await? as usize);
            let (tw, th) = (one(322, 0).await? as usize, one(323, 0).await? as usize);
            if tw == 0 || th == 0 {
                return Err(format!("{url}: striped TIFF (not a COG)"));
            }
            if one(277, 1).await? != 1 {
                return Err(format!("{url}: more than one band"));
            }
            if first {
                cog.bps = one(258, 8).await? as u16;
                cog.sample_format = one(339, 1).await? as u16;
                cog.compression = one(259, 1).await? as u16;
                cog.predictor = one(317, 1).await? as u16;
                if let Some(e) = get1(42113) {
                    let s = String::from_utf8_lossy(&h.raw(e).await?).trim_matches(char::from(0)).trim().to_string();
                    cog.nodata = s.parse().ok();
                }
                let scale = match get1(33550) {
                    Some(e) => h.doubles(e).await?,
                    None => return Err(format!("{url}: no ModelPixelScale")),
                };
                let tie = match get1(33922) {
                    Some(e) => h.doubles(e).await?,
                    None => return Err(format!("{url}: no ModelTiepoint")),
                };
                cog.sx = scale[0];
                cog.sy = scale[1];
                cog.x0 = tie[3] - tie[0] * cog.sx;
                cog.y0 = tie[4] + tie[1] * cog.sy;
                first = false;
            }
            let (Some(eo), Some(ec)) = (get1(324), get1(325)) else {
                return Err(format!("{url}: no TileOffsets"));
            };
            let offsets = h.uints(eo).await?;
            let counts = h.uints(ec).await?;
            cog.levels.push(Level { w, h: ht, tw, th, offsets, counts });
        }
        if cog.levels.is_empty() {
            return Err(format!("{url}: no image"));
        }
        match cog.compression {
            1 | 5 | 8 | 32946 => {}
            c => return Err(format!("{url}: unsupported compression {c}")),
        }
        if !matches!(cog.bps, 8 | 16 | 32 | 64) {
            return Err(format!("{url}: unsupported {} bits per sample", cog.bps));
        }
        cog.size_hint = cog.levels.iter().flat_map(|l| l.counts.iter()).sum();
        Ok(cog)
    }

    /// Pixel size of a level (map units).
    pub fn res(&self, l: usize) -> (f64, f64) {
        let (l0, lv) = (&self.levels[0], &self.levels[l]);
        (self.sx * l0.w as f64 / lv.w as f64, self.sy * l0.h as f64 / lv.h as f64)
    }

    fn is_nodata(&self, v: f64) -> bool {
        if !v.is_finite() || v.abs() > 1e30 {
            return true;
        }
        match self.nodata {
            Some(n) if n < -1e30 => v <= n * 0.999,
            Some(n) => v == n,
            None => false,
        }
    }

    fn decode(&self, lv: &Level, raw: &[u8]) -> Result<Vec<f32>, String> {
        let bpp = self.bps as usize / 8;
        let need = lv.tw * lv.th * bpp;
        let mut b = match self.compression {
            1 => raw.to_vec(),
            8 | 32946 => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(raw, need)
                .map_err(|e| format!("{}: inflate: {e:?}", self.url))?,
            5 => weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
                .decode(raw)
                .map_err(|e| format!("{}: LZW: {e:?}", self.url))?,
            c => return Err(format!("unsupported compression {c}")),
        };
        b.resize(need, 0);
        let row = lv.tw * bpp;
        match self.predictor {
            2 => {
                for r in b.chunks_exact_mut(row) {
                    match bpp {
                        1 => {
                            for i in 1..r.len() {
                                r[i] = r[i].wrapping_add(r[i - 1]);
                            }
                        }
                        2 => {
                            let mut p = 0u16;
                            for c in r.chunks_exact_mut(2) {
                                let v = if self.le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) };
                                p = p.wrapping_add(v);
                                c.copy_from_slice(&if self.le { p.to_le_bytes() } else { p.to_be_bytes() });
                            }
                        }
                        4 => {
                            let mut p = 0u32;
                            for c in r.chunks_exact_mut(4) {
                                let a = [c[0], c[1], c[2], c[3]];
                                let v = if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) };
                                p = p.wrapping_add(v);
                                c.copy_from_slice(&if self.le { p.to_le_bytes() } else { p.to_be_bytes() });
                            }
                        }
                        _ => {
                            let mut p = 0u64;
                            for c in r.chunks_exact_mut(8) {
                                let a: [u8; 8] = c[..8].try_into().unwrap();
                                let v = if self.le { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) };
                                p = p.wrapping_add(v);
                                c.copy_from_slice(&if self.le { p.to_le_bytes() } else { p.to_be_bytes() });
                            }
                        }
                    }
                }
            }
            3 => {
                // floating-point predictor: byte-wise differencing over the row, bytes stored
                // most-significant plane first; rebuild in the file's byte order
                let w = lv.tw;
                let mut tmp = vec![0u8; row];
                for r in b.chunks_exact_mut(row) {
                    for i in 1..row {
                        r[i] = r[i].wrapping_add(r[i - 1]);
                    }
                    tmp.copy_from_slice(r);
                    for i in 0..w {
                        for k in 0..bpp {
                            let src = tmp[k * w + i]; // plane k = k-th most significant byte
                            let dst = if self.le { bpp - 1 - k } else { k };
                            r[i * bpp + dst] = src;
                        }
                    }
                }
            }
            _ => {}
        }
        let le = self.le;
        let conv = |c: &[u8]| -> f64 {
            macro_rules! rd {
                ($t:ty, $n:expr) => {{
                    let a: [u8; $n] = c[..$n].try_into().unwrap();
                    (if le { <$t>::from_le_bytes(a) } else { <$t>::from_be_bytes(a) }) as f64
                }};
            }
            match (self.sample_format, self.bps) {
                (3, 32) => rd!(f32, 4),
                (3, 64) => rd!(f64, 8),
                (2, 8) => c[0] as i8 as f64,
                (2, 16) => rd!(i16, 2),
                (2, 32) => rd!(i32, 4),
                (_, 8) => c[0] as f64,
                (_, 16) => rd!(u16, 2),
                (_, 32) => rd!(u32, 4),
                _ => rd!(u64, 8),
            }
        };
        Ok(b.chunks_exact(bpp).map(|c| {
            let v = conv(c);
            if self.is_nodata(v) { f32::NAN } else { v as f32 }
        }).collect())
    }

    /// Read tiles [tc0, tc1) x [tr0, tr1) of a level into one raster (NaN = nodata / missing),
    /// reporting downloaded bytes through `on_bytes`.
    async fn read_window(
        &self,
        l: usize,
        (tr0, tr1, tc0, tc1): (usize, usize, usize, usize),
        mut on_bytes: impl FnMut(u64, u64),
    ) -> Result<Raster, String> {
        let lv = &self.levels[l];
        let (ww, wh) = ((tc1 - tc0) * lv.tw, (tr1 - tr0) * lv.th);
        let mut px = vec![f32::NAN; ww * wh];
        let mut tiles: Vec<(usize, usize, u64, u64)> = Vec::new();
        for tr in tr0..tr1 {
            for tc in tc0..tc1 {
                let i = tr * lv.tiles_across() + tc;
                if let (Some(&o), Some(&n)) = (lv.offsets.get(i), lv.counts.get(i)) {
                    if n > 0 {
                        tiles.push((tr, tc, o, n));
                    }
                }
            }
        }
        tiles.sort_by_key(|t| t.2);
        let mut groups: Vec<(u64, u64, Vec<usize>)> = Vec::new();
        for (i, t) in tiles.iter().enumerate() {
            match groups.last_mut() {
                Some(g) if t.2 >= g.1 && t.2 - g.1 <= GAP && t.2 + t.3 - g.0 <= MAX_REQ => {
                    g.1 = t.2 + t.3;
                    g.2.push(i);
                }
                _ => groups.push((t.2, t.2 + t.3, vec![i])),
            }
        }
        let total: u64 = groups.iter().map(|g| g.1 - g.0).sum();
        let ranges: Vec<(u64, u64)> = groups.iter().map(|g| (g.0, g.1 - 1)).collect();
        let mut batch = Batch::start(&self.url, &ranges);
        let mut got = 0u64;
        on_bytes(0, total);
        while let Some(r) = batch.next().await {
            let (gi, data) = r?;
            let (g0, _, members) = &groups[gi];
            got += data.len() as u64;
            for &ti in members {
                let (tr, tc, o, n) = tiles[ti];
                let s = (o - g0) as usize;
                let raw = data.get(s..s + n as usize).ok_or_else(|| format!("{}: short range", self.url))?;
                let t = self.decode(lv, raw)?;
                let (r0, c0) = ((tr - tr0) * lv.th, (tc - tc0) * lv.tw);
                // edge tiles are padded (with 0, not nodata) past the raster size: crop
                let (nr, nc) = (lv.th.min(lv.h - tr * lv.th), lv.tw.min(lv.w - tc * lv.tw));
                for rr in 0..nr {
                    let dst = (r0 + rr) * ww + c0;
                    px[dst..dst + nc].copy_from_slice(&t[rr * lv.tw..rr * lv.tw + nc]);
                }
            }
            on_bytes(got, total);
        }
        let (rx, ry) = self.res(l);
        Ok(Raster { px, w: ww, h: wh, x0: self.x0 + (tc0 * lv.tw) as f64 * rx, y0: self.y0 - (tr0 * lv.th) as f64 * ry, rx, ry, bytes: got })
    }
}

/// A georeferenced window of one COG level (pixel-is-area; (x0, y0) = upper-left corner).
struct Raster {
    px: Vec<f32>,
    w: usize,
    h: usize,
    x0: f64,
    y0: f64,
    rx: f64,
    ry: f64,
    bytes: u64,
}

impl Raster {
    /// Bilinear sample at map (x, y); nodata-aware (valid neighbours re-weighted). NaN if the
    /// nearest pixel is nodata or outside.
    fn sample(&self, x: f64, y: f64) -> f32 {
        let fc = (x - self.x0) / self.rx - 0.5;
        let fr = (self.y0 - y) / self.ry - 0.5;
        if !(fc > -0.5 && fr > -0.5 && fc < self.w as f64 - 0.5 && fr < self.h as f64 - 0.5) {
            return f32::NAN;
        }
        let (nc, nr) = (fc.round().clamp(0.0, self.w as f64 - 1.0) as usize, fr.round().clamp(0.0, self.h as f64 - 1.0) as usize);
        if !self.px[nr * self.w + nc].is_finite() {
            return f32::NAN;
        }
        let (c0, r0) = (fc.floor().max(0.0) as usize, fr.floor().max(0.0) as usize);
        let (c1, r1) = ((c0 + 1).min(self.w - 1), (r0 + 1).min(self.h - 1));
        let (tc, tr) = ((fc - c0 as f64).clamp(0.0, 1.0), (fr - r0 as f64).clamp(0.0, 1.0));
        let (mut s, mut n) = (0.0f64, 0.0f64);
        for (r, c, wgt) in [(r0, c0, (1.0 - tr) * (1.0 - tc)), (r0, c1, (1.0 - tr) * tc), (r1, c0, tr * (1.0 - tc)), (r1, c1, tr * tc)] {
            let v = self.px[r * self.w + c];
            if v.is_finite() && wgt > 0.0 {
                s += v as f64 * wgt;
                n += wgt;
            }
        }
        if n > 0.0 { (s / n) as f32 } else { f32::NAN }
    }
}

// ------------------------------------------------------------------------------------ texture

/// Same tone mapping as converter/hirise2engine.make_texture_mapping, from a DN histogram.
struct Mapping {
    lo: f64,
    hi: f64,
    gamma: f64,
    dither: f64,
}

fn mapping(hist: &[u64]) -> Mapping {
    let n: u64 = hist[1..].iter().sum();
    if n == 0 {
        return Mapping { lo: 0.0, hi: 1.0, gamma: 1.0, dither: 0.0 };
    }
    let pct = |p: f64| -> f64 {
        let want = (p / 100.0 * (n - 1) as f64).round() as u64;
        let mut acc = 0;
        for (i, &c) in hist.iter().enumerate().skip(1) {
            acc += c;
            if acc > want {
                return i as f64;
            }
        }
        (hist.len() - 1) as f64
    };
    let vmax = hist.iter().rposition(|&c| c > 0).unwrap_or(0) as f64;
    let (mut lo, mut hi, med) = (pct(0.05), pct(99.95), pct(50.0));
    let min_win = 254.0 / 4.0;
    if hi - lo < min_win {
        let c = (lo + hi) / 2.0;
        (lo, hi) = (c - min_win / 2.0, c + min_win / 2.0);
        if lo < 1.0 {
            (lo, hi) = (1.0, 1.0 + min_win);
        }
    }
    let xm = ((med - lo) / (hi - lo)).clamp(0.05, 0.95);
    let gamma = (0.45f64.ln() / xm.ln()).clamp(0.75, 1.33);
    let dither = if 254.0 / (hi - lo) > 2.0 && vmax <= 255.0 { 0.5 } else { 0.0 };
    Mapping { lo, hi, gamma, dither }
}

fn hash01(r: u32, c: u32) -> f64 {
    let mut h = r.wrapping_mul(0x9E3779B1) ^ c.wrapping_mul(0x85EBCA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A2D39);
    h ^= h >> 15;
    h as f64 / 4294967296.0
}

// ------------------------------------------------------------------------------------ load

#[allow(dead_code)] // read by the native --live-dump only
pub struct Stats {
    pub dtm_bytes: u64,
    pub ortho_bytes: u64,
    pub dtm_level: usize,
    pub ortho_level: Option<usize>,
}

/// Fetch the index, then the DTM (+ ortho) of `id`, and build a Grid. `min_mpp` = finest post
/// spacing wanted (default ~2 m).
pub async fn load_grid(index: Vec<u8>, id: &str, min_mpp: Option<f32>, progress: &Progress) -> Result<(Grid, Stats), String> {
    let spec = spec_from_index(&index, id)?;
    drop(index);
    let t0 = bevy::platform::time::Instant::now();
    let mb = |b: u64| b as f64 / 1e6;
    say(progress, format!("{}\nreading the DTM header from NASA (astrogeo-ard)", spec.title));
    let dtm = Cog::open(&spec.dtm).await?;
    let [kx, ky] = spec.k;
    let true_res = |c: &Cog, l: usize| {
        let (rx, ry) = c.res(l);
        (rx / kx, ry / ky)
    };
    // level: the finest whose coarser axis is >= min_mpp (~2 m); then the post spacing that keeps
    // the data under budget (estimated from the non-empty COG tiles); if that spacing gets near the
    // next overview, or this level is a big download, read the next one instead (4x fewer bytes)
    let want = min_mpp.unwrap_or(1.8) as f64 * 0.999;
    let coarse = |l: usize| {
        let (rx, ry) = true_res(&dtm, l);
        rx.max(ry)
    };
    let need_mpp = |l: usize| {
        let lv = &dtm.levels[l];
        let (rx, ry) = true_res(&dtm, l);
        let full = lv.counts.iter().filter(|&&c| c > 4096).count() as f64 / lv.counts.len().max(1) as f64;
        let area = full * (lv.w as f64 * rx) * (lv.h as f64 * ry) * 1.3; // + tile padding
        (area / MAX_DATA_POSTS as f64).sqrt()
    };
    let n_lv = dtm.levels.len();
    let mut level = (0..n_lv).find(|&l| coarse(l) >= want).unwrap_or(n_lv - 1);
    let bytes = |l: usize| dtm.levels[l].counts.iter().sum::<u64>();
    while level + 1 < n_lv
        && (need_mpp(level) >= coarse(level + 1) * 0.8 || (bytes(level) > MAX_DTM_BYTES && coarse(level + 1) <= 6.0))
    {
        level += 1;
    }
    let mpp0 = coarse(level).max(need_mpp(level));
    let lv = dtm.levels[level].clone();
    let (rx, ry) = true_res(&dtm, level);
    let title = spec.title.clone();
    let dl = |what: &'static str| {
        let title = title.clone();
        move |got: u64, total: u64| say(progress, format!("{title}\n{what}: {:.1} / {:.1} MB from NASA", mb(got), mb(total)))
    };
    let src = dtm.read_window(level, (0, lv.tiles_down(), 0, lv.tiles_across()), dl("DTM")).await?;
    let dtm_bytes = src.bytes;
    say(progress, format!("{}\nresampling heights", spec.title));

    // valid footprint in true metres relative to the centre
    let (cx, cy) = (spec.c[0], spec.c[1]);
    let (mut rmin, mut rmax, mut cmin, mut cmax) = (usize::MAX, 0, usize::MAX, 0);
    let (mut zmin, mut zmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for r in 0..src.h {
        for c in 0..src.w {
            let v = src.px[r * src.w + c];
            if v.is_finite() {
                (rmin, rmax, cmin, cmax) = (rmin.min(r), rmax.max(r), cmin.min(c), cmax.max(c));
                (zmin, zmax) = (zmin.min(v), zmax.max(v));
            }
        }
    }
    if rmin == usize::MAX {
        return Err(format!("{}: the DTM has no valid data", spec.id));
    }
    let e_of = |c: f64| (src.x0 + c * src.rx - cx) / kx;
    let n_of = |r: f64| (src.y0 - r * src.ry - cy) / ky;
    let (emin, emax) = (e_of(cmin as f64), e_of(cmax as f64 + 1.0));
    let (nmax, nmin) = (n_of(rmin as f64), n_of(rmax as f64 + 1.0));
    let mut mpp = mpp0;
    let grid = |mpp: f64| {
        let (kx0, kx1) = ((emin / mpp).floor() as i64, (emax / mpp).ceil() as i64);
        let (ky0, ky1) = ((nmin / mpp).floor() as i64, (nmax / mpp).ceil() as i64);
        let nx = ((kx1 - kx0) as usize).div_ceil(STEP).max(1);
        let ny = ((ky1 - ky0) as usize).div_ceil(STEP).max(1);
        let kx0 = kx0 - ((nx * STEP) as i64 - (kx1 - kx0)) / 2;
        let ky1 = ky1 + ((ny * STEP) as i64 - (ky1 - ky0)) / 2;
        (kx0, ky1, nx, ny)
    };
    // shrink if the dense grid is over budget (rare: huge or diagonal footprints)
    loop {
        let (_, _, nx, ny) = grid(mpp);
        if (nx * STEP + 1) * (ny * STEP + 1) <= MAX_GRID_POSTS {
            break;
        }
        mpp *= 1.25;
    }
    let mut z;
    let (mut kx0, mut ky1, mut nx, mut ny);
    loop {
        (kx0, ky1, nx, ny) = grid(mpp);
        let (w, h) = (nx * STEP + 1, ny * STEP + 1);
        z = vec![f32::NAN; w * h];
        for i in 0..h {
            let gy = cy + (ky1 - i as i64) as f64 * mpp * ky;
            for j in 0..w {
                let gx = cx + (kx0 + j as i64) as f64 * mpp * kx;
                z[i * w + j] = src.sample(gx, gy);
            }
        }
        let data_tiles = (0..ny * nx).filter(|&t| tile_has_data(&z, w, t / nx, t % nx)).count();
        if data_tiles * (STEP + 1) * (STEP + 1) <= MAX_DATA_POSTS * 5 / 4 {
            break;
        }
        mpp *= ((data_tiles * (STEP + 1) * (STEP + 1)) as f64 / MAX_DATA_POSTS as f64).sqrt();
    }
    drop(src);
    let (w, h) = (nx * STEP + 1, ny * STEP + 1);
    let base = zmin as f64;
    for v in z.iter_mut() {
        *v -= zmin;
    }
    let range = ((zmax - zmin) as f64).max(1e-3);
    let mut tiles: Vec<GridTile> = (0..ny * nx)
        .filter(|&t| tile_has_data(&z, w, t / nx, t % nx))
        .map(|t| GridTile { x: t % nx, y: t / nx, tex: Tex::None })
        .collect();
    info!(
        "live {}: DTM level {level} ({:.2} x {:.2} m true), grid {w}x{h} at {mpp:.2} m, {} tiles, {:.1} MB",
        spec.id, rx, ry, tiles.len(), mb(dtm_bytes)
    );

    // ortho
    let mut ortho_bytes = 0;
    let mut ortho_level = None;
    if let Some(ourl) = &spec.ortho {
        match load_ortho(&spec, ourl, &z, w, (kx0, ky1, mpp), &mut tiles, progress).await {
            Ok((b, l)) => {
                ortho_bytes = b;
                ortho_level = Some(l);
            }
            Err(e) => warn!("live {}: ortho failed, drawing untextured: {e}", spec.id),
        }
    }
    info!(
        "live {}: downloaded {:.1} MB (DTM {:.1} + ortho {:.1}) in {:.1} s",
        spec.id,
        mb(dtm_bytes + ortho_bytes),
        mb(dtm_bytes),
        mb(ortho_bytes),
        t0.elapsed().as_secs_f32()
    );
    let g = Grid {
        name: spec.id.clone(),
        rationale: spec.title.clone(),
        lat: spec.lat,
        lon: spec.lon,
        base_m: base,
        mpp: mpp as f32,
        east0: (kx0 as f64 * mpp) as f32,
        north0: (ky1 as f64 * mpp) as f32,
        w,
        h,
        z,
        quantum: (range / 65534.0) as f32,
        sun: spec.sun.map(|(az, el)| (((az as f64 + spec.conv).rem_euclid(360.0)) as f32, el)),
        tile_step: STEP,
        tiles,
        texture_kind: if ortho_level.is_some() { "ortho" } else { "none" },
    };
    Ok((g, Stats { dtm_bytes, ortho_bytes, dtm_level: level, ortho_level }))
}

fn tile_has_data(z: &[f32], w: usize, tr: usize, tc: usize) -> bool {
    (tr * STEP..=tr * STEP + STEP).any(|r| z[r * w + tc * STEP..=r * w + tc * STEP + STEP].iter().any(|v| v.is_finite()))
}

async fn load_ortho(
    spec: &Spec,
    url: &str,
    z: &[f32],
    w: usize,
    (kx0, ky1, mpp): (i64, i64, f64),
    tiles: &mut [GridTile],
    progress: &Progress,
) -> Result<(u64, usize), String> {
    say(progress, format!("{}\nreading the ortho header", spec.title));
    let cog = Cog::open(url).await?;
    let [kx, ky] = spec.k;
    let (cx, cy) = (spec.c[0], spec.c[1]);
    // coarsest level not much coarser than the grid
    let mut level = 0;
    for l in 0..cog.levels.len() {
        let (rx, ry) = cog.res(l);
        if (rx / kx).max(ry / ky) <= mpp * 1.3 {
            level = l;
        }
    }
    // map (DTM CRS) -> ortho file coordinates (undo the northing fix and shift)
    let to_file = |gx: f64, gy: f64| (gx - spec.osh[0], (gy - spec.osh[1]) / spec.oys);
    let post_map = |i: usize, j: usize| (cx + (kx0 + j as i64) as f64 * mpp * kx, cy + (ky1 - i as i64) as f64 * mpp * ky);
    let lv = cog.levels[level].clone();
    let (rx, ry) = cog.res(level);
    let (mut c0, mut c1, mut r0, mut r1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
    for t in tiles.iter() {
        for (i, j) in [(t.y * STEP, t.x * STEP), (t.y * STEP + STEP, t.x * STEP + STEP)] {
            let (fx, fy) = to_file(post_map(i, j).0, post_map(i, j).1);
            let (c, r) = ((fx - cog.x0) / rx, (cog.y0 - fy) / ry);
            (c0, c1, r0, r1) = (c0.min(c), c1.max(c), r0.min(r), r1.max(r));
        }
    }
    let clampt = |v: f64, n: usize, t: usize| ((v / t as f64).floor().max(0.0) as usize).min(n);
    let (tc0, tc1) = (clampt(c0 - 2.0, lv.tiles_across(), lv.tw), clampt(c1 + 2.0, lv.tiles_across() - 1, lv.tw) + 1);
    let (tr0, tr1) = (clampt(r0 - 2.0, lv.tiles_down(), lv.th), clampt(r1 + 2.0, lv.tiles_down() - 1, lv.th) + 1);
    if tc0 >= tc1 || tr0 >= tr1 {
        return Err("ortho does not overlap the DTM".into());
    }
    let title = spec.title.clone();
    let src = cog
        .read_window(level, (tr0, tr1, tc0, tc1), |got, total| {
            say(progress, format!("{title}\northo image: {:.1} / {:.1} MB from NASA", got as f64 / 1e6, total as f64 / 1e6))
        })
        .await?;
    say(progress, format!("{}\ndraping the ortho", spec.title));
    let tp = STEP + 1;
    let mut dn: Vec<Vec<f32>> = Vec::with_capacity(tiles.len());
    let mut hist = vec![0u64; 65536];
    for t in tiles.iter() {
        let mut v = vec![f32::NAN; tp * tp];
        for i in 0..tp {
            for j in 0..tp {
                let (gi, gj) = (t.y * STEP + i, t.x * STEP + j);
                let (fx, fy) = {
                    let (gx, gy) = post_map(gi, gj);
                    to_file(gx, gy)
                };
                let d = src.sample(fx, fy);
                if d > 0.0 {
                    v[i * tp + j] = d;
                    // converter samples on a 4 m lattice; histogram only posts with heights
                    if z[gi * w + gj].is_finite() {
                        hist[(d.round() as usize).min(65535)] += 1;
                    }
                }
            }
        }
        dn.push(v);
    }
    let bytes = src.bytes;
    drop(src);
    let m = mapping(&hist);
    for (t, v) in tiles.iter_mut().zip(dn) {
        let px: Vec<u16> = v
            .iter()
            .enumerate()
            .map(|(k, &d)| {
                if !(d > 0.0) {
                    return 0;
                }
                let (i, j) = (k / tp, k % tp);
                let mut x = d as f64;
                if m.dither > 0.0 {
                    x += (hash01((t.y * STEP + i) as u32, (t.x * STEP + j) as u32) - 0.5) * 2.0 * m.dither;
                }
                let y = ((x - m.lo) / (m.hi - m.lo)).clamp(0.0, 1.0).powf(m.gamma);
                (1.0 + 254.0 * y).round().clamp(1.0, 255.0) as u16
            })
            .collect();
        if px.iter().any(|&p| p > 0) {
            t.tex = Tex::Px(px, tp, tp);
        }
    }
    info!(
        "live {}: ortho level {level} ({:.2} m), tone map DN {:.0}..{:.0} gamma {:.3}, northing scale {}, {:.1} MB",
        spec.id,
        (rx / kx).max(ry / ky),
        m.lo,
        m.hi,
        m.gamma,
        spec.oys,
        bytes as f64 / 1e6
    );
    Ok((bytes, level))
}
