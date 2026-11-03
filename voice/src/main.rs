//! seshat-voice: Seshat's optional natural-voice helper.
//!
//! Runs the Kokoro 82M text-to-speech model, and Piper voices for other
//! languages (both via sherpa-onnx), offline on the CPU and plays the audio itself. Seshat downloads this program only when
//! the user asks for the natural voice, so the main app stays small, and a
//! crash in here never takes a reader window down with it.
//!
//! Commands (<root> is the voice folder: Kokoro in kokoro-multi-lang-v1_0/,
//! Piper packs in piper/<pack>/):
//!   seshat-voice install <root> [kokoro|piper:<pack>]   download + unpack a voice
//!   seshat-voice serve <root>                  JSON-lines protocol on stdin/stdout (used by Seshat)
//!   seshat-voice say <root> <kokoro-id|pack> <text>     speak once (for testing)
//!   seshat-voice wav <root> <kokoro-id|pack> <out.wav> <text>   write a WAV and print the speed
//!   seshat-voice --version
//!
//! Serve protocol, one JSON object per line:
//!   in : {"op":"speak","id":7,"text":"…","voice":3,"speed":1.0}          Kokoro speaker 3
//!        {"op":"speak","id":8,"text":"…","pack":"de_DE-thorsten-medium"}  a Piper voice
//!        {"op":"stop"}     drop everything queued and stop playback now
//!   out: {"ev":"ready"}  {"ev":"started","id":7}  {"ev":"done","id":7}
//!        {"ev":"error","id":7,"msg":"…"}   (id is omitted for fatal errors)
//! The helper exits when its stdin closes, so it never outlives Seshat.

use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

include!("piper_voices.rs");
use serde_json::json;
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig, OfflineTtsVitsModelConfig};

/// The model archive published by the sherpa-onnx project, pinned by hash.
/// This is the fp32 model on purpose: ONNX Runtime runs Kokoro's int8 build
/// about 2.5x slower on x86 CPUs (RTF ~1.1 vs ~0.4 on an i7-13620H), which
/// is too slow to read aloud in real time.
const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2";
const MODEL_SHA256: &str = "60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4";
const MODEL_SIZE: u64 = 349_886_476;
/// Folder name inside the install dir; Seshat looks for the same name.
const MODEL_DIR: &str = "kokoro-multi-lang-v1_0";
/// Written last, so a half-finished install never looks complete.
const COMPLETE: &str = ".complete";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let code = match a(0) {
        "--version" | "-V" => {
            println!("seshat-voice {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "install" if !a(1).is_empty() => match pack(if a(2).is_empty() { "kokoro" } else { a(2) }).and_then(|p| install(Path::new(a(1)), &p)) {
            Ok(()) => {
                emit(json!({"ev": "installed"}));
                0
            }
            Err(e) => {
                emit(json!({"ev": "error", "msg": e}));
                1
            }
        },
        "serve" if !a(1).is_empty() => serve(Path::new(a(1))),
        "say" if args.len() >= 4 => once(Path::new(a(1)), a(2), &args[3..].join(" "), None),
        "wav" if args.len() >= 5 => once(Path::new(a(1)), a(2), &args[4..].join(" "), Some(a(3))),
        _ => {
            eprintln!("usage: seshat-voice install <root> [kokoro|piper:<pack>] | serve <root> | say <root> <kokoro-id|piper-pack> <text> | wav <root> <kokoro-id|piper-pack> <out.wav> <text>");
            2
        }
    };
    std::process::exit(code);
}

/// One JSON line on stdout. Several threads write, so lock for the whole line.
fn emit(v: serde_json::Value) {
    let out = std::io::stdout();
    let mut out = out.lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

// ---------- install ----------

/// Counts and hashes the compressed bytes as they stream past.
struct Tap<R> {
    inner: R,
    read: u64,
    total: u64,
    hash: sha2::Sha256,
    last: Instant,
}

impl<R: Read> Read for Tap<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        self.read += n as u64;
        if self.last.elapsed() > Duration::from_millis(150) || n == 0 {
            self.last = Instant::now();
            emit(json!({"ev": "progress", "done": self.read, "total": self.total}));
        }
        Ok(n)
    }
}

/// Something `install` can fetch: an archive pinned by size and hash, the
/// folder it unpacks to (under the voice root), and which files to keep.
struct Pack {
    url: String,
    sha256: String,
    size: u64,
    folder: String,
    kokoro: bool,
}

fn pack(spec: &str) -> Result<Pack, String> {
    if spec == "kokoro" {
        return Ok(Pack { url: MODEL_URL.into(), sha256: MODEL_SHA256.into(), size: MODEL_SIZE, folder: MODEL_DIR.into(), kokoro: true });
    }
    let name = spec.strip_prefix("piper:").ok_or_else(|| format!("unknown voice pack {spec}"))?;
    let v = PIPER_VOICES.iter().find(|v| v.2 == name).ok_or_else(|| format!("unknown Piper voice {name}"))?;
    Ok(Pack {
        url: format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-{name}.tar.bz2"),
        sha256: v.4.into(),
        size: v.3,
        folder: format!("piper/{name}"),
        kokoro: false,
    })
}

/// Files Seshat needs from the archive. The Chinese dictionaries and number
/// rules are skipped: Seshat offers the English voices. espeak-ng-data is
/// kept: it pronounces names and numbers missing from the lexicon.
fn wanted(rel: &Path) -> bool {
    let s = rel.to_string_lossy().replace('\\', "/");
    matches!(s.as_str(), "model.onnx" | "voices.bin" | "tokens.txt" | "lexicon-us-en.txt" | "lexicon-gb-en.txt" | "LICENSE" | "README.md")
        || s.starts_with("espeak-ng-data/")
}

fn install(dir: &Path, pk: &Pack) -> Result<(), String> {
    let fin = dir.join(&pk.folder);
    if fin.join(COMPLETE).exists() {
        return Ok(());
    }
    let parent = fin.parent().unwrap_or(dir).to_path_buf();
    std::fs::create_dir_all(&parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let staging = parent.join(format!(".{}.part", fin.file_name().unwrap_or_default().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
    let res = (|| {
        let resp = ureq::get(&pk.url).timeout(Duration::from_secs(60 * 60)).call().map_err(|e| format!("download failed: {e}"))?;
        let total = resp.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(pk.size);
        let mut tap = Tap { inner: resp.into_reader(), read: 0, total, hash: Default::default(), last: Instant::now() };
        {
            let mut ar = tar::Archive::new(bzip2::read::BzDecoder::new(&mut tap));
            for entry in ar.entries().map_err(|e| format!("bad archive: {e}"))? {
                let mut entry = entry.map_err(|e| format!("download interrupted: {e}"))?;
                let path = entry.path().map_err(|e| format!("bad archive: {e}"))?.into_owned();
                // strip the top folder; refuse anything that climbs out
                let rel: PathBuf = path.components().skip(1).collect();
                if rel.as_os_str().is_empty() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) || (pk.kokoro && !wanted(&rel)) {
                    continue;
                }
                let dest = staging.join(&rel);
                if entry.header().entry_type().is_dir() {
                    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                    continue;
                }
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                if let Some(p) = dest.parent() {
                    std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
                }
                let mut f = std::fs::File::create(&dest).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
                std::io::copy(&mut entry, &mut f).map_err(|e| format!("download interrupted: {e}"))?;
            }
        }
        // drain whatever the tar reader left so the hash covers the whole file
        std::io::copy(&mut tap, &mut std::io::sink()).map_err(|e| format!("download interrupted: {e}"))?;
        use sha2::Digest;
        let got: String = tap.hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != pk.sha256 {
            return Err("the downloaded model is damaged (checksum mismatch); please try again".to_string());
        }
        let piper_model = format!("{}.onnx", fin.file_name().unwrap_or_default().to_string_lossy());
        let need: Vec<&str> = if pk.kokoro { vec!["model.onnx", "voices.bin", "tokens.txt", "lexicon-us-en.txt", "espeak-ng-data"] } else { vec![piper_model.as_str(), "tokens.txt", "espeak-ng-data"] };
        for f in need {
            if !staging.join(f).exists() {
                return Err(format!("the model archive is missing {f}"));
            }
        }
        Ok(())
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&fin);
    std::fs::rename(&staging, &fin).map_err(|e| format!("cannot finish install: {e}"))?;
    std::fs::write(fin.join(COMPLETE), env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- synthesis ----------

/// Kokoro's American voices read best with the US lexicon, British ones with
/// the UK lexicon. Ids follow the model's own speaker table (bf_/bm_ are 20..=27).
fn british(voice: i32) -> bool {
    (20..=27).contains(&voice)
}

fn load(model: &Path, british: bool) -> Result<OfflineTts, String> {
    let p = |f: &str| Some(model.join(f).to_string_lossy().into_owned());
    if !model.join("model.onnx").exists() {
        return Err(format!("no voice model in {}", model.display()));
    }
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    let config = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            kokoro: OfflineTtsKokoroModelConfig {
                model: p(&std::env::var("SESHAT_VOICE_MODEL").unwrap_or_else(|_| "model.onnx".into())),
                voices: p("voices.bin"),
                tokens: p("tokens.txt"),
                lexicon: p(if british { "lexicon-gb-en.txt" } else { "lexicon-us-en.txt" }),
                // espeak-ng spells out names and numbers the lexicon lacks
                // (its British English voice is plain "en")
                data_dir: p("espeak-ng-data"),
                lang: Some(if british { "en" } else { "en-us" }.into()),
                ..Default::default()
            },
            // ONNX on CPU is memory-bound; past a few threads it stops helping.
            num_threads: std::env::var("SESHAT_VOICE_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or((threads / 2).clamp(1, 4) as i32),
            ..Default::default()
        },
        max_num_sentences: 1,
        ..Default::default()
    };
    OfflineTts::create(&config).ok_or_else(|| "could not load the voice model".to_string())
}

fn load_piper(dir: &Path) -> Result<OfflineTts, String> {
    let name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let p = |f: &str| Some(dir.join(f).to_string_lossy().into_owned());
    if !dir.join(format!("{name}.onnx")).exists() {
        return Err(format!("the {name} voice is not installed"));
    }
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    let config = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            vits: OfflineTtsVitsModelConfig { model: p(&format!("{name}.onnx")), tokens: p("tokens.txt"), data_dir: p("espeak-ng-data"), ..Default::default() },
            num_threads: (threads / 2).clamp(1, 4) as i32,
            ..Default::default()
        },
        max_num_sentences: 1,
        ..Default::default()
    };
    OfflineTts::create(&config).ok_or_else(|| format!("could not load the {name} voice"))
}

/// Which model a request wants: a Kokoro speaker, or a Piper pack.
fn engine_key(voice: i32, pack: &str) -> String {
    if pack.is_empty() {
        if british(voice) { "kokoro-gb" } else { "kokoro-us" }.to_string()
    } else {
        format!("piper:{pack}")
    }
}

/// Lazily loaded engines. Each model is ~100-350 MB in memory, so only the
/// two most recently used stay loaded.
struct Engines {
    root: PathBuf,
    loaded: Vec<(String, Arc<OfflineTts>)>,
}

impl Engines {
    fn new(root: &Path) -> Self {
        Engines { root: root.to_path_buf(), loaded: Vec::new() }
    }

    fn get(&mut self, voice: i32, pack: &str) -> Result<Arc<OfflineTts>, String> {
        let key = engine_key(voice, pack);
        if let Some(i) = self.loaded.iter().position(|e| e.0 == key) {
            let e = self.loaded.remove(i);
            self.loaded.insert(0, e);
            return Ok(self.loaded[0].1.clone());
        }
        let tts = if pack.is_empty() {
            load(&self.root.join(MODEL_DIR), british(voice))?
        } else {
            if pack.contains(['/', '\\']) || pack.starts_with('.') {
                return Err("bad voice name".into());
            }
            load_piper(&self.root.join("piper").join(pack))?
        };
        self.loaded.insert(0, (key, Arc::new(tts)));
        self.loaded.truncate(2);
        Ok(self.loaded[0].1.clone())
    }
}

fn clean(text: &str) -> String {
    text.chars().map(|c| if c == '\0' || c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

fn gen_config(voice: i32, pack: &str, speed: f32) -> GenerationConfig {
    let sid = if pack.is_empty() { voice.max(0) } else { 0 };
    GenerationConfig { sid, speed: speed.clamp(0.5, 2.0), ..Default::default() }
}

fn once(root: &Path, which: &str, text: &str, wav: Option<&str>) -> i32 {
    let (voice, pack) = match which.parse::<i32>() {
        Ok(v) => (v, ""),
        Err(_) => (0, which),
    };
    let mut eng = Engines::new(root);
    let t0 = Instant::now();
    let tts = match eng.get(voice, pack) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    eprintln!("loaded in {:.2}s", t0.elapsed().as_secs_f32());
    let t1 = Instant::now();
    let Some(audio) = tts.generate_with_config(&clean(text), &gen_config(voice, pack, 1.0), None::<fn(&[f32], f32) -> bool>) else {
        eprintln!("generation failed");
        return 1;
    };
    let secs = audio.samples().len() as f32 / audio.sample_rate().max(1) as f32;
    let took = t1.elapsed().as_secs_f32();
    eprintln!("{secs:.2}s of audio in {took:.2}s (real-time factor {:.2})", took / secs.max(0.01));
    if let Some(path) = wav {
        return if audio.save(path) { 0 } else { 1 };
    }
    let Ok(stream) = rodio::OutputStreamBuilder::open_default_stream() else {
        eprintln!("no audio output");
        return 1;
    };
    let sink = rodio::Sink::connect_new(stream.mixer());
    sink.append(rodio::buffer::SamplesBuffer::new(1, audio.sample_rate() as u32, audio.samples().to_vec()));
    sink.sleep_until_end();
    0
}

// ---------- serve ----------

#[derive(Deserialize)]
struct Cmd {
    op: String,
    #[serde(default)]
    id: u64,
    #[serde(default)]
    text: String,
    #[serde(default = "default_voice")]
    voice: i32,
    #[serde(default = "one")]
    speed: f32,
    /// A Piper pack name; empty means Kokoro.
    #[serde(default)]
    pack: String,
}

fn default_voice() -> i32 {
    3
}
fn one() -> f32 {
    1.0
}

struct Job {
    gen: u64,
    id: u64,
    text: String,
    voice: i32,
    speed: f32,
    pack: String,
}

struct Shared {
    /// Bumped by every stop; work from an older generation is thrown away.
    gen: AtomicU64,
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
}

fn serve(root: &Path) -> i32 {
    let shared = Arc::new(Shared { gen: AtomicU64::new(0), queue: Mutex::new(VecDeque::new()), wake: Condvar::new() });
    let (audio_tx, audio_rx) = mpsc::channel::<(u64, u64, u32, Vec<f32>)>();

    // stdin: commands. EOF means Seshat is gone.
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                let Ok(cmd) = serde_json::from_str::<Cmd>(&line) else { continue };
                match cmd.op.as_str() {
                    "speak" => {
                        let gen = shared.gen.load(Ordering::SeqCst);
                        shared.queue.lock().unwrap().push_back(Job { gen, id: cmd.id, text: cmd.text, voice: cmd.voice, speed: cmd.speed, pack: cmd.pack });
                        shared.wake.notify_all();
                    }
                    "stop" => {
                        shared.gen.fetch_add(1, Ordering::SeqCst);
                        shared.queue.lock().unwrap().clear();
                    }
                    "quit" => break,
                    _ => {}
                }
            }
            std::process::exit(0);
        });
    }

    // synthesis: one sentence at a time, running ahead of playback.
    {
        let shared = shared.clone();
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let mut engines = Engines::new(&root);
            loop {
                let job = {
                    let mut q = shared.queue.lock().unwrap();
                    loop {
                        if let Some(j) = q.pop_front() {
                            break j;
                        }
                        q = shared.wake.wait(q).unwrap();
                    }
                };
                if job.gen != shared.gen.load(Ordering::SeqCst) {
                    continue;
                }
                let tts = match engines.get(job.voice, &job.pack) {
                    Ok(t) => t,
                    Err(e) => {
                        emit(json!({"ev": "error", "id": job.id, "msg": e}));
                        continue;
                    }
                };
                let text = clean(&job.text);
                if text.is_empty() {
                    let _ = audio_tx.send((job.gen, job.id, 24000, Vec::new()));
                    continue;
                }
                let (sh, g) = (shared.clone(), job.gen);
                let cb = move |_: &[f32], _: f32| sh.gen.load(Ordering::SeqCst) == g;
                match tts.generate_with_config(&text, &gen_config(job.voice, &job.pack, job.speed), Some(cb)) {
                    Some(a) => {
                        let _ = audio_tx.send((job.gen, job.id, a.sample_rate() as u32, a.samples().to_vec()));
                    }
                    None if job.gen == shared.gen.load(Ordering::SeqCst) => {
                        emit(json!({"ev": "error", "id": job.id, "msg": "could not synthesise this sentence"}));
                        let _ = audio_tx.send((job.gen, job.id, 24000, Vec::new()));
                    }
                    None => {}
                }
            }
        });
    }

    // playback, on the main thread.
    let stream = match rodio::OutputStreamBuilder::open_default_stream() {
        Ok(mut s) => {
            s.log_on_drop(false);
            s
        }
        Err(e) => {
            emit(json!({"ev": "error", "msg": format!("no audio output: {e}")}));
            return 1;
        }
    };
    let sink = rodio::Sink::connect_new(stream.mixer());
    emit(json!({"ev": "ready"}));
    let mut pending: VecDeque<(u64, u64, u32, Vec<f32>)> = VecDeque::new();
    let mut playing: Option<(u64, u64)> = None;
    loop {
        match audio_rx.recv_timeout(Duration::from_millis(15)) {
            Ok(a) => pending.push_back(a),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return 1,
        }
        let gen = shared.gen.load(Ordering::SeqCst);
        pending.retain(|a| a.0 == gen);
        if let Some((g, _)) = playing {
            if g != gen {
                sink.clear();
                sink.play();
                playing = None;
            } else if sink.empty() {
                emit(json!({"ev": "done", "id": playing.unwrap().1}));
                playing = None;
            }
        }
        if playing.is_none() {
            if let Some((g, id, rate, samples)) = pending.pop_front() {
                emit(json!({"ev": "started", "id": id}));
                if samples.is_empty() {
                    emit(json!({"ev": "done", "id": id}));
                } else {
                    sink.append(rodio::buffer::SamplesBuffer::new(1, rate, samples));
                    playing = Some((g, id));
                }
            }
        }
    }
}
