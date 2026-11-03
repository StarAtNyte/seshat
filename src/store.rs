//! Persistent settings and per-book data (progress, annotations, stats).

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::book::Loc;

// ---------- paths ----------

pub fn config_dir() -> PathBuf {
    let dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("zlib-reader");
    // One-time migration from the old app name.
    let old = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("zlib-lite");
    if !dir.exists() && old.exists() {
        let _ = std::fs::create_dir_all(&dir);
        for f in ["session.json", "settings.json"] {
            let _ = std::fs::copy(old.join(f), dir.join(f));
        }
    }
    dir
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("zlib-reader")
}

pub fn covers_dir() -> PathBuf {
    cache_dir().join("covers")
}

pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Atomic write: temp file + rename, so a crash never corrupts state.
pub fn save_json<T: Serialize>(path: &Path, v: &T) {
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let Ok(text) = serde_json::to_string(v) else { return };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
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
        return "never".into();
    }
    let d = now() - ts;
    match d {
        i64::MIN..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{} h ago", d / 3600),
        86400..=172799 => "yesterday".into(),
        _ if d < 86400 * 30 => format!("{} days ago", d / 86400),
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
            Theme::Light => "Light",
            Theme::Paper => "Paper",
            Theme::Sepia => "Sepia",
            Theme::Gray => "Gray",
            Theme::Dark => "Dark",
            Theme::Nord => "Nord",
            Theme::Black => "Black",
            Theme::Custom => "Custom",
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
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KoSync {
    pub enabled: bool,
    pub server: String,
    pub username: String,
    /// md5(password), as the protocol expects.
    pub userkey: String,
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
    pub translate_to: String,
    pub tts_rate: f32,
    pub reading_speed_cpm: f32,
    /// Z-Library source plugin: blend results into library search.
    pub zlib_enabled: bool,
    pub right_panel: bool,
    /// Library grouping: 0 none, 1 author, 2 status, 3 format, 4 folder.
    pub lib_group: u8,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            download_dir: dirs::download_dir().unwrap_or_else(|| PathBuf::from(".")).to_string_lossy().into_owned(),
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
            translate_to: "en".into(),
            tts_rate: 1.0,
            reading_speed_cpm: 1200.0,
            zlib_enabled: true,
            right_panel: true,
            lib_group: 0,
        }
    }
}

impl AppSettings {
    pub fn load() -> Self {
        let p = config_dir().join("settings.json");
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
        s
    }
    pub fn save(&self) {
        save_json(&config_dir().join("settings.json"), self);
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

// ---------- Z-Library session (kept from v0.2) ----------

#[derive(Default, Serialize, Deserialize)]
pub struct SavedSession {
    pub domain: String,
    pub user_id: String,
    pub user_key: String,
    pub email: String,
}

pub fn load_session() -> Option<crate::api::Session> {
    let s: SavedSession = load_json(&config_dir().join("session.json"));
    if s.user_id.is_empty() || s.user_key.is_empty() {
        return None;
    }
    Some(crate::api::Session { domain: s.domain, user_id: s.user_id, user_key: s.user_key, email: s.email })
}

pub fn save_session(sess: &crate::api::Session) {
    let s = SavedSession { domain: sess.domain.clone(), user_id: sess.user_id.clone(), user_key: sess.user_key.clone(), email: sess.email.clone() };
    save_json(&config_dir().join("session.json"), &s);
}

pub fn clear_session() {
    let _ = std::fs::remove_file(config_dir().join("session.json"));
}
