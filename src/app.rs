//! Application shell: library (grid/list, search, filters, sort, import,
//! drag & drop), book details, settings, Z-Library tab, and the reader.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Rect, RichText, Sense, Stroke};

use crate::book::Book;
use crate::fonts::{self, FontDb, Installed};
use crate::library::{self, Entry, Index};
use crate::online::Online;
use crate::reader::{Action, Env, Reader};
use crate::store::{self, AppSettings, LibView, SortKey, Theme, UiTheme};
use crate::texload::{Src, TexLoader};
use crate::ui::{self, chip, icon_button, icon_toggle, Icon, Palette, ACCENT};

enum Msg {
    Scanned(Index),
    Opened(Result<(String, PathBuf, Arc<Book>), String>),
    Fonts(FontDb),
    Kosync(Result<String, String>),
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Library,
    Online,
}

#[derive(PartialEq, Clone, Copy)]
enum Filter {
    All,
    Reading,
    Unread,
    Finished,
}

pub struct App {
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    settings: AppSettings,
    saved_settings: String,
    last_settings_check: Instant,
    index: Index,
    scanning: bool,
    scan_done: Arc<AtomicUsize>,
    scan_total: Arc<AtomicUsize>,
    rescan_after: bool,
    tab: Tab,
    query: String,
    filter: Filter,
    format: String,
    covers: TexLoader,
    reader: Option<Reader>,
    opening: Option<String>,
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
            scanning: false,
            scan_done: Arc::new(AtomicUsize::new(0)),
            scan_total: Arc::new(AtomicUsize::new(0)),
            rescan_after: false,
            tab: Tab::Library,
            query: String::new(),
            filter: Filter::All,
            format: String::new(),
            covers: TexLoader::new(&ctx, 2),
            reader: None,
            opening: None,
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
        app.rescan();
        if let Some(p) = app.pending_open.take() {
            app.open_path(&p);
        }
        app
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

    fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Scanned(idx) => {
                    self.scanning = false;
                    self.index = idx;
                    self.index.save();
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
                            let r = Reader::new(&self.ctx, id.clone(), path.clone(), book, &self.settings);
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
            self.covers.forget(&r.id);
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Title("zlib-reader".into()));
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    fn library_palette(&self, ctx: &egui::Context) -> Palette {
        let dark = match self.settings.ui_theme {
            UiTheme::Dark => true,
            UiTheme::Light => false,
            UiTheme::System => ctx.input(|i| i.raw.system_theme) != Some(egui::Theme::Light),
        };
        let mut p = ui::palette_for(if dark { Theme::Dark } else { Theme::Light }, [0; 3], [0; 3]);
        if dark {
            p.bg = Color32::from_rgb(24, 24, 27);
            p.panel = Color32::from_rgb(32, 32, 36);
            p.hover = Color32::from_rgb(44, 44, 50);
            p.border = Color32::from_rgb(52, 52, 58);
        } else {
            p.bg = Color32::from_rgb(247, 247, 248);
            p.panel = Color32::WHITE;
            p.hover = Color32::from_rgb(236, 236, 240);
            p.border = Color32::from_rgb(226, 226, 230);
        }
        p
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

    fn handle_drops(&mut self, ctx: &egui::Context, pal: &Palette) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            let r = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, "drop".into()));
            painter.rect_filled(r, CornerRadius::ZERO, Color32::from_black_alpha(120));
            painter.rect_stroke(r.shrink(18.0), CornerRadius::same(16), Stroke::new(3.0, ACCENT), egui::StrokeKind::Inside);
            painter.text(r.center(), egui::Align2::CENTER_CENTER, "Drop books to add them to your library", egui::FontId::proportional(24.0), Color32::WHITE);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if dropped.is_empty() {
            return;
        }
        let _ = pal;
        let books: Vec<PathBuf> = dropped.into_iter().filter(|p| crate::book::is_supported(p) || p.is_dir()).collect();
        let mut files = Vec::new();
        for p in books {
            if p.is_dir() {
                let s = p.to_string_lossy().into_owned();
                if !self.settings.library_dirs.contains(&s) {
                    self.settings.library_dirs.push(s);
                }
            } else {
                files.push(p);
            }
        }
        if files.len() == 1 {
            let f = files.remove(0);
            self.open_path(&f);
        } else {
            for f in files {
                let s = f.to_string_lossy().into_owned();
                if !self.settings.extra_files.contains(&s) {
                    self.settings.extra_files.push(s.clone());
                }
                self.settings.hidden_paths.retain(|h| h != &s);
            }
        }
        self.rescan();
    }

    fn import_dialog(&mut self) {
        let exts: Vec<&str> = crate::book::SUPPORTED.to_vec();
        if let Some(files) = rfd::FileDialog::new().add_filter("Books", &exts).pick_files() {
            if files.len() == 1 {
                self.open_path(&files[0]);
                return;
            }
            for f in files {
                let s = f.to_string_lossy().into_owned();
                if !self.settings.extra_files.contains(&s) {
                    self.settings.extra_files.push(s.clone());
                }
                self.settings.hidden_paths.retain(|h| h != &s);
            }
            self.rescan();
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
        for (_, p) in std::mem::take(&mut self.online.downloaded) {
            let _ = p;
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
            self.handle_drops(ctx, &pal);
            return;
        }

        let pal = self.library_palette(ctx);
        self.apply_style(ctx, &pal);
        if ctx.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.command) {
            self.import_dialog();
        }
        self.top_bar(ctx, &pal);
        match self.tab {
            Tab::Library => self.library_ui(ctx, &pal),
            Tab::Online => {
                if let Some(p) = self.online.ui(ctx, &mut self.settings, &pal) {
                    self.open_path(&p);
                }
            }
        }
        self.details_window(ctx, &pal);
        self.settings_window(ctx, &pal);
        self.status_overlays(ctx, &pal);
        self.handle_drops(ctx, &pal);
    }
}

impl App {
    /// `ZR_SHOT=out.png` (debug aid): save a screenshot after startup settles, then quit.
    /// `ZR_KEYS=Right,Right,t` presses keys first.
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
                        let pos = egui::pos2(v[0], v[1]);
                        ctx.input_mut(|inp| {
                            inp.events.push(egui::Event::PointerMoved(pos));
                            inp.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
                            inp.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
                        });
                    }
                } else if let Some(k) = egui::Key::from_name(name) {
                    let modifiers = if std::env::var("ZR_CTRL").is_ok() { egui::Modifiers::COMMAND } else { Default::default() };
                    ctx.input_mut(|inp| inp.events.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }));
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

    fn top_bar(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::TopBottomPanel::top("top").frame(egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin::symmetric(14, 8)).stroke(Stroke::new(1.0, pal.border))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(7), ACCENT);
                ui::paint_icon(ui.painter(), r.shrink(5.0), Icon::Book, Color32::WHITE);
                ui.label(RichText::new("zlib-reader").strong().size(16.0));
                ui.add_space(12.0);
                if chip(ui, self.tab == Tab::Library, "Library").clicked() {
                    self.tab = Tab::Library;
                }
                if chip(ui, self.tab == Tab::Online, "Z-Library").clicked() {
                    self.tab = Tab::Online;
                }
                ui.add_space(12.0);
                if self.tab == Tab::Library {
                    let w = (ui.available_width() - 190.0).clamp(120.0, 520.0);
                    ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search title, author, series…").desired_width(w).margin(vec2(10.0, 6.0)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, Icon::Gear, "Settings").clicked() {
                        self.settings_open = !self.settings_open;
                    }
                    if self.tab == Tab::Library {
                        let r = icon_button(ui, Icon::Sort, "Sort & view");
                        let pid = ui.make_persistent_id("sort-menu");
                        if r.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(pid));
                        }
                        egui::popup_below_widget(ui, pid, &r, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                            ui.set_min_width(190.0);
                            ui.label(RichText::new("Sort by").small().color(pal.muted));
                            for (k, l) in [(SortKey::Recent, "Recently read"), (SortKey::Added, "Date added"), (SortKey::Title, "Title"), (SortKey::Author, "Author"), (SortKey::Progress, "Progress"), (SortKey::Size, "File size")] {
                                if ui.selectable_label(self.settings.sort == k, l).clicked() {
                                    if self.settings.sort == k {
                                        self.settings.sort_desc = !self.settings.sort_desc;
                                    } else {
                                        self.settings.sort = k;
                                        self.settings.sort_desc = matches!(k, SortKey::Recent | SortKey::Added | SortKey::Progress | SortKey::Size);
                                    }
                                }
                            }
                            ui.checkbox(&mut self.settings.sort_desc, "Descending");
                            ui.separator();
                            ui.label(RichText::new("Cover size").small().color(pal.muted));
                            ui.add(egui::Slider::new(&mut self.settings.cover_size, 100.0..=240.0).show_value(false));
                        });
                        let grid = self.settings.lib_view == LibView::Grid;
                        if icon_toggle(ui, if grid { Icon::List } else { Icon::Grid }, if grid { "List view" } else { "Grid view" }, false).clicked() {
                            self.settings.lib_view = if grid { LibView::List } else { LibView::Grid };
                        }
                        if icon_button(ui, Icon::Plus, "Import books (Ctrl+O)").clicked() {
                            self.import_dialog();
                        }
                        let spin = self.scanning;
                        if icon_toggle(ui, Icon::Refresh, "Rescan library folders", spin).clicked() {
                            self.rescan();
                        }
                    }
                });
            });
        });
    }

    fn visible_entries(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.index.entries.len())
            .filter(|&i| {
                let e = &self.index.entries[i];
                library::matches(e, &self.query)
                    && (self.format.is_empty() || e.format == self.format)
                    && match self.filter {
                        Filter::All => true,
                        Filter::Reading => e.fraction > 0.0 && !e.finished,
                        Filter::Unread => e.fraction == 0.0 && !e.finished,
                        Filter::Finished => e.finished,
                    }
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

    fn paint_cover(&mut self, ui: &egui::Ui, rect: Rect, e: &Entry, shadow: bool) {
        let painter = ui.painter();
        if shadow {
            painter.rect_filled(rect.translate(vec2(0.0, 3.0)).expand(1.0), CornerRadius::same(7), Color32::from_black_alpha(40));
        }
        match self.cover_tex(e) {
            Some(id) => {
                let img = egui::Image::new((id, rect.size())).corner_radius(6);
                img.paint_at(ui, rect);
            }
            None => {
                let painter = ui.painter().with_clip_rect(rect);
                ui::paint_placeholder_cover(&painter, rect, &e.title, &e.author);
            }
        }
        if e.finished {
            let c = rect.right_top() + vec2(-14.0, 14.0);
            ui.painter().circle_filled(c, 10.0, Color32::from_rgb(34, 160, 90));
            ui.painter().text(c, egui::Align2::CENTER_CENTER, "✓", egui::FontId::proportional(13.0), Color32::WHITE);
        }
    }

    fn library_ui(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 22, right: 10, top: 14, bottom: 6 })).show(ctx, |ui| {
            if self.index.entries.is_empty() {
                self.empty_state(ui, pal);
                return;
            }
            // continue reading
            let recent = self.index.entries.iter().filter(|e| e.fraction > 0.0 && !e.finished && e.last_read > 0).max_by_key(|e| e.last_read).cloned();
            if let (Some(e), true) = (recent, self.query.is_empty() && self.filter == Filter::All) {
                egui::Frame::new().fill(pal.panel).corner_radius(14).stroke(Stroke::new(1.0, pal.border)).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                    ui.set_width(ui.available_width() - 14.0);
                    ui.horizontal(|ui| {
                        let (r, resp) = ui.allocate_exact_size(vec2(64.0, 96.0), Sense::click());
                        self.paint_cover(ui, r, &e, true);
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new("CONTINUE READING").size(11.0).strong().color(ACCENT));
                            ui.add(egui::Label::new(RichText::new(&e.title).size(19.0).strong()).truncate());
                            ui.label(RichText::new(&e.author).color(pal.muted));
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add(egui::ProgressBar::new(e.fraction).desired_width(220.0).desired_height(6.0).fill(ACCENT));
                                ui.label(RichText::new(format!("{:.0}% · {}", e.fraction * 100.0, store::relative_time(e.last_read))).size(12.0).color(pal.muted));
                            });
                            if ui::primary_button(ui, "Resume").clicked() || resp.clicked() {
                                self.open_path(Path::new(&e.path));
                            }
                        });
                    });
                });
                ui.add_space(12.0);
            }
            // filter chips
            let counts = |f: Filter, es: &[Entry]| {
                es.iter()
                    .filter(|e| match f {
                        Filter::All => true,
                        Filter::Reading => e.fraction > 0.0 && !e.finished,
                        Filter::Unread => e.fraction == 0.0 && !e.finished,
                        Filter::Finished => e.finished,
                    })
                    .count()
            };
            ui.horizontal(|ui| {
                for (f, l) in [(Filter::All, "All"), (Filter::Reading, "Reading"), (Filter::Unread, "Unread"), (Filter::Finished, "Finished")] {
                    if chip(ui, self.filter == f, &format!("{l}  {}", counts(f, &self.index.entries))).clicked() {
                        self.filter = f;
                    }
                }
                let mut formats: Vec<String> = self.index.entries.iter().map(|e| e.format.clone()).collect();
                formats.sort();
                formats.dedup();
                if formats.len() > 1 {
                    egui::ComboBox::from_id_salt("fmt").selected_text(if self.format.is_empty() { "All formats".to_string() } else { self.format.clone() }).width(110.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.format, String::new(), "All formats");
                        for f in formats {
                            ui.selectable_value(&mut self.format, f.clone(), f);
                        }
                    });
                }
                if self.scanning {
                    ui.spinner();
                    let (d, t) = (self.scan_done.load(Ordering::Relaxed), self.scan_total.load(Ordering::Relaxed));
                    ui.label(RichText::new(if t > 0 { format!("Scanning {d}/{t}") } else { "Scanning…".into() }).size(12.0).color(pal.muted));
                }
            });
            ui.add_space(8.0);
            let items = self.visible_entries();
            if items.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(RichText::new("No books match.").color(pal.muted));
                });
                return;
            }
            match self.settings.lib_view {
                LibView::Grid => self.grid(ui, pal, &items),
                LibView::List => self.list(ui, pal, &items),
            }
        });
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.18);
            let (r, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
            ui::paint_icon(ui.painter(), r, Icon::Library, pal.muted);
            ui.add_space(10.0);
            ui.label(RichText::new("Your library is empty").size(22.0).strong());
            ui.label(RichText::new("Import EPUB, MOBI, AZW3, FB2, PDF, CBZ or TXT files, add a folder, or find books on Z-Library.").color(pal.muted));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 420.0).max(0.0) / 2.0);
                if ui::primary_button(ui, "Import books").clicked() {
                    self.import_dialog();
                }
                if ui.button("Add folder").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.settings.library_dirs.push(d.to_string_lossy().into_owned());
                        self.rescan();
                    }
                }
                if ui.button("Browse Z-Library").clicked() {
                    self.tab = Tab::Online;
                }
            });
            ui.add_space(10.0);
            ui.label(RichText::new("Tip: drag and drop files or folders onto this window.").size(12.0).color(pal.muted));
            if self.scanning {
                ui.add_space(10.0);
                ui.spinner();
            }
        });
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
            let mut d = store::BookData::load(&e.id);
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
        if ui.button(RichText::new("Delete file…").color(Color32::from_rgb(220, 70, 60))).clicked() {
            self.confirm_delete = Some(e.id.clone());
            ui.close_menu();
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui, pal: &Palette, items: &[usize]) {
        let cw = self.settings.cover_size;
        let ch = cw * 1.5;
        let card_h = ch + 58.0;
        let gap = 22.0;
        let avail = ui.available_width() - 12.0;
        let cols = ((avail + gap) / (cw + gap)).floor().max(1.0) as usize;
        let rows = items.len().div_ceil(cols);
        let gap_x = if cols > 1 { ((avail - cols as f32 * cw) / (cols as f32 - 1.0)).min(gap * 2.0) } else { 0.0 };
        let mut open = None;
        let mut menu_for: Option<(Entry, egui::Response)> = None;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, card_h + gap, rows, |ui, range| {
            for row in range {
                let (row_rect, _) = ui.allocate_exact_size(vec2(avail, card_h + gap), Sense::hover());
                for c in 0..cols {
                    let Some(&ei) = items.get(row * cols + c) else { break };
                    let e = self.index.entries[ei].clone();
                    let x = row_rect.left() + c as f32 * (cw + gap_x);
                    let card = Rect::from_min_size(pos2(x, row_rect.top()), vec2(cw, card_h));
                    let resp = ui.interact(card, ui.id().with(("card", &e.id)), Sense::click());
                    let lift = if resp.hovered() { -3.0 } else { 0.0 };
                    let cover = Rect::from_min_size(card.min + vec2(0.0, lift), vec2(cw, ch));
                    self.paint_cover(ui, cover, &e, true);
                    if e.fraction > 0.0 && !e.finished {
                        let bar = Rect::from_min_size(pos2(cover.left() + 6.0, cover.bottom() - 9.0), vec2(cw - 12.0, 4.0));
                        ui.painter().rect_filled(bar, CornerRadius::same(2), Color32::from_black_alpha(110));
                        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * e.fraction.clamp(0.02, 1.0), 4.0)), CornerRadius::same(2), Color32::WHITE);
                    }
                    let fmt_g = ui.painter().layout_no_wrap(e.format.clone(), egui::FontId::proportional(9.5), Color32::WHITE);
                    let badge = Rect::from_min_size(cover.left_top() + vec2(6.0, 6.0), fmt_g.size() + vec2(8.0, 4.0));
                    ui.painter().rect_filled(badge, CornerRadius::same(4), Color32::from_black_alpha(140));
                    ui.painter().galley(badge.min + vec2(4.0, 2.0), fmt_g, Color32::WHITE);
                    let tg = ui.painter().layout(e.title.clone(), egui::FontId::proportional(13.5), pal.fg, cw);
                    let tg = if tg.rows.len() > 2 {
                        let mut job = egui::text::LayoutJob::simple(e.title.clone(), egui::FontId::proportional(13.5), pal.fg, cw);
                        job.wrap.max_rows = 2;
                        job.wrap.overflow_character = Some('…');
                        ui.painter().layout_job(job)
                    } else {
                        tg
                    };
                    let th = tg.size().y;
                    ui.painter().galley(pos2(card.left(), cover.bottom() + 8.0), tg, pal.fg);
                    let mut job = egui::text::LayoutJob::simple(e.author.clone(), egui::FontId::proportional(12.0), pal.muted, cw);
                    job.wrap.max_rows = 1;
                    job.wrap.overflow_character = Some('…');
                    let ag = ui.painter().layout_job(job);
                    ui.painter().galley(pos2(card.left(), cover.bottom() + 10.0 + th), ag, pal.muted);
                    let resp = resp.on_hover_text(format!("{}\n{}{}", e.title, e.author, if e.fraction > 0.0 { format!("\n{:.0}% read", e.fraction * 100.0) } else { String::new() }));
                    if resp.clicked() {
                        open = Some(e.path.clone());
                    }
                    if resp.secondary_clicked() || resp.context_menu_opened() {
                        menu_for = Some((e.clone(), resp.clone()));
                    }
                    resp.context_menu(|ui| self.book_menu(ui, &e));
                }
            }
        });
        let _ = menu_for;
        if let Some(p) = open {
            self.open_path(Path::new(&p));
        }
    }

    fn list(&mut self, ui: &mut egui::Ui, pal: &Palette, items: &[usize]) {
        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, 64.0, items.len(), |ui, range| {
            for &ei in &items[range] {
                let e = self.index.entries[ei].clone();
                let w = ui.available_width() - 12.0;
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 60.0), Sense::click());
                ui.add_space(4.0);
                if resp.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(8), pal.hover);
                }
                let cover = Rect::from_min_size(rect.min + vec2(8.0, 5.0), vec2(34.0, 50.0));
                self.paint_cover(ui, cover, &e, false);
                let x = cover.right() + 14.0;
                let right_w = 260.0;
                let tw = (rect.right() - x - right_w).max(100.0);
                let mut job = egui::text::LayoutJob::simple(e.title.clone(), egui::FontId::proportional(14.5), pal.fg, tw);
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                let g = ui.painter().layout_job(job);
                ui.painter().galley(pos2(x, rect.top() + 11.0), g, pal.fg);
                ui.painter().text(pos2(x, rect.top() + 33.0), egui::Align2::LEFT_TOP, &e.author, egui::FontId::proportional(12.5), pal.muted);
                let rx = rect.right() - right_w;
                ui.painter().text(pos2(rx, rect.center().y), egui::Align2::LEFT_CENTER, format!("{} · {}", e.format, library::fmt_size(e.size)), egui::FontId::proportional(12.0), pal.muted);
                let status = if e.finished { "Finished".to_string() } else if e.fraction > 0.0 { format!("{:.0}%", e.fraction * 100.0) } else { "New".to_string() };
                ui.painter().text(pos2(rx + 120.0, rect.center().y), egui::Align2::LEFT_CENTER, status, egui::FontId::proportional(12.0), if e.fraction > 0.0 && !e.finished { ACCENT } else { pal.muted });
                ui.painter().text(pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, if e.last_read > 0 { store::relative_time(e.last_read) } else { String::new() }, egui::FontId::proportional(12.0), pal.muted);
                if resp.clicked() {
                    open = Some(e.path.clone());
                }
                resp.context_menu(|ui| self.book_menu(ui, &e));
            }
        });
        if let Some(p) = open {
            self.open_path(Path::new(&p));
        }
    }

    fn details_window(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(id) = self.details.clone() else { return };
        let Some(e) = self.index.entries.iter().find(|e| e.id == id).cloned() else {
            self.details = None;
            return;
        };
        let mut open = true;
        let data = store::BookData::load(&id);
        egui::Window::new("Book details").open(&mut open).collapsible(false).resizable(false).default_width(560.0).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(150.0, 225.0), Sense::hover());
                self.paint_cover(ui, r, &e, true);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.set_width(360.0);
                    ui.label(RichText::new(&e.title).size(20.0).strong());
                    ui.label(RichText::new(&e.author).color(pal.muted));
                    ui.add_space(8.0);
                    egui::Grid::new("meta").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                        let mut row = |k: &str, v: String| {
                            if !v.is_empty() {
                                ui.label(RichText::new(k).color(pal.muted));
                                ui.label(v);
                                ui.end_row();
                            }
                        };
                        row("Series", e.series.clone());
                        row("Publisher", e.publisher.clone());
                        row("Language", e.language.clone());
                        row("Format", format!("{} · {}", e.format, library::fmt_size(e.size)));
                        row("Progress", if e.finished { "Finished".into() } else { format!("{:.0}%", e.fraction * 100.0) });
                        let mins = (data.seconds / 60.0).round() as i64;
                        row("Time read", if mins >= 60 { format!("{} h {} min", mins / 60, mins % 60) } else { format!("{mins} min") });
                        row("Days read", if data.daily.is_empty() { String::new() } else { data.daily.len().to_string() });
                        row("Last opened", if e.last_read > 0 { store::relative_time(e.last_read) } else { "never".into() });
                        row("Highlights", if data.annotations.is_empty() { String::new() } else { data.annotations.len().to_string() });
                        row("Bookmarks", if data.bookmarks.is_empty() { String::new() } else { data.bookmarks.len().to_string() });
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui::primary_button(ui, if e.fraction > 0.0 { "Continue reading" } else { "Start reading" }).clicked() {
                            self.open_path(Path::new(&e.path));
                            self.details = None;
                        }
                        if ui.button("Show in folder").clicked() {
                            crate::platform::reveal(Path::new(&e.path));
                        }
                    });
                });
            });
            if !e.description.is_empty() {
                ui.separator();
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    ui.label(RichText::new(&e.description).size(13.5));
                });
            }
            ui.add_space(4.0);
            ui.label(RichText::new(&e.path).size(11.0).color(pal.muted));
        });
        if !open {
            self.details = None;
        }
    }

    fn status_overlays(&mut self, ctx: &egui::Context, pal: &Palette) {
        if let Some(name) = &self.opening {
            egui::Area::new("opening".into()).anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -30.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).fill(pal.panel).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Opening {name}…"));
                    });
                });
            });
        }
        if let Some(err) = self.error.clone() {
            egui::Area::new("err".into()).anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -30.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).fill(pal.panel).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(Color32::from_rgb(220, 70, 60), format!("Couldn't open book: {err}"));
                        if ui.small_button("Dismiss").clicked() {
                            self.error = None;
                        }
                    });
                });
            });
        }
        if let Some(id) = self.confirm_delete.clone() {
            let e = self.index.entries.iter().find(|e| e.id == id).cloned();
            let mut keep = true;
            egui::Window::new("Delete file?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                if let Some(e) = &e {
                    ui.label(format!("“{}” will be permanently deleted from disk:", e.title));
                    ui.label(RichText::new(&e.path).size(11.5).color(pal.muted));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Delete").color(Color32::WHITE)).fill(Color32::from_rgb(210, 60, 50))).clicked() {
                            let _ = std::fs::remove_file(&e.path);
                            let _ = std::fs::remove_file(library::cover_path(&e.id));
                            self.index.entries.retain(|x| x.id != e.id);
                            self.settings.extra_files.retain(|f| f != &e.path);
                            self.index.save();
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
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
        egui::Window::new("Settings").open(&mut open).collapsible(false).resizable(false).default_width(520.0).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() - 160.0).show(ui, |ui| {
                ui.set_width(500.0);
                ui::section_label(ui, "Appearance");
                ui.horizontal(|ui| {
                    ui.label("Theme");
                    ui.selectable_value(&mut self.settings.ui_theme, UiTheme::System, "System");
                    ui.selectable_value(&mut self.settings.ui_theme, UiTheme::Light, "Light");
                    ui.selectable_value(&mut self.settings.ui_theme, UiTheme::Dark, "Dark");
                });

                ui::section_label(ui, "Library folders");
                ui.horizontal(|ui| {
                    ui.label("Downloads:");
                    ui.add(egui::Label::new(RichText::new(&self.settings.download_dir).monospace().size(12.0)).truncate());
                    if ui.small_button("Change…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_directory(&self.settings.download_dir).pick_folder() {
                            self.settings.download_dir = d.to_string_lossy().into_owned();
                            rescan = true;
                        }
                    }
                });
                let mut remove = None;
                for (i, d) in self.settings.library_dirs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui::paint_icon(ui.painter(), Rect::from_min_size(ui.cursor().min, vec2(18.0, 18.0)), Icon::Folder, pal.muted);
                        ui.add_space(22.0);
                        ui.add(egui::Label::new(RichText::new(d).monospace().size(12.0)).truncate());
                        if ui.small_button("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.settings.library_dirs.remove(i);
                    rescan = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Add folder…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            let s = d.to_string_lossy().into_owned();
                            if !self.settings.library_dirs.contains(&s) {
                                self.settings.library_dirs.push(s);
                            }
                            rescan = true;
                        }
                    }
                    if !self.settings.hidden_paths.is_empty() && ui.button(format!("Restore {} removed books", self.settings.hidden_paths.len())).clicked() {
                        self.settings.hidden_paths.clear();
                        rescan = true;
                    }
                });
                ui.label(RichText::new("Folders are scanned recursively. Books opened from elsewhere are added automatically.").size(12.0).color(pal.muted));

                ui::section_label(ui, "Read aloud");
                ui.horizontal(|ui| {
                    ui.label("Speed");
                    ui.add(egui::Slider::new(&mut self.settings.tts_rate, 0.5..=2.5).step_by(0.1).suffix("×"));
                    ui.label(RichText::new(match crate::tts::available() {
                        Some(e) => format!("engine: {e}"),
                        None => "no speech engine found".into(),
                    }).size(12.0).color(pal.muted));
                });

                ui::section_label(ui, "Lookup & translation");
                ui.horizontal(|ui| {
                    ui.label("Translate to");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.translate_to).desired_width(70.0));
                    ui.label(RichText::new("language code, e.g. en, de, ja").size(12.0).color(pal.muted));
                });
                ui.horizontal(|ui| {
                    ui.label("Reading speed");
                    ui.add(egui::Slider::new(&mut self.settings.reading_speed_cpm, 300.0..=4000.0).suffix(" chars/min"));
                });
                ui.label(RichText::new("Used for “time left” estimates; adapts automatically as you read.").size(12.0).color(pal.muted));

                ui::section_label(ui, "Progress sync (KOReader-compatible)");
                ui.checkbox(&mut self.settings.kosync.enabled, "Sync reading progress");
                egui::Grid::new("ks").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                    ui.label("Server");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.kosync.server).desired_width(300.0));
                    ui.end_row();
                    ui.label("Username");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.kosync.username).desired_width(200.0));
                    ui.end_row();
                    ui.label("Password");
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
                    if ui.button("Log in").clicked() {
                        let (cfg, proxy, tx, ctx) = mk(self);
                        self.kosync_status = "Checking…".into();
                        std::thread::spawn(move || {
                            let _ = tx.send(Msg::Kosync(crate::kosync::auth(&cfg, &proxy).map(|_| "Logged in ✓".to_string())));
                            ctx.request_repaint();
                        });
                    }
                    if ui.button("Register").clicked() {
                        let (cfg, proxy, tx, ctx) = mk(self);
                        self.kosync_status = "Registering…".into();
                        std::thread::spawn(move || {
                            let _ = tx.send(Msg::Kosync(crate::kosync::register(&cfg, &proxy).map(|_| "Account created ✓".to_string())));
                            ctx.request_repaint();
                        });
                    }
                    ui.label(&self.kosync_status);
                });
                ui.label(RichText::new("Progress is pulled when you open a book and pushed when you close it. Works with KOReader and other kosync clients.").size(12.0).color(pal.muted));

                ui::section_label(ui, "Network");
                ui.horizontal(|ui| {
                    ui.label("Proxy");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.proxy).hint_text("socks5://127.0.0.1:9050").desired_width(260.0));
                });

                ui::section_label(ui, "About");
                ui.label(RichText::new(format!("zlib-reader {} · data in {}", env!("CARGO_PKG_VERSION"), store::config_dir().display())).size(12.0).color(pal.muted));
                ui.label(RichText::new("Shortcuts: ←/→ Space PgUp/PgDn turn pages · T contents · Ctrl+F search · Ctrl+B bookmark · F11 fullscreen · Ctrl +/− font size · Backspace back · Esc close").size(12.0).color(pal.muted));
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
