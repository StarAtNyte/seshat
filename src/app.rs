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

use std::collections::BTreeMap;
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
use crate::store::{self, Annotation, AppSettings, BookData, Bookmark, LibView, SortKey, Theme, UiTheme};
use crate::texload::{Src, TexLoader};
use crate::ui::{self, display, mono, mono_m, tag, text_button, ui_b, ui_m, Palette, ACCENT, INK, PAPER};

enum Msg {
    Scanned(Index),
    Opened(Result<(String, PathBuf, Arc<Book>), String>),
    Fonts(FontDb),
    Kosync(Result<String, String>),
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
    scanning: bool,
    scan_done: Arc<AtomicUsize>,
    scan_total: Arc<AtomicUsize>,
    rescan_after: bool,
    view: View,
    view_changed: Instant,
    query: String,
    format: String,
    covers: TexLoader,
    reader: Option<Reader>,
    opening: Option<String>,
    pending_jump: Option<(String, Loc)>,
    error: Option<String>,
    fontdb: Option<FontDb>,
    families: Vec<String>,
    installed: Installed,
    font_req: (String, bool),
    want_cjk: bool,
    online: Online,
    details: Option<String>,
    settings_open: bool,
    confirm_delete: Option<String>,
    style_key: Option<(Color32, Color32, bool)>,
    kosync_password: String,
    kosync_status: String,
    pending_open: Option<PathBuf>,
    debug_start: Option<Instant>,
    debug_keys_sent: usize,
    debug_shot_requested: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let settings = AppSettings::load();
        let installed = fonts::install(&ctx, None, "", false);
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
            scanning: false,
            scan_done: Arc::new(AtomicUsize::new(0)),
            scan_total: Arc::new(AtomicUsize::new(0)),
            rescan_after: false,
            view: View::All,
            view_changed: Instant::now(),
            query: String::new(),
            format: String::new(),
            covers: TexLoader::new(&ctx, 2),
            reader: None,
            opening: None,
            pending_jump: None,
            error: None,
            fontdb: None,
            families: Vec::new(),
            installed,
            font_req: (String::from("\u{0}"), false),
            want_cjk: false,
            online: Online::new(&ctx),
            details: None,
            settings_open: false,
            confirm_delete: None,
            style_key: None,
            kosync_password: String::new(),
            kosync_status: String::new(),
            pending_open: open,
            debug_start: None,
            debug_keys_sent: 0,
            debug_shot_requested: false,
        };
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

    fn refresh_overview(&mut self) {
        let mut o = Overview::default();
        for e in &self.index.entries {
            if !BookData::path_for(&e.id).exists() {
                continue;
            }
            let d = BookData::load(&e.id);
            for a in d.annotations {
                o.notes.push((e.id.clone(), e.title.clone(), a));
            }
            for b in d.bookmarks {
                o.bookmarks.push((e.id.clone(), e.title.clone(), b));
            }
            for (day, s) in d.daily {
                *o.daily.entry(day).or_insert(0.0) += s;
            }
            o.total_secs += d.seconds;
        }
        o.notes.sort_by(|a, b| b.2.created.cmp(&a.2.created));
        o.bookmarks.sort_by(|a, b| b.2.created.cmp(&a.2.created));
        self.overview = o;
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
        if self.opening.is_some() {
            return;
        }
        let in_lib = self.index.entries.iter().any(|e| Path::new(&e.path) == path);
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
                Msg::Scanned(idx) => {
                    self.scanning = false;
                    self.index = idx;
                    self.index.save();
                    self.refresh_overview();
                    if std::mem::take(&mut self.rescan_after) {
                        self.rescan();
                    }
                }
                Msg::Opened(r) => {
                    self.opening = None;
                    match r {
                        Ok((id, path, book)) => {
                            if let Some(mut old) = self.reader.take() {
                                old.close(&self.settings);
                            }
                            self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — zlib-reader", book.meta.title)));
                            let mut r = Reader::new(&self.ctx, id.clone(), path.clone(), book, &self.settings);
                            if let Some((pid, loc)) = self.pending_jump.take() {
                                if pid == id {
                                    r.jump(loc, false);
                                }
                            }
                            if let Some(e) = self.index.entries.iter_mut().find(|e| e.id == id) {
                                e.last_read = store::now();
                            }
                            self.reader = Some(r);
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Msg::Fonts(db) => {
                    self.families = db.families.clone();
                    self.fontdb = Some(db);
                    self.font_req = ("\u{0}".into(), false);
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

    fn close_reader(&mut self) {
        if let Some(mut r) = self.reader.take() {
            r.close(&self.settings);
            if let Some(e) = self.index.entries.iter_mut().find(|e| e.id == r.id) {
                e.fraction = r.data.fraction;
                e.last_read = r.data.last_read;
                e.finished = r.data.finished;
            }
            self.index.save();
            self.refresh_overview();
            self.view_changed = Instant::now();
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Title("zlib-reader".into()));
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
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
        let family = match &self.reader {
            Some(r) => r.effective(&self.settings.reader).font,
            None => self.settings.reader.font.clone(),
        };
        let req = (family, self.want_cjk);
        if req != self.font_req {
            self.font_req = req.clone();
            self.installed = fonts::install(ctx, self.fontdb.as_mut(), &req.0, req.1);
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
            painter.text(r.left_center() + vec2(60.0, -30.0), egui::Align2::LEFT_CENTER, "DROP", ui_b(140.0), INK);
            painter.text(r.left_center() + vec2(66.0, 60.0), egui::Align2::LEFT_CENTER, "BOOKS OR FOLDERS INTO THE LIBRARY", mono_m(14.0), INK);
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
        let Ok(path) = std::env::var("ZR_SHOT") else { return };
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
        self.close_reader();
        self.settings.save();
        self.index.save();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.debug_screenshot(ctx);
        self.drain();
        self.covers.poll();
        for _ in std::mem::take(&mut self.online.downloaded) {
            self.rescan();
        }
        self.sync_fonts(ctx);
        self.persist_settings();

        if self.reader.is_some() {
            let s = self.reader.as_ref().unwrap().effective(&self.settings.reader);
            let pal = ui::palette(&s);
            self.apply_style(ctx, &pal);
            let actions = {
                let r = self.reader.as_mut().unwrap();
                let mut env = Env { app: &mut self.settings, families: &self.families, installed: &self.installed };
                r.ui(ctx, &mut env)
            };
            for a in actions {
                match a {
                    Action::Close => self.close_reader(),
                    Action::InstallFont { cjk, .. } => {
                        if cjk {
                            self.want_cjk = true;
                        }
                    }
                }
            }
            self.handle_drops(ctx);
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
                egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 40, right: 28, top: 26, bottom: 0 })).show(ctx, |ui| {
                    ui.multiply_opacity(vt);
                    ui.add_space((1.0 - vt) * 18.0);
                    match self.view {
                        View::Notes => self.notes_view(ui, &pal),
                        View::Bookmarks => self.bookmarks_view(ui, &pal),
                        _ => self.library_view(ui, &pal),
                    }
                });
            }
        }
        self.details_window(ctx, &pal);
        self.settings_window(ctx, &pal);
        self.status_overlays(ctx, &pal);
        self.handle_drops(ctx);
    }
}

// ---------- navigation column ----------

impl App {
    fn count(&self, v: View) -> usize {
        self.index.entries.iter().filter(|e| view_match(v, e)).count()
    }

    fn nav(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::SidePanel::left("nav").exact_width(212.0).resizable(false).frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 22, right: 16, top: 24, bottom: 16 }).stroke(Stroke::new(1.0, pal.border))).show(ctx, |ui| {
            // wordmark
            let (r, _) = ui.allocate_exact_size(vec2(170.0, 30.0), Sense::hover());
            let g1 = ui.painter().layout_no_wrap("ZLIB".into(), ui_b(25.0), pal.fg);
            let w1 = g1.size().x;
            ui.painter().galley(r.left_top(), g1, pal.fg);
            ui.painter().text(r.left_top() + vec2(w1 + 1.0, 0.0), egui::Align2::LEFT_TOP, "/", ui_b(25.0), ACCENT);
            ui.painter().text(r.left_top() + vec2(w1 + 12.0, 0.0), egui::Align2::LEFT_TOP, "READER", ui_b(25.0), pal.fg);
            ui.label(tag("Desktop edition").color(pal.muted));
            ui.add_space(26.0);

            let mut marker_y: Option<f32> = None;
            let mut nav_item = |ui: &mut egui::Ui, me: &mut App, idx: &str, label: &str, count: Option<usize>, v: View, marker: &mut Option<f32>| {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                let active = me.view == v;
                let hov = ui.ctx().animate_bool_with_time(ui.id().with(("nav", idx)), resp.hovered() || active, 0.15);
                let c = pal.fg.linear_multiply(0.7 + 0.3 * hov);
                ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, idx, mono(10.0), if active { ACCENT } else { pal.muted });
                ui.painter().text(rect.left_center() + vec2(26.0 + 4.0 * hov, 0.0), egui::Align2::LEFT_CENTER, label, if active { ui_m(14.5) } else { egui::FontId::proportional(14.5) }, c);
                if let Some(n) = count {
                    ui.painter().text(rect.right_center(), egui::Align2::RIGHT_CENTER, format!("{n:03}"), mono(10.5), pal.muted);
                }
                if active {
                    *marker = Some(rect.top() + 6.0);
                }
                if resp.clicked() {
                    me.set_view(v);
                }
            };
            ui.label(tag("Library").color(pal.muted));
            ui::rule(ui);
            ui.add_space(4.0);
            for (i, (v, l)) in [(View::All, "All books"), (View::Reading, "Reading"), (View::Unread, "Unread"), (View::Finished, "Finished")].into_iter().enumerate() {
                let n = self.count(v);
                nav_item(ui, self, &format!("{:02}", i + 1), l, Some(n), v, &mut marker_y);
            }
            ui.add_space(18.0);
            ui.label(tag("Marginalia").color(pal.muted));
            ui::rule(ui);
            ui.add_space(4.0);
            let (nn, nb) = (self.overview.notes.len(), self.overview.bookmarks.len());
            nav_item(ui, self, "05", "Notes", Some(nn), View::Notes, &mut marker_y);
            nav_item(ui, self, "06", "Bookmarks", Some(nb), View::Bookmarks, &mut marker_y);
            ui.add_space(18.0);
            ui.label(tag("Sources").color(pal.muted));
            ui::rule(ui);
            ui.add_space(4.0);
            if self.settings.zlib_enabled {
                nav_item(ui, self, "07", "Z-Library", None, View::Source, &mut marker_y);
                let y = ui.cursor().top() - 14.0;
                let x = ui.max_rect().right() - 4.0;
                ui.painter().rect_filled(Rect::from_center_size(pos2(x, y), vec2(6.0, 6.0)), CornerRadius::ZERO, if self.online.connected() { Color32::from_rgb(40, 170, 90) } else { pal.border });
            } else {
                ui.label(RichText::new("No sources enabled").size(12.5).color(pal.muted));
            }
            // sliding accent marker
            if let Some(y) = marker_y {
                let ay = ui.ctx().animate_value_with_time(egui::Id::new("nav-marker"), y, 0.22);
                ui.painter().rect_filled(Rect::from_min_size(pos2(ui.max_rect().left() - 22.0, ay), vec2(4.0, 16.0)), CornerRadius::ZERO, ACCENT);
            }
            ui.add_space(14.0);
            if let Some(p) = self.online.downloads_ui(ui, pal) {
                self.open_path(&p);
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.label(tag("Reading is a form of attention.").color(pal.muted));
                ui.add_space(10.0);
                if self.scanning {
                    let (d, t) = (self.scan_done.load(Ordering::Relaxed), self.scan_total.load(Ordering::Relaxed));
                    ui.label(tag(if t > 0 { format!("Indexing {d}/{t}") } else { "Indexing…".into() }).color(ACCENT));
                }
                if text_button(ui, "Settings", self.settings_open).clicked() {
                    self.settings_open = !self.settings_open;
                }
                if text_button(ui, "Add folder", false).clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        let s = d.to_string_lossy().into_owned();
                        if !self.settings.library_dirs.contains(&s) {
                            self.settings.library_dirs.push(s);
                        }
                        self.rescan();
                    }
                }
                if text_button(ui, "+ Import", false).on_hover_text("Ctrl+O").clicked() {
                    self.import_dialog();
                }
            });
        });
    }

    // ---------- right column ----------

    fn right_column(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::SidePanel::right("right").exact_width(264.0).resizable(false).frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 20, right: 22, top: 26, bottom: 16 }).stroke(Stroke::new(1.0, pal.border))).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let current = self.index.entries.iter().filter(|e| e.fraction > 0.0 && !e.finished).max_by_key(|e| e.last_read).cloned();
                ui.label(tag("Progress").color(pal.muted));
                ui::rule(ui);
                if let Some(e) = &current {
                    let shown = ui.ctx().animate_value_with_time(egui::Id::new("pct"), e.fraction, 0.8);
                    if (shown - e.fraction).abs() > 0.001 {
                        ui.ctx().request_repaint();
                    }
                    ui.label(RichText::new(format!("{:.0}%", shown * 100.0)).font(ui_b(72.0)).color(ACCENT));
                    ui.add(egui::Label::new(RichText::new(&e.title).font(ui_m(14.0))).truncate());
                    let r = ui.available_rect_before_wrap();
                    let bar = Rect::from_min_size(r.min + vec2(0.0, 6.0), vec2(r.width(), 2.0));
                    ui.painter().rect_filled(bar, CornerRadius::ZERO, pal.border);
                    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * shown, 2.0)), CornerRadius::ZERO, pal.fg);
                    ui.add_space(14.0);
                } else {
                    ui.label(RichText::new("Nothing in progress.").color(pal.muted));
                }

                ui.add_space(18.0);
                ui.label(tag("This week").color(pal.muted));
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
                ui.label(tag(format!("{} min this week · {} h total", (week / 60.0).round(), (self.overview.total_secs / 3600.0 * 10.0).round() / 10.0)).color(pal.muted));

                ui.add_space(18.0);
                ui.label(tag("Recent notes").color(pal.muted));
                ui::rule(ui);
                ui.add_space(6.0);
                if self.overview.notes.is_empty() {
                    ui.label(RichText::new("Highlights you make while reading collect here.").size(12.5).color(pal.muted));
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
                ui::paint_placeholder_cover(&p, rect, &e.title, &e.author);
            }
        }
    }

    fn library_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let items = self.visible_entries();
        let title = match self.view {
            View::Reading => "Currently Reading",
            View::Unread => "Unread",
            View::Finished => "Finished",
            _ => "The Library",
        };
        ui.horizontal(|ui| {
            ui.label(display(title, 46.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                ui.label(tag(format!("{:03} objects / {}", self.index.entries.len(), store::date_string(store::now()))).color(pal.muted));
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let (sr, _) = ui.allocate_exact_size(vec2((ui.available_width() * 0.4).clamp(220.0, 440.0), 30.0), Sense::hover());
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(sr));
            let focused = child
                .horizontal(|ui| {
                    ui.label(RichText::new("FIND").font(mono_m(10.5)).color(ACCENT));
                    let r = ui.add(egui::TextEdit::singleline(&mut self.query).id(egui::Id::new("lib-search")).frame(false).hint_text(RichText::new("title, author — or any book in the world").color(pal.muted)).desired_width(f32::INFINITY).font(ui_m(15.0)));
                    r.has_focus()
                })
                .inner;
            let f = ui.ctx().animate_bool_with_time(egui::Id::new("search-line"), focused || !self.query.is_empty(), 0.25);
            ui.painter().hline(sr.x_range(), sr.bottom(), Stroke::new(1.0, pal.border));
            ui.painter().hline(sr.left()..=sr.left() + sr.width() * f, sr.bottom(), Stroke::new(1.5, pal.fg));
            ui.add_space(16.0);
            ui.label(tag("Sort").color(pal.muted));
            for (k, l) in [(SortKey::Recent, "Recent"), (SortKey::Title, "Title"), (SortKey::Author, "Author"), (SortKey::Added, "Added"), (SortKey::Progress, "Progress")] {
                if text_button(ui, l, self.settings.sort == k).clicked() {
                    if self.settings.sort == k {
                        self.settings.sort_desc = !self.settings.sort_desc;
                    } else {
                        self.settings.sort = k;
                        self.settings.sort_desc = !matches!(k, SortKey::Title | SortKey::Author);
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(tag("Shelve by").color(pal.muted));
            for (g, l) in [(0u8, "Nothing"), (1, "Author"), (2, "Status"), (3, "Format"), (4, "Folder")] {
                if text_button(ui, l, self.settings.lib_group == g).clicked() {
                    self.settings.lib_group = g;
                    self.view_changed = Instant::now();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, "Index", self.settings.lib_view == LibView::List).clicked() {
                    self.settings.lib_view = LibView::List;
                    self.view_changed = Instant::now();
                }
                if text_button(ui, "Covers", self.settings.lib_view == LibView::Grid).clicked() {
                    self.settings.lib_view = LibView::Grid;
                    self.view_changed = Instant::now();
                }
                ui.label(tag("View").color(pal.muted));
            });
        });
        ui.add_space(4.0);
        ui::rule(ui);

        if self.index.entries.is_empty() {
            self.empty_state(ui, pal);
            return;
        }

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(22.0);
            if self.query.is_empty() && self.view == View::All && self.settings.lib_group == 0 {
                self.featured(ui, pal);
            }
            if items.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new(if self.query.is_empty() { "Nothing here yet." } else { "Not on your shelves." }).font(ui_b(28.0)).color(pal.muted));
            } else if self.settings.lib_group == 0 {
                ui.horizontal(|ui| {
                    ui.label(tag(format!("Index / {:03}", items.len())).color(pal.muted));
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

    fn format_filter(&mut self, ui: &mut egui::Ui) {
        let mut formats: Vec<String> = self.index.entries.iter().map(|e| e.format.clone()).collect();
        formats.sort();
        formats.dedup();
        if formats.len() > 1 {
            ui.add_space(10.0);
            if text_button(ui, "All", self.format.is_empty()).clicked() {
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

    /// Typographic shelves: a giant label column beside each group of books.
    fn grouped(&mut self, ui: &mut egui::Ui, pal: &Palette, items: &[usize]) {
        let mode = self.settings.lib_group;
        let mut groups: Vec<(String, String, Vec<usize>)> = Vec::new();
        for &i in items {
            let e = &self.index.entries[i];
            let (key, big) = match mode {
                1 => {
                    let a = e.author.split([',', ';', '&']).next().unwrap_or("").trim().to_string();
                    let a = if a.is_empty() { "Unknown".to_string() } else { a };
                    let surname = a.split_whitespace().last().unwrap_or("?").to_string();
                    (a, surname.chars().next().unwrap_or('?').to_uppercase().to_string())
                }
                2 => {
                    let s = if e.finished { "Finished" } else if e.fraction > 0.0 { "Reading" } else { "Unread" };
                    (s.to_string(), s.to_string())
                }
                3 => (e.format.clone(), e.format.clone()),
                _ => {
                    let d = Path::new(&e.path).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    (d.clone(), d)
                }
            };
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => g.2.push(i),
                None => groups.push((key, big, vec![i])),
            }
        }
        match mode {
            1 => groups.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0))),
            2 => groups.sort_by_key(|g| match g.0.as_str() {
                "Reading" => 0,
                "Unread" => 1,
                _ => 2,
            }),
            _ => groups.sort_by(|a, b| b.2.len().cmp(&a.2.len())),
        }
        let mut offset = 0;
        let mut last_big = String::new();
        for (gi, (key, big, idx)) in groups.iter().enumerate() {
            ui.add_space(if gi == 0 { 6.0 } else { 34.0 });
            let w = ui.available_width();
            let label_w = if mode == 1 { 150.0 } else { (w * 0.3).clamp(200.0, 340.0) };
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(label_w);
                    let r = ui.available_rect_before_wrap();
                    ui.painter().hline(r.left()..=r.left() + label_w - 24.0, r.top(), Stroke::new(2.0, pal.fg));
                    ui.add_space(8.0);
                    if mode == 1 {
                        if *big != last_big {
                            ui.label(RichText::new(big).font(ui_b(110.0)).color(ACCENT).line_height(Some(100.0)));
                        }
                        ui.label(RichText::new(key).font(ui_m(15.0)));
                    } else {
                        let size = if big.chars().count() > 9 { 34.0 } else { 52.0 };
                        let mut job = egui::text::LayoutJob::single_section(big.to_uppercase(), egui::TextFormat { font_id: ui_b(size), color: pal.fg, line_height: Some(size * 0.95), extra_letter_spacing: -size * 0.03, ..Default::default() });
                        job.wrap.max_width = label_w - 24.0;
                        let g = ui.painter().layout_job(job);
                        let (rr, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(rr.min, g, pal.fg);
                    }
                    ui.label(RichText::new(format!("{:03}", idx.len())).font(ui_b(28.0)).color(if mode == 1 { pal.muted } else { ACCENT }));
                    ui.label(tag(if idx.len() == 1 { "object" } else { "objects" }).color(pal.muted));
                });
                ui.vertical(|ui| {
                    ui.set_width(w - label_w - 8.0);
                    ui.add_space(14.0);
                    self.shelf(ui, pal, idx, offset);
                });
            });
            last_big = big.clone();
            offset += idx.len();
        }
    }

    fn featured(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let mut reading: Vec<Entry> = self.index.entries.iter().filter(|e| e.last_read > 0).cloned().collect();
        reading.sort_by(|a, b| b.last_read.cmp(&a.last_read));
        let Some(cur) = reading.iter().find(|e| !e.finished && e.fraction > 0.0).cloned().or_else(|| reading.first().cloned()) else { return };
        let n_reading = self.count(View::Reading);
        ui.label(tag(format!("Currently reading / {:03}", n_reading.max(1))).color(pal.muted));
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
        let mut job = egui::text::LayoutJob::single_section(cur.title.clone(), egui::TextFormat { font_id: ui_b(56.0), color: pal.fg, line_height: Some(54.0), extra_letter_spacing: -1.8, ..Default::default() });
        job.wrap = egui::text::TextWrapping { max_width: tw, max_rows: 3, overflow_character: Some('…'), ..Default::default() };
        let g = ui.painter().layout_job(job);
        let th = g.size().y;
        ui.painter().galley(pos2(x, row.top() - 8.0), g, pal.fg);
        ui.painter().text(pos2(x, row.top() + th + 4.0), egui::Align2::LEFT_TOP, &cur.author, ui_m(20.0), pal.muted);
        let by = row.bottom() - 80.0;
        ui.painter().text(pos2(x, by), egui::Align2::LEFT_TOP, format!("{:.0}% COMPLETE  ·  LAST OPENED {}", cur.fraction * 100.0, store::relative_time(cur.last_read).to_uppercase()), mono(11.0), pal.fg);
        let fill = ui.ctx().animate_value_with_time(egui::Id::new(("feat-fill", &cur.id)), cur.fraction.clamp(0.0, 1.0), 0.9);
        let bar = Rect::from_min_size(pos2(x, by + 22.0), vec2(tw, 2.0));
        ui.painter().rect_filled(bar, CornerRadius::ZERO, pal.border);
        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(tw * fill, 2.0)), CornerRadius::ZERO, ACCENT);
        let btn = Rect::from_min_size(pos2(x, row.bottom() - 36.0), vec2(160.0 + 20.0 * hov, 36.0));
        let bresp = ui.interact(btn, ui.id().with("resume"), Sense::click());
        ui.painter().rect_filled(btn, CornerRadius::ZERO, if bresp.hovered() || resp.hovered() { ACCENT } else { pal.fg });
        ui.painter().text(btn.left_center() + vec2(16.0, 0.0), egui::Align2::LEFT_CENTER, if cur.fraction > 0.0 { "RESUME" } else { "OPEN" }, mono_m(12.0), pal.bg);
        ui.painter().text(btn.right_center() - vec2(16.0, 0.0), egui::Align2::RIGHT_CENTER, "→", mono_m(14.0), pal.bg);
        if resp.clicked() || bresp.clicked() {
            self.open_path(Path::new(&cur.path));
        }
        let recent: Vec<Entry> = reading.into_iter().filter(|e| e.id != cur.id).take(10).collect();
        if !recent.is_empty() {
            ui.add_space(38.0);
            ui.label(tag(format!("Recently opened / {:03}", recent.len())).color(pal.muted));
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
        ui.add_space(38.0);
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
                let info = format!("{}  ·  {}  ·  {}", if e.finished { "DONE".into() } else { format!("{:.0}%", e.fraction * 100.0) }, e.format, library::fmt_size(e.size));
                painter.with_clip_rect(strip).text(strip.left_center() + vec2(9.0, 0.0), egui::Align2::LEFT_CENTER, info, mono(10.0), PAPER);
            } else if e.fraction > 0.0 && !e.finished {
                painter.rect_filled(Rect::from_min_size(pos2(cover.left(), cover.bottom() - 3.0), vec2(cover.width() * e.fraction, 3.0)), CornerRadius::ZERO, ACCENT);
            }
            let y = cover.bottom() + 10.0 + 6.0 * t;
            let (st, sc) = if e.finished { ("FINISHED", pal.muted) } else if e.fraction > 0.0 { ("READING", ACCENT) } else { ("UNREAD", pal.muted) };
            painter.text(pos2(r.left(), y), egui::Align2::LEFT_TOP, format!("{:02} —", n + offset + 1), mono(10.0), if t > 0.5 { ACCENT } else { pal.muted });
            painter.text(pos2(r.right(), y), egui::Align2::RIGHT_TOP, st, mono(9.5), sc);
            let big = r.width() > cw + 1.0;
            let mut job = egui::text::LayoutJob::single_section(e.title.clone(), egui::TextFormat { font_id: if big { ui_b(21.0) } else { ui_m(14.0) }, color: pal.fg, line_height: Some(if big { 22.0 } else { 17.0 }), ..Default::default() });
            job.wrap = egui::text::TextWrapping { max_width: r.width(), max_rows: 2, overflow_character: Some('…'), ..Default::default() };
            let g = painter.layout_job(job);
            let th = g.size().y;
            painter.galley(pos2(r.left(), y + 16.0), g, pal.fg);
            let mut job = egui::text::LayoutJob::simple(e.author.to_uppercase(), mono(9.5), pal.muted, r.width());
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
        for (x, h) in cols.iter().zip(["NO.", "TITLE", "AUTHOR", "FORMAT", "READ", "OPENED"]) {
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
            clip(cols[1], cols[2]).text(pos2(r.left() + cols[1] + 8.0 * t, y), egui::Align2::LEFT_CENTER, &e.title, ui_m(15.0), ink);
            clip(cols[2], cols[3]).text(pos2(r.left() + cols[2], y), egui::Align2::LEFT_CENTER, &e.author, egui::FontId::proportional(13.0), pal.muted);
            ui.painter().text(pos2(r.left() + cols[3], y), egui::Align2::LEFT_CENTER, &e.format, mono(11.0), pal.muted);
            let pr = if e.finished { "DONE".to_string() } else { format!("{:.0}%", e.fraction * 100.0) };
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
        ui.label(display("Empty shelves.", 72.0));
        ui.add_space(6.0);
        ui.label(RichText::new("EPUB · MOBI · AZW3 · FB2 · PDF · CBZ · TXT").font(mono(12.0)).color(pal.muted));
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            if ui::primary_button(ui, "Import books").clicked() {
                self.import_dialog();
            }
            if text_button(ui, "Add a folder", false).clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.settings.library_dirs.push(d.to_string_lossy().into_owned());
                    self.rescan();
                }
            }
            if self.settings.zlib_enabled && text_button(ui, "Find books on Z-Library →", false).clicked() {
                self.set_view(View::Source);
            }
        });
        ui.add_space(14.0);
        ui.label(tag("Or drop files and folders anywhere on this window.").color(pal.muted));
    }

    fn book_menu(&mut self, ui: &mut egui::Ui, e: &Entry) {
        if ui.button("Read").clicked() {
            self.open_path(Path::new(&e.path));
            ui.close_menu();
        }
        if ui.button("Details").clicked() {
            self.details = Some(e.id.clone());
            ui.close_menu();
        }
        if ui.button(if e.finished { "Mark as unread" } else { "Mark as finished" }).clicked() {
            let fin = !e.finished;
            let mut d = BookData::load(&e.id);
            d.finished = fin;
            if !fin {
                d.fraction = 0.0;
                d.loc = None;
            }
            d.save(&e.id);
            if let Some(x) = self.index.entries.iter_mut().find(|x| x.id == e.id) {
                x.finished = fin;
                if !fin {
                    x.fraction = 0.0;
                }
            }
            self.index.save();
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Show in folder").clicked() {
            crate::platform::reveal(Path::new(&e.path));
            ui.close_menu();
        }
        if ui.button("Open with system app").clicked() {
            crate::platform::open_path(Path::new(&e.path));
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Remove from library").clicked() {
            self.settings.hidden_paths.push(e.path.clone());
            self.settings.extra_files.retain(|f| f != &e.path);
            self.index.entries.retain(|x| x.id != e.id);
            self.index.save();
            ui.close_menu();
        }
        if ui.button(RichText::new("Delete file…").color(ACCENT)).clicked() {
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
        ui.label(display("Notes", 46.0));
        ui.label(tag(format!("{:03} highlights across {:03} books", self.overview.notes.len(), books)).color(pal.muted));
        ui.add_space(8.0);
        ui::rule(ui);
        if self.overview.notes.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("Select text while reading to highlight it or write in the margin.").font(ui_m(17.0)).color(pal.muted));
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
        ui.label(display("Bookmarks", 46.0));
        ui.label(tag(format!("{:03} marked pages", self.overview.bookmarks.len())).color(pal.muted));
        ui.add_space(8.0);
        ui::rule(ui);
        if self.overview.bookmarks.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("Press B while reading to mark a page.").font(ui_m(17.0)).color(pal.muted));
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
        let Some(id) = self.details.clone() else { return };
        let Some(e) = self.index.entries.iter().find(|e| e.id == id).cloned() else {
            self.details = None;
            return;
        };
        let data = BookData::load(&id);
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
                            if text_button(ui, "Close", false).clicked() {
                                self.details = None;
                            }
                        });
                    });
                    ui.label(display(&e.title, 32.0));
                    ui.label(RichText::new(&e.author).font(ui_m(16.0)).color(pal.muted));
                    ui.add_space(12.0);
                    egui::Grid::new("meta").num_columns(2).spacing(vec2(16.0, 6.0)).show(ui, |ui| {
                        let mut row = |k: &str, v: String| {
                            if !v.is_empty() {
                                ui.label(tag(k).color(pal.muted));
                                ui.label(v);
                                ui.end_row();
                            }
                        };
                        row("Series", e.series.clone());
                        row("Publisher", e.publisher.clone());
                        row("Language", e.language.clone());
                        row("Progress", if e.finished { "Finished".into() } else { format!("{:.0}%", e.fraction * 100.0) });
                        let mins = (data.seconds / 60.0).round() as i64;
                        row("Time read", if mins >= 60 { format!("{} h {} min", mins / 60, mins % 60) } else { format!("{mins} min") });
                        row("Days read", if data.daily.is_empty() { String::new() } else { data.daily.len().to_string() });
                        row("Last opened", if e.last_read > 0 { store::relative_time(e.last_read) } else { "never".into() });
                        row("Highlights", if data.annotations.is_empty() { String::new() } else { data.annotations.len().to_string() });
                    });
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui::primary_button(ui, if e.fraction > 0.0 { "Continue" } else { "Start reading" }).clicked() {
                            self.open_path(Path::new(&e.path));
                            self.details = None;
                        }
                        if text_button(ui, "Show in folder", false).clicked() {
                            crate::platform::reveal(Path::new(&e.path));
                        }
                    });
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
                    ui.label(RichText::new(format!("OPENING  {}", trunc(&name, 50).to_uppercase())).font(mono(11.0)));
                });
            });
        }
        if let Some(err) = self.error.clone() {
            let mut dismiss = false;
            bar(ctx, "err", &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("COULDN'T OPEN").font(mono_m(11.0)).color(ACCENT));
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
            egui::Window::new("Delete file?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                if let Some(e) = &e {
                    ui.label(format!("“{}” will be permanently deleted from disk:", e.title));
                    ui.label(RichText::new(&e.path).font(mono(10.5)).color(pal.muted));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("DELETE").font(mono_m(11.5)).color(PAPER)).fill(ACCENT)).clicked() {
                            let _ = std::fs::remove_file(&e.path);
                            let _ = std::fs::remove_file(library::cover_path(&e.id));
                            self.index.entries.retain(|x| x.id != e.id);
                            self.settings.extra_files.retain(|f| f != &e.path);
                            self.index.save();
                            keep = false;
                        }
                        if text_button(ui, "Cancel", false).clicked() {
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
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut rescan = false;
        egui::Window::new("Settings").open(&mut open).collapsible(false).resizable(false).default_width(540.0).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() - 160.0).show(ui, |ui| {
                ui.set_width(520.0);
                ui::section_label(ui, "Appearance");
                ui.horizontal(|ui| {
                    for (t, l) in [(UiTheme::System, "System"), (UiTheme::Light, "Paper"), (UiTheme::Dark, "Ink")] {
                        if text_button(ui, l, self.settings.ui_theme == t).clicked() {
                            self.settings.ui_theme = t;
                        }
                    }
                    ui.separator();
                    ui.checkbox(&mut self.settings.right_panel, "Progress column");
                });
                ui.horizontal(|ui| {
                    ui.label(tag("Cover size").color(pal.muted));
                    ui.add(egui::Slider::new(&mut self.settings.cover_size, 110.0..=260.0).show_value(false));
                });

                ui::section_label(ui, "Library folders");
                ui.horizontal(|ui| {
                    ui.label(tag("Downloads").color(pal.muted));
                    ui.add(egui::Label::new(RichText::new(&self.settings.download_dir).font(mono(11.5))).truncate());
                    if text_button(ui, "Change", false).clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_directory(&self.settings.download_dir).pick_folder() {
                            self.settings.download_dir = d.to_string_lossy().into_owned();
                            rescan = true;
                        }
                    }
                });
                let mut remove = None;
                for (i, d) in self.settings.library_dirs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(tag(format!("{:02}", i + 1)).color(ACCENT));
                        ui.add(egui::Label::new(RichText::new(d).font(mono(11.5))).truncate());
                        if text_button(ui, "Remove", false).clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.settings.library_dirs.remove(i);
                    rescan = true;
                }
                ui.horizontal(|ui| {
                    if text_button(ui, "+ Add folder", false).clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            let s = d.to_string_lossy().into_owned();
                            if !self.settings.library_dirs.contains(&s) {
                                self.settings.library_dirs.push(s);
                            }
                            rescan = true;
                        }
                    }
                    if !self.settings.hidden_paths.is_empty() && text_button(ui, &format!("Restore {} removed", self.settings.hidden_paths.len()), false).clicked() {
                        self.settings.hidden_paths.clear();
                        rescan = true;
                    }
                });

                ui::section_label(ui, "Sources / plugins");
                ui.checkbox(&mut self.settings.zlib_enabled, "Z-Library — blend results into library search");
                ui.horizontal(|ui| {
                    ui.label(tag("Proxy").color(pal.muted));
                    ui.add(egui::TextEdit::singleline(&mut self.settings.proxy).hint_text("socks5://127.0.0.1:9050").desired_width(260.0));
                });

                ui::section_label(ui, "Read aloud");
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut self.settings.tts_rate, 0.5..=2.5).step_by(0.1).suffix("×"));
                    ui.label(tag(match crate::tts::available() {
                        Some(e) => format!("engine: {e}"),
                        None => "no speech engine found".into(),
                    }).color(pal.muted));
                });

                ui::section_label(ui, "Lookup & reading");
                ui.horizontal(|ui| {
                    ui.label(tag("Translate to").color(pal.muted));
                    ui.add(egui::TextEdit::singleline(&mut self.settings.translate_to).desired_width(70.0));
                });
                ui.horizontal(|ui| {
                    ui.label(tag("Reading speed").color(pal.muted));
                    ui.add(egui::Slider::new(&mut self.settings.reading_speed_cpm, 300.0..=4000.0).suffix(" chars/min"));
                });

                ui::section_label(ui, "Progress sync — KOReader compatible");
                ui.checkbox(&mut self.settings.kosync.enabled, "Sync reading progress");
                egui::Grid::new("ks").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                    ui.label(tag("Server").color(pal.muted));
                    ui.add(egui::TextEdit::singleline(&mut self.settings.kosync.server).desired_width(300.0));
                    ui.end_row();
                    ui.label(tag("Username").color(pal.muted));
                    ui.add(egui::TextEdit::singleline(&mut self.settings.kosync.username).desired_width(200.0));
                    ui.end_row();
                    ui.label(tag("Password").color(pal.muted));
                    ui.add(egui::TextEdit::singleline(&mut self.kosync_password).password(true).desired_width(200.0).hint_text(if self.settings.kosync.userkey.is_empty() { "" } else { "(saved)" }));
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    let mk = |s: &mut App| {
                        if !s.kosync_password.is_empty() {
                            s.settings.kosync.userkey = crate::kosync::md5_hex(&s.kosync_password);
                            s.kosync_password.clear();
                        }
                        (s.settings.kosync.clone(), s.settings.proxy_opt(), s.tx.clone(), s.ctx.clone())
                    };
                    if text_button(ui, "Log in", false).clicked() {
                        let (cfg, proxy, tx, ctx) = mk(self);
                        self.kosync_status = "Checking…".into();
                        std::thread::spawn(move || {
                            let _ = tx.send(Msg::Kosync(crate::kosync::auth(&cfg, &proxy).map(|_| "Logged in".to_string())));
                            ctx.request_repaint();
                        });
                    }
                    if text_button(ui, "Register", false).clicked() {
                        let (cfg, proxy, tx, ctx) = mk(self);
                        self.kosync_status = "Registering…".into();
                        std::thread::spawn(move || {
                            let _ = tx.send(Msg::Kosync(crate::kosync::register(&cfg, &proxy).map(|_| "Account created".to_string())));
                            ctx.request_repaint();
                        });
                    }
                    ui.label(tag(&self.kosync_status).color(ACCENT));
                });

                ui::section_label(ui, "About");
                ui.label(tag(format!("zlib-reader {} · {}", env!("CARGO_PKG_VERSION"), store::config_dir().display())).color(pal.muted));
                ui.label(RichText::new("← → page · T toolbar · C contents · B bookmark · Ctrl+K search · F fullscreen · Ctrl ± size · Backspace back · Esc close").font(mono(10.5)).color(pal.muted));
            });
        });
        if rescan {
            self.rescan();
        }
        if !open {
            self.settings_open = false;
            self.settings.save();
        }
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
