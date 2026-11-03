//! KOReader sync server protocol (kosync): share reading progress with
//! KOReader, Readest and other compatible readers.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::store::KoSync;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Progress {
    #[serde(default)]
    pub document: String,
    #[serde(default)]
    pub progress: String,
    #[serde(default)]
    pub percentage: f32,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub timestamp: i64,
}

pub fn md5_hex(s: &str) -> String {
    use md5::{Digest, Md5};
    Md5::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// This install's device id: generated once and persisted in settings
/// (`KoSync::device_id`). Falls back to a hostname-derived id only when the
/// settings carry none (e.g. a config built before the field existed).
fn device_id(cfg: &KoSync) -> String {
    if !cfg.device_id.is_empty() {
        return cfg.device_id.clone();
    }
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "desktop".into());
    md5_hex(&format!("seshat-{host}"))[..16].to_string()
}

/// A fresh random device id (32 hex chars, like KOReader's).
pub fn new_device_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    h.write_u128(nanos);
    h.write_u32(std::process::id());
    let a = h.finish();
    let b = std::collections::hash_map::RandomState::new().build_hasher().finish();
    md5_hex(&format!("{a:016x}{b:016x}{nanos}")).to_uppercase()
}

fn req(cfg: &KoSync, proxy: &Option<String>, method: reqwest::Method, path: &str) -> Result<reqwest::blocking::RequestBuilder, String> {
    let client = crate::api::client(proxy, Some(Duration::from_secs(15)))?;
    Ok(client
        .request(method, format!("{}{}", cfg.server.trim_end_matches('/'), path))
        .header("accept", "application/vnd.koreader.v1+json")
        .header("x-auth-user", &cfg.username)
        .header("x-auth-key", &cfg.userkey))
}

pub fn register(cfg: &KoSync, proxy: &Option<String>) -> Result<(), String> {
    let r = req(cfg, proxy, reqwest::Method::POST, "/users/create")?
        .json(&serde_json::json!({ "username": cfg.username, "password": cfg.userkey }))
        .send()
        .map_err(|e| e.to_string())?;
    match r.status().as_u16() {
        200 | 201 => Ok(()),
        402 => Err("username already taken".into()),
        s => Err(format!("registration failed (HTTP {s})")),
    }
}

pub fn auth(cfg: &KoSync, proxy: &Option<String>) -> Result<(), String> {
    let r = req(cfg, proxy, reqwest::Method::GET, "/users/auth")?.send().map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(format!("login failed (HTTP {})", r.status().as_u16()))
    }
}

/// Device name on the wire; Seshat's exact position rides along after it.
const DEVICE: &str = "Seshat";
const ZR: &str = "#zr:";

pub fn push(cfg: &KoSync, proxy: &Option<String>, document: &str, progress: &str, percentage: f32) -> Result<(), String> {
    // `progress` may carry Seshat's exact position as a `#zr:s:b:o` suffix
    // (see encode_flow). That is not a valid crengine XPointer, so the wire
    // value is the plain `/body/DocFragment[N]/body` KOReader understands and
    // the exact position goes into the device name, where only Seshat reads it.
    let (wire, device) = match progress.split_once(ZR) {
        Some((x, zr)) => (x.to_string(), format!("{DEVICE} zr:{zr}")),
        None => (progress.to_string(), DEVICE.to_string()),
    };
    let body = serde_json::json!({
        "document": document,
        "progress": wire,
        "percentage": percentage,
        "device": device,
        "device_id": device_id(cfg),
    });
    let r = req(cfg, proxy, reqwest::Method::PUT, "/syncs/progress")?.json(&body).send().map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(format!("sync push failed (HTTP {})", r.status().as_u16()))
    }
}

/// Remote progress, if another device has newer progress than ours.
pub fn pull(cfg: &KoSync, proxy: &Option<String>, document: &str) -> Result<Option<Progress>, String> {
    let r = req(cfg, proxy, reqwest::Method::GET, &format!("/syncs/progress/{document}"))?.send().map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("sync pull failed (HTTP {})", r.status().as_u16()));
    }
    let mut p: Progress = r.json().unwrap_or_default();
    if p.document.is_empty() && p.progress.is_empty() || p.device_id == device_id(cfg) {
        return Ok(None);
    }
    // Another Seshat install: restore the exact position from the device name,
    // as long as the plain XPointer has not been overwritten by another reader.
    if let Some((name, zr)) = p.device.split_once(" zr:") {
        if name == DEVICE && p.progress.starts_with("/body/DocFragment[") && !p.progress.contains(ZR) {
            p.progress = format!("{}{ZR}{zr}", p.progress);
        }
        p.device = name.to_string();
    }
    Ok(Some(p))
}

/// Progress string: KOReader-style xpointer so KOReader lands in the right
/// chapter, plus Seshat's exact position as a `#zr:` suffix. `push` strips the
/// suffix from the wire value (plain `/body/DocFragment[N]/body`) and `pull`
/// restores it, so `decode` sees the same string either way. Values pushed by
/// older versions with the suffix in the XPointer itself still decode.
pub fn encode_flow(spine: usize, section: usize, block: usize, offset: usize) -> String {
    format!("/body/DocFragment[{}]/body#zr:{section}:{block}:{offset}", spine + 1)
}

pub enum Decoded {
    Exact(usize, usize, usize),
    Spine(usize),
    Page(usize),
}

pub fn decode(progress: &str) -> Option<Decoded> {
    if let Some(z) = progress.split("#zr:").nth(1) {
        let v: Vec<usize> = z.split(':').filter_map(|x| x.parse().ok()).collect();
        if v.len() == 3 {
            return Some(Decoded::Exact(v[0], v[1], v[2]));
        }
    }
    if let Some(i) = progress.find("DocFragment[") {
        let n: String = progress[i + 12..].chars().take_while(|c| c.is_ascii_digit()).collect();
        return n.parse::<usize>().ok().map(|n| Decoded::Spine(n.saturating_sub(1)));
    }
    progress.trim().parse::<usize>().ok().map(|p| Decoded::Page(p.saturating_sub(1)))
}
