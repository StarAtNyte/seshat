//! Application shell.
//!
//! Layout: a narrow editorial navigation column, a flexible main area
//! (featured book, recently opened, an asymmetric masonry index that can be
//! grouped into typographic "shelves"), and an optional right column with
//! progress, reading history and recent notes. Z-Library is a quiet source
//! plugin whose results blend into library search.
//!
//! Motion is light: eased hover lifts, sliding nav marker, view fade and a
//! staggered reveal — all via egui's animation cache, repainting only while
//! something is actually moving.

use crate::i18n::tr;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Painter, Rect, RichText, Sense, Stroke};

use crate::book::{Book, Loc};
use crate::fonts::{self, FontDb, Installed};
use crate::library::{self, Entry, Index};
use crate::online::Online;
use crate::reader::{Action, Env, Reader};
use crate::store::{self, Annotation, AppSettings, BookData, Bookmark, LibView, SortKey, Theme, UiTheme, UserLib};
use crate::texload::{Src, TexLoader};
use crate::widgets;
use crate::ui::{self, display, mono, mono_m, tag, text_button, ui_b, ui_m, Icon, Palette, ACCENT, INK, PAPER};

enum Msg {
    Scanned(Index),
    /// Per-book stats loaded off the UI thread: (generation, stats by id).
    Stats(u64, Vec<(String, BookStats)>),
    Opened(Result<(String, PathBuf, Arc<Book>), String>),
    Fonts(FontDb),
    Kosync(Result<String, String>),
    /// Fingerprint of the library folders' directory mtimes.
    Stamp(u64),
    Clippings(crate::clippings::Outcome),
    Sync(crate::sync::Outcome),
    SyncPushed(Result<usize, String>),
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    All,
    Reading,
    Unread,
    Finished,
    Notes,
    Bookmarks,
    Source,
    Stats,
    Duplicates,
}

/// The Details window's metadata editor.
struct MetaForm {
    id: String,
    title: String,
    author: String,
    series: String,
    index: String,
    genre: String,
}

/// The parts of a book's BookData that feed the library overview.
struct BookStats {
    annotations: Vec<Annotation>,
    bookmarks: Vec<Bookmark>,
    daily: BTreeMap<String, f64>,
    seconds: f64,
}

impl BookStats {
    fn of(d: &BookData) -> Self {
        BookStats { annotations: d.annotations.clone(), bookmarks: d.bookmarks.clone(), daily: d.daily.clone(), seconds: d.seconds }
    }
}

#[derive(Default)]
struct Overview {
    notes: Vec<(String, String, Annotation)>,
    bookmarks: Vec<(String, String, Bookmark)>,
    daily: BTreeMap<String, f64>,
    total_secs: f64,
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub struct App {
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    settings: AppSettings,
    saved_settings: String,
    last_settings_check: Instant,
    index: Index,
    overview: Overview,
    /// Cached per-book stats behind `overview`, stamped with the generation
    /// they were loaded at (a newer stamp wins over a slower full reload).
    stats: HashMap<String, (u64, BookStats)>,
    stats_gen: u64,
    /// Ids in the index when the running scan started (to spot removals made meanwhile).
    scan_ids: HashSet<String>,
    scanning: bool,
    scan_done: Arc<AtomicUsize>,
    scan_total: Arc<AtomicUsize>,
    rescan_after: bool,
    view: View,
    view_changed: Instant,
    query: String,
    format: String,
    covers: TexLoader,
    readers: Vec<Reader>,
    opening: Option<String>,
    pending_jump: Option<(String, Loc)>,
    error: Option<String>,
    fontdb: Option<FontDb>,
    families: Vec<String>,
    installed: Installed,
    font_req: (String, bool, &'static str, bool),
    /// (library size when checked, whether any title/author needs a CJK face)
    lib_cjk: (usize, bool),
    want_cjk: bool,
    online: Online,
    details: Option<String>,
    /// BookData for the Details window, loaded once per opened book.
    details_data: Option<(String, BookData)>,
    settings_open: bool,
    confirm_delete: Option<String>,
    style_key: Option<(Color32, Color32, bool)>,
    kosync_password: String,
    kosync_status: String,
    sync_status: String,
    sync_busy: bool,
    sync_pending_full: bool,
    sync_pending_ids: Vec<String>,
    sync_last: Option<Instant>,
    sync_was_focused: bool,
    pending_open: Option<PathBuf>,
    shelf_open: Option<String>,
    shelf_jump: Option<String>,
    debug_start: Option<Instant>,
    debug_keys_sent: usize,
    debug_shot_requested: bool,
    /// Natural-voice download in progress (or just finished / failed).
    userlib: UserLib,
    dups: Vec<library::DupGroup>,
    last_watch: Instant,
    watch_inflight: bool,
    watch_stamp: Option<u64>,
    meta_edit: Option<MetaForm>,
    new_coll: String,
    clip_status: String,
    clip_busy: bool,
}

impl App {
    /// Read-aloud settings: system voice or the downloadable natural voices
    /// (Kokoro for English, Piper voices for other languages).
    /// Read-aloud settings: engine, speed, natural voices (download, preview, remove).
    pub(crate) fn voice_settings(ui: &mut egui::Ui, settings: &mut crate::store::AppSettings, pal: &Palette) {
        let mut guard = crate::voice::UI_STATE.lock().unwrap();
        let st = &mut *guard;
        use crate::voice::{self, Stage};
        use ui::{form_note, form_row, form_section, FORM_CTRL_W};
        let _ = pal;
        let kokoro = voice::installed();
        let pipers = voice::installed_piper();
        let any_natural = kokoro || (!pipers.is_empty() && voice::helper().is_some());
        let system = crate::tts::available();
        form_row(ui, tr("Voice"), |ui| {
            if text_button(ui, tr("System"), !settings.tts_natural || !any_natural).clicked() {
                settings.tts_natural = false;
            }
            ui.add_enabled_ui(any_natural, |ui| {
                if text_button(ui, tr("Natural"), settings.tts_natural && any_natural)
                    .on_hover_text(tr("Kokoro for English books, and your downloaded voices for other languages. Books in other languages use the system voice."))
                    .on_disabled_hover_text(tr("Download a natural voice below first."))
                    .clicked()
                {
                    settings.tts_natural = true;
                }
            });
        });
        if !settings.tts_natural || !any_natural {
            form_note(ui, &match system {
                Some(e) => trf!("System engine: {name}", name = e),
                None if cfg!(target_os = "linux") => tr("No system speech engine found. Install speech-dispatcher or espeak-ng, or use the natural voice.").into(),
                None => tr("No system speech engine found.").into(),
            });
        }
        form_row(ui, tr("Speed"), |ui| ui.add(egui::Slider::new(&mut settings.tts_rate, 0.5..=2.5).step_by(0.1).fixed_decimals(1).suffix("×")));

        form_section(ui, tr("Natural voices"), Some(tr("Lifelike voices that run offline on this computer. Each is an optional download.")), false);
        let progress = st.install.as_ref().map(|i| i.progress());
        match progress {
            Some(p) if matches!(p.stage, Stage::Helper | Stage::Model) => {
                let what = if p.stage == Stage::Helper { tr("Downloading the voice engine") } else { tr("Downloading the voice") };
                let frac = if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 };
                form_row(ui, what, |ui| ui.add(egui::ProgressBar::new(frac).desired_width(ui::form_ctrl_w(ui) + 60.0)));
                form_note(ui, &format!("{:.0} of {:.0} MB", p.done as f64 / 1e6, p.total as f64 / 1e6));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                return;
            }
            Some(p) => {
                st.install = None;
                match p.stage {
                    Stage::Done => {
                        settings.tts_natural = true;
                        st.msg = tr("Voice installed.").into();
                    }
                    Stage::Failed(e) => st.msg = e,
                    _ => {}
                }
            }
            None => {}
        }
        let start = |st: &mut voice::UiState, ui: &egui::Ui, spec: String| {
            st.msg.clear();
            st.install = Some(voice::Install::start(ui.ctx().clone(), spec));
        };

        // English: Kokoro
        if kokoro {
            form_row(ui, tr("English"), |ui| {
                let mut v = settings.tts_voice;
                egui::ComboBox::from_id_salt("kokoro-voice").selected_text(voice::voice_label(v)).width(ui::form_ctrl_w(ui)).show_ui(ui, |ui| {
                    for (id, _, _) in voice::VOICES {
                        ui.selectable_value(&mut v, *id, voice::voice_label(*id));
                    }
                });
                if v != settings.tts_voice {
                    settings.tts_voice = v;
                    let _ = crate::tts::preview(v, None, settings.tts_rate);
                }
                if text_button(ui, tr("Preview"), false).clicked() {
                    if let Err(e) = crate::tts::preview(settings.tts_voice, None, settings.tts_rate) {
                        st.msg = e;
                    }
                }
                if text_button(ui, tr("Remove"), false).on_hover_text(trf!("Deletes the English natural voice (about {mb} MB) from this computer.", mb = voice::DISK_MB)).clicked() {
                    voice::remove();
                    st.msg = tr("English natural voice removed.").into();
                }
            });
        } else if voice::needs_update() {
            form_row(ui, tr("Update"), |ui| {
                if text_button(ui, tr("Update natural voices"), false).clicked() {
                    let spec = if voice::kokoro_on_disk() { "kokoro".to_string() } else { format!("piper:{}", pipers[0].2) };
                    start(st, ui, spec);
                }
            });
            form_note(ui, tr("The natural voices need a small update for this version of Seshat (about 30 MB)."));
            return;
        } else {
            form_row(ui, tr("English"), |ui| {
                if text_button(ui, &trf!("Download English natural voice ({mb} MB)", mb = voice::DOWNLOAD_MB), false).clicked() {
                    start(st, ui, "kokoro".into());
                }
            });
            form_note(ui, tr("Kokoro: a lifelike English voice with 28 speakers."));
        }

        // Other languages: Piper, one small voice per language
        for (_, name, pack) in &pipers {
            form_row(ui, name, |ui| {
                ui.add_sized([ui::form_ctrl_w(ui), 20.0], egui::Label::new(RichText::new(*pack).font(mono(11.5)).color(pal.muted)).truncate());
                if text_button(ui, tr("Preview"), false).clicked() {
                    if let Err(e) = crate::tts::preview(0, Some(pack), settings.tts_rate) {
                        st.msg = e;
                    }
                }
                if text_button(ui, tr("Remove"), false).clicked() {
                    voice::remove_piper(pack);
                    st.msg = trf!("{name} voice removed.", name = name);
                }
            });
        }
        let id = egui::Id::new("piper-pick");
        let mut pick: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default();
        let missing: Vec<_> = voice::PIPER_VOICES.iter().filter(|v| !pipers.iter().any(|p| p.2 == v.2)).collect();
        if !missing.is_empty() {
            form_row(ui, tr("Other languages"), |ui| {
                let label = missing.iter().find(|v| v.2 == pick).map(|v| v.1.to_string()).unwrap_or_else(|| tr("Choose a language…").into());
                egui::ComboBox::from_id_salt("piper-lang").selected_text(label).width(ui::form_ctrl_w(ui)).show_ui(ui, |ui| {
                    for v in &missing {
                        ui.selectable_value(&mut pick, v.2.to_string(), format!("{} · {} MB", v.1, v.3 / 1_000_000));
                    }
                });
                if missing.iter().any(|v| v.2 == pick) && text_button(ui, &trf!("Download ({mb} MB)", mb = voice::piper_size_mb(&pick)), false).clicked() {
                    start(st, ui, format!("piper:{pick}"));
                    pick.clear();
                }
            });
            ui.data_mut(|d| d.insert_temp(id, pick));
        }
        if !st.msg.is_empty() {
            form_note(ui, &st.msg.clone());
        }
    }

    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        // one pass per frame: page turns, clicks and key actions are not
        // idempotent, and a discarded pass would replay the same input
        ctx.options_mut(|o| o.max_passes = std::num::NonZeroUsize::MIN);
        let (tx, rx) = std::sync::mpsc::channel();
        let settings = AppSettings::load();
        crate::i18n::apply(&settings.language);
        let installed = fonts::install(&ctx, None, "", false, crate::i18n::current(), false);
        {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let db = FontDb::load();
                let _ = tx.send(Msg::Fonts(db));
                ctx.request_repaint();
            });
        }
        let saved_settings = serde_json::to_string(&settings).unwrap_or_default();
        let mut app = App {
            ctx: ctx.clone(),
            tx,
            rx,
            settings,
            saved_settings,
            last_settings_check: Instant::now(),
            index: Index::load(),
            overview: Overview::default(),
            stats: HashMap::new(),
            stats_gen: 0,
            scan_ids: HashSet::new(),
            scanning: false,
            scan_done: Arc::new(AtomicUsize::new(0)),
            scan_total: Arc::new(AtomicUsize::new(0)),
            rescan_after: false,
            view: View::All,
            view_changed: Instant::now(),
            query: String::new(),
            format: String::new(),
            covers: TexLoader::new(&ctx, 2),
            readers: Vec::new(),
            opening: None,
            userlib: UserLib::load(),
            dups: Vec::new(),
            last_watch: Instant::now(),
            watch_inflight: false,
            watch_stamp: None,
            meta_edit: None,
            new_coll: String::new(),
            clip_status: String::new(),
            clip_busy: false,
            pending_jump: None,
            error: None,
            fontdb: None,
            families: Vec::new(),
            installed,
            font_req: (String::from("\u{0}"), false, "en", false),
            lib_cjk: (usize::MAX, false),
            want_cjk: false,
            online: Online::new(&ctx),
            details: None,
            details_data: None,
            settings_open: false,
            confirm_delete: None,
            style_key: None,
            kosync_password: String::new(),
            kosync_status: String::new(),
            sync_status: String::new(),
            sync_busy: false,
            sync_pending_full: false,
            sync_pending_ids: Vec::new(),
            sync_last: None,
            sync_was_focused: true,
            pending_open: open,
            shelf_open: None,
            shelf_jump: None,
            debug_start: None,
            debug_keys_sent: 0,
            debug_shot_requested: false,
        };
        app.apply_user_all();
        app.refresh_dups();
        app.refresh_overview();
        app.rescan();
        if let Some(p) = app.pending_open.take() {
            app.open_path(&p);
        }
        app
    }

    fn set_view(&mut self, v: View) {
        if self.view != v {
            self.view = v;
            self.view_changed = Instant::now();
        }
    }

    /// Reload every book's stats on a worker thread; the overview is rebuilt
    /// from the cache now and again when the load arrives (Msg::Stats).
    fn refresh_overview(&mut self) {
        self.stats_gen += 1;
        let gen = self.stats_gen;
        let ids: Vec<String> = self.index.entries.iter().map(|e| e.id.clone()).collect();
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let v = ids
                .into_iter()
                .filter(|id| BookData::path_for(id).exists())
                .map(|id| {
                    let d = BookData::load(&id);
                    (id, BookStats::of(&d))
                })
                .collect();
            let _ = tx.send(Msg::Stats(gen, v));
            ctx.request_repaint();
        });
        self.rebuild_overview();
    }

    /// Update one book's stats (e.g. on reader close) without touching disk.
    fn update_overview_book(&mut self, id: &str, d: &BookData) {
        self.stats_gen += 1;
        self.stats.insert(id.to_string(), (self.stats_gen, BookStats::of(d)));
        self.rebuild_overview();
    }

    fn rebuild_overview(&mut self) {
        let mut o = Overview::default();
        for e in &self.index.entries {
            let Some((_, d)) = self.stats.get(&e.id) else { continue };
            for a in &d.annotations {
                o.notes.push((e.id.clone(), e.title.clone(), a.clone()));
            }
            for b in &d.bookmarks {
                o.bookmarks.push((e.id.clone(), e.title.clone(), b.clone()));
            }
            for (day, s) in &d.daily {
                *o.daily.entry(day.clone()).or_insert(0.0) += s;
            }
            o.total_secs += d.seconds;
        }
        o.notes.sort_by(|a, b| b.2.created.cmp(&a.2.created));
        o.bookmarks.sort_by(|a, b| b.2.created.cmp(&a.2.created));
        self.overview = o;
    }

    fn apply_user_all(&mut self) {
        for e in &mut self.index.entries {
            library::apply_user(e, self.userlib.book(&e.id));
        }
    }

    fn refresh_dups(&mut self) {
        self.dups = library::find_duplicates(&self.index.entries);
    }

    /// Re-apply one book's user data to its entry and persist everything.
    fn user_changed(&mut self, id: &str) {
        if let Some(e) = self.index.entries.iter_mut().find(|e| e.id == id) {
            library::apply_user(e, self.userlib.book(id));
        }
        self.userlib.save();
        self.index.save();
        self.refresh_dups();
    }

    fn set_rating(&mut self, id: &str, r: u8) {
        self.userlib.book_mut(id).rating = r;
        self.user_changed(id);
    }

    fn toggle_collection(&mut self, id: &str, name: &str) {
        let b = self.userlib.book_mut(id);
        match b.collections.iter().position(|c| c == name) {
            Some(i) => {
                b.collections.remove(i);
            }
            None => b.collections.push(name.to_string()),
        }
        self.user_changed(id);
    }

    fn new_collection_for(&mut self, id: &str, name: &str) {
        let n = name.trim().to_string();
        if n.is_empty() {
            return;
        }
        self.userlib.add_collection(&n);
        let n = self.userlib.collections.iter().find(|c| c.eq_ignore_ascii_case(&n)).cloned().unwrap_or(n);
        if !self.userlib.book(id).is_some_and(|b| b.collections.contains(&n)) {
            self.userlib.book_mut(id).collections.push(n);
        }
        self.user_changed(id);
    }

    /// Hide a file from the library (the file itself is untouched).
    fn remove_path(&mut self, path: &str) {
        if !self.settings.hidden_paths.iter().any(|h| h == path) {
            self.settings.hidden_paths.push(path.to_string());
        }
        self.settings.extra_files.retain(|f| f != path);
        self.index.entries.retain(|e| e.path != path);
        for e in &mut self.index.entries {
            e.dup_paths.retain(|p| p != path);
        }
        self.index.save();
        self.refresh_dups();
        self.rescan();
    }

    /// Add imported annotations to a book (to the open reader's copy if it is open).
    fn merge_annotations(&mut self, id: &str, anns: Vec<Annotation>) -> usize {
        let key = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
        let path = self.index.entries.iter().find(|e| e.id == id).map(|e| e.path.clone()).unwrap_or_default();
        let mut added = 0;
        if let Some(r) = self.readers.iter_mut().find(|r| r.id == id) {
            for a in anns {
                if !r.data.annotations.iter().any(|x| key(&x.text) == key(&a.text)) {
                    r.data.annotations.push(a);
                    added += 1;
                }
            }
            r.data.annotations.sort_by_key(|a| a.start);
            r.save();
        } else {
            let mut d = BookData::load(id);
            for a in anns {
                if !d.annotations.iter().any(|x| key(&x.text) == key(&a.text)) {
                    d.annotations.push(a);
                    added += 1;
                }
            }
            d.annotations.sort_by_key(|a| a.start);
            if d.path.is_empty() {
                d.path = path;
            }
            d.save(id);
        }
        added
    }

    fn import_clippings(&mut self) {
        let Some(f) = rfd::FileDialog::new().add_filter("Kindle clippings", &["txt"]).pick_file() else { return };
        let Ok(bytes) = std::fs::read(&f) else {
            self.clip_status = tr("Couldn't read that file.").into();
            return;
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let targets: Vec<crate::clippings::Target> = self.index.entries.iter().map(|e| crate::clippings::Target { id: e.id.clone(), path: e.path.clone(), title: e.title.clone(), author: e.author.clone() }).collect();
        self.clip_busy = true;
        self.clip_status = tr("Importing…").into();
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let o = crate::clippings::import(&text, &targets, &ctx);
            let _ = tx.send(Msg::Clippings(o));
            ctx.request_repaint();
        });
    }

    /// Poll the library folders' directory mtimes (cheap, off the UI thread).
    fn watch_folders(&mut self, ctx: &egui::Context) {
        if !self.settings.watch_folders {
            return;
        }
        ctx.request_repaint_after(Duration::from_secs(30));
        if self.scanning || self.watch_inflight || self.last_watch.elapsed() < Duration::from_secs(30) || !ctx.input(|i| i.focused) {
            return;
        }
        self.last_watch = Instant::now();
        self.watch_inflight = true;
        let dirs = self.settings.all_dirs();
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Stamp(library::dir_stamp(&dirs)));
            ctx.request_repaint();
        });
    }

    fn rescan(&mut self) {
        if self.scanning {
            self.rescan_after = true;
            return;
        }
        self.scanning = true;
        self.scan_done.store(0, Ordering::Relaxed);
        self.scan_total.store(0, Ordering::Relaxed);
        let dirs = self.settings.all_dirs();
        let extra = self.settings.extra_files.clone();
        let hidden = self.settings.hidden_paths.clone();
        let old = Index { entries: self.index.entries.clone() };
        self.scan_ids = old.entries.iter().map(|e| e.id.clone()).collect();
        let (tx, ctx, done, total) = (self.tx.clone(), self.ctx.clone(), self.scan_done.clone(), self.scan_total.clone());
        std::thread::spawn(move || {
            let ctx2 = ctx.clone();
            let idx = library::scan(&dirs, &extra, &hidden, &old, &move |d, t| {
                done.store(d, Ordering::Relaxed);
                total.store(t, Ordering::Relaxed);
                ctx2.request_repaint();
            });
            let _ = tx.send(Msg::Scanned(idx));
            ctx.request_repaint();
        });
    }

    fn open_path(&mut self, path: &Path) {
        self.open_path_with(path, true);
    }

    /// `shelve = false` opens a book (e.g. read online) without adding it to the library.
    fn open_path_with(&mut self, path: &Path, shelve: bool) {
        if self.opening.is_some() {
            return;
        }
        let in_lib = !shelve || self.index.entries.iter().any(|e| Path::new(&e.path) == path);
        let _ = in_lib;
        let in_lib = !shelve || self.index.entries.iter().any(|e| Path::new(&e.path) == path);
        if !in_lib {
            let s = path.to_string_lossy().into_owned();
            if !self.settings.extra_files.contains(&s) && !self.settings.all_dirs().iter().any(|d| path.starts_with(d)) {
                self.settings.extra_files.push(s.clone());
            }
            self.settings.hidden_paths.retain(|h| h != &s);
            self.rescan();
        }
        self.opening = Some(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        self.error = None;
        let p = path.to_path_buf();
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let res = (|| {
                let id = store::book_id(&p).ok_or("cannot read file")?;
                let book = crate::book::open(&p)?;
                Ok((id, p, Arc::new(book)))
            })();
            let _ = tx.send(Msg::Opened(res));
            ctx.request_repaint();
        });
    }

    fn open_at(&mut self, id: &str, loc: Loc) {
        if let Some(e) = self.index.entries.iter().find(|e| e.id == id) {
            let p = PathBuf::from(&e.path);
            self.pending_jump = Some((id.to_string(), loc));
            self.open_path(&p);
        }
    }

    fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Scanned(mut idx) => {
                    self.scanning = false;
                    // The scan worked from a snapshot; keep what changed since it started.
                    let live: HashMap<&str, &Entry> = self.index.entries.iter().map(|e| (e.id.as_str(), e)).collect();
                    let scan_ids = std::mem::take(&mut self.scan_ids);
                    let hidden = &self.settings.hidden_paths;
                    idx.entries.retain(|e| {
                        // removed from the library (or its file deleted) during the scan
                        let gone = scan_ids.contains(&e.id) && !live.contains_key(e.id.as_str());
                        !gone && !hidden.iter().any(|h| Path::new(h) == Path::new(&e.path))
                    });
                    for e in &mut idx.entries {
                        if let Some(l) = live.get(e.id.as_str()) {
                            e.fraction = l.fraction;
                            e.last_read = l.last_read;
                            e.finished = l.finished;
                            if l.has_cover {
                                e.has_cover = true;
                                e.cover_aspect = l.cover_aspect;
                            }
                            if l.genre_checked && !e.genre_checked {
                                e.genre = l.genre.clone();
                                e.genre_checked = true;
                            }
                        }
                    }
                    self.index = idx;
                    self.apply_user_all();
                    self.refresh_dups();
                    self.index.save();
                    self.refresh_overview();
                    library::prune_covers(self.index.entries.iter().map(|e| e.id.clone()).collect());
                    if std::mem::take(&mut self.rescan_after) {
                        self.rescan();
                    }
                }
                Msg::Opened(r) => {
                    self.opening = None;
                    match r {
                        Ok((id, path, book)) => {
                            if let Some(r) = self.readers.iter().find(|r| r.id == id) {
                                // already open: bring its window forward
                                self.ctx.send_viewport_cmd_to(r.viewport_id(), egui::ViewportCommand::Focus);
                                self.pending_jump = None;
                                continue;
                            }
                            let mut r = Reader::new(&self.ctx, id.clone(), path.clone(), book, &self.settings);
                            if let Some((pid, loc)) = self.pending_jump.take() {
                                if pid == id {
                                    r.jump(loc, false);
                                }
                            }
                            if let Some(e) = self.index.entries.iter_mut().find(|e| e.id == id) {
                                e.last_read = store::now();
                            }
                            self.readers.push(r);
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Msg::Stats(gen, v) => {
                    for (id, st) in v {
                        if self.stats.get(&id).is_some_and(|(g, _)| *g > gen) {
                            continue; // updated after this load began
                        }
                        self.stats.insert(id, (gen, st));
                    }
                    self.rebuild_overview();
                }
                Msg::Fonts(db) => {
                    self.families = db.families.clone();
                    self.fontdb = Some(db);
                    self.font_req = ("\u{0}".into(), false, "en", false);
                }
                Msg::Stamp(h) => {
                    self.watch_inflight = false;
                    if self.watch_stamp.is_some_and(|o| o != h) {
                        self.rescan();
                    }
                    self.watch_stamp = Some(h);
                }
                Msg::Clippings(o) => {
                    self.clip_busy = false;
                    let (books, unmatched, unlocated) = (o.books, o.unmatched, o.unlocated);
                    let mut added = 0;
                    for (id, anns) in o.per_book {
                        added += self.merge_annotations(&id, anns);
                    }
                    self.clip_status = trf!("Added {added} highlights to {books} books. {unmatched} not in your library, {unlocated} not found in the book text.", added = added, books = books, unmatched = unmatched, unlocated = unlocated);
                    self.refresh_overview();
                }
                Msg::Sync(o) => self.finish_sync(o),
                Msg::SyncPushed(r) => {
                    if let Err(e) = r {
                        self.sync_status = trf!("Sync upload failed: {err}", err = e);
                    }
                }
                Msg::Kosync(r) => {
                    self.kosync_status = match r {
                        Ok(s) => s,
                        Err(e) => e,
                    }
                }
            }
        }
    }

    /// Sync highlights/notes/bookmarks (and the library file for a full run)
    /// off the UI thread. `only` limits it to the given books.
    fn start_sync(&mut self, only: Option<Vec<String>>) {
        if self.settings.sync.backend == store::SyncBackend::Off {
            return;
        }
        if self.sync_busy {
            match only {
                None => self.sync_pending_full = true,
                Some(v) => self.sync_pending_ids.extend(v),
            }
            return;
        }
        self.sync_busy = true;
        self.sync_last = Some(Instant::now());
        self.sync_status = tr("Syncing…").into();
        let job = crate::sync::Job {
            cfg: self.settings.sync.clone(),
            proxy: self.settings.proxy_opt(),
            device: self.settings.kosync.device_id.clone(),
            only,
            known: self.index.entries.iter().map(|e| (e.id.clone(), e.path.clone())).collect(),
            open: self.readers.iter().map(|r| r.id.clone()).collect(),
        };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Sync(crate::sync::run(job)));
            ctx.request_repaint();
        });
    }

    fn finish_sync(&mut self, o: crate::sync::Outcome) {
        self.sync_busy = false;
        let cfg = self.settings.sync.clone();
        let device = self.settings.kosync.device_id.clone();
        let fp = crate::sync::fingerprint(&cfg);
        let mut pushes = Vec::new();
        // open readers: merge into their in-memory data so they don't overwrite it on close
        let mut touched = o.changed.clone();
        for (id, remotes) in &o.open {
            if let Some(r) = self.readers.iter_mut().find(|r| &r.id == id) {
                let before = (r.data.annotations.len(), r.data.bookmarks.len());
                if let Some(p) = crate::sync::merge_open(&mut r.data, id, &cfg, &device, remotes, &fp) {
                    pushes.push(p);
                }
                if before != (r.data.annotations.len(), r.data.bookmarks.len()) {
                    touched.push(id.clone());
                }
            }
        }
        if o.full {
            let (changed, p) = crate::sync::merge_library(&mut self.userlib, &cfg, &device, &o.libs, &fp);
            pushes.extend(p);
            if changed {
                self.rescan_userlib_views();
            }
        }
        if !pushes.is_empty() {
            let (proxy, tx, ctx) = (self.settings.proxy_opt(), self.tx.clone(), self.ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Msg::SyncPushed(crate::sync::push(&cfg, &proxy, pushes)));
                ctx.request_repaint();
            });
        }
        self.sync_status = match o.result {
            Some(Ok(s)) => trf!("Synced {time} — {detail}", time = chrono_hm(), detail = s),
            Some(Err(e)) => trf!("Sync problem {time} — {detail}", time = chrono_hm(), detail = e),
            None => String::new(),
        };
        if !touched.is_empty() {
            self.refresh_overview();
            if let Some(id) = self.details_data.as_ref().map(|(i, _)| i.clone()) {
                self.details_data = Some((id.clone(), BookData::load(&id)));
            }
        }
        if std::mem::take(&mut self.sync_pending_full) {
            self.sync_pending_ids.clear();
            self.start_sync(None);
        } else if !self.sync_pending_ids.is_empty() {
            let mut ids = std::mem::take(&mut self.sync_pending_ids);
            ids.sort();
            ids.dedup();
            self.start_sync(Some(ids));
        }
    }

    /// Re-apply collections/ratings/overrides to the library entries.
    fn rescan_userlib_views(&mut self) {
        for e in self.index.entries.iter_mut() {
            library::apply_user(e, self.userlib.book(&e.id));
        }
    }

    /// Full sync on start and when the window regains focus (debounced).
    fn sync_triggers(&mut self, ctx: &egui::Context) {
        if self.settings.sync.backend == store::SyncBackend::Off {
            return;
        }
        let focused = ctx.input(|i| i.focused);
        let regained = focused && !self.sync_was_focused;
        self.sync_was_focused = focused;
        let due = match self.sync_last {
            None => !self.scanning && !self.index.entries.is_empty(),
            Some(t) => regained && t.elapsed() > Duration::from_secs(120),
        };
        if due {
            self.start_sync(None);
        }
    }

    fn close_reader(&mut self, i: usize) {
        if i < self.readers.len() {
            let mut r = self.readers.remove(i);
            r.close(&self.settings);
            if let Some(e) = self.index.entries.iter_mut().find(|e| e.id == r.id) {
                e.fraction = r.data.fraction;
                e.last_read = r.data.last_read;
                e.finished = r.data.finished;
            }
            self.index.save();
            let (id, data) = (r.id.clone(), r.data.clone());
            self.update_overview_book(&id, &data);
            if self.details_data.as_ref().is_some_and(|(d, _)| d == &id) {
                self.details_data = Some((id, data));
            }
            self.view_changed = Instant::now();
            self.start_sync(Some(vec![r.id.clone()]));
        }
    }

    fn library_palette(&self, ctx: &egui::Context) -> Palette {
        let dark = match self.settings.ui_theme {
            UiTheme::Dark => true,
            UiTheme::Light => false,
            UiTheme::System => ctx.input(|i| i.raw.system_theme) == Some(egui::Theme::Dark),
        };
        ui::palette_for(if dark { Theme::Dark } else { Theme::Paper }, [0; 3], [0; 3])
    }

    fn apply_style(&mut self, ctx: &egui::Context, p: &Palette) {
        let key = (p.bg, p.fg, p.dark);
        if self.style_key != Some(key) {
            ui::style(ctx, p);
            self.style_key = Some(key);
        }
    }

    fn sync_fonts(&mut self, ctx: &egui::Context) {
        let family = match self.readers.last() {
            Some(r) => r.effective(&self.settings.reader).font,
            None => self.settings.reader.font.clone(),
        };
        if self.lib_cjk.0 != self.index.entries.len() {
            let any = self.index.entries.iter().any(|e| e.title.chars().chain(e.author.chars()).any(crate::book::script::is_cjk));
            self.lib_cjk = (self.index.entries.len(), any);
        }
        let req = (family, self.want_cjk, crate::i18n::current(), self.lib_cjk.1);
        if req != self.font_req {
            self.font_req = req.clone();
            self.installed = fonts::install(ctx, self.fontdb.as_mut(), &req.0, req.1, req.2, req.3);
        }
    }

    fn persist_settings(&mut self) {
        if self.last_settings_check.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_settings_check = Instant::now();
        let now = serde_json::to_string(&self.settings).unwrap_or_default();
        if now != self.saved_settings {
            self.settings.save();
            self.saved_settings = now;
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let r = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, "drop".into()));
            painter.rect_filled(r, CornerRadius::ZERO, ACCENT);
            painter.text(r.left_center() + vec2(60.0, -30.0), egui::Align2::LEFT_CENTER, tr("DROP"), ui_b(140.0), INK);
            painter.text(r.left_center() + vec2(66.0, 60.0), egui::Align2::LEFT_CENTER, tr("BOOKS OR FOLDERS INTO THE LIBRARY"), mono_m(14.0), INK);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if dropped.is_empty() {
            return;
        }
        let mut files = Vec::new();
        for p in dropped {
            if p.is_dir() {
                let s = p.to_string_lossy().into_owned();
                if !self.settings.library_dirs.contains(&s) {
                    self.settings.library_dirs.push(s);
                }
            } else if crate::book::is_supported(&p) {
                files.push(p);
            }
        }
        if files.len() == 1 {
            let f = files.remove(0);
            self.open_path(&f);
        } else {
            self.add_files(files);
        }
        self.rescan();
    }

    fn add_files(&mut self, files: Vec<PathBuf>) {
        for f in files {
            let s = f.to_string_lossy().into_owned();
            if !self.settings.extra_files.contains(&s) {
                self.settings.extra_files.push(s.clone());
            }
            self.settings.hidden_paths.retain(|h| h != &s);
        }
    }

    fn import_dialog(&mut self) {
        let exts: Vec<&str> = crate::book::SUPPORTED.to_vec();
        if let Some(mut files) = rfd::FileDialog::new().add_filter("Books", &exts).pick_files() {
            if files.len() == 1 {
                let f = files.remove(0);
                self.open_path(&f);
                return;
            }
            self.add_files(files);
            self.rescan();
        }
    }

    /// `ZR_SHOT=out.png` (debug aid): save a screenshot once startup settles, then quit.
    /// `ZR_KEYS=Right,type:woolf,click:400x300` injects input first; `ZR_WAIT` ms delay.
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        // reader-window captures may surface on the root's input
        if let Ok(rp) = std::env::var("ZR_SHOT_READER") {
            let shot = ctx.input(|i| i.raw.events.iter().find_map(|e| if let egui::Event::Screenshot { image, viewport_id, .. } = e { (*viewport_id != egui::ViewportId::ROOT).then(|| image.clone()) } else { None }));
            if let Some(img) = shot {
                let [w, h] = img.size;
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                    let _ = buf.save(&rp);
                }
                std::process::exit(0);
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        let Ok(path) = std::env::var("ZR_SHOT") else { return };
        if self.debug_start.is_none() {
            if let Some(p) = std::env::var("ZR_SETTINGS").ok().and_then(|p| p.parse::<usize>().ok()) {
                self.settings_open = true;
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("settings-page"), p));
            }
        }
        let started = *self.debug_start.get_or_insert_with(Instant::now);
        let t = started.elapsed();
        ctx.request_repaint_after(Duration::from_millis(50));
        if let Ok(keys) = std::env::var("ZR_KEYS") {
            let steps: Vec<&str> = keys.split(',').filter(|k| !k.is_empty()).collect();
            let i = ((t.as_millis() as i64 - 2500) / 400).max(-1);
            if i >= 0 && (i as usize) < steps.len() && self.debug_keys_sent <= i as usize {
                self.debug_keys_sent = i as usize + 1;
                let name = steps[i as usize];
                if let Some(rest) = name.strip_prefix("click:") {
                    let v: Vec<f32> = rest.split('x').filter_map(|x| x.parse().ok()).collect();
                    if v.len() == 2 {
                        let pos = pos2(v[0], v[1]);
                        ctx.input_mut(|inp| {
                            inp.events.push(egui::Event::PointerMoved(pos));
                            inp.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
                            inp.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
                        });
                    }
                } else if let Some(rest) = name.strip_prefix("hover:") {
                    let v: Vec<f32> = rest.split('x').filter_map(|x| x.parse().ok()).collect();
                    if v.len() == 2 {
                        ctx.input_mut(|inp| inp.events.push(egui::Event::PointerMoved(pos2(v[0], v[1]))));
                    }
                } else if let Some(text) = name.strip_prefix("type:") {
                    ctx.input_mut(|inp| inp.events.push(egui::Event::Text(text.to_string())));
                } else if let Some(k) = egui::Key::from_name(name) {
                    ctx.input_mut(|inp| inp.events.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }));
                }
            }
        }
        let wait = std::env::var("ZR_WAIT").ok().and_then(|w| w.parse().ok()).unwrap_or(4000u64);
        if t > Duration::from_millis(wait) && !self.debug_shot_requested {
            self.debug_shot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| i.raw.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }));
        if let Some(img) = shot {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                let _ = buf.save(&path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        while !self.readers.is_empty() {
            self.close_reader(0);
        }
        crate::tts::shutdown_helper();
        self.settings.save();
        self.index.save();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if crate::i18n::apply(&self.settings.language).1 {
            ctx.request_repaint();
        }
        self.debug_screenshot(ctx);
        self.drain();
        self.watch_folders(ctx);
        self.sync_triggers(ctx);

        self.covers.poll();
        for _ in std::mem::take(&mut self.online.downloaded) {
            self.rescan();
        }
        for id in std::mem::take(&mut self.online.covers_updated) {
            if let Some(e) = self.index.entries.iter_mut().find(|e| e.id == id) {
                e.has_cover = true;
                e.cover_aspect = library::thumb_aspect_of(&id);
            }
            self.covers.forget(&id);
            self.index.save();
        }
        self.sync_fonts(ctx);
        self.persist_settings();

        // debug aid: ZR_READER_IN_ROOT=1 renders the first book in the main window (capturable)
        if std::env::var("ZR_READER_IN_ROOT").is_ok() && !self.readers.is_empty() {
            if let Ok(keys) = std::env::var("ZR_READER_KEYS") {
                let started = *self.debug_start.get_or_insert_with(Instant::now);
                let steps: Vec<&str> = keys.split(',').filter(|k| !k.is_empty()).collect();
                let n = ((started.elapsed().as_millis() as i64 - 2500) / 700).max(-1);
                if n >= 0 && (n as usize) < steps.len() && self.debug_keys_sent <= n as usize {
                    self.debug_keys_sent = n as usize + 1;
                    let step = steps[n as usize];
                    if let Some(rest) = step.strip_prefix("click:") {
                        let v: Vec<f32> = rest.split('x').filter_map(|x| x.parse().ok()).collect();
                        if v.len() == 2 {
                            let pos = pos2(v[0], v[1]);
                            ctx.input_mut(|inp| {
                                inp.events.push(egui::Event::PointerMoved(pos));
                                inp.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
                                inp.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
                            });
                        }
                    } else if let Some(k) = egui::Key::from_name(step) {
                        ctx.input_mut(|inp| inp.events.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }));
                    }
                }
            }
            let r = &mut self.readers[0];
            let s = r.effective(&self.settings.reader);
            ui::style(ctx, &ui::palette(&s));
            let mut env = Env { app: &mut self.settings, families: &self.families, installed: &self.installed };
            // honour font requests here too, so captures show what a reader window shows
            for a in r.ui(ctx, &mut env) {
                if let Action::InstallFont { cjk: true, .. } = a {
                    self.want_cjk = true;
                }
            }
            return;
        }
        let pal = self.library_palette(ctx);
        self.apply_style(ctx, &pal);
        if ctx.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.command) {
            self.import_dialog();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::K) && i.modifiers.command) {
            self.set_view(View::All);
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("lib-search")));
        }
        let q = self.query.clone();
        self.online.tick(&q, &self.settings);
        self.title_bar(ctx, &pal);
        self.nav(ctx, &pal);
        // view transition: quick fade + rise
        let vt = ease(self.view_changed.elapsed().as_secs_f32() / 0.28);
        if vt < 1.0 {
            ctx.request_repaint();
        }
        match self.view {
            View::Source => {
                if let Some(p) = self.online.ui(ctx, &mut self.settings, &pal) {
                    self.open_path(&p);
                }
            }
            _ => {
                self.online.side_details(ctx, &mut self.settings, &pal);
                let show_right = self.settings.right_panel && ctx.screen_rect().width() > 1280.0 && matches!(self.view, View::All | View::Reading) && !self.index.entries.is_empty();
                if show_right {
                    self.right_column(ctx, &pal);
                }
                egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 32, right: 32, top: 24, bottom: 0 })).show(ctx, |ui| {
                    ui.multiply_opacity(vt);
                    ui.add_space((1.0 - vt) * 18.0);
                    match self.view {
                        View::Notes => self.notes_view(ui, &pal),
                        View::Bookmarks => self.bookmarks_view(ui, &pal),
                        View::Stats => self.stats_view(ui, &pal),
                        View::Duplicates => self.duplicates_view(ui, &pal),
                        _ => self.library_view(ui, &pal),
                    }
                });
            }
        }
        self.details_window(ctx, &pal);
        self.settings_window(ctx, &pal);
        self.status_overlays(ctx, &pal);
        self.handle_drops(ctx);
        ui::resize_edges(ctx);
        self.reader_windows(ctx);
    }
}

// ---------- windows ----------

impl App {
    /// Every open book lives in its own native window.
    fn reader_windows(&mut self, ctx: &egui::Context) {
        let mut close = Vec::new();
        for i in 0..self.readers.len() {
            let vid = self.readers[i].viewport_id();
            let builder = self.readers[i].viewport_builder();
            let mut actions = Vec::new();
            {
                let settings = &mut self.settings;
                let families = &self.families;
                let installed = &self.installed;
                let r = &mut self.readers[i];
                let debug_start = &mut self.debug_start;
                let keys_sent = &mut self.debug_keys_sent;
                let shot_req = &mut self.debug_shot_requested;
                ctx.show_viewport_immediate(vid, builder, |vctx, _class| {
                    let s = r.effective(&settings.reader);
                    ui::style(vctx, &ui::palette(&s));
                    let mut env = Env { app: settings, families, installed };
                    actions = r.ui(vctx, &mut env);
                    if vctx.input(|i| i.viewport().close_requested()) {
                        actions.push(Action::Close);
                    }
                    // debug aid: ZR_SHOT_READER=out.png captures this window
                    if let Ok(path) = std::env::var("ZR_SHOT_READER") {
                        let started = *debug_start.get_or_insert_with(Instant::now);
                        if let Ok(tf) = std::env::var("ZR_TRACE_FILE") {
                            use std::io::Write;
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(tf) {
                                let _ = writeln!(f, "reader frame {:?} req={}", started.elapsed(), shot_req);
                            }
                        }
                        vctx.request_repaint_after(Duration::from_millis(50));
                        if let Ok(keys) = std::env::var("ZR_READER_KEYS") {
                            let steps: Vec<&str> = keys.split(',').filter(|k| !k.is_empty()).collect();
                            let n = ((started.elapsed().as_millis() as i64 - 2500) / 500).max(-1);
                            if n >= 0 && (n as usize) < steps.len() && *keys_sent <= n as usize {
                                *keys_sent = n as usize + 1;
                                if let Some(k) = egui::Key::from_name(steps[n as usize]) {
                                    vctx.input_mut(|inp| inp.events.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }));
                                }
                            }
                        }
                        let wait = std::env::var("ZR_WAIT").ok().and_then(|w| w.parse().ok()).unwrap_or(4000u64);
                        if started.elapsed() > Duration::from_millis(wait) && !*shot_req {
                            *shot_req = true;
                            vctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                        }
                        let shot = vctx.input(|i| i.raw.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }));
                        if let Some(img) = shot {
                            let [w, h] = img.size;
                            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                                let _ = buf.save(&path);
                            }
                            std::process::exit(0);
                        }
                    }
                });
            }
            for a in actions {
                match a {
                    Action::Close => close.push(i),
                    Action::InstallFont { cjk, .. } => {
                        if cjk {
                            self.want_cjk = true;
                        }
                    }
                }
            }
        }
        // the shared style now belongs to the last reader; restore the library's next frame
        if !self.readers.is_empty() {
            self.style_key = None;
        }
        for i in close.into_iter().rev() {
            self.close_reader(i);
        }
    }

    /// Library title bar: wordmark, a live reading line, and playful window controls.
    fn title_bar(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::TopBottomPanel::top("titlebar").exact_height(46.0).frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 18, right: 12, top: 0, bottom: 0 })).show_separator_line(false).show(ctx, |ui| {
            let rect = ui.max_rect();
            let drag = ui.interact(rect, egui::Id::new("lib-drag"), Sense::click_and_drag());
            ui::titlebar_behaviour(ctx, &drag);
            ui.horizontal_centered(|ui| {
                // the Seshat mark + name
                let (m, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                crate::logo::paint(ui.painter(), m);
                ui.label(RichText::new(crate::APP_NAME).font(ui_b(18.0)));
                ui.add_space(14.0);
                let week: f64 = (0..7).map(|d| self.overview.daily.get(&store::date_string(store::now() - d * 86400)).copied().unwrap_or(0.0)).sum();
                let open = self.readers.len();
                let line = if open > 0 {
                    if open == 1 { trf!("{n} book open", n = open) } else { trf!("{n} books open", n = open) }
                } else if week >= 60.0 {
                    trf!("{n} min read this week", n = (week / 60.0).round())
                } else {
                    tr("Pick something good").into()
                };
                ui.label(RichText::new(line).font(ui_m(13.0)).color(pal.muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::window_controls(ui, pal.fg) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, pal.border));
        });
    }
}

// ---------- navigation column ----------

impl App {
    fn count(&self, v: View) -> usize {
        self.index.entries.iter().filter(|e| view_match(v, e)).count()
    }

    fn nav(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::SidePanel::left("nav").exact_width(220.0).resizable(false).frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 16, right: 16, top: 16, bottom: 16 })).show(ctx, |ui| {
            let mut marker: Option<Rect> = None;
            let row = |ui: &mut egui::Ui, me: &mut App, icon: Icon, label: &str, count: Option<usize>, v: Option<View>, marker: &mut Option<Rect>| -> bool {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                let active = v.is_some_and(|v| me.view == v);
                let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                if t > 0.01 && !active {
                    ui.painter().rect_filled(rect, CornerRadius::same(8), pal.hover.linear_multiply(t));
                }
                let fg = if active { ACCENT } else { pal.fg.linear_multiply(0.82 + 0.18 * t) };
                ui::paint_icon(ui.painter(), Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(16.0, 16.0)), icon, if active { ACCENT } else { pal.muted.lerp_to_gamma(pal.fg, t) });
                let max_w = rect.width() - 36.0 - if count.is_some() { 40.0 } else { 8.0 };
                let label = ui::fit(ui, label, ui_m(14.0), max_w);
                ui.painter().text(pos2(rect.left() + 36.0, rect.center().y), egui::Align2::LEFT_CENTER, label, ui_m(14.0), fg);
                if let Some(n) = count {
                    ui.painter().text(pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, n.to_string(), ui_m(12.0), if active { ACCENT } else { pal.muted });
                }
                if active {
                    *marker = Some(rect);
                }
                resp.clicked()
            };
            let heading = |ui: &mut egui::Ui, text: &str| {
                ui.add_space(14.0);
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
                let text = ui::fit(ui, text, ui_m(11.5), r.width() - 8.0);
                ui.painter().text(pos2(r.left() + 8.0, r.center().y), egui::Align2::LEFT_CENTER, text, ui_m(11.5), pal.muted);
                ui.add_space(2.0);
            };
            // the active pill is painted first (behind rows) at its animated position
            let pill_layer = ui.painter().add(egui::Shape::Noop);
            heading(ui, tr("Library"));
            for (v, l, ic) in [(View::All, tr("All books"), Icon::Library), (View::Reading, tr("Reading"), Icon::Book), (View::Unread, tr("Unread"), Icon::Bookmark), (View::Finished, tr("Finished"), Icon::BookmarkOn)] {
                let n = self.count(v);
                if row(ui, self, ic, l, Some(n), Some(v), &mut marker) {
                    self.set_view(v);
                    self.shelf_open = None;
                }
            }
            heading(ui, tr("Marginalia"));
            let (nn, nb) = (self.overview.notes.len(), self.overview.bookmarks.len());
            if row(ui, self, Icon::Note, tr("Notes"), Some(nn), Some(View::Notes), &mut marker) {
                self.set_view(View::Notes);
            }
            if row(ui, self, Icon::Bookmark, tr("Bookmarks"), Some(nb), Some(View::Bookmarks), &mut marker) {
                self.set_view(View::Bookmarks);
            }
            heading(ui, tr("Insights"));
            if row(ui, self, Icon::Info, tr("Statistics"), None, Some(View::Stats), &mut marker) {
                self.set_view(View::Stats);
            }
            if !self.dups.is_empty() {
                let n = self.dups.len();
                if row(ui, self, Icon::Copy, tr("Duplicates"), Some(n), Some(View::Duplicates), &mut marker) {
                    self.set_view(View::Duplicates);
                }
            }
            if !self.userlib.collections.is_empty() {
                heading(ui, tr("Collections"));
                let mut m2 = None;
                for c in self.userlib.collections.clone().into_iter().take(8) {
                    let n = self.index.entries.iter().filter(|e| e.collections.contains(&c)).count();
                    if row(ui, self, Icon::Folder, &c, Some(n), None, &mut m2) {
                        self.settings.lib_group = 6;
                        self.shelf_open = Some(c);
                        self.set_view(View::All);
                        self.view_changed = Instant::now();
                    }
                }
            }
            if self.settings.zlib_enabled {
                heading(ui, tr("Sources"));
                let r0 = ui.cursor().top();
                if row(ui, self, Icon::Globe, "Z-Library", None, Some(View::Source), &mut marker) {
                    self.set_view(View::Source);
                }
                let right = ui.max_rect().right();
                ui.painter().circle_filled(pos2(right - 14.0, r0 + 16.0), 3.5, if self.online.connected() { Color32::from_rgb(52, 190, 110) } else { pal.border });
            }
            if let Some(m) = marker {
                let y = ui.ctx().animate_value_with_time(egui::Id::new("nav-pill"), m.top(), 0.2);
                let pill = Rect::from_min_size(pos2(m.left(), y), m.size());
                ui.painter().set(pill_layer, egui::Shape::rect_filled(pill, CornerRadius::same(8), ACCENT.linear_multiply(0.11)));
            }
            ui.add_space(12.0);
            if let Some(p) = self.online.downloads_ui(ui, pal) {
                self.open_path(&p);
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                if self.scanning {
                    let (d, t) = (self.scan_done.load(Ordering::Relaxed), self.scan_total.load(Ordering::Relaxed));
                    ui.label(tag(if t > 0 { trf!("Indexing {done} of {total}", done = d, total = t) } else { tr("Indexing…").into() }).color(ACCENT));
                }
                let mut m2 = None;
                if row(ui, self, Icon::Gear, tr("Settings"), None, None, &mut m2) {
                    self.settings_open = !self.settings_open;
                }
                if row(ui, self, Icon::Folder, tr("Add folder"), None, None, &mut m2) {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        let s = d.to_string_lossy().into_owned();
                        if !self.settings.library_dirs.contains(&s) {
                            self.settings.library_dirs.push(s);
                        }
                        self.rescan();
                    }
                }
                if row(ui, self, Icon::Plus, tr("Import books"), None, None, &mut m2) {
                    self.import_dialog();
                }
            });
        });
    }

    // ---------- right column ----------

    fn right_column(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::SidePanel::right("right").exact_width(264.0).resizable(false).frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 24, right: 24, top: 24, bottom: 16 }).stroke(Stroke::new(1.0, pal.border))).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let current = self.index.entries.iter().filter(|e| e.fraction > 0.0 && !e.finished).max_by_key(|e| e.last_read).cloned();
                ui.label(tag(tr("Progress")).color(pal.muted));
                ui::rule(ui);
                if let Some(e) = &current {
                    let shown = ui.ctx().animate_value_with_time(egui::Id::new("pct"), e.fraction, 0.8);
                    if (shown - e.fraction).abs() > 0.001 {
                        ui.ctx().request_repaint();
                    }
                    ui.label(RichText::new(format!("{:.0}%", shown * 100.0)).font(ui_b(52.0)).color(ACCENT));
                    ui.add(egui::Label::new(RichText::new(crate::book::script::visual(&e.title)).font(ui_m(14.0))).truncate());
                    let r = ui.available_rect_before_wrap();
                    let bar = Rect::from_min_size(r.min + vec2(0.0, 6.0), vec2(r.width(), 2.0));
                    ui.painter().rect_filled(bar, CornerRadius::ZERO, pal.border);
                    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * shown, 2.0)), CornerRadius::ZERO, pal.fg);
                    ui.add_space(14.0);
                } else {
                    ui.label(RichText::new(tr("Nothing in progress.")).color(pal.muted));
                }

                ui.add_space(18.0);
                ui.label(tag(tr("This week")).color(pal.muted));
                ui::rule(ui);
                ui.add_space(6.0);
                let today = store::now();
                let days: Vec<(String, f64)> = (0..7)
                    .rev()
                    .map(|d| {
                        let k = store::date_string(today - d * 86400);
                        let v = self.overview.daily.get(&k).copied().unwrap_or(0.0);
                        (k, v)
                    })
                    .collect();
                let max = days.iter().map(|d| d.1).fold(60.0, f64::max);
                let grow = ease(self.view_changed.elapsed().as_secs_f32() / 0.6);
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 88.0), Sense::hover());
                let bw = r.width() / 7.0;
                for (i, (k, v)) in days.iter().enumerate() {
                    let h = ((v / max) as f32 * 62.0 * grow).max(1.0);
                    let x = r.left() + i as f32 * bw;
                    let bar = Rect::from_min_max(pos2(x + 2.0, r.top() + 64.0 - h), pos2(x + bw - 4.0, r.top() + 64.0));
                    ui.painter().rect_filled(bar, CornerRadius::ZERO, if i == 6 { ACCENT } else { pal.fg });
                    ui.painter().text(pos2(x + bw / 2.0 - 1.0, r.top() + 78.0), egui::Align2::CENTER_CENTER, k.get(8..10).unwrap_or(""), mono(9.5), pal.muted);
                }
                let week: f64 = days.iter().map(|d| d.1).sum();
                ui.label(tag(trf!("{min} min this week · {h} h total", min = (week / 60.0).round(), h = (self.overview.total_secs / 3600.0 * 10.0).round() / 10.0)).color(pal.muted));

                ui.add_space(18.0);
                ui.label(tag(tr("Recent notes")).color(pal.muted));
                ui::rule(ui);
                ui.add_space(6.0);
                if self.overview.notes.is_empty() {
                    ui.label(RichText::new(tr("Highlights you make while reading collect here.")).size(12.5).color(pal.muted));
                }
                let mut open = None;
                for (id, title, a) in self.overview.notes.iter().take(5) {
                    let top = ui.cursor().top();
                    let resp = ui.add(egui::Label::new(RichText::new(trunc(&a.text, 140)).family(egui::FontFamily::Name("book-i".into())).size(14.5)).sense(Sense::click()));
                    ui.label(tag(trunc(title, 30)).color(pal.muted));
                    let [cr, cg, cb] = a.color.rgb();
                    ui.painter().rect_filled(Rect::from_min_max(pos2(ui.max_rect().left() - 10.0, top + 2.0), pos2(ui.max_rect().left() - 8.0, ui.cursor().top() - 4.0)), CornerRadius::ZERO, Color32::from_rgb(cr, cg, cb));
                    if resp.clicked() {
                        open = Some((id.clone(), a.start));
                    }
                    ui.add_space(12.0);
                }
                if let Some((id, l)) = open {
                    self.open_at(&id, l);
                }
            });
        });
    }

    // ---------- library ----------

    fn visible_entries(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.index.entries.len())
            .filter(|&i| {
                let e = &self.index.entries[i];
                library::matches(e, &self.query) && (self.format.is_empty() || e.format == self.format) && view_match(self.view, e)
            })
            .collect();
        let es = &self.index.entries;
        let key = self.settings.sort;
        v.sort_by(|&a, &b| {
            let (x, y) = (&es[a], &es[b]);
            let o = match key {
                SortKey::Recent => x.last_read.cmp(&y.last_read).then(x.added.cmp(&y.added)),
                SortKey::Added => x.added.cmp(&y.added),
                SortKey::Title => y.title.to_lowercase().cmp(&x.title.to_lowercase()),
                SortKey::Author => y.author.to_lowercase().cmp(&x.author.to_lowercase()),
                SortKey::Progress => x.fraction.partial_cmp(&y.fraction).unwrap_or(std::cmp::Ordering::Equal),
                SortKey::Size => x.size.cmp(&y.size),
                SortKey::Rating => x.rating.cmp(&y.rating).then(x.last_read.cmp(&y.last_read)),
            };
            if self.settings.sort_desc {
                o.reverse()
            } else {
                o
            }
        });
        v
    }

    fn cover_tex(&mut self, e: &Entry) -> Option<egui::TextureId> {
        if !e.has_cover {
            return None;
        }
        let p = library::cover_path(&e.id);
        self.covers.request(&e.id, || Some(Src::File(p)), 600);
        self.covers.get(&e.id).map(|l| l.tex.id())
    }

    fn aspect(&self, e: &Entry) -> f32 {
        if e.cover_aspect > 0.2 {
            e.cover_aspect.clamp(0.9, 1.75)
        } else {
            1.5
        }
    }

    fn paint_cover(&mut self, painter: &Painter, rect: Rect, e: &Entry) {
        match self.cover_tex(e) {
            Some(id) => {
                painter.image(id, rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                let p = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
                ui::paint_placeholder_cover(&p, rect, &crate::book::script::visual(&e.title), &crate::book::script::visual(&e.author));
            }
        }
    }

    fn library_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let items = self.visible_entries();
        let title = match self.view {
            View::Reading => tr("Currently Reading"),
            View::Unread => tr("Unread"),
            View::Finished => tr("Finished"),
            _ => tr("The Library"),
        };
        self.header(ui, pal, title);

        if self.index.entries.is_empty() {
            self.empty_state(ui, pal);
            return;
        }

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(24.0);
            if self.query.is_empty() && self.view == View::All && self.settings.lib_group == 0 {
                self.featured(ui, pal);
            }
            if items.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new(if self.query.is_empty() { tr("Nothing here yet.") } else { tr("Not on your shelves.") }).font(ui_b(28.0)).color(pal.muted));
            } else if self.settings.lib_group == 0 {
                ui.horizontal(|ui| {
                    ui.label(tag(trf!("Index / {n}", n = format!("{:03}", items.len()))).color(pal.muted));
                    self.format_filter(ui);
                });
                ui::rule(ui);
                ui.add_space(16.0);
                self.shelf(ui, pal, &items, 0);
            } else {
                self.grouped(ui, pal, &items);
            }
            if self.online.quiet_strip(ui, pal, &self.settings) {
                self.set_view(View::Source);
            }
            ui.add_space(48.0);
        });
    }

    /// One calm row: title + count, search, and two dropdowns + view icons.
    fn header(&mut self, ui: &mut egui::Ui, pal: &Palette, title: &str) {
        ui.horizontal(|ui| {
            ui.label(display(title, 30.0));
            ui.label(RichText::new(self.count(self.view).to_string()).font(ui_m(14.0)).color(pal.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let grid = self.settings.lib_view == LibView::Grid;
                if ui::icon_toggle(ui, Icon::List, tr("List"), !grid).clicked() {
                    self.settings.lib_view = LibView::List;
                    self.view_changed = Instant::now();
                }
                if ui::icon_toggle(ui, Icon::Grid, tr("Covers"), grid).clicked() {
                    self.settings.lib_view = LibView::Grid;
                    self.view_changed = Instant::now();
                }
                ui.add_space(6.0);
                let shelves = [(0u8, tr("None")), (5, tr("Genre")), (1, tr("Author")), (2, tr("Status")), (3, tr("Format")), (4, tr("Folder")), (6, tr("Collection")), (7, tr("Series"))];
                let cur = shelves.iter().find(|s| s.0 == self.settings.lib_group).map(|s| s.1).unwrap_or(tr("None"));
                let mut pick_group = None;
                dropdown(ui, "shelf-dd", &trf!("Shelves · {name}", name = cur), self.settings.lib_group != 0, |ui| {
                    for (g, l) in shelves {
                        if ui.selectable_label(self.settings.lib_group == g, l).clicked() {
                            pick_group = Some(g);
                        }
                    }
                });
                if let Some(g) = pick_group {
                    self.settings.lib_group = g;
                    self.shelf_open = None;
                    self.view_changed = Instant::now();
                }
                let sorts = [(SortKey::Recent, tr("Recent")), (SortKey::Title, tr("Title")), (SortKey::Author, tr("Author")), (SortKey::Added, tr("Added")), (SortKey::Progress, tr("Progress")), (SortKey::Size, tr("Size")), (SortKey::Rating, tr("Rating"))];
                let cur = sorts.iter().find(|s| s.0 == self.settings.sort).map(|s| s.1).unwrap_or(tr("Recent"));
                let mut pick_sort = None;
                dropdown(ui, "sort-dd", &trf!("Sort · {name} {dir}", name = cur, dir = if self.settings.sort_desc { "↓" } else { "↑" }), false, |ui| {
                    for (k, l) in sorts {
                        if ui.selectable_label(self.settings.sort == k, l).clicked() {
                            pick_sort = Some(k);
                        }
                    }
                });
                if let Some(k) = pick_sort {
                    if self.settings.sort == k {
                        self.settings.sort_desc = !self.settings.sort_desc;
                    } else {
                        self.settings.sort = k;
                        self.settings.sort_desc = !matches!(k, SortKey::Title | SortKey::Author);
                    }
                }
                ui.add_space(8.0);
                // search pill
                let w = (ui.available_width() - 8.0).clamp(160.0, 380.0);
                let (sr, _) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
                let focused = ui.memory(|m| m.has_focus(egui::Id::new("lib-search")));
                let f = ui.ctx().animate_bool_with_time(egui::Id::new("search-focus"), focused || !self.query.is_empty(), 0.18);
                ui.painter().rect_filled(sr, CornerRadius::same(17), pal.hover);
                ui.painter().rect_stroke(sr, CornerRadius::same(17), Stroke::new(1.0 + f, ACCENT.linear_multiply(f)), egui::StrokeKind::Inside);
                ui::paint_icon(ui.painter(), Rect::from_center_size(pos2(sr.left() + 18.0, sr.center().y), vec2(15.0, 15.0)), Icon::Search, pal.muted.lerp_to_gamma(ACCENT, f));
                let inner = Rect::from_min_max(pos2(sr.left() + 34.0, sr.top() + 6.0), pos2(sr.right() - 14.0, sr.bottom() - 4.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                child.add(egui::TextEdit::singleline(&mut self.query).id(egui::Id::new("lib-search")).frame(false).hint_text(RichText::new("Search your shelves — and beyond").color(pal.muted)).desired_width(f32::INFINITY).font(ui_m(14.0)));
            });
        });
        ui.add_space(10.0);
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, pal.border));
    }

    fn format_filter(&mut self, ui: &mut egui::Ui) {
        let mut formats: Vec<String> = self.index.entries.iter().map(|e| e.format.clone()).collect();
        formats.sort();
        formats.dedup();
        if formats.len() > 1 {
            ui.add_space(10.0);
            if text_button(ui, tr("All"), self.format.is_empty()).clicked() {
                self.format.clear();
            }
            for f in formats {
                if text_button(ui, &f, self.format == f).clicked() {
                    self.format = if self.format == f { String::new() } else { f.clone() };
                }
            }
        }
    }

    fn shelf(&mut self, ui: &mut egui::Ui, pal: &Palette, items: &[usize], offset: usize) {
        match self.settings.lib_view {
            LibView::Grid => self.masonry(ui, pal, items, offset),
            LibView::List => self.index_list(ui, pal, items, offset),
        }
    }

    fn group_key(&self, e: &Entry) -> String {
        match self.settings.lib_group {
            1 => {
                let a = e.author.split([',', ';', '&']).next().unwrap_or("").trim().to_string();
                if a.is_empty() { tr("Unknown author").into() } else { a }
            }
            2 => (if e.finished { tr("Finished") } else if e.fraction > 0.0 { tr("Reading") } else { tr("Unread") }).into(),
            3 => e.format.clone(),
            5 => if e.genre.is_empty() { tr("Uncategorised").into() } else { e.genre.clone() },
            _ => Path::new(&e.path).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }

    fn group_keys(&self, e: &Entry) -> Vec<String> {
        match self.settings.lib_group {
            6 if e.collections.is_empty() => vec![tr("Uncollected").into()],
            6 => e.collections.clone(),
            7 => vec![if e.series.is_empty() { tr("Standalone").into() } else { e.series.clone() }],
            _ => vec![self.group_key(e)],
        }
    }

    /// Shelves as rows: each group is one horizontally sliding shelf, with a
    /// jump strip on top and "See all" to drill into a single shelf.
    fn grouped(&mut self, ui: &mut egui::Ui, pal: &Palette, items: &[usize]) {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for &i in items {
            for k in self.group_keys(&self.index.entries[i]) {
                match groups.iter_mut().find(|g| g.0 == k) {
                    Some(g) => g.1.push(i),
                    None => groups.push((k, vec![i])),
                }
            }
        }
        match self.settings.lib_group {
            1 => groups.sort_by(|a, b| a.0.split_whitespace().last().cmp(&b.0.split_whitespace().last())),
            2 => groups.sort_by_key(|g| if g.0 == tr("Reading") { 0 } else if g.0 == tr("Unread") { 1 } else { 2 }),
            6 | 7 => groups.sort_by(|a, b| {
                let last = |k: &str| k == tr("Uncollected") || k == tr("Standalone");
                last(&a.0).cmp(&last(&b.0)).then(a.0.to_lowercase().cmp(&b.0.to_lowercase()))
            }),
            _ => groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0))),
        }
        if self.settings.lib_group == 7 {
            let es = &self.index.entries;
            for g in groups.iter_mut().filter(|g| g.0 != tr("Standalone")) {
                g.1.sort_by(|&a, &b| es[a].series_index.partial_cmp(&es[b].series_index).unwrap_or(std::cmp::Ordering::Equal).then(es[a].title.cmp(&es[b].title)));
            }
        }
        // drilled into one shelf
        if let Some(open) = self.shelf_open.clone() {
            if let Some((k, idx)) = groups.iter().find(|g| g.0 == open) {
                ui.horizontal(|ui| {
                    if text_button(ui, tr("← All shelves"), false).clicked() {
                        self.shelf_open = None;
                        self.view_changed = Instant::now();
                    }
                    ui.label(display(k, 24.0));
                    ui.label(RichText::new(idx.len().to_string()).font(ui_m(14.0)).color(pal.muted));
                });
                ui.add_space(16.0);
                let idx = idx.clone();
                self.shelf(ui, pal, &idx, 0);
                return;
            }
            self.shelf_open = None;
        }
        // jump strip — text flush with the shelf titles below
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(22.0, 8.0);
            for (k, idx) in &groups {
                let g = ui.painter().layout_no_wrap(k.clone(), ui_m(13.5), pal.fg);
                let n = ui.painter().layout_no_wrap(idx.len().to_string(), ui_m(12.0), pal.muted);
                let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 6.0 + n.size().x, 24.0), Sense::click());
                let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                let gw = g.size().x;
                ui.painter().galley(pos2(r.left(), r.center().y - g.size().y / 2.0), g, pal.fg);
                ui.painter().galley(pos2(r.left() + gw + 6.0, r.center().y - n.size().y / 2.0 + 1.0), n, pal.muted);
                if t > 0.01 {
                    ui.painter().hline(r.left()..=r.left() + gw * t, r.bottom() - 2.0, Stroke::new(1.5, ACCENT));
                }
                if resp.clicked() {
                    self.shelf_jump = Some(k.clone());
                }
            }
        });
        ui.add_space(10.0);
        let jump = self.shelf_jump.take();
        for (k, idx) in &groups {
            ui.add_space(24.0);
            let hdr = ui.horizontal(|ui| {
                ui.label(RichText::new(k).font(ui_b(22.0)));
                ui.label(RichText::new(idx.len().to_string()).font(ui_m(13.0)).color(pal.muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if text_button(ui, tr("See all →"), false).clicked() {
                        self.shelf_open = Some(k.clone());
                        self.view_changed = Instant::now();
                    }
                });
            });
            if jump.as_deref() == Some(k.as_str()) {
                hdr.response.scroll_to_me(Some(egui::Align::TOP));
            }
            ui.add_space(8.0);
            self.shelf_row(ui, pal, k, idx);
        }
    }

    /// One shelf: a clipped strip of covers that slides with ‹ › (animated).
    fn shelf_row(&mut self, ui: &mut egui::Ui, pal: &Palette, key: &str, idx: &[usize]) {
        let h = (self.settings.cover_size * 1.15).clamp(140.0, 240.0);
        let gap = 20.0;
        let w = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(w, h + 52.0), Sense::hover());
        let widths: Vec<f32> = idx.iter().map(|&i| h / self.aspect(&self.index.entries[i])).collect();
        let total: f32 = widths.iter().sum::<f32>() + gap * idx.len().saturating_sub(1) as f32;
        let max_off = (total - w).max(0.0);
        let id = egui::Id::new(("shelf", key));
        let mut target = ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0);
        // wheel over the row scrolls it sideways
        if ui.rect_contains_pointer(row) {
            let d = ui.input(|i| i.raw_scroll_delta);
            let dx = if d.x != 0.0 { d.x } else if ui.input(|i| i.modifiers.shift) { d.y } else { 0.0 };
            target = (target - dx).clamp(0.0, max_off);
        }
        let step = w * 0.8;
        let off = ui.ctx().animate_value_with_time(id.with("anim"), target, 0.3);
        let painter = ui.painter().with_clip_rect(row.intersect(ui.clip_rect()));
        let mut x = row.left() - off;
        let mut open = None;
        for (n, &i) in idx.iter().enumerate() {
            let cw = widths[n];
            if x > row.right() {
                break;
            }
            if x + cw >= row.left() {
                let e = self.index.entries[i].clone();
                let r = Rect::from_min_size(pos2(x, row.top()), vec2(cw, h + 52.0));
                let resp = ui.interact(r.intersect(row), id.with(n), Sense::click());
                let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.15);
                let cover = Rect::from_min_size(r.min + vec2(0.0, 6.0 - 6.0 * t), vec2(cw, h));
                self.paint_cover(&painter, cover, &e);
                if e.fraction > 0.0 {
                    painter.rect_filled(Rect::from_min_size(pos2(cover.left(), cover.bottom() - 3.0), vec2(cw * e.fraction, 3.0)), CornerRadius::ZERO, if e.finished { pal.fg } else { ACCENT });
                }
                let mut job = egui::text::LayoutJob::simple(crate::book::script::visual(&e.title).into_owned(), ui_m(13.0), pal.fg, cw);
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                painter.galley(pos2(x, row.top() + h + 14.0), painter.layout_job(job), pal.fg);
                let mut job = egui::text::LayoutJob::simple(crate::book::script::visual(&e.author).into_owned(), egui::FontId::proportional(12.0), pal.muted, cw);
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                painter.galley(pos2(x, row.top() + h + 32.0), painter.layout_job(job), pal.muted);
                if resp.clicked() {
                    open = Some(e.path.clone());
                }
                resp.context_menu(|ui| self.book_menu(ui, &e));
            }
            x += cw + gap;
        }
        // arrows appear only when the shelf overflows
        for (left, show) in [(true, target > 1.0), (false, target < max_off - 1.0)] {
            if !show {
                continue;
            }
            let c = if left { pos2(row.left() + 20.0, row.top() + h / 2.0) } else { pos2(row.right() - 20.0, row.top() + h / 2.0) };
            let r = Rect::from_center_size(c, vec2(36.0, 36.0));
            let resp = ui.interact(r, id.with(("arrow", left)), Sense::click());
            let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
            ui.painter().circle_filled(c, 17.0 + 2.0 * t, pal.bg);
            ui.painter().circle_stroke(c, 17.0 + 2.0 * t, Stroke::new(1.0, pal.border));
            ui::paint_icon(ui.painter(), r.shrink(10.0), if left { Icon::Left } else { Icon::Right }, if t > 0.5 { ACCENT } else { pal.fg });
            if resp.clicked() {
                target = if left { (target - step).max(0.0) } else { (target + step).min(max_off) };
            }
        }
        ui.ctx().data_mut(|d| d.insert_temp(id, target));
        if (off - target).abs() > 0.5 {
            ui.ctx().request_repaint();
        }
        if let Some(p) = open {
            self.open_path(Path::new(&p));
        }
    }

    fn featured(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let mut reading: Vec<Entry> = self.index.entries.iter().filter(|e| e.last_read > 0).cloned().collect();
        reading.sort_by(|a, b| b.last_read.cmp(&a.last_read));
        let Some(cur) = reading.iter().find(|e| !e.finished && e.fraction > 0.0).cloned().or_else(|| reading.first().cloned()) else { return };
        let n_reading = self.count(View::Reading);
        ui.label(tag(trf!("Currently reading / {n}", n = format!("{:03}", n_reading.max(1)))).color(pal.muted));
        ui.add_space(10.0);
        let w = ui.available_width();
        let ch = 310.0f32.min(w * 0.45);
        let cw = ch / self.aspect(&cur);
        let (row, resp) = ui.allocate_exact_size(vec2(w, ch), Sense::click());
        let hov = ui.ctx().animate_bool_with_time(ui.id().with("feat"), resp.hovered(), 0.2);
        // offset block behind the cover — a printed-matter shadow, not a blur
        let cover = Rect::from_min_size(row.min, vec2(cw, ch));
        ui.painter().rect_filled(cover.translate(vec2(10.0 * hov, 10.0 * hov)), CornerRadius::ZERO, ACCENT);
        let painter = ui.painter().clone();
        self.paint_cover(&painter, cover, &cur);
        let x = cover.right() + 40.0;
        let tw = (row.right() - x).max(120.0);
        let mut job = egui::text::LayoutJob::single_section(crate::book::script::visual(&cur.title).into_owned(), egui::TextFormat { font_id: ui_b(40.0), color: pal.fg, line_height: Some(42.0), extra_letter_spacing: -0.6, ..Default::default() });
        job.wrap = egui::text::TextWrapping { max_width: tw, max_rows: 3, overflow_character: Some('…'), ..Default::default() };
        let g = ui.painter().layout_job(job);
        let th = g.size().y;
        ui.painter().galley(pos2(x, row.top() - 8.0), g, pal.fg);
        ui.painter().text(pos2(x, row.top() + th + 4.0), egui::Align2::LEFT_TOP, &cur.author, ui_m(20.0), pal.muted);
        let by = row.bottom() - 80.0;
        ui.painter().text(pos2(x, by), egui::Align2::LEFT_TOP, trf!("{pct}% COMPLETE  ·  LAST OPENED {when}", pct = format!("{:.0}", cur.fraction * 100.0), when = store::relative_time(cur.last_read).to_uppercase()), mono(11.0), pal.fg);
        let fill = ui.ctx().animate_value_with_time(egui::Id::new(("feat-fill", &cur.id)), cur.fraction.clamp(0.0, 1.0), 0.9);
        let bar = Rect::from_min_size(pos2(x, by + 22.0), vec2(tw, 2.0));
        ui.painter().rect_filled(bar, CornerRadius::ZERO, pal.border);
        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(tw * fill, 2.0)), CornerRadius::ZERO, ACCENT);
        let btn = Rect::from_min_size(pos2(x, row.bottom() - 36.0), vec2(160.0 + 20.0 * hov, 36.0));
        let bresp = ui.interact(btn, ui.id().with("resume"), Sense::click());
        ui.painter().rect_filled(btn, CornerRadius::ZERO, if bresp.hovered() || resp.hovered() { ACCENT } else { pal.fg });
        ui.painter().text(btn.left_center() + vec2(16.0, 0.0), egui::Align2::LEFT_CENTER, if cur.fraction > 0.0 { tr("RESUME") } else { tr("OPEN") }, mono_m(12.0), pal.bg);
        ui.painter().text(btn.right_center() - vec2(16.0, 0.0), egui::Align2::RIGHT_CENTER, "→", mono_m(14.0), pal.bg);
        if resp.clicked() || bresp.clicked() {
            self.open_path(Path::new(&cur.path));
        }
        let recent: Vec<Entry> = reading.into_iter().filter(|e| e.id != cur.id).take(10).collect();
        if !recent.is_empty() {
            ui.add_space(32.0);
            ui.label(tag(trf!("Recently opened / {n}", n = format!("{:03}", recent.len()))).color(pal.muted));
            ui::rule(ui);
            ui.add_space(14.0);
            let h = 136.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 24.0;
                for e in &recent {
                    let cw = h / self.aspect(e);
                    if ui.available_width() < cw + 10.0 {
                        break;
                    }
                    let (r, resp) = ui.allocate_exact_size(vec2(cw, h + 28.0), Sense::click());
                    let t = ui.ctx().animate_bool_with_time(ui.id().with(("rec", &e.id)), resp.hovered(), 0.16);
                    let c = Rect::from_min_size(r.min - vec2(0.0, 6.0 * t), vec2(cw, h));
                    let painter = ui.painter().clone();
                    self.paint_cover(&painter, c, e);
                    let fr = c.width() * e.fraction;
                    ui.painter().rect_filled(Rect::from_min_size(pos2(r.left(), r.top() + h + 8.0), vec2(c.width(), 1.0)), CornerRadius::ZERO, pal.border);
                    ui.painter().rect_filled(Rect::from_min_size(pos2(r.left(), r.top() + h + 7.5), vec2(fr, 2.0)), CornerRadius::ZERO, if e.finished { pal.fg } else { ACCENT });
                    ui.painter().text(pos2(r.left(), r.top() + h + 14.0), egui::Align2::LEFT_TOP, format!("{:.0}%", e.fraction * 100.0), mono(10.0), pal.muted);
                    let resp = resp.on_hover_text(format!("{}\n{}", e.title, e.author));
                    if resp.clicked() {
                        self.open_path(Path::new(&e.path));
                    }
                }
            });
        }
        ui.add_space(32.0);
    }

    /// Asymmetric masonry: real cover proportions, every seventh book set large,
    /// staggered reveal, eased hover lift with a sliding info strip.
    fn masonry(&mut self, ui: &mut egui::Ui, pal: &Palette, items: &[usize], offset: usize) {
        let avail = ui.available_width() - 4.0;
        let gap = 30.0;
        let base = self.settings.cover_size.clamp(110.0, 260.0);
        let cols = ((avail + gap) / (base + gap)).floor().max(1.0) as usize;
        let cw = (avail - gap * (cols as f32 - 1.0)) / cols as f32;
        let text_h = 76.0;
        let mut heights = vec![0.0f32; cols];
        let mut placed: Vec<(usize, Rect, f32)> = Vec::with_capacity(items.len());
        for (n, &ei) in items.iter().enumerate() {
            let e = &self.index.entries[ei];
            let span = if cols >= 4 && (n + offset) % 7 == 0 { 2 } else { 1 };
            let w = cw * span as f32 + gap * (span as f32 - 1.0);
            let ch = w * self.aspect(e);
            let (c, y) = if span == 1 {
                let c = (0..cols).min_by(|&a, &b| heights[a].partial_cmp(&heights[b]).unwrap()).unwrap();
                (c, heights[c])
            } else {
                let c = (0..cols - 1).min_by(|&a, &b| heights[a].max(heights[a + 1]).partial_cmp(&heights[b].max(heights[b + 1])).unwrap()).unwrap();
                (c, heights[c].max(heights[c + 1]))
            };
            // small vertical offsets on alternate columns break the grid's rigidity
            let jog = if y == 0.0 && c % 2 == 1 { 28.0 } else { 0.0 };
            let r = Rect::from_min_size(pos2(c as f32 * (cw + gap), y + jog), vec2(w, ch + text_h));
            for k in c..c + span {
                heights[k] = y + jog + ch + text_h + gap;
            }
            placed.push((ei, r, ch));
        }
        let total = heights.iter().cloned().fold(0.0, f32::max);
        let (area, _) = ui.allocate_exact_size(vec2(avail, total), Sense::hover());
        let clip = ui.clip_rect();
        let since = self.view_changed.elapsed().as_secs_f32();
        let mut open = None;
        for (n, (ei, r, ch)) in placed.into_iter().enumerate() {
            let r = r.translate(area.min.to_vec2());
            if !r.intersects(clip) {
                continue;
            }
            let e = self.index.entries[ei].clone();
            // staggered reveal (first ~0.6 s after a view change)
            let reveal = ease((since - (n.min(24) as f32) * 0.025) / 0.3);
            if reveal < 1.0 {
                ui.ctx().request_repaint();
            }
            let resp = ui.interact(r, ui.id().with(("b", &e.id)), Sense::click());
            let t = ui.ctx().animate_bool_with_time(ui.id().with(("h", &e.id)), resp.hovered(), 0.18);
            let mut painter = ui.painter().clone();
            painter.multiply_opacity(reveal);
            let rise = (1.0 - reveal) * 16.0 - 6.0 * t;
            let cover = Rect::from_min_size(r.min + vec2(0.0, rise), vec2(r.width(), ch));
            if t > 0.01 {
                painter.rect_filled(cover.translate(vec2(7.0 * t, 7.0 * t)), CornerRadius::ZERO, pal.fg);
            }
            self.paint_cover(&painter, cover, &e);
            if t > 0.01 {
                let sh = 28.0 * t;
                let strip = Rect::from_min_max(pos2(cover.left(), cover.bottom() - sh), cover.max);
                painter.rect_filled(strip, CornerRadius::ZERO, INK);
                let info = format!("{}  ·  {}  ·  {}", if e.finished { tr("DONE").into() } else { format!("{:.0}%", e.fraction * 100.0) }, e.format, library::fmt_size(e.size));
                painter.with_clip_rect(strip).text(strip.left_center() + vec2(9.0, 0.0), egui::Align2::LEFT_CENTER, info, mono(10.0), PAPER);
            } else if e.fraction > 0.0 && !e.finished {
                painter.rect_filled(Rect::from_min_size(pos2(cover.left(), cover.bottom() - 3.0), vec2(cover.width() * e.fraction, 3.0)), CornerRadius::ZERO, ACCENT);
            }
            let y = cover.bottom() + 10.0 + 6.0 * t;
            let (st, sc) = if e.finished { (tr("FINISHED"), pal.muted) } else if e.fraction > 0.0 { (tr("READING"), ACCENT) } else { (tr("UNREAD"), pal.muted) };
            painter.text(pos2(r.left(), y), egui::Align2::LEFT_TOP, format!("{:02} —", n + offset + 1), mono(10.0), if t > 0.5 { ACCENT } else { pal.muted });
            painter.text(pos2(r.right(), y), egui::Align2::RIGHT_TOP, st, mono(9.5), sc);
            if e.rating > 0 {
                widgets::paint_star(&painter, pos2(r.left() + 40.0, y + 6.0), 4.5, true, ACCENT);
                painter.text(pos2(r.left() + 48.0, y), egui::Align2::LEFT_TOP, e.rating.to_string(), mono(9.5), pal.muted);
            }
            let big = r.width() > cw + 1.0;
            let mut job = egui::text::LayoutJob::single_section(crate::book::script::visual(&e.title).into_owned(), egui::TextFormat { font_id: if big { ui_b(21.0) } else { ui_m(14.0) }, color: pal.fg, line_height: Some(if big { 22.0 } else { 17.0 }), ..Default::default() });
            job.wrap = egui::text::TextWrapping { max_width: r.width(), max_rows: 2, overflow_character: Some('…'), ..Default::default() };
            let g = painter.layout_job(job);
            let th = g.size().y;
            painter.galley(pos2(r.left(), y + 16.0), g, pal.fg);
            let mut job = egui::text::LayoutJob::simple(crate::book::script::visual(&e.author.to_uppercase()).into_owned(), mono(9.5), pal.muted, r.width());
            job.wrap.max_rows = 1;
            job.wrap.overflow_character = Some('…');
            painter.galley(pos2(r.left(), y + 20.0 + th), painter.layout_job(job), pal.muted);
            if resp.clicked() {
                open = Some(e.path.clone());
            }
            resp.context_menu(|ui| self.book_menu(ui, &e));
        }
        if let Some(p) = open {
            self.open_path(Path::new(&p));
        }
    }

    fn index_list(&mut self, ui: &mut egui::Ui, pal: &Palette, items: &[usize], offset: usize) {
        let w = ui.available_width();
        let cols = [0.0, 54.0, w * 0.5, w * 0.74, w * 0.84, w * 0.92];
        let (hr, _) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
        for (x, h) in cols.iter().zip([tr("NO."), tr("TITLE"), tr("AUTHOR"), tr("FORMAT"), tr("READ"), tr("OPENED")]) {
            ui.painter().text(pos2(hr.left() + x, hr.center().y), egui::Align2::LEFT_CENTER, h, mono(9.5), pal.muted);
        }
        let mut open = None;
        for (n, &ei) in items.iter().enumerate() {
            let e = self.index.entries[ei].clone();
            let (r, resp) = ui.allocate_exact_size(vec2(w, 42.0), Sense::click());
            let t = ui.ctx().animate_bool_with_time(ui.id().with(("row", &e.id)), resp.hovered(), 0.15);
            ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, pal.border));
            // hover: an accent bar wipes in from the left
            if t > 0.01 {
                ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * t, r.height())), CornerRadius::ZERO, pal.hover);
                ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(3.0, r.height())), CornerRadius::ZERO, ACCENT);
            }
            let y = r.center().y;
            let ink = if t > 0.5 { ACCENT } else { pal.fg };
            ui.painter().text(pos2(r.left() + cols[0] + 8.0 * t, y), egui::Align2::LEFT_CENTER, format!("{:03}", n + offset + 1), mono(11.0), pal.muted);
            let clip = |x0: f32, x1: f32| ui.painter().with_clip_rect(Rect::from_min_max(pos2(r.left() + x0, r.top()), pos2(r.left() + x1 - 12.0, r.bottom())));
            clip(cols[1], cols[2]).text(pos2(r.left() + cols[1] + 8.0 * t, y), egui::Align2::LEFT_CENTER, crate::book::script::visual(&e.title), ui_m(15.0), ink);
            clip(cols[2], cols[3]).text(pos2(r.left() + cols[2], y), egui::Align2::LEFT_CENTER, crate::book::script::visual(&e.author), egui::FontId::proportional(13.0), pal.muted);
            ui.painter().text(pos2(r.left() + cols[3], y), egui::Align2::LEFT_CENTER, &e.format, mono(11.0), pal.muted);
            if e.rating > 0 {
                widgets::paint_star(ui.painter(), pos2(r.left() + cols[3] + 52.0, y), 4.5, true, ACCENT);
                ui.painter().text(pos2(r.left() + cols[3] + 60.0, y), egui::Align2::LEFT_CENTER, e.rating.to_string(), mono(10.5), pal.muted);
            }
            let pr = if e.finished { tr("DONE").to_string() } else { format!("{:.0}%", e.fraction * 100.0) };
            ui.painter().text(pos2(r.left() + cols[4], y), egui::Align2::LEFT_CENTER, pr, mono(11.0), if e.fraction > 0.0 && !e.finished { ACCENT } else { pal.muted });
            ui.painter().text(pos2(r.left() + cols[5], y), egui::Align2::LEFT_CENTER, if e.last_read > 0 { store::relative_time(e.last_read) } else { "—".into() }, mono(10.5), pal.muted);
            if resp.clicked() {
                open = Some(e.path.clone());
            }
            resp.context_menu(|ui| self.book_menu(ui, &e));
        }
        if let Some(p) = open {
            self.open_path(Path::new(&p));
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.add_space(60.0);
        ui.label(display(tr("Empty shelves."), 48.0));
        ui.add_space(6.0);
        ui.label(RichText::new(tr("EPUB · MOBI · AZW3 · FB2 · PDF · CBZ · TXT")).font(mono(12.0)).color(pal.muted));
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            if ui::primary_button(ui, tr("Import books")).clicked() {
                self.import_dialog();
            }
            if text_button(ui, tr("Add a folder"), false).clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.settings.library_dirs.push(d.to_string_lossy().into_owned());
                    self.rescan();
                }
            }
            if self.settings.zlib_enabled && text_button(ui, tr("Find books on Z-Library →"), false).clicked() {
                self.set_view(View::Source);
            }
        });
        ui.add_space(14.0);
        ui.label(tag(tr("Or drop files and folders anywhere on this window.")).color(pal.muted));
    }

    fn book_menu(&mut self, ui: &mut egui::Ui, e: &Entry) {
        if ui.button(tr("Read")).clicked() {
            self.open_path(Path::new(&e.path));
            ui.close_menu();
        }
        if ui.button(tr("Details")).clicked() {
            self.details = Some(e.id.clone());
            ui.close_menu();
        }
        if ui.button(if e.finished { tr("Mark as unread") } else { tr("Mark as finished") }).clicked() {
            let fin = !e.finished;
            // An open reader holds its own BookData and saves it back later:
            // change that copy, or the reader would undo this on close.
            let fraction = if let Some(r) = self.readers.iter_mut().find(|r| r.id == e.id) {
                r.data.finished = fin;
                r.save();
                r.data.fraction
            } else {
                let mut d = BookData::load(&e.id);
                d.finished = fin;
                if !fin {
                    d.fraction = 0.0;
                    d.loc = None;
                }
                d.save(&e.id);
                d.fraction
            };
            if let Some(x) = self.index.entries.iter_mut().find(|x| x.id == e.id) {
                x.finished = fin;
                x.fraction = fraction;
            }
            self.index.save();
            ui.close_menu();
        }
        let cur = self.userlib.book(&e.id).cloned().unwrap_or_default();
        ui.menu_button(tr("Rating"), |ui| {
            for r in 0..=5u8 {
                let l = if r == 0 { tr("None").to_string() } else { format!("{r} star{}", if r == 1 { "" } else { "s" }) };
                if ui.selectable_label(cur.rating == r, l).clicked() {
                    self.set_rating(&e.id, r);
                    ui.close_menu();
                }
            }
        });
        ui.menu_button(tr("Collections"), |ui| {
            for c in self.userlib.collections.clone() {
                let mut on = cur.collections.contains(&c);
                if ui.checkbox(&mut on, &c).changed() {
                    self.toggle_collection(&e.id, &c);
                }
            }
            if !self.userlib.collections.is_empty() {
                ui.separator();
            }
            let r = ui.add(egui::TextEdit::singleline(&mut self.new_coll).hint_text(tr("New collection…")).desired_width(150.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let n = std::mem::take(&mut self.new_coll);
                self.new_collection_for(&e.id, &n);
                ui.close_menu();
            }
        });
        if self.settings.zlib_enabled && ui.button(tr("Fetch cover from Z-Library")).clicked() {
            self.online.fetch_cover_for(e.id.clone(), PathBuf::from(&e.path), e.title.clone(), e.author.clone(), &self.settings);
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("Show in folder")).clicked() {
            crate::platform::reveal(Path::new(&e.path));
            ui.close_menu();
        }
        if ui.button(tr("Open with system app")).clicked() {
            crate::platform::open_path(Path::new(&e.path));
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("Remove from library")).clicked() {
            self.settings.hidden_paths.push(e.path.clone());
            self.settings.extra_files.retain(|f| f != &e.path);
            self.index.entries.retain(|x| x.id != e.id);
            self.index.save();
            ui.close_menu();
        }
        if ui.button(RichText::new(tr("Delete file…")).color(ACCENT)).clicked() {
            self.confirm_delete = Some(e.id.clone());
            ui.close_menu();
        }
    }

    // ---------- marginalia ----------

    fn notes_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let books = {
            let mut b: Vec<&String> = self.overview.notes.iter().map(|n| &n.0).collect();
            b.sort();
            b.dedup();
            b.len()
        };
        ui.label(display(tr("Notes"), 34.0));
        ui.label(tag(trf!("{n} highlights across {books} books", n = format!("{:03}", self.overview.notes.len()), books = format!("{:03}", books))).color(pal.muted));
        ui.add_space(8.0);
        ui::rule(ui);
        if self.overview.notes.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new(tr("Select text while reading to highlight it or write in the margin.")).font(ui_m(17.0)).color(pal.muted));
            return;
        }
        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let mut groups: Vec<(String, String, Vec<Annotation>)> = Vec::new();
            for (id, title, a) in &self.overview.notes {
                match groups.iter_mut().find(|g| &g.0 == id) {
                    Some(g) => g.2.push(a.clone()),
                    None => groups.push((id.clone(), title.clone(), vec![a.clone()])),
                }
            }
            for (gi, (id, title, notes)) in groups.iter().enumerate() {
                ui.add_space(26.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:02}", gi + 1)).font(ui_b(28.0)).color(ACCENT));
                    ui.label(RichText::new(title).font(ui_b(26.0)));
                    ui.label(tag(format!("{:03}", notes.len())).color(pal.muted));
                });
                ui::rule(ui);
                ui.add_space(8.0);
                for a in notes {
                    let resp = ui.horizontal_top(|ui| {
                        ui.add_space(42.0);
                        let [cr, cg, cb] = a.color.rgb();
                        let top = ui.cursor().top();
                        let inner = ui.vertical(|ui| {
                            ui.set_max_width(700.0);
                            let resp = ui.add(egui::Label::new(RichText::new(format!("“{}”", a.text)).family(egui::FontFamily::Name("book".into())).size(18.0)).sense(Sense::click()));
                            if !a.note.is_empty() {
                                ui.label(RichText::new(&a.note).font(ui_m(14.0)).color(ACCENT));
                            }
                            ui.label(tag(format!("{} · {}", a.chapter, store::relative_time(a.created))).color(pal.muted));
                            resp
                        });
                        let x = inner.response.rect.left() - 14.0;
                        ui.painter().rect_filled(Rect::from_min_max(pos2(x, top + 3.0), pos2(x + 4.0, inner.response.rect.bottom() - 2.0)), CornerRadius::ZERO, Color32::from_rgb(cr, cg, cb));
                        inner.inner
                    });
                    if resp.inner.clicked() {
                        open = Some((id.clone(), a.start));
                    }
                    ui.add_space(16.0);
                }
            }
            ui.add_space(40.0);
        });
        if let Some((id, l)) = open {
            self.open_at(&id, l);
        }
    }

    fn bookmarks_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.label(display(tr("Bookmarks"), 34.0));
        ui.label(tag(trf!("{n} marked pages", n = format!("{:03}", self.overview.bookmarks.len()))).color(pal.muted));
        ui.add_space(8.0);
        ui::rule(ui);
        if self.overview.bookmarks.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new(tr("Press B while reading to mark a page.")).font(ui_m(17.0)).color(pal.muted));
            return;
        }
        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let w = ui.available_width();
            for (n, (id, title, b)) in self.overview.bookmarks.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(w, 66.0), Sense::click());
                let t = ui.ctx().animate_bool_with_time(ui.id().with(("bm", n)), resp.hovered(), 0.15);
                ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0, pal.border));
                if t > 0.01 {
                    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * t, r.height())), CornerRadius::ZERO, pal.hover);
                }
                ui.painter().text(pos2(r.left(), r.top() + 12.0), egui::Align2::LEFT_TOP, format!("{:03}", n + 1), ui_b(22.0), ACCENT);
                ui.painter().text(pos2(r.left() + 70.0 + 6.0 * t, r.top() + 10.0), egui::Align2::LEFT_TOP, trunc(title, 60), ui_m(16.0), pal.fg);
                ui.painter().text(pos2(r.left() + 70.0 + 6.0 * t, r.top() + 36.0), egui::Align2::LEFT_TOP, trunc(&format!("{} — {}", b.chapter, b.excerpt), 110), egui::FontId::proportional(12.5), pal.muted);
                ui.painter().text(pos2(r.right() - 6.0, r.top() + 14.0), egui::Align2::RIGHT_TOP, store::relative_time(b.created).to_uppercase(), mono(10.0), pal.muted);
                if resp.clicked() {
                    open = Some((id.clone(), b.loc));
                }
            }
        });
        if let Some((id, l)) = open {
            self.open_at(&id, l);
        }
    }

    // ---------- windows ----------

    fn details_window(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(id) = self.details.clone() else {
            self.details_data = None;
            return;
        };
        let Some(e) = self.index.entries.iter().find(|e| e.id == id).cloned() else {
            self.details = None;
            return;
        };
        if self.details_data.as_ref().is_none_or(|(d, _)| d != &id) {
            self.details_data = Some((id.clone(), BookData::load(&id)));
        }
        let (secs, days, highlights) = self.details_data.as_ref().map(|(_, d)| (d.seconds, d.daily.len(), d.annotations.len())).unwrap_or_default();
        egui::Window::new("details").title_bar(false).collapsible(false).resizable(false).default_width(640.0).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                let h = 280.0;
                let (r, _) = ui.allocate_exact_size(vec2(h / self.aspect(&e), h), Sense::hover());
                let painter = ui.painter().clone();
                self.paint_cover(&painter, r, &e);
                ui.add_space(22.0);
                ui.vertical(|ui| {
                    ui.set_width(370.0);
                    ui.horizontal(|ui| {
                        ui.label(tag(format!("{} · {}", e.format, library::fmt_size(e.size))).color(pal.muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if text_button(ui, tr("Close"), false).clicked() {
                                self.details = None;
                            }
                        });
                    });
                    ui.label(display(crate::book::script::visual(&e.title).as_ref(), 32.0));
                    ui.label(RichText::new(crate::book::script::visual(&e.author)).font(ui_m(16.0)).color(pal.muted));
                    ui.add_space(12.0);
                    egui::Grid::new("meta").num_columns(2).spacing(vec2(16.0, 6.0)).show(ui, |ui| {
                        let mut row = |k: &str, v: String| {
                            if !v.is_empty() {
                                ui.label(tag(k).color(pal.muted));
                                ui.label(v);
                                ui.end_row();
                            }
                        };
                        row(tr("Series"), e.series.clone());
                        row(tr("Publisher"), e.publisher.clone());
                        row(tr("Language"), e.language.clone());
                        row(tr("Progress"), if e.finished { tr("Finished").into() } else { format!("{:.0}%", e.fraction * 100.0) });
                        let mins = (secs / 60.0).round() as i64;
                        row(tr("Time read"), if mins >= 60 { format!("{} h {} min", mins / 60, mins % 60) } else { format!("{mins} min") });
                        row(tr("Days read"), if days == 0 { String::new() } else { days.to_string() });
                        row(tr("Last opened"), if e.last_read > 0 { store::relative_time(e.last_read) } else { "never".into() });
                        row(tr("Highlights"), if highlights == 0 { String::new() } else { highlights.to_string() });
                    });
                    ui.add_space(8.0);
                    let ub = self.userlib.book(&e.id).cloned().unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label(tag(tr("Rating")).color(pal.muted));
                        if let Some(r) = widgets::star_row(ui, e.rating, 16.0, ACCENT, pal.muted) {
                            self.set_rating(&e.id, r);
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tag(tr("Collections")).color(pal.muted));
                        for c in self.userlib.collections.clone() {
                            if text_button(ui, &c, ub.collections.contains(&c)).clicked() {
                                self.toggle_collection(&e.id, &c);
                            }
                        }
                        let r = ui.add(egui::TextEdit::singleline(&mut self.new_coll).hint_text("+ New").desired_width(80.0));
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let n = std::mem::take(&mut self.new_coll);
                            self.new_collection_for(&e.id, &n);
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui::primary_button(ui, if e.fraction > 0.0 { tr("Continue") } else { tr("Start reading") }).clicked() {
                            self.open_path(Path::new(&e.path));
                            self.details = None;
                        }
                        if text_button(ui, tr("Show in folder"), false).clicked() {
                            crate::platform::reveal(Path::new(&e.path));
                        }
                        if text_button(ui, tr("Edit details"), false).clicked() {
                            self.meta_edit = Some(MetaForm { id: e.id.clone(), title: e.title.clone(), author: e.author.clone(), series: e.series.clone(), index: if e.series_index > 0.0 { e.series_index.to_string() } else { String::new() }, genre: e.genre.clone() });
                        }
                    });
                    if let Some(mut f) = self.meta_edit.take() {
                        if f.id == e.id {
                            let mut keep = true;
                            ui.add_space(8.0);
                            egui::Grid::new("meta-edit").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                                for (k, v) in [(tr("Title"), &mut f.title), (tr("Author"), &mut f.author), (tr("Series"), &mut f.series), (tr("Number"), &mut f.index), (tr("Genre / tag"), &mut f.genre)] {
                                    ui.label(tag(k).color(pal.muted));
                                    ui.add(egui::TextEdit::singleline(v).desired_width(250.0));
                                    ui.end_row();
                                }
                            });
                            ui.label(tag(tr("Stored in Seshat only; the book file is never changed.")).color(pal.muted));
                            ui.horizontal(|ui| {
                                if ui::primary_button(ui, tr("Save")).clicked() {
                                    let orig = e.raw.clone().unwrap_or(library::Raw { title: e.title.clone(), author: e.author.clone(), series: e.series.clone(), series_index: e.series_index, genre: e.genre.clone() });
                                    let ov = |new: &str, o: &str| if new.trim() == o { String::new() } else { new.trim().to_string() };
                                    let ub = self.userlib.book_mut(&e.id);
                                    ub.title = ov(&f.title, &orig.title);
                                    ub.author = ov(&f.author, &orig.author);
                                    ub.series = ov(&f.series, &orig.series);
                                    ub.genre = ov(&f.genre, &orig.genre);
                                    ub.series_index = f.index.trim().parse::<f32>().ok().filter(|i| (*i - orig.series_index).abs() > 1e-6);
                                    self.user_changed(&e.id);
                                    keep = false;
                                }
                                if text_button(ui, tr("Reset to file"), false).clicked() {
                                    let ub = self.userlib.book_mut(&e.id);
                                    ub.title.clear();
                                    ub.author.clear();
                                    ub.series.clear();
                                    ub.genre.clear();
                                    ub.series_index = None;
                                    self.user_changed(&e.id);
                                    keep = false;
                                }
                                if text_button(ui, tr("Cancel"), false).clicked() {
                                    keep = false;
                                }
                            });
                            if keep {
                                self.meta_edit = Some(f);
                            }
                        }
                    }
                });
            });
            if !e.description.is_empty() {
                ui.add_space(10.0);
                ui::rule(ui);
                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                    ui.label(RichText::new(&e.description).size(13.5));
                });
            }
        });
    }

    fn status_overlays(&mut self, ctx: &egui::Context, pal: &Palette) {
        let bar = |ctx: &egui::Context, id: &str, f: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Area::new(egui::Id::new(id)).anchor(egui::Align2::RIGHT_BOTTOM, vec2(-24.0, -24.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::new().fill(INK).inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
                    ui.visuals_mut().override_text_color = Some(PAPER);
                    f(ui)
                });
            });
        };
        if let Some(name) = self.opening.clone() {
            bar(ctx, "opening", &mut |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(trf!("OPENING  {name}", name = trunc(&name, 50).to_uppercase())).font(mono(11.0)));
                });
            });
        }
        if let Some(err) = self.error.clone() {
            let mut dismiss = false;
            bar(ctx, "err", &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr("COULDN'T OPEN")).font(mono_m(11.0)).color(ACCENT));
                    ui.label(&err);
                    if ui.small_button("×").clicked() {
                        dismiss = true;
                    }
                });
            });
            if dismiss {
                self.error = None;
            }
        }
        if let Some(id) = self.confirm_delete.clone() {
            let e = self.index.entries.iter().find(|e| e.id == id).cloned();
            let mut keep = true;
            egui::Window::new(tr("Delete file?")).id(egui::Id::new("win-delete")).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                if let Some(e) = &e {
                    ui.label(trf!("“{title}” will be permanently deleted from disk:", title = e.title));
                    ui.label(RichText::new(&e.path).font(mono(10.5)).color(pal.muted));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new(tr("DELETE")).font(mono_m(11.5)).color(PAPER)).fill(ACCENT)).clicked() {
                            let _ = std::fs::remove_file(&e.path);
                            let _ = std::fs::remove_file(library::cover_path(&e.id));
                            self.index.entries.retain(|x| x.id != e.id);
                            self.settings.extra_files.retain(|f| f != &e.path);
                            self.index.save();
                            keep = false;
                        }
                        if text_button(ui, tr("Cancel"), false).clicked() {
                            keep = false;
                        }
                    });
                } else {
                    keep = false;
                }
            });
            if !keep {
                self.confirm_delete = None;
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context, pal: &Palette) {
        use ui::{form_row, form_section, FORM_CTRL_W};
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut rescan = false;
        let screen = ctx.screen_rect();
        let size = vec2((screen.width() - 80.0).clamp(560.0, 880.0), (screen.height() - 120.0).clamp(420.0, 680.0));
        let page_id = egui::Id::new("settings-page");
        let mut page: usize = ctx.data_mut(|d| d.get_temp(page_id)).unwrap_or(0);
        let pages = [tr("General"), tr("Library"), tr("Reading"), tr("Read aloud"), tr("Sync"), tr("About")];
        egui::Window::new(tr("Settings")).id(egui::Id::new("win-settings")).open(&mut open).collapsible(false).resizable(false).fixed_size(size).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                // navigation rail
                ui.allocate_ui_with_layout(vec2(170.0, size.y), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_size(vec2(170.0, size.y));
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    for (i, l) in pages.iter().enumerate() {
                        if ui::form_nav(ui, l, page == i, pal.fg, pal.hover) {
                            page = i;
                        }
                    }
                });
                ui.add_space(12.0);
                let (sep, _) = ui.allocate_exact_size(vec2(1.0, size.y), Sense::hover());
                ui.painter().vline(sep.center().x, sep.y_range(), Stroke::new(1.0, pal.border));
                ui.add_space(24.0);
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().id_salt(("settings-scroll", page)).auto_shrink([false, false]).max_height(size.y).show(ui, |ui| {
                        ui.set_width(ui.available_width() - 16.0);
                        ui.add_space(4.0);
                        match page {
                            0 => self.settings_general(ui, pal),
                            1 => rescan |= self.settings_library(ui, pal),
                            2 => self.settings_reading(ui, pal),
                            3 => {
                                form_section(ui, tr("Read aloud"), Some(tr("Choose the voice used when a book is read to you.")), true);
                                Self::voice_settings(ui, &mut self.settings, pal);
                            }
                            4 => self.settings_sync(ui, pal),
                            _ => {
                                form_section(ui, tr("About"), None, true);
                                form_row(ui, tr("Version"), |ui| ui.label(format!("{} {}", crate::APP_NAME, env!("CARGO_PKG_VERSION"))));
                                form_row(ui, tr("Settings folder"), |ui| ui.add(egui::Label::new(RichText::new(store::config_dir().display().to_string()).font(mono(11.5))).truncate()));
                                form_section(ui, tr("Keyboard"), None, false);
                                for (k, what) in [("← →", tr("Turn the page")), ("T", tr("Show or hide the toolbar")), ("C", tr("Contents")), ("B", tr("Bookmark")), ("Ctrl+K", tr("Search")), ("F", tr("Full screen")), ("Ctrl ±", tr("Text size")), ("Backspace", tr("Back")), ("Esc", tr("Close"))] {
                                    form_row(ui, what, |ui| ui.label(RichText::new(k).font(mono(12.0))));
                                }
                            }
                        }
                        ui.add_space(16.0);
                        let _ = FORM_CTRL_W;
                    });
                });
            });
        });
        ctx.data_mut(|d| d.insert_temp(page_id, page));
        if rescan {
            self.rescan();
        }
        if !open {
            self.settings_open = false;
            self.settings.save();
        }
    }

    fn settings_general(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        use ui::{form_row, form_section, FORM_CTRL_W};
        let _ = pal;
        form_section(ui, tr("Appearance"), None, true);
        form_row(ui, tr("Theme"), |ui| {
            for (t, l) in [(UiTheme::System, tr("System")), (UiTheme::Light, tr("Paper")), (UiTheme::Dark, tr("Ink"))] {
                if text_button(ui, l, self.settings.ui_theme == t).clicked() {
                    self.settings.ui_theme = t;
                }
            }
        });
        form_row(ui, tr("Cover size"), |ui| ui.add(egui::Slider::new(&mut self.settings.cover_size, 110.0..=260.0).show_value(false)));
        form_row(ui, tr("Progress column"), |ui| ui.checkbox(&mut self.settings.right_panel, tr("Show on the library page")));
        form_section(ui, tr("Language"), None, false);
        form_row(ui, tr("Interface language"), |ui| {
            let native = |code: &str| crate::i18n::LANGS.iter().find(|l| l.0 == code).map(|l| l.1).unwrap_or("English");
            let auto = trf!("Auto ({name})", name = native(crate::i18n::detect()));
            let shown = if self.settings.language.is_empty() { auto.clone() } else { native(&self.settings.language).to_string() };
            egui::ComboBox::from_id_salt("ui-lang").selected_text(shown).width(FORM_CTRL_W - 60.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.settings.language, String::new(), auto);
                for (code, name, _) in crate::i18n::LANGS {
                    ui.selectable_value(&mut self.settings.language, code.to_string(), *name);
                }
            });
        });
        form_section(ui, tr("Sources"), Some(tr("Z-Library is the only online source; it is off unless you turn it on.")), false);
        form_row(ui, "Z-Library", |ui| ui.checkbox(&mut self.settings.zlib_enabled, tr("Blend results into library search")));
        form_row(ui, tr("Proxy"), |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.proxy).hint_text("socks5://127.0.0.1:9050").desired_width(FORM_CTRL_W)));
    }

    /// Returns true when the library must be rescanned.
    fn settings_library(&mut self, ui: &mut egui::Ui, pal: &Palette) -> bool {
        use ui::{form_note, form_row, form_section};
        let _ = pal;
        let mut rescan = false;
        form_section(ui, tr("Library folders"), Some(tr("Seshat shows every book found in these folders.")), true);
        form_row(ui, tr("Downloads"), |ui| {
            ui.add_sized([ui::FORM_CTRL_W, 20.0], egui::Label::new(RichText::new(&self.settings.download_dir).font(mono(11.5))).truncate());
            if text_button(ui, tr("Change"), false).clicked() {
                if let Some(d) = rfd::FileDialog::new().set_directory(&self.settings.download_dir).pick_folder() {
                    self.settings.download_dir = d.to_string_lossy().into_owned();
                    rescan = true;
                }
            }
        });
        let mut remove = None;
        for (i, d) in self.settings.library_dirs.iter().enumerate() {
            form_row(ui, &trf!("Folder {n}", n = i + 1), |ui| {
                ui.add_sized([ui::FORM_CTRL_W, 20.0], egui::Label::new(RichText::new(d).font(mono(11.5))).truncate());
                if text_button(ui, tr("Remove"), false).clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.settings.library_dirs.remove(i);
            rescan = true;
        }
        form_row(ui, "", |ui| {
            if text_button(ui, tr("+ Add folder"), false).clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    let s = d.to_string_lossy().into_owned();
                    if !self.settings.library_dirs.contains(&s) {
                        self.settings.library_dirs.push(s);
                    }
                    rescan = true;
                }
            }
            if !self.settings.hidden_paths.is_empty() && text_button(ui, &trf!("Restore {n} removed", n = self.settings.hidden_paths.len()), false).clicked() {
                self.settings.hidden_paths.clear();
                rescan = true;
            }
        });
        form_row(ui, tr("Watch folders"), |ui| ui.checkbox(&mut self.settings.watch_folders, tr("Add new books automatically")));
        form_note(ui, tr("Checks the folders every 30 seconds."));

        if !self.userlib.collections.is_empty() {
            form_section(ui, tr("Collections"), None, false);
            let mut del = None;
            for c in &self.userlib.collections {
                form_row(ui, c, |ui| {
                    if text_button(ui, tr("Delete"), false).clicked() {
                        del = Some(c.clone());
                    }
                });
            }
            if let Some(c) = del {
                self.userlib.remove_collection(&c);
                self.userlib.save();
                self.apply_user_all();
                self.index.save();
                if self.shelf_open.as_deref() == Some(c.as_str()) {
                    self.shelf_open = None;
                }
            }
        }

        form_section(ui, tr("Import"), Some(tr("Bring highlights from a Kindle into your notes.")), false);
        form_row(ui, "Kindle", |ui| {
            ui.add_enabled_ui(!self.clip_busy, |ui| {
                if text_button(ui, tr("Kindle My Clippings.txt…"), false).clicked() {
                    self.import_clippings();
                }
            });
        });
        if !self.clip_status.is_empty() {
            form_note(ui, &self.clip_status.clone());
        }
        rescan
    }

    fn settings_reading(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        use ui::{form_note, form_row, form_section};
        let _ = pal;
        form_section(ui, tr("Lookup & reading"), None, true);
        form_row(ui, tr("Translate to"), |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.translate_to).desired_width(80.0)));
        form_note(ui, tr("A language code such as en, de or ja."));
        form_row(ui, tr("Reading speed"), |ui| ui.add(egui::Slider::new(&mut self.settings.reading_speed_cpm, 300.0..=4000.0).suffix(" chars/min")));
        form_note(ui, tr("Used to estimate the time left in a chapter."));
    }

    fn settings_sync(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        use ui::{form_note, form_row, form_section, FORM_CTRL_W};
        form_section(ui, tr("Reading progress"), Some(tr("Compatible with KOReader sync servers.")), true);
        form_row(ui, tr("Sync"), |ui| ui.checkbox(&mut self.settings.kosync.enabled, tr("Sync reading progress")));
        form_row(ui, tr("Server"), |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.kosync.server).desired_width(FORM_CTRL_W)));
        form_row(ui, tr("Username"), |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.kosync.username).desired_width(FORM_CTRL_W)));
        let saved = !self.settings.kosync.userkey.is_empty();
        form_row(ui, tr("Password"), |ui| ui.add(egui::TextEdit::singleline(&mut self.kosync_password).password(true).desired_width(FORM_CTRL_W).hint_text(if saved { "(saved)" } else { "" })));
        form_row(ui, "", |ui| {
            let mk = |s: &mut App| {
                if !s.kosync_password.is_empty() {
                    s.settings.kosync.userkey = crate::kosync::md5_hex(&s.kosync_password);
                    s.kosync_password.clear();
                }
                (s.settings.kosync.clone(), s.settings.proxy_opt(), s.tx.clone(), s.ctx.clone())
            };
            if text_button(ui, tr("Log in"), false).clicked() {
                let (cfg, proxy, tx, ctx) = mk(self);
                self.kosync_status = tr("Checking…").into();
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Kosync(crate::kosync::auth(&cfg, &proxy).map(|_| tr("Logged in").to_string())));
                    ctx.request_repaint();
                });
            }
            if text_button(ui, tr("Register"), false).clicked() {
                let (cfg, proxy, tx, ctx) = mk(self);
                self.kosync_status = tr("Registering…").into();
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Kosync(crate::kosync::register(&cfg, &proxy).map(|_| tr("Account created").to_string())));
                    ctx.request_repaint();
                });
            }
            ui.label(RichText::new(&self.kosync_status).size(12.5).color(ACCENT));
        });

        form_section(ui, tr("Notes, highlights & library"), Some(tr("Syncs highlights, notes, bookmarks, collections, ratings and edited details between machines.")), false);
        form_row(ui, tr("Method"), |ui| {
            for (b, l) in [(store::SyncBackend::Off, tr("Off")), (store::SyncBackend::Folder, tr("Sync folder")), (store::SyncBackend::WebDav, "WebDAV")] {
                if text_button(ui, l, self.settings.sync.backend == b).clicked() {
                    self.settings.sync.backend = b;
                }
            }
        });
        match self.settings.sync.backend {
            store::SyncBackend::Off => {}
            store::SyncBackend::Folder => {
                form_row(ui, tr("Folder"), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.settings.sync.folder).desired_width(FORM_CTRL_W));
                    if text_button(ui, tr("Browse…"), false).clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            self.settings.sync.folder = d.to_string_lossy().into_owned();
                        }
                    }
                });
                form_note(ui, tr("Use a folder your sync tool (Syncthing, Dropbox, Nextcloud…) keeps in step."));
            }
            store::SyncBackend::WebDav => {
                form_row(ui, "URL", |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.sync.dav_url).desired_width(FORM_CTRL_W).hint_text("https://cloud.example.com/remote.php/dav/files/me")));
                form_row(ui, tr("Username"), |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.sync.dav_user).desired_width(FORM_CTRL_W)));
                form_row(ui, tr("Password"), |ui| ui.add(egui::TextEdit::singleline(&mut self.settings.sync.dav_pass).password(true).desired_width(FORM_CTRL_W)));
            }
        }
        if self.settings.sync.backend != store::SyncBackend::Off {
            form_row(ui, tr("Reading time"), |ui| ui.checkbox(&mut self.settings.sync.reading_time, tr("Also sync reading time")));
            form_row(ui, "", |ui| {
                if text_button(ui, tr("Sync now"), false).clicked() && !self.sync_busy {
                    self.settings.save();
                    self.start_sync(None);
                }
                ui.label(RichText::new(&self.sync_status).size(12.5).color(ACCENT));
            });
        }
        let _ = pal;
    }
}

fn view_match(v: View, e: &Entry) -> bool {
    match v {
        View::Reading => e.fraction > 0.0 && !e.finished,
        View::Unread => e.fraction == 0.0 && !e.finished,
        View::Finished => e.finished,
        _ => true,
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// Pill dropdown: label chip that opens a small menu.
fn dropdown(ui: &mut egui::Ui, id: &str, label: &str, active: bool, body: impl FnOnce(&mut egui::Ui)) {
    let r = text_button(ui, &format!("{label}    "), active);
    let c = pos2(r.rect.right() - 14.0, r.rect.center().y);
    let col = if active { ACCENT } else { ui.visuals().text_color() };
    ui.painter().line_segment([c + vec2(-3.5, -1.5), c + vec2(0.0, 2.0)], Stroke::new(1.3, col));
    ui.painter().line_segment([c + vec2(0.0, 2.0), c + vec2(3.5, -1.5)], Stroke::new(1.3, col));
    let pid = ui.make_persistent_id(id);
    if r.clicked() {
        ui.memory_mut(|m| m.toggle_popup(pid));
    }
    egui::popup_below_widget(ui, pid, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(150.0);
        body(ui);
    });
}

// ---------- statistics and duplicates ----------

fn day_number(date: &str) -> Option<i64> {
    let mut it = date.split('-').filter_map(|x| x.parse::<i64>().ok());
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

fn fmt_dur(secs: f64) -> String {
    let m = (secs / 60.0).round() as i64;
    if m >= 60 {
        trf!("{h} h {m} min", h = m / 60, m = format!("{:02}", m % 60))
    } else {
        trf!("{n} min", n = m)
    }
}

impl App {
    fn stats_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.label(display(tr("Statistics"), 34.0));
        let daily = &self.overview.daily;
        let today = store::now().div_euclid(86400);
        let active: Vec<i64> = daily.iter().filter(|(_, s)| **s > 0.0).filter_map(|(d, _)| day_number(d)).collect();
        let mut sorted = active.clone();
        sorted.sort_unstable();
        sorted.dedup();
        let (mut longest, mut run, mut prev) = (0, 0, i64::MIN);
        for &d in &sorted {
            run = if d == prev + 1 { run + 1 } else { 1 };
            longest = longest.max(run);
            prev = d;
        }
        let mut current = 0;
        let mut d = if sorted.contains(&today) { today } else { today - 1 };
        while sorted.contains(&d) {
            current += 1;
            d -= 1;
        }
        let finished = self.index.entries.iter().filter(|e| e.finished).count();
        let started = self.index.entries.iter().filter(|e| e.fraction > 0.0 || e.finished).count();
        let (mut psecs, mut pfrac) = (0.0f64, 0.0f64);
        let mut per_book: Vec<(String, String, f64)> = Vec::new();
        for e in &self.index.entries {
            let Some((_, st)) = self.stats.get(&e.id) else { continue };
            if st.seconds > 0.0 {
                per_book.push((e.id.clone(), e.title.clone(), st.seconds));
            }
            let f = if e.finished { 1.0 } else { e.fraction as f64 };
            if st.seconds >= 300.0 && f >= 0.05 {
                psecs += st.seconds;
                pfrac += f;
            }
        }
        per_book.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let total = self.overview.total_secs;
        ui.label(tag(trf!("{n} days of reading", n = sorted.len())).color(pal.muted));
        ui.add_space(8.0);
        ui::rule(ui);
        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(18.0);
            let tiles = [
                (tr("Time read"), fmt_dur(total)),
                (tr("Current streak"), if current == 1 { trf!("{n} day", n = current) } else { trf!("{n} days", n = current) }),
                (tr("Longest streak"), if longest == 1 { trf!("{n} day", n = longest) } else { trf!("{n} days", n = longest) }),
                (tr("Books finished"), trf!("{done} of {started} started", done = finished, started = started)),
                (tr("Average per day read"), if sorted.is_empty() { "-".into() } else { fmt_dur(total / sorted.len() as f64) }),
                (tr("Pace per book"), if pfrac > 0.0 { trf!("about {time} per book", time = fmt_dur(psecs / pfrac)) } else { "-".into() }),
                (tr("Highlights"), self.overview.notes.len().to_string()),
            ];
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(36.0, 18.0);
                for (k, v) in &tiles {
                    ui.vertical(|ui| {
                        ui.label(tag(*k).color(pal.muted));
                        ui.label(RichText::new(v).font(ui_b(24.0)));
                    });
                }
            });

            // calendar heatmap: the last 26 weeks, one column per week, Monday on top
            ui.add_space(30.0);
            ui.label(tag(tr("Last six months")).color(pal.muted));
            ui::rule(ui);
            ui.add_space(8.0);
            let (cell, gap, weeks) = (13.0f32, 3.0f32, 27usize);
            let wd = (today + 3).rem_euclid(7);
            let start = today - wd - (weeks as i64 - 1) * 7;
            let (area, resp) = ui.allocate_exact_size(vec2(weeks as f32 * (cell + gap), 18.0 + 7.0 * (cell + gap)), Sense::hover());
            let month = [tr("Jan"), tr("Feb"), tr("Mar"), tr("Apr"), tr("May"), tr("Jun"), tr("Jul"), tr("Aug"), tr("Sep"), tr("Oct"), tr("Nov"), tr("Dec")];
            let origin = area.min + vec2(0.0, 18.0);
            let mut last_month = 99usize;
            for c in 0..weeks {
                let date = store::date_string((start + c as i64 * 7) * 86400);
                let m = date.get(5..7).and_then(|x| x.parse::<usize>().ok()).unwrap_or(1);
                if m != last_month {
                    last_month = m;
                    ui.painter().text(pos2(origin.x + c as f32 * (cell + gap), area.top() + 6.0), egui::Align2::LEFT_CENTER, month[(m - 1).min(11)], mono(9.5), pal.muted);
                }
                for r in 0..7 {
                    let day = start + c as i64 * 7 + r;
                    if day > today {
                        continue;
                    }
                    let secs = self.overview.daily.get(&store::date_string(day * 86400)).copied().unwrap_or(0.0);
                    let col = if secs <= 0.0 {
                        pal.hover
                    } else {
                        let a = match secs / 60.0 {
                            m if m < 15.0 => 0.3,
                            m if m < 30.0 => 0.55,
                            m if m < 60.0 => 0.8,
                            _ => 1.0,
                        };
                        ACCENT.linear_multiply(a)
                    };
                    let rect = Rect::from_min_size(origin + vec2(c as f32 * (cell + gap), r as f32 * (cell + gap)), vec2(cell, cell));
                    ui.painter().rect_filled(rect, CornerRadius::same(2), col);
                }
            }
            if let Some(p) = resp.hover_pos() {
                let (x, y) = (p.x - origin.x, p.y - origin.y);
                if x >= 0.0 && y >= 0.0 {
                    let (c, r) = ((x / (cell + gap)) as i64, (y / (cell + gap)) as i64);
                    let day = start + c * 7 + r;
                    if c < weeks as i64 && r < 7 && day <= today {
                        let date = store::date_string(day * 86400);
                        let secs = self.overview.daily.get(&date).copied().unwrap_or(0.0);
                        resp.on_hover_ui_at_pointer(|ui| {
                            ui.label(format!("{date}: {}", if secs > 0.0 { fmt_dur(secs) } else { "no reading".into() }));
                        });
                    }
                }
            }

            ui.add_space(30.0);
            ui.label(tag(tr("Time per book")).color(pal.muted));
            ui::rule(ui);
            ui.add_space(8.0);
            if per_book.is_empty() {
                ui.label(RichText::new(tr("Reading time is recorded while you read.")).color(pal.muted));
            }
            let max = per_book.first().map(|b| b.2).unwrap_or(1.0).max(1.0);
            for (id, title, secs) in per_book.iter().take(15) {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width().min(760.0), 26.0), Sense::click());
                let frac = (*secs / max) as f32;
                ui.painter().rect_filled(Rect::from_min_size(r.min + vec2(0.0, 22.0), vec2(r.width() * frac, 2.0)), CornerRadius::ZERO, if resp.hovered() { ACCENT } else { pal.fg });
                ui.painter().with_clip_rect(Rect::from_min_max(r.min, pos2(r.right() - 90.0, r.bottom()))).text(pos2(r.left(), r.top() + 10.0), egui::Align2::LEFT_CENTER, title, ui_m(14.0), pal.fg);
                ui.painter().text(pos2(r.right(), r.top() + 10.0), egui::Align2::RIGHT_CENTER, fmt_dur(*secs), mono(11.0), pal.muted);
                if resp.clicked() {
                    open = Some(id.clone());
                }
            }
            ui.add_space(40.0);
        });
        if let Some(id) = open {
            self.details = Some(id);
        }
    }

    fn duplicates_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.label(display(tr("Duplicates"), 34.0));
        ui.label(tag(trf!("{n} sets of repeated books", n = format!("{:03}", self.dups.len()))).color(pal.muted));
        ui.add_space(8.0);
        ui::rule(ui);
        if self.dups.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new(tr("No duplicates found.")).font(ui_m(17.0)).color(pal.muted));
            return;
        }
        let mut remove: Option<String> = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (gi, g) in self.dups.iter().enumerate() {
                ui.add_space(22.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:02}", gi + 1)).font(ui_b(22.0)).color(ACCENT));
                    ui.label(RichText::new(&g.items[0].title).font(ui_b(20.0)));
                    ui.label(tag(tr(g.reason)).color(pal.muted));
                });
                for it in &g.items {
                    ui.horizontal(|ui| {
                        ui.add_space(34.0);
                        ui.vertical(|ui| {
                            ui.label(tag(format!("{} · {} · {}", it.format, library::fmt_size(it.size), it.author)).color(pal.muted));
                            ui.add(egui::Label::new(RichText::new(&it.path).font(mono(10.5))).truncate());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if text_button(ui, tr("Remove from library"), false).clicked() {
                                remove = Some(it.path.clone());
                            }
                            if text_button(ui, tr("Open folder"), false).clicked() {
                                crate::platform::reveal(Path::new(&it.path));
                            }
                        });
                    });
                }
            }
            ui.add_space(40.0);
        });
        if let Some(p) = remove {
            self.remove_path(&p);
        }
    }
}

fn chrono_hm() -> String {
    let t = store::now() % 86400;
    format!("{:02}:{:02} UTC", t / 3600, t % 3600 / 60)
}
