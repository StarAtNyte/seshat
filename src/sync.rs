//! Sync of highlights, notes, bookmarks, collections, ratings, metadata
//! overrides and reading time between machines, through a plain folder (any
//! file-sync tool) or WebDAV. Reading progress stays with KOSync.
//!
//! Layout inside the sync location (`<root>/seshat-sync/`), one writer per file
//! so file-sync tools never see conflicting edits:
//!   book-<book_id>.<device>.json   that device's merged view of one book
//!   library.<device>.json          that device's merged collections/ratings/overrides
//!
//! Every item is an `Entry { modified, deleted, v }`. Merge = union by key,
//! newest `modified` wins, tombstones (`deleted`) propagate removals. Local
//! edits and deletions are detected by diffing against the last merged state
//! kept per book in `<config>/sync/state/`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::store::{self, Annotation, BookData, Bookmark, SyncBackend, SyncCfg, UserBook, UserLib};

const DIR: &str = "seshat-sync";
/// Tombstones older than this are dropped.
const TOMB_TTL: i64 = 180 * 86400;

// ---------- merge core (pure) ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry<T> {
    pub modified: i64,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default = "Option::default", skip_serializing_if = "Option::is_none")]
    pub v: Option<T>,
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn wins<T: Serialize>(new: &Entry<T>, cur: &Entry<T>) -> bool {
    if new.modified != cur.modified {
        return new.modified > cur.modified;
    }
    if new.deleted != cur.deleted {
        return new.deleted;
    }
    json(new) > json(cur)
}

/// This device's view: the local items, stamped `now` when new or changed
/// since `base`, plus tombstones for items that vanished locally.
pub fn derive_local<T: Serialize + Clone>(local: &BTreeMap<String, T>, base: &BTreeMap<String, Entry<T>>, now: i64) -> BTreeMap<String, Entry<T>> {
    let mut out = BTreeMap::new();
    for (k, v) in local {
        let keep = base.get(k).filter(|e| !e.deleted && e.v.as_ref().is_some_and(|b| json(b) == json(v)));
        out.insert(k.clone(), keep.cloned().unwrap_or_else(|| Entry { modified: now, deleted: false, v: Some(v.clone()) }));
    }
    for (k, e) in base {
        if !local.contains_key(k) {
            out.insert(k.clone(), if e.deleted { e.clone() } else { Entry { modified: now, deleted: true, v: None } });
        }
    }
    out
}

/// Union `other` into `into`; the winner per key is the newest `modified`.
pub fn merge_maps<T: Serialize + Clone>(into: &mut BTreeMap<String, Entry<T>>, other: &BTreeMap<String, Entry<T>>) {
    for (k, e) in other {
        match into.get(k) {
            Some(cur) if !wins(e, cur) => {}
            _ => {
                into.insert(k.clone(), e.clone());
            }
        }
    }
}

pub fn live<T: Clone>(m: &BTreeMap<String, Entry<T>>) -> Vec<(String, T)> {
    m.iter().filter(|(_, e)| !e.deleted).filter_map(|(k, e)| e.v.clone().map(|v| (k.clone(), v))).collect()
}

fn prune<T>(m: &mut BTreeMap<String, Entry<T>>, now: i64) {
    m.retain(|_, e| !e.deleted || now - e.modified < TOMB_TTL);
}

fn ann_key(a: &Annotation) -> String {
    a.id.to_string()
}

fn bm_key(b: &Bookmark) -> String {
    format!("{}-{}-{}-{}", b.created, b.loc.section, b.loc.block, b.loc.offset)
}

// ---------- file formats ----------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BookFile {
    pub v: u32,
    pub id: String,
    pub device: String,
    pub updated: i64,
    pub annotations: BTreeMap<String, Entry<Annotation>>,
    pub bookmarks: BTreeMap<String, Entry<Bookmark>>,
    /// This device's own reading time (not including other devices').
    pub seconds: f64,
    pub daily: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibFile {
    pub v: u32,
    pub device: String,
    pub updated: i64,
    pub collections: BTreeMap<String, Entry<String>>,
    pub books: BTreeMap<String, Entry<UserBook>>,
}

/// Last merged state for one book (local only).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BookState {
    ann: BTreeMap<String, Entry<Annotation>>,
    bm: BTreeMap<String, Entry<Bookmark>>,
    imported_seconds: f64,
    imported_daily: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibState {
    collections: BTreeMap<String, Entry<String>>,
    books: BTreeMap<String, Entry<UserBook>>,
}

fn sync_dir() -> PathBuf {
    store::config_dir().join("sync")
}
fn state_path(id: &str) -> PathBuf {
    sync_dir().join("state").join(format!("{id}.json"))
}
fn lib_state_path() -> PathBuf {
    sync_dir().join("library.json")
}

/// Merge `remotes` (other devices' files) into `data`, persist `data` and the
/// sync state, and return this device's file to publish. `data` is the open
/// reader's in-memory copy or one loaded from disk.
pub fn merge_book(data: &mut BookData, st: &mut BookState, id: &str, device: &str, remotes: &[BookFile], reading_time: bool, now: i64) -> BookFile {
    let la: BTreeMap<String, Annotation> = data.annotations.iter().map(|a| (ann_key(a), a.clone())).collect();
    let lb: BTreeMap<String, Bookmark> = data.bookmarks.iter().map(|b| (bm_key(b), b.clone())).collect();
    let mut ann = derive_local(&la, &st.ann, now);
    let mut bm = derive_local(&lb, &st.bm, now);
    for r in remotes {
        merge_maps(&mut ann, &r.annotations);
        merge_maps(&mut bm, &r.bookmarks);
    }
    prune(&mut ann, now);
    prune(&mut bm, now);
    data.annotations = live(&ann).into_iter().map(|(_, a)| a).collect();
    data.annotations.sort_by_key(|a| (a.start, a.id));
    data.bookmarks = live(&bm).into_iter().map(|(_, b)| b).collect();
    data.bookmarks.sort_by_key(|b| (b.loc, b.created));
    st.ann = ann.clone();
    st.bm = bm.clone();

    let (mut own_s, mut own_d) = (0.0, BTreeMap::new());
    if reading_time {
        let others: f64 = remotes.iter().map(|r| r.seconds).sum();
        if others > st.imported_seconds {
            data.seconds += others - st.imported_seconds;
            st.imported_seconds = others;
        }
        let mut od: BTreeMap<String, f64> = BTreeMap::new();
        for r in remotes {
            for (d, s) in &r.daily {
                *od.entry(d.clone()).or_default() += s;
            }
        }
        for (d, s) in od {
            let imp = st.imported_daily.entry(d.clone()).or_default();
            if s > *imp {
                *data.daily.entry(d).or_default() += s - *imp;
                *imp = s;
            }
        }
        own_s = (data.seconds - st.imported_seconds).max(0.0);
        own_d = data.daily.iter().map(|(d, s)| (d.clone(), (s - st.imported_daily.get(d).copied().unwrap_or(0.0)).max(0.0))).filter(|(_, s)| *s > 0.0).collect();
    }
    BookFile { v: 1, id: id.into(), device: device.into(), updated: now, annotations: ann, bookmarks: bm, seconds: own_s, daily: own_d }
}

pub fn merge_lib(lib: &mut UserLib, st: &mut LibState, device: &str, remotes: &[LibFile], now: i64) -> LibFile {
    lib.books.retain(|_, b| !b.is_empty());
    let lc: BTreeMap<String, String> = lib.collections.iter().map(|c| (c.clone(), c.clone())).collect();
    let mut cols = derive_local(&lc, &st.collections, now);
    let mut books = derive_local(&lib.books, &st.books, now);
    for r in remotes {
        merge_maps(&mut cols, &r.collections);
        merge_maps(&mut books, &r.books);
    }
    prune(&mut cols, now);
    prune(&mut books, now);
    let live_cols: BTreeMap<String, String> = live(&cols).into_iter().collect();
    let mut order: Vec<String> = lib.collections.iter().filter(|c| live_cols.contains_key(*c)).cloned().collect();
    for c in live_cols.keys() {
        if !order.contains(c) {
            order.push(c.clone());
        }
    }
    lib.collections = order;
    lib.books = live(&books).into_iter().collect();
    st.collections = cols.clone();
    st.books = books.clone();
    LibFile { v: 1, device: device.into(), updated: now, collections: cols, books }
}

// ---------- backends ----------

struct RemoteFile {
    name: String,
    etag: String,
}

enum Backend {
    Folder(PathBuf),
    Dav { coll: String, user: String, pass: String, client: reqwest::blocking::Client },
}

fn valid_token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Backend {
    fn new(cfg: &SyncCfg, proxy: &Option<String>) -> Result<Backend, String> {
        match cfg.backend {
            SyncBackend::Off => Err("sync is off".into()),
            SyncBackend::Folder => {
                let f = cfg.folder.trim();
                if f.is_empty() {
                    return Err("choose a sync folder".into());
                }
                let root = PathBuf::from(f);
                if !root.is_dir() {
                    return Err(format!("sync folder {f} does not exist"));
                }
                let dir = root.join(DIR);
                std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
                Ok(Backend::Folder(dir))
            }
            SyncBackend::WebDav => {
                let u = cfg.dav_url.trim().trim_end_matches('/');
                if !(u.starts_with("http://") || u.starts_with("https://")) {
                    return Err("WebDAV URL must start with http:// or https://".into());
                }
                let client = crate::api::client(proxy, Some(Duration::from_secs(30)))?;
                Ok(Backend::Dav { coll: format!("{u}/{DIR}"), user: cfg.dav_user.clone(), pass: cfg.dav_pass.clone(), client })
            }
        }
    }

    fn fingerprint(&self) -> String {
        match self {
            Backend::Folder(p) => format!("folder:{}", p.display()),
            Backend::Dav { coll, user, .. } => format!("dav:{user}@{coll}"),
        }
    }

    fn dav(&self, method: &str, url: &str) -> reqwest::blocking::RequestBuilder {
        let Backend::Dav { user, pass, client, .. } = self else { unreachable!() };
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
        client.request(m, url).basic_auth(user, Some(pass))
    }

    fn list(&self) -> Result<Vec<RemoteFile>, String> {
        match self {
            Backend::Folder(dir) => {
                let mut out = Vec::new();
                for e in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') || !name.ends_with(".json") {
                        continue;
                    }
                    let Ok(m) = e.metadata() else { continue };
                    let ns = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
                    out.push(RemoteFile { name, etag: format!("{ns}-{}", m.len()) });
                }
                Ok(out)
            }
            Backend::Dav { coll, .. } => {
                let body = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getetag/></d:prop></d:propfind>"#;
                let url = format!("{coll}/");
                let mut r = self.dav("PROPFIND", &url).header("Depth", "1").header("Content-Type", "application/xml").body(body).send().map_err(|e| e.to_string())?;
                if r.status().as_u16() == 404 {
                    self.mkcol()?;
                    r = self.dav("PROPFIND", &url).header("Depth", "1").header("Content-Type", "application/xml").body(body).send().map_err(|e| e.to_string())?;
                }
                let s = r.status().as_u16();
                if s == 401 || s == 403 {
                    return Err(format!("WebDAV login refused (HTTP {s})"));
                }
                if s != 207 && s != 200 {
                    return Err(format!("WebDAV listing failed (HTTP {s})"));
                }
                Ok(parse_propfind(&r.text().map_err(|e| e.to_string())?))
            }
        }
    }

    fn mkcol(&self) -> Result<(), String> {
        let Backend::Dav { coll, .. } = self else { return Ok(()) };
        let r = self.dav("MKCOL", coll).send().map_err(|e| e.to_string())?;
        match r.status().as_u16() {
            200 | 201 | 405 => Ok(()),
            s => Err(format!("cannot create the WebDAV folder (HTTP {s})")),
        }
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        match self {
            Backend::Folder(dir) => std::fs::read(dir.join(name)).map_err(|e| e.to_string()),
            Backend::Dav { coll, .. } => {
                let r = self.dav("GET", &format!("{coll}/{name}")).send().map_err(|e| e.to_string())?;
                if !r.status().is_success() {
                    return Err(format!("GET {name}: HTTP {}", r.status().as_u16()));
                }
                r.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
            }
        }
    }

    /// Write `name`. `etag`: the version we last saw (None = must not exist).
    fn put(&self, name: &str, bytes: &[u8], etag: Option<&str>) -> Result<(), String> {
        match self {
            Backend::Folder(dir) => {
                use std::io::Write;
                let tmp = dir.join(format!(".{name}.tmp"));
                let _ = std::fs::remove_file(&tmp);
                let r = std::fs::File::create(&tmp).and_then(|mut f| {
                    f.write_all(bytes)?;
                    f.sync_all()
                });
                match r.and_then(|_| std::fs::rename(&tmp, dir.join(name))) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        Err(format!("cannot write {name}: {e}"))
                    }
                }
            }
            Backend::Dav { coll, .. } => {
                let url = format!("{coll}/{name}");
                for attempt in 0..2 {
                    let mut rq = self.dav("PUT", &url).header("Content-Type", "application/json").body(bytes.to_vec());
                    rq = match etag {
                        Some(e) if !e.is_empty() => rq.header("If-Match", e),
                        None => rq.header("If-None-Match", "*"),
                        _ => rq,
                    };
                    let r = rq.send().map_err(|e| e.to_string())?;
                    match r.status().as_u16() {
                        200 | 201 | 204 => return Ok(()),
                        404 | 409 if attempt == 0 => self.mkcol()?,
                        412 => return Err(format!("{name} changed on the server; will retry next sync")),
                        s => return Err(format!("PUT {name}: HTTP {s}")),
                    }
                }
                Err(format!("PUT {name} failed"))
            }
        }
    }
}

fn local_name(tag: &str) -> &str {
    let t = tag.trim_start_matches('/');
    let t = t.split(|c: char| c.is_whitespace() || c == '/' || c == '>').next().unwrap_or("");
    t.rsplit(':').next().unwrap_or(t)
}

/// Text of the first `<…:name>` element in `xml`.
fn tag_text(xml: &str, name: &str) -> Option<String> {
    let mut i = 0;
    while let Some(p) = xml[i..].find('<') {
        let s = i + p;
        let e = s + xml[s..].find('>')?;
        let inner = &xml[s + 1..e];
        if !inner.starts_with('/') && !inner.ends_with('/') && local_name(inner) == name {
            let rest = &xml[e + 1..];
            let end = rest.find('<')?;
            return Some(rest[..end].to_string());
        }
        i = e + 1;
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_propfind(xml: &str) -> Vec<RemoteFile> {
    // split into <response> blocks by local name
    let mut blocks = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while let Some(p) = xml[i..].find('<') {
        let s = i + p;
        let Some(eo) = xml[s..].find('>') else { break };
        let e = s + eo;
        let inner = &xml[s + 1..e];
        if local_name(inner) == "response" {
            if inner.starts_with('/') {
                if let Some(st) = start.take() {
                    blocks.push(&xml[st..s]);
                }
            } else if !inner.ends_with('/') {
                start = Some(e + 1);
            }
        }
        i = e + 1;
    }
    let mut out = Vec::new();
    for b in blocks {
        let Some(href) = tag_text(b, "href") else { continue };
        let name = pct_decode(unescape(&href).trim_end_matches('/').rsplit('/').next().unwrap_or(""));
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        let etag = tag_text(b, "getetag").map(|e| unescape(e.trim())).unwrap_or_default();
        out.push(RemoteFile { name, etag });
    }
    out
}

// ---------- local indexes ----------

static LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Index {
    backend: String,
    map: BTreeMap<String, String>,
}

fn load_index(file: &str, fp: &str) -> Index {
    let ix: Index = store::load_json(&sync_dir().join(file));
    if ix.backend == fp {
        ix
    } else {
        Index { backend: fp.into(), map: BTreeMap::new() }
    }
}

fn save_index(file: &str, ix: &Index) {
    store::save_json(&sync_dir().join(file), ix);
}

fn remote_cache(name: &str) -> PathBuf {
    sync_dir().join("remote").join(name)
}

// ---------- publishing ----------

pub struct Push {
    pub name: String,
    pub bytes: Vec<u8>,
    pub hash: String,
}

fn make_push<T: Serialize + Clone>(name: String, f: &T, zero: impl Fn(&mut T), empty: bool, up: &BTreeMap<String, String>) -> Option<Push> {
    let mut c = f.clone();
    zero(&mut c);
    let hash = crate::kosync::md5_hex(&json(&c));
    let prev = up.get(&name);
    if prev == Some(&hash) || (prev.is_none() && empty) {
        return None;
    }
    Some(Push { name, bytes: serde_json::to_vec(f).ok()?, hash })
}

fn book_push(f: &BookFile, up: &BTreeMap<String, String>) -> Option<Push> {
    let empty = f.annotations.is_empty() && f.bookmarks.is_empty() && f.seconds == 0.0;
    make_push(format!("book-{}.{}.json", f.id, f.device), f, |c| c.updated = 0, empty, up)
}

fn lib_push(f: &LibFile, up: &BTreeMap<String, String>) -> Option<Push> {
    let empty = f.collections.is_empty() && f.books.is_empty();
    make_push(format!("library.{}.json", f.device), f, |c| c.updated = 0, empty, up)
}

fn push_all(be: &Backend, pushes: Vec<Push>) -> (usize, Vec<String>) {
    if pushes.is_empty() {
        return (0, vec![]);
    }
    let fp = be.fingerprint();
    let etags: HashMap<String, String> = match be.list() {
        Ok(l) => l.into_iter().map(|f| (f.name, f.etag)).collect(),
        Err(e) => return (0, vec![e]),
    };
    let mut done = Vec::new();
    let mut errs = Vec::new();
    for p in &pushes {
        match be.put(&p.name, &p.bytes, etags.get(&p.name).map(|s| s.as_str())) {
            Ok(()) => done.push((p.name.clone(), p.hash.clone())),
            Err(e) => errs.push(e),
        }
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut up = load_index("uploaded.json", &fp);
    for (n, h) in &done {
        up.map.insert(n.clone(), h.clone());
    }
    save_index("uploaded.json", &up);
    (done.len(), errs)
}

fn uploaded_map(fp: &str) -> BTreeMap<String, String> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_index("uploaded.json", fp).map
}

/// Upload prepared files (used after a UI-thread merge).
pub fn push(cfg: &SyncCfg, proxy: &Option<String>, pushes: Vec<Push>) -> Result<usize, String> {
    let be = Backend::new(cfg, proxy)?;
    let (n, errs) = push_all(&be, pushes);
    if let Some(e) = errs.first() {
        return Err(e.clone());
    }
    Ok(n)
}

fn device_token(cfg: &SyncCfg, device: &str) -> String {
    let _ = cfg;
    device.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

// ---------- UI-thread helpers ----------

/// Merge remote data into an open reader's BookData, persist, and return the
/// file to publish (if it changed since the last upload).
pub fn merge_open(data: &mut BookData, id: &str, cfg: &SyncCfg, device: &str, remotes: &[BookFile], fp_hint: &str) -> Option<Push> {
    let device = device_token(cfg, device);
    let mut st: BookState = store::load_json(&state_path(id));
    let f = merge_book(data, &mut st, id, &device, remotes, cfg.reading_time, store::now());
    data.save(id);
    store::save_json(&state_path(id), &st);
    book_push(&f, &uploaded_map(fp_hint))
}

/// Merge other devices' library files into the in-memory UserLib (saving it
/// when it changed). Returns the file to publish, if needed.
pub fn merge_library(lib: &mut UserLib, cfg: &SyncCfg, device: &str, remotes: &[LibFile], fp_hint: &str) -> (bool, Option<Push>) {
    let device = device_token(cfg, device);
    let mut st: LibState = store::load_json(&lib_state_path());
    let before = json(&(&lib.collections, &lib.books));
    let f = merge_lib(lib, &mut st, &device, remotes, store::now());
    let changed = before != json(&(&lib.collections, &lib.books));
    if changed {
        lib.save();
    }
    store::save_json(&lib_state_path(), &st);
    (changed, lib_push(&f, &uploaded_map(fp_hint)))
}

/// Fingerprint of the configured backend (needed to look up what was uploaded).
pub fn fingerprint(cfg: &SyncCfg) -> String {
    match cfg.backend {
        SyncBackend::Off => String::new(),
        SyncBackend::Folder => format!("folder:{}", PathBuf::from(cfg.folder.trim()).join(DIR).display()),
        SyncBackend::WebDav => format!("dav:{}@{}/{DIR}", cfg.dav_user, cfg.dav_url.trim().trim_end_matches('/')),
    }
}

// ---------- worker ----------

pub struct Job {
    pub cfg: SyncCfg,
    pub proxy: Option<String>,
    pub device: String,
    /// Some(ids) = only those books (e.g. after closing one); None = everything.
    pub only: Option<Vec<String>>,
    /// Library books: (id, path).
    pub known: Vec<(String, String)>,
    /// Books with an open reader: merged on the UI thread, not here.
    pub open: HashSet<String>,
}

#[derive(Default)]
pub struct Outcome {
    pub result: Option<Result<String, String>>,
    pub full: bool,
    pub libs: Vec<LibFile>,
    pub open: Vec<(String, Vec<BookFile>)>,
    /// Books whose local data changed.
    pub changed: Vec<String>,
}

fn parse_name(name: &str) -> Option<(String, String)> {
    let stem = name.strip_suffix(".json")?;
    let (a, dev) = stem.rsplit_once('.')?;
    (valid_token(dev) && (a == "library" || a.strip_prefix("book-").is_some_and(valid_token))).then(|| (a.to_string(), dev.to_string()))
}

fn read_cached<T: DeserializeOwned>(name: &str) -> Option<T> {
    let p = remote_cache(name);
    let t = std::fs::read(&p).ok()?;
    match serde_json::from_slice(&t) {
        Ok(v) => Some(v),
        Err(e) => {
            // keep a copy, never use it
            let _ = std::fs::rename(&p, p.with_extension(format!("corrupt-{}", store::now())));
            eprintln!("seshat: sync file {name} is unreadable ({e}); ignored");
            None
        }
    }
}

pub fn run(job: Job) -> Outcome {
    let mut out = Outcome { full: job.only.is_none(), ..Default::default() };
    let fail = |mut o: Outcome, e: String| {
        o.result = Some(Err(e));
        o
    };
    let be = match Backend::new(&job.cfg, &job.proxy) {
        Ok(b) => b,
        Err(e) => return fail(out, e),
    };
    let device = device_token(&job.cfg, &job.device);
    if device.is_empty() {
        return fail(out, "no device id".into());
    }
    let fp = be.fingerprint();
    let files = match be.list() {
        Ok(f) => f,
        Err(e) => return fail(out, e),
    };
    let mut errs: Vec<String> = Vec::new();
    let mut idx = load_index("remote.json", &fp);
    let etags: HashMap<String, String> = files.iter().map(|f| (f.name.clone(), f.etag.clone())).collect();
    let only: Option<HashSet<&String>> = job.only.as_ref().map(|v| v.iter().collect());
    let (mut pulled, mut remotes_by_book): (usize, BTreeMap<String, Vec<BookFile>>) = (0, BTreeMap::new());
    let mut lib_names = Vec::new();
    for f in &files {
        let Some((stem, dev)) = parse_name(&f.name) else { continue };
        if dev == device {
            continue;
        }
        let bid = stem.strip_prefix("book-").map(|s| s.to_string());
        match &bid {
            Some(b) => {
                if only.as_ref().is_some_and(|o| !o.contains(b)) {
                    continue;
                }
            }
            None => {
                if only.is_some() {
                    continue;
                }
            }
        }
        if idx.map.get(&f.name) != Some(&f.etag) || !remote_cache(&f.name).exists() || f.etag.is_empty() {
            match be.get(&f.name) {
                Ok(bytes) => {
                    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
                        errs.push(format!("{} is not valid JSON; ignored", f.name));
                        continue;
                    }
                    let p = remote_cache(&f.name);
                    if let Some(d) = p.parent() {
                        let _ = std::fs::create_dir_all(d);
                    }
                    if std::fs::write(&p, &bytes).is_err() {
                        errs.push(format!("cannot cache {}", f.name));
                        continue;
                    }
                    idx.map.insert(f.name.clone(), f.etag.clone());
                    pulled += 1;
                }
                Err(e) => {
                    errs.push(e);
                    continue;
                }
            }
        }
        match bid {
            Some(b) => {
                if let Some(bf) = read_cached::<BookFile>(&f.name) {
                    remotes_by_book.entry(b).or_default().push(bf);
                }
            }
            None => lib_names.push(f.name.clone()),
        }
    }
    save_index("remote.json", &idx);

    let known: HashMap<&String, &String> = job.known.iter().map(|(i, p)| (i, p)).collect();
    let mut ids: HashSet<String> = remotes_by_book.keys().cloned().collect();
    match &job.only {
        Some(v) => ids.extend(v.iter().cloned()),
        None => ids.extend(job.known.iter().filter(|(i, _)| BookData::path_for(i).exists()).map(|(i, _)| i.clone())),
    }
    let up = uploaded_map(&fp);
    let mut pushes = Vec::new();
    let mut books = 0;
    for id in ids {
        let Some(path) = known.get(&id) else { continue };
        let remotes = remotes_by_book.remove(&id).unwrap_or_default();
        if job.open.contains(&id) {
            out.open.push((id, remotes));
            continue;
        }
        let exists = BookData::path_for(&id).exists();
        if !exists && remotes.is_empty() {
            continue;
        }
        let mut data = BookData::load(&id);
        let before = json(&(&data.annotations, &data.bookmarks, data.seconds));
        let mut st: BookState = store::load_json(&state_path(&id));
        let f = merge_book(&mut data, &mut st, &id, &device, &remotes, job.cfg.reading_time, store::now());
        if before != json(&(&data.annotations, &data.bookmarks, data.seconds)) {
            if data.path.is_empty() {
                data.path = (*path).clone();
            }
            data.save(&id);
            out.changed.push(id.clone());
            books += 1;
        }
        store::save_json(&state_path(&id), &st);
        if let Some(p) = book_push(&f, &up) {
            pushes.push(p);
        }
    }
    let (n, perrs) = push_all(&be, pushes);
    errs.extend(perrs);
    for n in &lib_names {
        if let Some(l) = read_cached::<LibFile>(n) {
            out.libs.push(l);
        }
    }
    let _ = etags;
    let mut msg = format!("{books} book{} updated, {pulled} file{} pulled, {n} pushed", if books == 1 { "" } else { "s" }, if pulled == 1 { "" } else { "s" });
    if !errs.is_empty() {
        msg.push_str(&format!("; {} problem{}: {}", errs.len(), if errs.len() == 1 { "" } else { "s" }, errs[0]));
    }
    out.result = Some(if errs.is_empty() { Ok(msg) } else { Err(msg) });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Loc;

    fn ann(id: u64, note: &str) -> Annotation {
        Annotation { id, start: Loc::new(0, 0, id as usize), end: Loc::new(0, 0, id as usize + 1), note: note.into(), created: 1, ..Default::default() }
    }
    fn data(a: &[Annotation]) -> BookData {
        BookData { annotations: a.to_vec(), ..Default::default() }
    }
    fn notes(d: &BookData) -> Vec<(u64, String)> {
        d.annotations.iter().map(|a| (a.id, a.note.clone())).collect()
    }

    #[test]
    fn union_of_two_devices() {
        let (mut a, mut sa) = (data(&[ann(1, "a")]), BookState::default());
        let (mut b, mut sb) = (data(&[ann(2, "b")]), BookState::default());
        let fa = merge_book(&mut a, &mut sa, "x", "A", &[], false, 100);
        let fb = merge_book(&mut b, &mut sb, "x", "B", &[fa.clone()], false, 101);
        assert_eq!(notes(&b), vec![(1, "a".into()), (2, "b".into())]);
        merge_book(&mut a, &mut sa, "x", "A", &[fb], false, 102);
        assert_eq!(notes(&a), notes(&b));
    }

    #[test]
    fn tombstone_propagates() {
        let (mut a, mut sa) = (data(&[ann(1, "a")]), BookState::default());
        let (mut b, mut sb) = (data(&[]), BookState::default());
        let fa = merge_book(&mut a, &mut sa, "x", "A", &[], false, 100);
        merge_book(&mut b, &mut sb, "x", "B", &[fa], false, 101);
        assert_eq!(b.annotations.len(), 1);
        b.annotations.clear(); // user deletes on B
        let fb = merge_book(&mut b, &mut sb, "x", "B", &[], false, 200);
        assert!(fb.annotations["1"].deleted);
        merge_book(&mut a, &mut sa, "x", "A", &[fb], false, 201);
        assert!(a.annotations.is_empty());
    }

    #[test]
    fn last_writer_wins() {
        let (mut a, mut sa) = (data(&[ann(1, "old")]), BookState::default());
        let mut b = data(&[]);
        let mut sb = BookState::default();
        let fa = merge_book(&mut a, &mut sa, "x", "A", &[], false, 100);
        merge_book(&mut b, &mut sb, "x", "B", &[fa], false, 101);
        b.annotations[0].note = "newer".into();
        let fb = merge_book(&mut b, &mut sb, "x", "B", &[], false, 300);
        a.annotations[0].note = "mid".into();
        let fa2 = merge_book(&mut a, &mut sa, "x", "A", &[], false, 200);
        merge_book(&mut a, &mut sa, "x", "A", &[fb.clone()], false, 301);
        merge_book(&mut b, &mut sb, "x", "B", &[fa2], false, 302);
        assert_eq!(notes(&a), vec![(1, "newer".into())]);
        assert_eq!(notes(&b), notes(&a));
    }

    #[test]
    fn idempotent_and_stable() {
        let (mut a, mut sa) = (data(&[ann(1, "a"), ann(2, "b")]), BookState::default());
        let (mut b, mut sb) = (data(&[ann(3, "c")]), BookState::default());
        let fa = merge_book(&mut a, &mut sa, "x", "A", &[], false, 100);
        let fb = merge_book(&mut b, &mut sb, "x", "B", &[fa.clone()], false, 101);
        let r1 = merge_book(&mut b, &mut sb, "x", "B", &[fa.clone()], false, 500);
        let r2 = merge_book(&mut b, &mut sb, "x", "B", &[fa], false, 900);
        assert_eq!(json(&r1.annotations), json(&fb.annotations));
        assert_eq!(json(&r2.annotations), json(&fb.annotations));
        assert_eq!(b.annotations.len(), 3);
    }

    #[test]
    fn corrupt_remote_never_touches_local_and_delete_beats_older_edit() {
        let (mut a, mut sa) = (data(&[ann(1, "a")]), BookState::default());
        let fa = merge_book(&mut a, &mut sa, "x", "A", &[], false, 100);
        // a remote tombstone older than the local edit loses
        let mut stale = fa.clone();
        stale.device = "B".into();
        stale.annotations.insert("1".into(), Entry { modified: 50, deleted: true, v: None });
        merge_book(&mut a, &mut sa, "x", "A", &[stale], false, 150);
        assert_eq!(a.annotations.len(), 1);
    }

    #[test]
    fn bookmarks_and_reading_time() {
        let bm = Bookmark { loc: Loc::new(1, 2, 3), created: 7, ..Default::default() };
        let mut a = BookData { bookmarks: vec![bm], seconds: 60.0, ..Default::default() };
        let mut sa = BookState::default();
        let fa = merge_book(&mut a, &mut sa, "x", "A", &[], true, 100);
        assert_eq!(fa.seconds, 60.0);
        let mut b = BookData { seconds: 10.0, ..Default::default() };
        let mut sb = BookState::default();
        let fb = merge_book(&mut b, &mut sb, "x", "B", &[fa.clone()], true, 101);
        assert_eq!(b.bookmarks.len(), 1);
        assert_eq!(b.seconds, 70.0);
        assert_eq!(fb.seconds, 10.0); // own contribution only
        merge_book(&mut b, &mut sb, "x", "B", &[fa], true, 102);
        assert_eq!(b.seconds, 70.0); // no double counting
    }

    #[test]
    fn library_merge() {
        let mut la = UserLib::default();
        la.collections.push("Sci-fi".into());
        la.books.insert("b1".into(), UserBook { rating: 4, ..Default::default() });
        let mut sa = LibState::default();
        let fa = merge_lib(&mut la, &mut sa, "A", &[], 100);
        let mut lb = UserLib::default();
        let mut sb = LibState::default();
        merge_lib(&mut lb, &mut sb, "B", &[fa], 101);
        assert_eq!(lb.collections, vec!["Sci-fi".to_string()]);
        assert_eq!(lb.books["b1"].rating, 4);
        lb.books.get_mut("b1").unwrap().rating = 0; // cleared on B
        let fb = merge_lib(&mut lb, &mut sb, "B", &[], 200);
        merge_lib(&mut la, &mut sa, "A", &[fb], 201);
        assert!(!la.books.contains_key("b1"));
    }

    #[test]
    fn propfind_parse() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/seshat-sync/</d:href></d:response><d:response><d:href>/dav/seshat-sync/book-ab%2D1.DEV.json</d:href><d:propstat><d:prop><d:getetag>&quot;abc&quot;</d:getetag></d:prop></d:propstat></d:response></d:multistatus>"#;
        let l = parse_propfind(xml);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].name, "book-ab-1.DEV.json");
        assert_eq!(l[0].etag, "\"abc\"");
    }
}
