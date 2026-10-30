//! Remembered MarsScape name and per-server login tokens. Native: a small JSON file in the
//! user's config folder; browser: localStorage (any failure just means "not remembered").

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Default)]
struct Profile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    tokens: HashMap<String, String>,
}

#[cfg(not(target_arch = "wasm32"))]
fn path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("XDG_CONFIG_HOME")).map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("marsmaker").join("marsscape.json"))
}

#[cfg(not(target_arch = "wasm32"))]
fn load() -> Profile {
    path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn save(p: &Profile) {
    let Some(path) = path() else { return };
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(s) = serde_json::to_string_pretty(p) {
        let _ = std::fs::write(path, s);
    }
}

#[cfg(target_arch = "wasm32")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(target_arch = "wasm32")]
fn load() -> Profile {
    storage().and_then(|s| s.get_item("marsscape").ok().flatten()).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

#[cfg(target_arch = "wasm32")]
fn save(p: &Profile) {
    if let (Some(s), Ok(v)) = (storage(), serde_json::to_string(p)) {
        let _ = s.set_item("marsscape", &v);
    }
}

pub fn name() -> Option<String> {
    let n = load().name;
    (!n.is_empty()).then_some(n)
}

pub fn set_name(n: &str) {
    let mut p = load();
    p.name = n.to_string();
    save(&p);
}

pub fn token(server: &str) -> String {
    load().tokens.get(server).cloned().unwrap_or_default()
}

pub fn set_token(server: &str, t: &str) {
    let mut p = load();
    p.tokens.insert(server.to_string(), t.to_string());
    save(&p);
}
