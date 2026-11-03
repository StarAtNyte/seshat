//! The optional natural voice for read-aloud: Kokoro 82M, run offline on the CPU.
//!
//! Nothing here is part of the ~20 MB app. When the user asks for it, Seshat
//! downloads `seshat-voice` (a separate helper program built from `voice/`)
//! from this version's GitHub release, checks it against the published
//! SHA-256, and runs `seshat-voice install`, which fetches the model and
//! checks it against a hash pinned in the helper. `tts.rs` then drives the
//! helper over stdin/stdout. Removing the voice deletes all of it.

use crate::i18n::tr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

include!("../voice/src/piper_voices.rs");

/// Must match `MODEL_DIR` in voice/src/main.rs.
const MODEL_DIR: &str = "kokoro-multi-lang-v1_0";
/// Rough total download (helper + model), shown before the user commits.
pub const DOWNLOAD_MB: u32 = 360;
pub const DISK_MB: u32 = 400;
/// Default voice: af_heart, the best-rated Kokoro voice.
pub const DEFAULT_VOICE: i32 = 3;

/// The English voices, by the model's own speaker ids.
pub const VOICES: &[(i32, &str, &str)] = &[
    (3, "Heart", "US female"),
    (2, "Bella", "US female"),
    (6, "Nicole", "US female"),
    (9, "Sarah", "US female"),
    (0, "Alloy", "US female"),
    (1, "Aoede", "US female"),
    (4, "Jessica", "US female"),
    (5, "Kore", "US female"),
    (7, "Nova", "US female"),
    (8, "River", "US female"),
    (10, "Sky", "US female"),
    (16, "Michael", "US male"),
    (13, "Eric", "US male"),
    (11, "Adam", "US male"),
    (12, "Echo", "US male"),
    (14, "Fenrir", "US male"),
    (15, "Liam", "US male"),
    (17, "Onyx", "US male"),
    (18, "Puck", "US male"),
    (19, "Santa", "US male"),
    (21, "Emma", "UK female"),
    (22, "Isabella", "UK female"),
    (20, "Alice", "UK female"),
    (23, "Lily", "UK female"),
    (26, "George", "UK male"),
    (25, "Fable", "UK male"),
    (24, "Daniel", "UK male"),
    (27, "Lewis", "UK male"),
];

pub fn voice_label(id: i32) -> String {
    VOICES.iter().find(|v| v.0 == id).map(|v| format!("{} · {}", v.1, tr(v.2))).unwrap_or_else(|| trf!("Voice {id}", id = id))
}

/// Where the helper and model live: the user's local data folder.
pub fn root() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(crate::store::cache_dir).join("seshat").join("voice")
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "seshat-voice.exe"
    } else {
        "seshat-voice"
    }
}

fn downloaded_helper() -> PathBuf {
    root().join("bin").join(exe_name())
}

/// The helper to run: `$SESHAT_VOICE`, one shipped next to Seshat (dev builds,
/// packagers), or the one Seshat downloaded.
pub fn helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SESHAT_VOICE").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(exe_name()))) {
        if p.is_file() {
            return Some(p);
        }
    }
    // A helper downloaded by an older Seshat may not speak this version's
    // protocol; it counts as missing and gets replaced (not kept alongside).
    let p = downloaded_helper();
    let current = std::fs::read_to_string(version_file()).is_ok_and(|v| v.trim() == env!("CARGO_PKG_VERSION"));
    Some(p).filter(|p| p.is_file() && current)
}

fn version_file() -> PathBuf {
    root().join("bin").join("VERSION")
}

/// The model is on disk, but the helper is missing or from an older Seshat:
/// a much smaller download brings it up to date.
pub fn needs_update() -> bool {
    (model_dir().join(".complete").is_file() || !installed_piper().is_empty()) && helper().is_none()
}

/// Delete everything in the voice folder that isn't the current helper or
/// model: older models, older helpers, interrupted downloads.
fn prune() {
    let keep_bin = [std::ffi::OsString::from(exe_name()), "VERSION".into()];
    if let Ok(rd) = std::fs::read_dir(root()) {
        for e in rd.flatten() {
            let name = e.file_name();
            let p = e.path();
            if name == "bin" {
                if let Ok(bin) = std::fs::read_dir(&p) {
                    for b in bin.flatten().filter(|b| !keep_bin.contains(&b.file_name())) {
                        let _ = std::fs::remove_file(b.path());
                    }
                }
            } else if name == "piper" {
                // keep the installed packs we still offer; drop the rest and half-finished ones
                if let Ok(packs) = std::fs::read_dir(&p) {
                    for v in packs.flatten() {
                        let n = v.file_name().to_string_lossy().into_owned();
                        if !PIPER_VOICES.iter().any(|x| x.2 == n) || !v.path().join(".complete").is_file() {
                            let _ = std::fs::remove_dir_all(v.path());
                        }
                    }
                }
            } else if name != MODEL_DIR {
                let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
            }
        }
    }
}

pub fn model_dir() -> PathBuf {
    root().join(MODEL_DIR)
}

/// Kokoro (the English natural voice) is ready to use.
pub fn installed() -> bool {
    model_dir().join(".complete").is_file() && helper().is_some()
}

pub fn kokoro_on_disk() -> bool {
    model_dir().join(".complete").is_file()
}

fn piper_dir(pack: &str) -> PathBuf {
    root().join("piper").join(pack)
}

/// Piper packs on disk: (language code, language name, pack).
pub fn installed_piper() -> Vec<(&'static str, &'static str, &'static str)> {
    PIPER_VOICES.iter().filter(|v| piper_dir(v.2).join(".complete").is_file()).map(|v| (v.0, v.1, v.2)).collect()
}

/// The installed Piper voice for a book language ("de", "pt-BR", "ru_RU"…).
pub fn piper_for(lang: &str) -> Option<&'static str> {
    let l = lang2(lang)?;
    if helper().is_none() {
        return None;
    }
    installed_piper().into_iter().find(|v| v.0 == l).map(|v| v.2)
}

/// Lower-case two-letter language code, if the tag has one.
pub fn lang2(lang: &str) -> Option<String> {
    let l: String = lang.trim().chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase();
    (l.len() == 2).then_some(l)
}

pub fn piper_size_mb(pack: &str) -> u64 {
    PIPER_VOICES.iter().find(|v| v.2 == pack).map(|v| v.3 / 1_000_000).unwrap_or(0)
}

/// Delete one Piper voice.
pub fn remove_piper(pack: &str) {
    crate::tts::shutdown_helper();
    let _ = std::fs::remove_dir_all(piper_dir(pack));
}

/// Release asset name of the helper for this platform.
fn asset() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("seshat-voice-linux-x86_64")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("seshat-voice-windows-x64.exe")
    } else if cfg!(target_os = "macos") {
        Some("seshat-voice-macos-universal")
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Helper,
    Model,
    Done,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub stage: Stage,
    pub done: u64,
    pub total: u64,
}

/// A running (or finished) install, polled by the settings page.
/// Install progress and the last message, shared by every settings view
/// (the library's Settings window and each reader's drawer).
pub struct UiState {
    pub install: Option<Install>,
    pub msg: String,
}

pub static UI_STATE: Mutex<UiState> = Mutex::new(UiState { install: None, msg: String::new() });

pub struct Install {
    state: Arc<Mutex<Progress>>,
}

impl Install {
    pub fn progress(&self) -> Progress {
        self.state.lock().unwrap().clone()
    }

    /// `spec` is "kokoro" or "piper:<pack>".
    pub fn start(ctx: eframe::egui::Context, spec: String) -> Install {
        let state = Arc::new(Mutex::new(Progress { stage: Stage::Helper, done: 0, total: 0 }));
        let st = state.clone();
        std::thread::spawn(move || {
            let set = |stage: Stage, done: u64, total: u64| {
                *st.lock().unwrap() = Progress { stage, done, total };
                ctx.request_repaint();
            };
            let res = (|| -> Result<(), String> {
                if helper().is_none() {
                    fetch_helper(&|d, t| set(Stage::Helper, d, t))?;
                }
                set(Stage::Model, 0, 0);
                run_model_install(&spec, &|d, t| set(Stage::Model, d, t))?;
                prune();
                Ok(())
            })();
            match res {
                Ok(()) => set(Stage::Done, 0, 0),
                Err(e) => set(Stage::Failed(e), 0, 0),
            }
        });
        Install { state }
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("Seshat/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(None)
        .build()
        .map_err(|e| e.to_string())
}

fn fetch_helper(progress: &dyn Fn(u64, u64)) -> Result<(), String> {
    use sha2::Digest;
    let asset = asset().ok_or(tr("The natural voice is not available for this platform yet."))?;
    let base = format!("{}/releases/download/v{}/{asset}", env!("CARGO_PKG_REPOSITORY"), env!("CARGO_PKG_VERSION"));
    let c = client()?;
    let sum = c.get(format!("{base}.sha256")).send().and_then(|r| r.error_for_status()).and_then(|r| r.text()).map_err(|e| format!("Could not reach the Seshat release ({e})."))?;
    let want = sum.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if want.len() != 64 {
        return Err(tr("The release has no valid checksum for the voice helper.").into());
    }
    let mut resp = c.get(&base).send().and_then(|r| r.error_for_status()).map_err(|e| format!("Could not download the voice helper ({e})."))?;
    let total = resp.content_length().unwrap_or(0);
    let dest = downloaded_helper();
    let dir = dest.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| trf!("Cannot create {path}: {err}", path = dir.display(), err = e))?;
    let tmp = dir.join(".seshat-voice.part");
    let res = (|| -> Result<(), String> {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut h = sha2::Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut done = 0u64;
        loop {
            let n = resp.read(&mut buf).map_err(|e| trf!("Download interrupted ({err}).", err = e))?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            h.update(&buf[..n]);
            done += n as u64;
            progress(done, total);
        }
        f.sync_all().map_err(|e| e.to_string())?;
        let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != want {
            return Err(tr("The voice helper download is damaged (checksum mismatch). Please try again.").into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        // the old helper (if any) is replaced in place, never kept
        crate::tts::shutdown_helper();
        let _ = std::fs::remove_file(&dest);
        std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
        std::fs::write(version_file(), env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

fn run_model_install(spec: &str, progress: &dyn Fn(u64, u64)) -> Result<(), String> {
    let exe = helper().ok_or(tr("The voice helper is missing."))?;
    let mut child = crate::platform::command(&exe.to_string_lossy())
        .arg("install")
        .arg(root())
        .arg(spec)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| trf!("Could not start the voice helper ({err}).", err = e))?;
    let out = child.stdout.take().unwrap();
    let mut result = Err(tr("The voice install stopped unexpectedly.").to_string());
    for line in BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        match v["ev"].as_str() {
            Some("progress") => progress(v["done"].as_u64().unwrap_or(0), v["total"].as_u64().unwrap_or(0)),
            Some("installed") => result = Ok(()),
            Some("error") => result = Err(v["msg"].as_str().unwrap_or("install failed").to_string()),
            _ => {}
        }
    }
    let _ = child.wait();
    result
}

/// Delete the Kokoro model, and the helper too if no Piper voice still needs it.
pub fn remove() {
    crate::tts::shutdown_helper();
    let _ = std::fs::remove_dir_all(model_dir());
    if installed_piper().is_empty() {
        let _ = std::fs::remove_dir_all(root());
    }
}
