//! Persistent settings and per-book data (progress, annotations, stats).

use crate::i18n::tr;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::book::Loc;

// ---------- paths ----------

fn copy_tree(from: &Path, to: &Path) {
    let _ = std::fs::create_dir_all(to);
    if let Ok(rd) = std::fs::read_dir(from) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                copy_tree(&p, &to.join(e.file_name()));
            } else {
                let _ = std::fs::copy(&p, to.join(e.file_name()));
            }
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("seshat");
    // One-time migration from earlier names (zlib-reader, zlib-lite).
    if !dir.exists() {
        for old in ["zlib-reader", "zlib-lite"] {
            let o = base.join(old);
            if o.exists() {
                copy_tree(&o, &dir);
                break;
            }
        }
    }
    dir
}

pub fn cache_dir() -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    let dir = base.join("seshat");
    if !dir.exists() && base.join("zlib-reader").exists() {
        copy_tree(&base.join("zlib-reader"), &dir);
    }
    dir
}

pub fn covers_dir() -> PathBuf {
    cache_dir().join("covers")
}

/// Load JSON state, or Default if the file is missing. A file that exists but
/// fails to parse is moved aside to `<name>.corrupt-<unix ts>` first, so the
/// next save cannot silently overwrite the user's data.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            eprintln!("seshat: cannot read {}: {e}", path.display());
            return T::default();
        }
    };
    match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            name.push(format!(".corrupt-{}", now()));
            let aside = path.with_file_name(name);
            match std::fs::rename(path, &aside) {
                Ok(()) => eprintln!("seshat: {} is unreadable ({e}); moved it to {} and starting fresh", path.display(), aside.display()),
                Err(re) => eprintln!("seshat: {} is unreadable ({e}) and could not be moved aside: {re}", path.display()),
            }
            T::default()
        }
    }
}

/// Atomic write: temp file + fsync + rename, so a crash never corrupts state.
pub fn save_json<T: Serialize>(path: &Path, v: &T) {
    write_json(path, v, false);
}

/// Like `save_json`, but the file is readable by the owner only (0600 on
/// unix): for files holding credentials.
pub fn save_json_private<T: Serialize>(path: &Path, v: &T) {
    write_json(path, v, true);
}

/// Tighten an existing credentials file written by older versions (0644).
pub fn restrict_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(path) {
            if m.permissions().mode() & 0o077 != 0 {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn write_json<T: Serialize>(path: &Path, v: &T, private: bool) {
    use std::io::Write;
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let Ok(text) = serde_json::to_string(v) else { return };
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let ok = opts.open(&tmp).and_then(|mut f| {
        f.write_all(text.as_bytes())?;
        f.sync_all()
    });
    match ok {
        Ok(()) => {
            if std::fs::rename(&tmp, path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        Err(e) => {
            eprintln!("seshat: cannot save {}: {e}", path.display());
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// UTC calendar date "YYYY-MM-DD" for a unix timestamp.
pub fn date_string(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    // Howard Hinnant's civil_from_days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn relative_time(ts: i64) -> String {
    if ts == 0 {
        return tr("never").into();
    }
    let d = now() - ts;
    match d {
        i64::MIN..=59 => tr("just now").into(),
        60..=3599 => trf!("{n} min ago", n = d / 60),
        3600..=86399 => trf!("{n} h ago", n = d / 3600),
        86400..=172799 => tr("yesterday").into(),
        _ if d < 86400 * 30 => trf!("{n} days ago", n = d / 86400),
        _ => date_string(ts),
    }
}

/// KOReader-compatible partial MD5 document id (enables KOSync interop).
pub fn book_id(path: &Path) -> Option<String> {
    use md5::{Digest, Md5};
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 1024];
    for i in -1i32..=10 {
        let off: u64 = if i < 0 { 0 } else { 1024u64 << (2 * i) };
        if f.seek(SeekFrom::Start(off)).is_err() {
            break;
        }
        let mut n = 0;
        while n < 1024 {
            match f.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(_) => break,
            }
        }
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ---------- reader settings ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Light,
    Paper,
    Sepia,
    Gray,
    Dark,
    Nord,
    Black,
    Custom,
}

pub const THEMES: &[Theme] = &[Theme::Light, Theme::Paper, Theme::Sepia, Theme::Gray, Theme::Dark, Theme::Nord, Theme::Black, Theme::Custom];

impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Theme::Light => tr("Light"),
            Theme::Paper => tr("Paper"),
            Theme::Sepia => tr("Sepia"),
            Theme::Gray => tr("Gray"),
            Theme::Dark => tr("Dark"),
            Theme::Nord => tr("Nord"),
            Theme::Black => tr("Black"),
            Theme::Custom => tr("Custom"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Layout {
    #[default]
    Paginated,
    Scrolled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Columns {
    #[default]
    Auto,
    One,
    Two,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderSettings {
    pub theme: Theme,
    pub custom_bg: [u8; 3],
    pub custom_fg: [u8; 3],
    /// Font family name; empty = best available serif.
    pub font: String,
    pub font_size: f32,
    pub line_height: f32,
    pub para_spacing: f32,
    pub letter_spacing: f32,
    pub margin: f32,
    pub max_width: f32,
    pub justify: bool,
    pub indent: bool,
    /// Ignore the book's own alignment/size choices.
    pub override_book: bool,
    pub layout: Layout,
    pub columns: Columns,
    pub show_header: bool,
    pub show_footer: bool,
    pub tap_zones: bool,
    pub wheel_turns_page: bool,
    pub dim_images: bool,
    /// Auto-advance: seconds per page (paged) / pixels per second (scroll).
    pub auto_secs: f32,
    pub auto_px: f32,
    pub page_anim: bool,
    /// Motion: 0 full (3D cover + page flips), 1 simple (quick fades), 2 off.
    pub motion: u8,
}

impl Default for ReaderSettings {
    fn default() -> Self {
        ReaderSettings {
            theme: Theme::Paper,
            custom_bg: [250, 246, 236],
            custom_fg: [40, 36, 30],
            font: String::new(),
            font_size: 20.0,
            line_height: 1.55,
            para_spacing: 0.5,
            letter_spacing: 0.0,
            margin: 56.0,
            max_width: 720.0,
            justify: true,
            indent: true,
            override_book: false,
            layout: Layout::Paginated,
            columns: Columns::Auto,
            show_header: true,
            show_footer: true,
            tap_zones: true,
            wheel_turns_page: true,
            dim_images: true,
            auto_secs: 25.0,
            auto_px: 30.0,
            page_anim: true,
            motion: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UiTheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LibView {
    #[default]
    Grid,
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortKey {
    #[default]
    Recent,
    Title,
    Author,
    Added,
    Progress,
    Size,
    Rating,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KoSync {
    pub enabled: bool,
    pub server: String,
    pub username: String,
    /// md5(password), as the protocol expects.
    pub userkey: String,
    /// This install's sync device id, generated once (see AppSettings::load).
    pub device_id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncBackend {
    #[default]
    Off,
    Folder,
    WebDav,
}

/// Highlights / notes / bookmarks / library sync (see sync.rs). The WebDAV
/// password lives in settings.json, which is written 0600.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncCfg {
    pub backend: SyncBackend,
    pub folder: String,
    pub dav_url: String,
    pub dav_user: String,
    pub dav_pass: String,
    pub reading_time: bool,
}

impl Default for SyncCfg {
    fn default() -> Self {
        SyncCfg { backend: SyncBackend::Off, folder: String::new(), dav_url: String::new(), dav_user: String::new(), dav_pass: String::new(), reading_time: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub download_dir: String,
    pub library_dirs: Vec<String>,
    pub extra_files: Vec<String>,
    pub hidden_paths: Vec<String>,
    pub proxy: String,
    pub ui_theme: UiTheme,
    pub reader: ReaderSettings,
    pub lib_view: LibView,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub cover_size: f32,
    pub kosync: KoSync,
    pub sync: SyncCfg,
    pub translate_to: String,
    /// Interface language: "" follows the OS, else a code from `i18n::LANGS`.
    pub language: String,
    pub tts_rate: f32,
    /// Read aloud with the downloadable natural voice (Kokoro) when installed.
    pub tts_natural: bool,
    /// Kokoro speaker id (see voice::VOICES).
    pub tts_voice: i32,
    pub reading_speed_cpm: f32,
    /// Z-Library source plugin: blend results into library search.
    pub zlib_enabled: bool,
    pub right_panel: bool,
    /// Library grouping: 0 none, 1 author, 2 status, 3 format, 4 folder, 5 genre, 6 collection, 7 series.
    pub lib_group: u8,
    pub motion_hint_shown: bool,
    /// Rescan when the library folders change on disk (polled while focused).
    pub watch_folders: bool,
    /// Folders holding offline StarDict / dictd dictionaries.
    pub dict_dirs: Vec<String>,
    /// Reader key bindings that differ from the defaults: action id -> chords.
    pub keymap: std::collections::BTreeMap<String, Vec<String>>,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            // a folder of its own: the whole Downloads folder is full of unrelated files
            download_dir: dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads"))).unwrap_or_else(|| PathBuf::from(".")).join("Seshat").to_string_lossy().into_owned(),
            library_dirs: Vec::new(),
            extra_files: Vec::new(),
            hidden_paths: Vec::new(),
            proxy: String::new(),
            ui_theme: UiTheme::System,
            reader: ReaderSettings::default(),
            lib_view: LibView::Grid,
            sort: SortKey::Recent,
            sort_desc: true,
            cover_size: 168.0,
            kosync: KoSync { server: "https://sync.koreader.rocks".into(), ..Default::default() },
            sync: SyncCfg::default(),
            translate_to: "en".into(),
            language: String::new(),
            tts_rate: 1.0,
            tts_natural: false,
            tts_voice: crate::voice::DEFAULT_VOICE,
            reading_speed_cpm: 1200.0,
            zlib_enabled: true,
            right_panel: true,
            lib_group: 0,
            motion_hint_shown: false,
            watch_folders: true,
            dict_dirs: Vec::new(),
            keymap: Default::default(),
        }
    }
}

impl AppSettings {
    pub fn load() -> Self {
        let p = config_dir().join("settings.json");
        restrict_perms(&p);
        let mut s: AppSettings = load_json(&p);
        if s.download_dir.is_empty() {
            s.download_dir = AppSettings::default().download_dir;
        }
        // First run: pick up a ~/Books folder if the user has one.
        if !config_dir().join("settings.json").exists() || s.library_dirs.is_empty() && !config_dir().join(".books-checked").exists() {
            let _ = std::fs::create_dir_all(config_dir());
            let _ = std::fs::write(config_dir().join(".books-checked"), "");
            if let Some(h) = dirs::home_dir() {
                for name in ["Books", "books", "Ebooks", "eBooks", "Calibre Library"] {
                    let d = h.join(name);
                    if d.is_dir() && !s.library_dirs.iter().any(|x| Path::new(x) == d) {
                        s.library_dirs.push(d.to_string_lossy().into_owned());
                    }
                }
            }
        }
        if s.kosync.server.is_empty() {
            s.kosync.server = "https://sync.koreader.rocks".into();
        }
        if s.kosync.device_id.is_empty() {
            s.kosync.device_id = crate::kosync::new_device_id();
            s.save();
        }
        s
    }
    pub fn save(&self) {
        // holds the KOSync userkey
        save_json_private(&config_dir().join("settings.json"), self);
    }
    pub fn proxy_opt(&self) -> Option<String> {
        let p = self.proxy.trim();
        (!p.is_empty()).then(|| p.to_string())
    }
    pub fn all_dirs(&self) -> Vec<PathBuf> {
        let mut v = vec![PathBuf::from(&self.download_dir)];
        for d in &self.library_dirs {
            let p = PathBuf::from(d);
            if !v.contains(&p) {
                v.push(p);
            }
        }
        v
    }
}

// ---------- per-book data ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HlColor {
    #[default]
    Yellow,
    Green,
    Blue,
    Pink,
    Purple,
}

pub const HL_COLORS: &[HlColor] = &[HlColor::Yellow, HlColor::Green, HlColor::Blue, HlColor::Pink, HlColor::Purple];

impl HlColor {
    pub fn rgb(self) -> [u8; 3] {
        match self {
            HlColor::Yellow => [250, 204, 21],
            HlColor::Green => [74, 222, 128],
            HlColor::Blue => [96, 165, 250],
            HlColor::Pink => [244, 114, 182],
            HlColor::Purple => [167, 139, 250],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HlStyle {
    #[default]
    Highlight,
    Underline,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Annotation {
    pub id: u64,
    pub start: Loc,
    /// Exclusive end; same section as `start`.
    pub end: Loc,
    pub color: HlColor,
    pub style: HlStyle,
    pub note: String,
    pub text: String,
    pub chapter: String,
    pub created: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Bookmark {
    pub loc: Loc,
    pub excerpt: String,
    pub chapter: String,
    pub created: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BookData {
    pub path: String,
    pub loc: Option<Loc>,
    pub fraction: f32,
    pub last_read: i64,
    pub finished: bool,
    pub bookmarks: Vec<Bookmark>,
    pub annotations: Vec<Annotation>,
    pub seconds: f64,
    pub daily: BTreeMap<String, f64>,
    pub settings: Option<ReaderSettings>,
    /// Fixed-layout zoom (0 = fit page, 1 = fit width).
    pub fixed_zoom: u8,
    pub fixed_rtl: bool,
    /// Fixed layout: crop white margins.
    pub fixed_crop: bool,
}

impl BookData {
    pub fn path_for(id: &str) -> PathBuf {
        config_dir().join("books").join(format!("{id}.json"))
    }
    pub fn load(id: &str) -> Self {
        load_json(&Self::path_for(id))
    }
    pub fn save(&self, id: &str) {
        save_json(&Self::path_for(id), self);
    }
}

// ---------- user library data: collections, ratings, metadata overrides ----------

/// What the user has set on one book. Book files are never modified: these
/// live in Seshat's own data and are applied on top of the file's metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserBook {
    /// 0 = unrated, 1..=5 stars
    pub rating: u8,
    pub collections: Vec<String>,
    // metadata overrides; empty / None = use the file's own
    pub title: String,
    pub author: String,
    pub series: String,
    pub series_index: Option<f32>,
    pub genre: String,
}

impl UserBook {
    pub fn has_override(&self) -> bool {
        !self.title.is_empty() || !self.author.is_empty() || !self.series.is_empty() || self.series_index.is_some() || !self.genre.is_empty()
    }
    pub fn is_empty(&self) -> bool {
        self.rating == 0 && self.collections.is_empty() && !self.has_override()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserLib {
    /// Collection names, in the order the user created them.
    pub collections: Vec<String>,
    pub books: BTreeMap<String, UserBook>,
}

impl UserLib {
    fn path() -> PathBuf {
        config_dir().join("userlib.json")
    }
    pub fn load() -> Self {
        load_json(&Self::path())
    }
    pub fn save(&mut self) {
        self.books.retain(|_, b| !b.is_empty());
        save_json(&Self::path(), self);
    }
    pub fn book(&self, id: &str) -> Option<&UserBook> {
        self.books.get(id)
    }
    pub fn book_mut(&mut self, id: &str) -> &mut UserBook {
        self.books.entry(id.to_string()).or_default()
    }
    pub fn add_collection(&mut self, name: &str) -> bool {
        let n = name.trim();
        if n.is_empty() || self.collections.iter().any(|c| c.eq_ignore_ascii_case(n)) {
            return false;
        }
        self.collections.push(n.to_string());
        true
    }
    pub fn remove_collection(&mut self, name: &str) {
        self.collections.retain(|c| c != name);
        for b in self.books.values_mut() {
            b.collections.retain(|c| c != name);
        }
    }
}

// ---------- Z-Library session (kept from v0.2) ----------

#[derive(Default, Serialize, Deserialize)]
pub struct SavedSession {
    pub domain: String,
    pub user_id: String,
    pub user_key: String,
    pub email: String,
}

pub fn load_session() -> Option<crate::api::Session> {
    let p = config_dir().join("session.json");
    restrict_perms(&p);
    let s: SavedSession = load_json(&p);
    if s.user_id.is_empty() || s.user_key.is_empty() {
        return None;
    }
    Some(crate::api::Session { domain: s.domain, user_id: s.user_id, user_key: s.user_key, email: s.email })
}

pub fn save_session(sess: &crate::api::Session) {
    let s = SavedSession { domain: sess.domain.clone(), user_id: sess.user_id.clone(), user_key: sess.user_key.clone(), email: sess.email.clone() };
    save_json_private(&config_dir().join("session.json"), &s);
}

pub fn clear_session() {
    let _ = std::fs::remove_file(config_dir().join("session.json"));
}
