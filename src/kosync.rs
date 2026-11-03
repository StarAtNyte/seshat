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

fn device_id() -> String {
    let host = std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")).unwrap_or_else(|_| "desktop".into());
    md5_hex(&format!("seshat-{host}"))[..16].to_string()
}

fn req(cfg: &KoSync, proxy: &Option<String>, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
    let client = crate::api::build_client(proxy, Some(Duration::from_secs(15)));
    client
        .request(method, format!("{}{}", cfg.server.trim_end_matches('/'), path))
        .header("accept", "application/vnd.koreader.v1+json")
        .header("x-auth-user", &cfg.username)
        .header("x-auth-key", &cfg.userkey)
}

pub fn register(cfg: &KoSync, proxy: &Option<String>) -> Result<(), String> {
    let r = req(cfg, proxy, reqwest::Method::POST, "/users/create")
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
    let r = req(cfg, proxy, reqwest::Method::GET, "/users/auth").send().map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(format!("login failed (HTTP {})", r.status().as_u16()))
    }
}

pub fn push(cfg: &KoSync, proxy: &Option<String>, document: &str, progress: &str, percentage: f32) -> Result<(), String> {
    let body = serde_json::json!({
        "document": document,
        "progress": progress,
        "percentage": percentage,
        "device": "Seshat",
        "device_id": device_id(),
    });
    let r = req(cfg, proxy, reqwest::Method::PUT, "/syncs/progress").json(&body).send().map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(format!("sync push failed (HTTP {})", r.status().as_u16()))
    }
}

/// Remote progress, if another device has newer progress than ours.
pub fn pull(cfg: &KoSync, proxy: &Option<String>, document: &str) -> Result<Option<Progress>, String> {
    let r = req(cfg, proxy, reqwest::Method::GET, &format!("/syncs/progress/{document}")).send().map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("sync pull failed (HTTP {})", r.status().as_u16()));
    }
    let p: Progress = r.json().unwrap_or_default();
    if p.document.is_empty() && p.progress.is_empty() || p.device_id == device_id() {
        return Ok(None);
    }
    Ok(Some(p))
}

/// Progress string: KOReader-style xpointer so KOReader lands in the right chapter.
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
