//! Library index: incremental, parallel scanning of book folders with cached
//! metadata and cover thumbnails (only new/changed files are parsed).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::store::{self, BookData};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub path: String,
    pub title: String,
    pub author: String,
    pub format: String,
    pub size: u64,
    pub mtime: u64,
    pub added: i64,
    pub language: String,
    pub series: String,
    pub publisher: String,
    pub description: String,
    pub has_cover: bool,
    /// cover height / width (0 = unknown)
    pub cover_aspect: f32,
    pub fraction: f32,
    pub last_read: i64,
    pub finished: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub entries: Vec<Entry>,
}

impl Index {
    fn path() -> PathBuf {
        store::config_dir().join("library.json")
    }
    pub fn load() -> Index {
        store::load_json(&Self::path())
    }
    pub fn save(&self) {
        store::save_json(&Self::path(), self);
    }
}

pub fn cover_path(id: &str) -> PathBuf {
    store::covers_dir().join(format!("{id}.jpg"))
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, out);
            }
        } else if crate::book::is_supported(&p) && !name.ends_with(".part") && !is_loose_doc(&p) {
            out.push(p);
        }
    }
}

/// Plain text / HTML / Markdown are only indexed when imported explicitly;
/// folder scans would otherwise sweep up every stray note and web page.
fn is_loose_doc(p: &Path) -> bool {
    matches!(crate::book::ext_of(p).as_str(), "txt" | "md" | "html" | "htm" | "xhtml")
}

fn mtime(m: &std::fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn thumb_aspect_of(id: &str) -> f32 {
    thumb_aspect(id)
}

pub fn save_thumbnail(id: &str, bytes: &[u8]) -> Option<f32> {
    let img = image::load_from_memory(bytes).ok()?;
    let aspect = img.height() as f32 / img.width().max(1) as f32;
    let img = if img.height() > 480 { img.resize(480, 480, image::imageops::FilterType::Triangle) } else { img };
    let rgb = img.to_rgb8();
    let p = cover_path(id);
    let _ = std::fs::create_dir_all(store::covers_dir());
    let f = std::fs::File::create(&p).ok()?;
    let mut w = std::io::BufWriter::new(f);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, 85).encode_image(&rgb).ok()?;
    Some(aspect)
}

fn thumb_aspect(id: &str) -> f32 {
    image::image_dimensions(cover_path(id)).map(|(w, h)| h as f32 / w.max(1) as f32).unwrap_or(0.0)
}

fn make_entry(path: &Path, meta: &std::fs::Metadata) -> Option<Entry> {
    let id = store::book_id(path)?;
    let m = crate::book::read_meta(path);
    let cover_aspect = if cover_path(&id).exists() { thumb_aspect(&id) } else { m.cover.as_ref().and_then(|c| save_thumbnail(&id, c)).unwrap_or(0.0) };
    let has_cover = cover_aspect > 0.0;
    let mut e = Entry {
        id: id.clone(),
        path: path.to_string_lossy().into_owned(),
        title: m.title,
        author: if m.author.is_empty() { crate::book::util::author_from_filename(path) } else { m.author },
        format: crate::book::ext_of(path).to_uppercase().replace(".ZIP", ""),
        size: meta.len(),
        mtime: mtime(meta),
        added: store::now(),
        language: m.language,
        series: m.series,
        publisher: m.publisher,
        description: m.description.chars().take(4000).collect(),
        has_cover,
        cover_aspect,
        ..Default::default()
    };
    // catalogue metadata saved at download time beats sparse embedded metadata
    if let Ok(t) = std::fs::read_to_string(store::covers_dir().join(format!("{id}.meta.json"))) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            let get = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            if let Some(x) = get("title") {
                e.title = x;
            }
            if let Some(x) = get("author") {
                e.author = x;
            }
            if e.publisher.is_empty() {
                e.publisher = get("publisher").unwrap_or_default();
            }
            if e.language.is_empty() {
                e.language = get("language").unwrap_or_default();
            }
            if e.description.is_empty() {
                e.description = get("description").unwrap_or_default();
            }
        }
    }
    // existing reading data (e.g. after the library index was deleted)
    if BookData::path_for(&id).exists() {
        let d = BookData::load(&id);
        e.fraction = d.fraction;
        e.last_read = d.last_read;
        e.finished = d.finished;
    }
    Some(e)
}

/// Rescan. `progress(done, total)` is called from worker threads.
pub fn scan(dirs: &[PathBuf], extra: &[String], hidden: &[String], old: &Index, progress: &(dyn Fn(usize, usize) + Sync)) -> Index {
    let mut files = Vec::new();
    for d in dirs {
        walk(d, 4, &mut files);
    }
    for f in extra {
        let p = PathBuf::from(f);
        if p.is_file() {
            files.push(p);
        }
    }
    files.sort();
    files.dedup();
    files.retain(|p| !hidden.iter().any(|h| Path::new(h) == p));
    let by_path: HashMap<&str, &Entry> = old.entries.iter().map(|e| (e.path.as_str(), e)).collect();
    let total = files.len();
    let done = AtomicUsize::new(0);
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Entry>> = Mutex::new(Vec::with_capacity(total));
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 6);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(p) = files.get(i) else { break };
                let Ok(meta) = std::fs::metadata(p) else { continue };
                let key = p.to_string_lossy();
                let e = match by_path.get(key.as_ref()) {
                    Some(old) if old.size == meta.len() && old.mtime == mtime(&meta) && !old.id.is_empty() => {
                        let mut e = (*old).clone();
                        if e.has_cover && e.cover_aspect == 0.0 {
                            e.cover_aspect = thumb_aspect(&e.id);
                        }
                        Some(e)
                    }
                    Some(old) => make_entry(p, &meta).map(|mut e| {
                        e.added = old.added;
                        e
                    }),
                    None => make_entry(p, &meta),
                };
                if let Some(e) = e {
                    out.lock().unwrap().push(e);
                }
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                if d % 8 == 0 || d == total {
                    progress(d, total);
                }
            });
        }
    });
    let mut entries = out.into_inner().unwrap();
    // same file copied twice → keep one entry per book id
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| seen.insert(e.id.clone()));
    Index { entries }
}

pub fn fmt_size(size: u64) -> String {
    if size >= 1_048_576 {
        format!("{:.1} MB", size as f64 / 1_048_576.0)
    } else {
        format!("{} KB", (size / 1024).max(1))
    }
}

pub fn matches(e: &Entry, q: &str) -> bool {
    let terms: Vec<String> = q.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
    if terms.is_empty() {
        return true;
    }
    let hay = format!("{} {} {} {} {}", e.title, e.author, e.series, e.format, e.publisher).to_lowercase();
    terms.iter().all(|t| hay.contains(t))
}
