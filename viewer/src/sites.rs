//! Site catalogue for the main menu: the converted sites on disk (or next to the web page) plus the
//! 1,262 HiRISE DTMs that live mode streams straight from NASA (`live_index.json`).
//!
//! Native: every sub-folder of the sites directory (default `D:\marsmaker_data\sites`, or
//! `--sites-dir DIR`) that has a `meta.json`; `*.tmp` folders (conversions in progress) are skipped.
//! Browser: the folder names listed in `sites.json` next to the page (a JSON array of names, or of
//! objects with a `name`), falling back to the two sites the default web build ships; each site's
//! `sites/<name>/meta.json` is then fetched for its title and position.

use serde_json::Value;

/// Where a catalogue entry is loaded from.
#[derive(Clone, Debug, PartialEq)]
pub enum Where {
    /// A converted site: folder path (native) or URL prefix `sites/<name>` (browser).
    Local(String),
    /// A site id in live_index.json, streamed from NASA.
    Live(String),
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// folder name / live id (HiRISE pair id, e.g. PSP_007018_1255_PSP_007229_1255)
    pub id: String,
    pub title: String,
    pub lat: f64,
    /// east longitude, -180..180
    pub lon: f64,
    pub size_km: Option<(f32, f32)>,
    /// DTM post spacing (m)
    pub res_m: Option<f32>,
    /// live: approximate DTM download size (MB)
    pub mb: Option<f32>,
    pub at: Where,
    /// lowercase id + title, for search
    pub key: String,
}

impl Entry {
    pub fn is_live(&self) -> bool {
        matches!(self.at, Where::Live(_))
    }
    pub fn polar(&self) -> bool {
        self.lat.abs() >= 60.0
    }
}

/// Search filters (menu chips): label and the words any of which must appear in the title/id.
/// "polar" is by latitude instead (|lat| >= 60).
pub const FILTERS: &[(&str, &[&str])] = &[
    ("all", &[]),
    ("dunes", &["dune"]),
    ("craters", &["crater"]),
    ("polar", &[]),
    ("gullies", &["gull"]),
    ("layers", &["layer"]),
    ("channels", &["channel", "valley", "valles", "vallis", "fluvial", "delta", "fan"]),
    ("landing sites", &["landing", "msl", "rover", "lander", "phoenix", "insight", "2020", "exomars"]),
    ("ice", &["ice", "frost", "glacier", "polar"]),
];

pub fn matches(e: &Entry, filter: usize, words: &[String]) -> bool {
    let ok = match FILTERS.get(filter) {
        Some(("polar", _)) => e.polar(),
        Some((_, ws)) if !ws.is_empty() => ws.iter().any(|w| e.key.contains(w)),
        _ => true,
    };
    ok && words.iter().all(|w| e.key.contains(w.as_str()))
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn wrap_lon(lon: f64) -> f64 {
    if lon > 180.0 { lon - 360.0 } else { lon }
}

/// A converted site's meta.json -> catalogue entry.
pub fn from_meta(name: &str, at: Where, bytes: &[u8]) -> Result<Entry, String> {
    let m: Value = serde_json::from_slice(bytes).map_err(|e| format!("{name}/meta.json: {e}"))?;
    let desc = m["description"].as_str().unwrap_or("");
    let title = desc
        .split("rationale for creation was: ")
        .nth(1)
        .and_then(|s| s.split(". This DTM").next())
        .unwrap_or(name)
        .trim()
        .to_string();
    let mpp = m["metres_per_pixel"].as_f64().unwrap_or(1.0);
    let size_km = m["grid_px"].as_array().filter(|a| a.len() == 2).map(|a| ((f(&a[0]) * mpp / 1000.0) as f32, (f(&a[1]) * mpp / 1000.0) as f32));
    Ok(Entry {
        key: format!("{name} {title}").to_lowercase(),
        id: name.to_string(),
        title,
        lat: f(&m["center_lat"]),
        lon: wrap_lon(f(&m["center_lon_east"])),
        size_km,
        res_m: Some(mpp as f32),
        mb: None,
        at,
    })
}

/// live_index.json (`{"base", "fields", "sites": [[...]]}`) -> catalogue entries.
pub fn from_live_index(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("live_index.json: {e}"))?;
    let fields: Vec<&str> = v["fields"].as_array().ok_or("live_index.json: no fields")?.iter().filter_map(|f| f.as_str()).collect();
    let col = |n: &str| fields.iter().position(|f| *f == n);
    let (ci, ct, clat, clon) = (col("id").ok_or("no id field")?, col("title"), col("lat"), col("lon"));
    let (cres, cw, ch, cmb) = (col("res"), col("w"), col("h"), col("mb"));
    let get = |r: &Value, c: Option<usize>| c.map(|c| r[c].clone()).unwrap_or(Value::Null);
    let mut out = Vec::new();
    for r in v["sites"].as_array().ok_or("live_index.json: no sites")? {
        let Some(id) = r[ci].as_str() else { continue };
        let title = get(r, ct).as_str().unwrap_or(id).trim().to_string();
        let res = get(r, cres).as_f64();
        let size_km = match (res, get(r, cw).as_f64(), get(r, ch).as_f64()) {
            (Some(res), Some(w), Some(h)) => Some(((w * res / 1000.0) as f32, (h * res / 1000.0) as f32)),
            _ => None,
        };
        out.push(Entry {
            key: format!("{id} {title}").to_lowercase(),
            id: id.to_string(),
            title,
            lat: f(&get(r, clat)),
            lon: wrap_lon(f(&get(r, clon))),
            size_km,
            res_m: res.map(|r| r as f32),
            mb: get(r, cmb).as_f64().map(|m| m as f32),
            at: Where::Live(id.to_string()),
        });
    }
    Ok(out)
}

/// Everything the menu can offer: local sites first (sorted by title), then the live ones.
/// Problems (missing dir, bad index) come back as notes for the menu to show; never fatal.
pub async fn load_catalog(sites_dir: String, live_index: String) -> (Vec<Entry>, Vec<String>) {
    let mut notes = Vec::new();
    let mut local = match local_sites(&sites_dir).await {
        Ok(v) => v,
        Err(e) => {
            notes.push(e);
            Vec::new()
        }
    };
    local.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    let live = match read_live_index(&live_index).await.and_then(|b| from_live_index(&b)) {
        Ok(v) => v,
        Err(e) => {
            notes.push(e);
            Vec::new()
        }
    };
    local.extend(live);
    (local, notes)
}

#[cfg(not(target_arch = "wasm32"))]
async fn read_live_index(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

#[cfg(target_arch = "wasm32")]
async fn read_live_index(url: &str) -> Result<Vec<u8>, String> {
    crate::live::get(url, None).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn local_sites(dir: &str) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("sites dir {dir}: {e}"))?;
    let mut out = Vec::new();
    for d in rd.flatten() {
        let name = d.file_name().to_string_lossy().to_string();
        if name.ends_with(".tmp") || !d.path().is_dir() {
            continue;
        }
        let Ok(bytes) = std::fs::read(d.path().join("meta.json")) else { continue };
        match from_meta(&name, Where::Local(d.path().to_string_lossy().to_string()), &bytes) {
            Ok(e) => out.push(e),
            Err(e) => bevy::log::warn!("{e}"),
        }
    }
    Ok(out)
}

/// Sites the default web build ships (WEB.md) when there is no sites.json.
#[cfg(target_arch = "wasm32")]
const WEB_DEFAULT_SITES: [&str; 2] = ["PSP_007018_1255_PSP_007229_1255", "PSP_003800_1325_PSP_004077_1325"];

#[cfg(target_arch = "wasm32")]
async fn local_sites(_dir: &str) -> Result<Vec<Entry>, String> {
    use crate::live::get;
    let names: Vec<String> = match get("sites.json", None).await.ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from).or_else(|| v["name"].as_str().map(String::from)))
            .collect(),
        _ => WEB_DEFAULT_SITES.iter().map(|s| s.to_string()).collect(),
    };
    let mut out = Vec::new();
    for name in names {
        let base = format!("sites/{name}");
        match get(&format!("{base}/meta.json"), None).await.and_then(|b| from_meta(&name, Where::Local(base.clone()), &b)) {
            Ok(e) => out.push(e),
            Err(e) => bevy::log::warn!("site {name}: {e}"),
        }
    }
    Ok(out)
}
