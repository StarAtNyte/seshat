//! Library index: incremental, parallel scanning of book folders with cached
//! metadata and cover thumbnails (only new/changed files are parsed).

use crate::i18n::tr;
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
    pub genre: String,
    pub genre_checked: bool,
    pub fraction: f32,
    pub last_read: i64,
    pub finished: bool,
    /// Position in `series` (0 = unknown).
    pub series_index: f32,
    /// Stars shown (the user's rating, else Calibre's).
    pub rating: u8,
    pub base_rating: u8,
    /// Collections this book is in (from the user's library data).
    pub collections: Vec<String>,
    /// Calibre sidecar / series index have been looked at for this entry.
    pub enriched: bool,
    /// The file's own metadata while a user override is applied.
    pub raw: Option<Raw>,
    /// Other paths holding the identical file (same content id).
    pub dup_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Raw {
    pub title: String,
    pub author: String,
    pub series: String,
    pub series_index: f32,
    pub genre: String,
}

/// Put the user's overrides, rating and collections on top of an entry. Safe
/// to call repeatedly; clearing an override restores the file's own values.
pub fn apply_user(e: &mut Entry, ub: Option<&store::UserBook>) {
    let ov = ub.is_some_and(|u| u.has_override());
    if ov && e.raw.is_none() {
        e.raw = Some(Raw { title: e.title.clone(), author: e.author.clone(), series: e.series.clone(), series_index: e.series_index, genre: e.genre.clone() });
    }
    if let Some(r) = e.raw.clone() {
        e.title = r.title;
        e.author = r.author;
        e.series = r.series;
        e.series_index = r.series_index;
        e.genre = r.genre;
    }
    if !ov {
        e.raw = None;
    }
    if let Some(u) = ub {
        if !u.title.is_empty() {
            e.title = u.title.clone();
        }
        if !u.author.is_empty() {
            e.author = u.author.clone();
        }
        if !u.series.is_empty() {
            e.series = u.series.clone();
        }
        if let Some(i) = u.series_index {
            e.series_index = i;
        }
        if !u.genre.is_empty() {
            e.genre = u.genre.clone();
        }
    }
    e.rating = ub.map(|u| u.rating).filter(|&r| r > 0).unwrap_or(e.base_rating);
    e.collections = ub.map(|u| u.collections.clone()).unwrap_or_default();
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

pub struct DupItem {
    pub id: String,
    pub path: String,
    pub title: String,
    pub author: String,
    pub format: String,
    pub size: u64,
}

pub struct DupGroup {
    pub reason: &'static str,
    pub items: Vec<DupItem>,
}

/// Books that appear twice: the identical file at several paths, or the same
/// title and author in different files or formats.
pub fn find_duplicates(entries: &[Entry]) -> Vec<DupGroup> {
    let mut by_key: HashMap<String, Vec<&Entry>> = HashMap::new();
    for e in entries {
        let t = norm(&e.title);
        if t.len() < 3 {
            // too little to compare by name; only identical copies count
            by_key.entry(format!("id:{}", e.id)).or_default().push(e);
            continue;
        }
        let a = norm(e.author.split([',', ';', '&']).next().unwrap_or(""));
        by_key.entry(format!("{t}|{a}")).or_default().push(e);
    }
    let mut groups = Vec::new();
    for es in by_key.values() {
        let mut items = Vec::new();
        for e in es {
            items.push(DupItem { id: e.id.clone(), path: e.path.clone(), title: e.title.clone(), author: e.author.clone(), format: e.format.clone(), size: e.size });
            for p in &e.dup_paths {
                let path = Path::new(p);
                items.push(DupItem {
                    id: e.id.clone(),
                    path: p.clone(),
                    title: e.title.clone(),
                    author: e.author.clone(),
                    format: crate::book::ext_of(path).to_uppercase(),
                    size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(e.size),
                });
            }
        }
        if items.len() < 2 {
            continue;
        }
        let same = es.len() == 1;
        items.sort_by(|a, b| a.path.cmp(&b.path));
        groups.push(DupGroup { reason: if same { tr("Identical file in several places") } else { tr("Same title and author") }, items });
    }
    groups.sort_by(|a, b| a.items[0].title.to_lowercase().cmp(&b.items[0].title.to_lowercase()));
    groups
}

/// A cheap fingerprint of the library folders' directory tree (directory
/// mtimes, a few levels deep). It changes when files are added, removed or
/// renamed, so the caller knows when a rescan is worth doing.
pub fn dir_stamp(dirs: &[PathBuf]) -> u64 {
    use std::hash::{Hash, Hasher};
    fn go(dir: &Path, depth: usize, acc: &mut u64) {
        let Ok(md) = std::fs::metadata(dir) else { return };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dir.hash(&mut h);
        mtime(&md).hash(&mut h);
        *acc = acc.wrapping_add(h.finish());
        if depth == 0 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                go(&e.path(), depth - 1, acc);
            }
        }
    }
    let mut acc = 0u64;
    for d in dirs {
        go(d, 4, &mut acc);
    }
    acc
}

/// Series position from an EPUB's package file (Calibre or EPUB 3 style).
fn epub_series_index(path: &Path) -> f32 {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else { return 0.0 };
    let Ok(mut zip) = zip::ZipArchive::new(f) else { return 0.0 };
    let Some(name) = zip.file_names().find(|n| n.to_lowercase().ends_with(".opf")).map(|s| s.to_string()) else { return 0.0 };
    let Ok(mut file) = zip.by_name(&name) else { return 0.0 };
    let mut xml = String::new();
    if file.by_ref().take(2_000_000).read_to_string(&mut xml).is_err() {
        return 0.0;
    }
    if let Some(i) = xml.find("calibre:series_index") {
        let start = xml[..i].rfind('<').unwrap_or(0);
        let end = xml[i..].find('>').map(|x| i + x).unwrap_or(xml.len());
        let tag = &xml[start..end];
        if let Some(c) = tag.find("content=\"") {
            let v = &tag[c + 9..];
            return v.split('"').next().unwrap_or("").trim().parse().unwrap_or(0.0);
        }
    }
    if let Some(i) = xml.find("group-position") {
        if let Some(gt) = xml[i..].find('>') {
            let v = &xml[i + gt + 1..];
            return v.split('<').next().unwrap_or("").trim().parse().unwrap_or(0.0);
        }
    }
    0.0
}

/// Fill in what a cached entry lacks: Calibre sidecar data and series position.
fn enrich(e: &mut Entry, path: &Path) {
    if let Some(r) = e.raw.take() {
        e.title = r.title;
        e.author = r.author;
        e.series = r.series;
        e.series_index = r.series_index;
        e.genre = r.genre;
    }
    if let Some(c) = crate::calibre::read(path) {
        apply_calibre(e, &c);
        if !e.has_cover {
            if let Some(a) = c.cover.as_ref().and_then(|b| save_thumbnail(&e.id, b)) {
                e.has_cover = true;
                e.cover_aspect = a;
            }
        }
    }
    if !e.series.is_empty() && e.series_index == 0.0 && crate::book::ext_of(path) == "epub" {
        e.series_index = epub_series_index(path);
    }
    e.enriched = true;
}

fn apply_calibre(e: &mut Entry, c: &crate::calibre::CalMeta) {
    if !c.title.is_empty() {
        e.title = c.title.clone();
    }
    if !c.author.is_empty() {
        e.author = c.author.clone();
    }
    if !c.series.is_empty() {
        e.series = c.series.clone();
        e.series_index = c.series_index;
    }
    if c.rating > 0 {
        e.base_rating = c.rating;
    }
    let g = genre_of(&c.tags);
    if !g.is_empty() {
        e.genre = g;
    }
    if e.publisher.is_empty() {
        e.publisher = c.publisher.clone();
    }
    if e.language.is_empty() {
        e.language = c.language.clone();
    }
    if e.description.is_empty() {
        e.description = c.description.chars().take(4000).collect();
    }
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

/// After each scan, delete thumbnails of books that are no longer in the
/// library (deleted, moved out of the watched folders, or removed), so the
/// cover cache never outgrows the library. Covers fetched from the catalogue
/// (`{id}.meta.json`) are kept: the file itself can't recreate them if the
/// book is restored. Runs in the background.
pub fn prune_covers(ids: std::collections::HashSet<String>) {
    std::thread::spawn(move || {
        let dir = store::covers_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".jpg") else { continue };
            if !ids.contains(id) && !dir.join(format!("{id}.meta.json")).exists() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        // leftovers from the old "Read online", which downloaded into the cache
        let _ = std::fs::remove_dir_all(store::cache_dir().join("online"));
    });
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
    let cal = crate::calibre::read(path);
    let cover_aspect = if cover_path(&id).exists() {
        thumb_aspect(&id)
    } else {
        // a Calibre cover.jpg beside the book wins over the embedded cover
        cal.as_ref().and_then(|c| c.cover.as_ref()).or(m.cover.as_ref()).and_then(|c| save_thumbnail(&id, c)).unwrap_or(0.0)
    };
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
        genre: genre_of(&m.subjects),
        genre_checked: true,
        enriched: true,
        ..Default::default()
    };
    if let Some(c) = &cal {
        apply_calibre(&mut e, c);
    }
    if !e.series.is_empty() && e.series_index == 0.0 && crate::book::ext_of(path) == "epub" {
        e.series_index = epub_series_index(path);
    }
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
                        if !e.genre_checked {
                            e.genre = genre_of(&crate::book::read_meta(p).subjects);
                            e.genre_checked = true;
                        }
                        if e.has_cover && e.cover_aspect == 0.0 {
                            e.cover_aspect = thumb_aspect(&e.id);
                        }
                        if !e.enriched {
                            // one-time: series position and Calibre sidecar for entries indexed before
                            if let Some(r) = e.raw.take() {
                                e.title = r.title;
                                e.author = r.author;
                                e.series = r.series;
                                e.series_index = r.series_index;
                                e.genre = r.genre;
                            }
                            if e.series_index == 0.0 && !e.series.is_empty() && crate::book::ext_of(p) == "epub" {
                                e.series_index = epub_series_index(p);
                            }
                            if let Some(c) = crate::calibre::read(p) {
                                apply_calibre(&mut e, &c);
                                if !e.has_cover {
                                    if let Some(a) = c.cover.as_ref().and_then(|b| save_thumbnail(&e.id, b)) {
                                        e.has_cover = true;
                                        e.cover_aspect = a;
                                    }
                                }
                            }
                            e.enriched = true;
                        }
                        if !e.enriched {
                            enrich(&mut e, p);
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
    // ...but remember the other paths, so the duplicates view can list them
    let mut first: HashMap<String, usize> = HashMap::new();
    let mut kept: Vec<Entry> = Vec::with_capacity(entries.len());
    for mut e in entries {
        e.dup_paths.clear();
        match first.get(&e.id) {
            Some(&i) => kept[i].dup_paths.push(e.path),
            None => {
                first.insert(e.id.clone(), kept.len());
                kept.push(e);
            }
        }
    }
    Index { entries: kept }
}

/// A readable shelf name from free-form subjects ("FICTION / Literary" → "Fiction").
pub fn genre_of(subjects: &[String]) -> String {
    let generic = ["general", "fiction - general", "ebook", "ebooks", "book", "books", "english"];
    for s in subjects {
        let first = s.split(['/', '>', ';', ',', '|']).next().unwrap_or("").split(" -- ").next().unwrap_or("").trim();
        let low = first.to_lowercase();
        if first.is_empty() || first.chars().count() > 32 || generic.contains(&low.as_str()) || first.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        let mut out = String::new();
        for (i, w) in low.split_whitespace().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let mut c = w.chars();
            if let Some(f) = c.next() {
                if i == 0 || !matches!(w, "and" | "of" | "the" | "in" | "&") {
                    out.extend(f.to_uppercase());
                } else {
                    out.push(f);
                }
                out.push_str(c.as_str());
            }
        }
        return out;
    }
    String::new()
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
    let hay = format!("{} {} {} {} {} {}", e.title, e.author, e.series, e.format, e.publisher, e.collections.join(" ")).to_lowercase();
    terms.iter().all(|t| hay.contains(t))
}
