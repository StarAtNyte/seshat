//! Z-Library tab: mirror discovery, sign-in/registration, search, details, downloads.

use crate::i18n::tr;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense};

use crate::api;
use crate::store::{self, AppSettings};
use crate::texload::{Src, TexLoader};
use crate::ui::{self, Palette};

enum Msg {
    ProbeDone(Vec<(String, bool)>, Option<String>),
    LoginDone(Result<api::Session, String>),
    RegisterDone(Result<(), String>),
    ConfirmDone(Result<api::Session, String>),
    ResendDone(Result<(), String>),
    SearchDone(Result<api::SearchPage, String>),
    DetailsDone(Result<api::Book, String>),
    DlProgress(u64, Option<u64>),
    DlDone(Result<(String, PathBuf), String>),
    Quiet(u64, Result<api::SearchPage, String>),
    CoverFetched(String, bool),
    Quota(Result<(u32, u32), String>),
}

#[derive(PartialEq, Clone, Copy)]
enum AuthMode {
    SignIn,
    Register,
}

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Connecting,
    Login,
    Search,
}

pub struct Online {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    screen: Screen,
    session: Option<api::Session>,
    probe_results: Vec<(String, bool)>,
    probing: bool,
    started: bool,
    email: String,
    password: String,
    cookie_userid: String,
    cookie_userkey: String,
    login_error: String,
    busy: bool,
    use_cookies: bool,
    manual_domain: String,
    auth_mode: AuthMode,
    reg_nickname: String,
    reg_code: String,
    code_sent: bool,
    reg_info: String,
    query: String,
    filters: api::Filters,
    results: api::SearchPage,
    searching: bool,
    search_error: String,
    page: u32,
    selected: Option<api::Book>,
    details: Option<Result<api::Book, String>>,
    fetching_details: bool,
    active_dl: Option<(String, u64, Option<u64>)>,
    dl_error: String,
    /// Finished downloads (title, path) for the app to pick up.
    pub downloaded: Vec<(String, PathBuf)>,
    /// Book ids whose cover was replaced from the catalogue.
    pub covers_updated: Vec<String>,
    pub recent: Vec<(String, PathBuf)>,
    covers: TexLoader,
    quota: Option<(u32, u32)>,
    refresh_quota_after: bool,
    quota_asked: bool,
    dl_started: std::time::Instant,
    dl_book: Option<String>,
    quiet_q: String,
    quiet_sent: String,
    quiet_changed: std::time::Instant,
    quiet_gen: u64,
    quiet_busy: bool,
    quiet: Option<Result<Vec<api::Book>, String>>,
}

impl Online {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Online {
            tx,
            rx,
            ctx: ctx.clone(),
            screen: Screen::Connecting,
            session: store::load_session(),
            probe_results: Vec::new(),
            probing: false,
            started: false,
            email: String::new(),
            password: String::new(),
            cookie_userid: String::new(),
            cookie_userkey: String::new(),
            login_error: String::new(),
            busy: false,
            use_cookies: false,
            manual_domain: String::new(),
            auth_mode: AuthMode::SignIn,
            reg_nickname: String::new(),
            reg_code: String::new(),
            code_sent: false,
            reg_info: String::new(),
            query: String::new(),
            filters: api::Filters::default(),
            results: api::SearchPage::default(),
            searching: false,
            search_error: String::new(),
            page: 1,
            selected: None,
            details: None,
            fetching_details: false,
            active_dl: None,
            dl_error: String::new(),
            downloaded: Vec::new(),
            covers_updated: Vec::new(),
            recent: Vec::new(),
            covers: TexLoader::new(ctx, 3),
            quota: None,
            refresh_quota_after: false,
            quota_asked: false,
            dl_started: std::time::Instant::now(),
            dl_book: None,
            quiet_q: String::new(),
            quiet_sent: String::new(),
            quiet_changed: std::time::Instant::now(),
            quiet_gen: 0,
            quiet_busy: false,
            quiet: None,
        }
    }

    pub fn is_downloading(&self) -> bool {
        self.active_dl.is_some()
    }

    fn spawn(&self, f: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    /// Lazily connect the first time the tab is shown.
    fn start(&mut self, app: &AppSettings) {
        if self.started {
            return;
        }
        self.started = true;
        if let Some(s) = &self.session {
            self.screen = Screen::Search;
            let d = s.domain.clone();
            let proxy = app.proxy_opt();
            self.spawn(move || {
                let ok = api::probe(&d, &proxy);
                Msg::ProbeDone(vec![(d.clone(), ok)], ok.then_some(d))
            });
        } else {
            self.start_probe(app);
        }
    }

    fn start_probe(&mut self, app: &AppSettings) {
        self.screen = Screen::Connecting;
        self.probing = true;
        self.probe_results.clear();
        let proxy = app.proxy_opt();
        let mut all: Vec<String> = Vec::new();
        let md = self.manual_domain.trim().to_string();
        if !md.is_empty() {
            all.push(api::normalize(&md));
        }
        all.extend(api::DEFAULT_DOMAINS.iter().map(|s| s.to_string()));
        self.spawn(move || {
            let handles: Vec<_> = all
                .into_iter()
                .map(|d| {
                    let proxy = proxy.clone();
                    std::thread::spawn(move || {
                        let ok = api::probe(&d, &proxy);
                        (d, ok)
                    })
                })
                .collect();
            let mut statuses = Vec::new();
            let mut first = None;
            for h in handles {
                if let Ok((d, ok)) = h.join() {
                    if ok && first.is_none() {
                        first = Some(d.clone());
                    }
                    statuses.push((d, ok));
                }
            }
            Msg::ProbeDone(statuses, first)
        });
    }

    fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::ProbeDone(results, domain) => {
                    self.probing = false;
                    self.probe_results = results;
                    if self.session.is_none() {
                        self.screen = Screen::Login;
                        if let Some(d) = domain {
                            self.manual_domain = d;
                        }
                    }
                }
                Msg::LoginDone(r) | Msg::ConfirmDone(r) => {
                    self.busy = false;
                    match r {
                        Ok(s) => {
                            store::save_session(&s);
                            self.session = Some(s);
                            self.screen = Screen::Search;
                            self.password.clear();
                            self.code_sent = false;
                        }
                        Err(e) => self.login_error = e,
                    }
                }
                Msg::RegisterDone(r) => {
                    self.busy = false;
                    match r {
                        Ok(()) => {
                            self.code_sent = true;
                            self.reg_info = tr("Check your email for the confirmation code.").into();
                        }
                        Err(e) => self.login_error = e,
                    }
                }
                Msg::ResendDone(r) => {
                    self.busy = false;
                    match r {
                        Ok(()) => self.reg_info = tr("Code resent — check your email.").into(),
                        Err(e) => self.login_error = e,
                    }
                }
                Msg::SearchDone(r) => {
                    self.searching = false;
                    match r {
                        Ok(p) => {
                            self.results = p;
                            self.search_error.clear();
                        }
                        Err(e) => self.search_error = e,
                    }
                }
                Msg::DetailsDone(r) => {
                    self.fetching_details = false;
                    self.details = Some(r);
                }
                Msg::DlProgress(w, t) => {
                    if let Some(d) = self.active_dl.as_mut() {
                        d.1 = w;
                        d.2 = t;
                    }
                }
                Msg::Quota(r) => self.quota = r.ok(),
                Msg::CoverFetched(id, ok) => {
                    if ok {
                        self.covers_updated.push(id);
                    } else {
                        self.dl_error = tr("No catalogue cover found").into();
                    }
                }
                Msg::Quiet(gen, r) => {
                    if gen == self.quiet_gen {
                        self.quiet_busy = false;
                        self.quiet = Some(r.map(|p| p.books));
                    }
                }
                Msg::DlDone(r) => {
                    self.active_dl = None;
                    match r {
                        Ok((title, p)) => {
                            self.dl_error.clear();
                            self.downloaded.push((title.clone(), p.clone()));
                            self.recent.insert(0, (title, p));
                            self.refresh_quota_after = true;
                        }
                        Err(e) => self.dl_error = e,
                    }
                }
            }
        }
    }

    fn domain(&self) -> String {
        if !self.manual_domain.trim().is_empty() {
            api::normalize(self.manual_domain.trim())
        } else {
            self.probe_results.iter().find(|(_, ok)| *ok).map(|(d, _)| d.clone()).unwrap_or_else(|| "https://z-lib.gd".into())
        }
    }

    pub fn logout(&mut self, app: &AppSettings) {
        self.session = None;
        self.results = api::SearchPage::default();
        self.selected = None;
        self.details = None;
        store::clear_session();
        self.start_probe(app);
    }

    fn do_search(&mut self, page: u32, app: &AppSettings) {
        if self.searching || self.query.trim().is_empty() {
            return;
        }
        let Some(sess) = self.session.clone() else { return };
        self.searching = true;
        self.search_error.clear();
        self.page = page;
        let proxy = app.proxy_opt();
        let q = self.query.trim().to_string();
        let f = self.filters.clone();
        self.spawn(move || Msg::SearchDone(api::search(&sess, &proxy, &q, page, &f)));
    }

    fn fetch_details(&mut self, book: &api::Book, app: &AppSettings) {
        let Some(sess) = self.session.clone() else { return };
        self.fetching_details = true;
        self.selected = Some(book.clone());
        self.details = None;
        let proxy = app.proxy_opt();
        let (id, hash) = (book.id.clone(), book.hash.clone());
        self.spawn(move || Msg::DetailsDone(api::book_details(&sess, &proxy, &id, &hash)));
    }

    fn do_download(&mut self, book: api::Book, app: &AppSettings) {
        if self.active_dl.is_some() {
            return;
        }
        let Some(sess) = self.session.clone() else { return };
        self.dl_error.clear();
        self.dl_started = std::time::Instant::now();
        let proxy = app.proxy_opt();
        let dest = PathBuf::from(&app.download_dir);
        let title = book.title.clone();
        self.active_dl = Some((title.clone(), 0, None));
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let tx2 = tx.clone();
            let ctx2 = ctx.clone();
            let mut last = std::time::Instant::now();
            let res = api::download_book(&sess, &proxy, &book, &dest, move |w, t| {
                if last.elapsed().as_millis() > 100 {
                    last = std::time::Instant::now();
                    let _ = tx2.send(Msg::DlProgress(w, t));
                    ctx2.request_repaint();
                }
            });
            if let Ok(p) = &res {
                // carry the catalogue's cover and metadata over to the library
                attach_catalogue_data(p, &book, &proxy);
            }
            let _ = tx.send(Msg::DlDone(res.map(|p| (title, p))));
            ctx.request_repaint();
        });
    }

    /// Look a local book up in the catalogue and adopt its cover.
    pub fn fetch_cover_for(&mut self, id: String, path: PathBuf, title: String, author: String, app: &AppSettings) {
        let Some(sess) = self.session.clone() else {
            self.dl_error = tr("Connect the Z-Library source first").into();
            return;
        };
        let proxy = app.proxy_opt();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let q = format!("{} {}", title, author.split(',').next().unwrap_or("")).trim().to_string();
            let ok = api::search(&sess, &proxy, &q, 1, &api::Filters::default())
                .ok()
                .and_then(|p| p.books.into_iter().find(|b| !b.cover.is_empty()))
                .map(|b| attach_catalogue_data(&path, &b, &proxy))
                .unwrap_or(false);
            let _ = tx.send(Msg::CoverFetched(id, ok));
            ctx.request_repaint();
        });
    }

    pub fn connected(&self) -> bool {
        self.session.is_some()
    }

    fn fetch_quota(&mut self, app: &AppSettings) {
        if let Some(sess) = self.session.clone() {
            let proxy = app.proxy_opt();
            self.spawn(move || Msg::Quota(api::quota(&sess, &proxy)));
        }
    }

    /// Ten little squares: today's downloads used vs. left.
    pub fn quota_ui(&self, ui: &mut egui::Ui, pal: &Palette) {
        let Some((used, limit)) = self.quota else { return };
        let limit = limit.max(1);
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf!("{left} of {limit} left today", left = limit.saturating_sub(used), limit = limit)).font(ui::ui_m(12.0)).color(pal.muted));
        });
        let n = limit.min(20);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().min(n as f32 * 12.0), 10.0), Sense::hover());
        let w = r.width() / n as f32;
        for i in 0..n {
            let cell = egui::Rect::from_min_size(egui::pos2(r.left() + i as f32 * w, r.top()), vec2(w - 3.0, 8.0));
            let filled = (i as f32) < used as f32 * n as f32 / limit as f32;
            ui.painter().rect_filled(cell, CornerRadius::same(2), if filled { pal.border } else { ui::ACCENT });
        }
        ui.add_space(4.0);
    }

    /// Background tick for the quiet library-search integration.
    pub fn tick(&mut self, query: &str, app: &AppSettings) {
        self.drain();
        self.covers.poll();
        if self.session.is_some() && (self.quota.is_none() && !self.quota_asked || std::mem::take(&mut self.refresh_quota_after)) {
            self.quota_asked = true;
            self.fetch_quota(app);
        }
        let q = query.trim();
        if q != self.quiet_q {
            self.quiet_q = q.to_string();
            self.quiet_changed = std::time::Instant::now();
        }
        if !app.zlib_enabled || self.session.is_none() || q.chars().count() < 3 {
            if q.chars().count() < 3 {
                self.quiet = None;
                self.quiet_sent.clear();
            }
            return;
        }
        if q != self.quiet_sent {
            let idle = self.quiet_changed.elapsed();
            if idle < std::time::Duration::from_millis(650) {
                self.ctx.request_repaint_after(std::time::Duration::from_millis(700) - idle);
                return;
            }
            self.quiet_sent = q.to_string();
            self.quiet_gen += 1;
            self.quiet_busy = true;
            let (gen, sess, proxy, q2) = (self.quiet_gen, self.session.clone().unwrap(), app.proxy_opt(), q.to_string());
            self.spawn(move || Msg::Quiet(gen, api::search(&sess, &proxy, &q2, 1, &api::Filters::default())));
        }
    }

    /// Compact "elsewhere" strip under local search results.
    pub fn quiet_strip(&mut self, ui: &mut egui::Ui, pal: &Palette, app: &AppSettings) -> bool {
        if !app.zlib_enabled || self.quiet_q.chars().count() < 3 {
            return false;
        }
        let mut go_source = false;
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.label(ui::tag(tr("Elsewhere — Z-Library")).color(pal.muted));
            if self.session.is_none() {
                if ui::text_button(ui, tr("Connect source →"), false).clicked() {
                    go_source = true;
                }
                return;
            }
            if self.quiet_busy {
                ui.spinner();
            } else if let Some(Ok(b)) = &self.quiet {
                ui.label(ui::tag(format!("{:02} results", b.len())).color(pal.muted));
            }
            if ui::text_button(ui, tr("Open in source →"), false).clicked() {
                self.query = self.quiet_q.clone();
                go_source = true;
            }
        });
        ui::rule(ui);
        ui.add_space(8.0);
        let books = match &self.quiet {
            Some(Ok(b)) => b.clone(),
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(pal.muted).size(12.0));
                return go_source;
            }
            None => return go_source,
        };
        let proxy = app.proxy_opt();
        egui::ScrollArea::horizontal().id_salt("quiet").show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for (i, b) in books.iter().take(14).enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(vec2(112.0, 236.0), Sense::click());
                    let cover = egui::Rect::from_min_size(rect.min + vec2(0.0, if resp.hovered() { -3.0 } else { 0.0 }), vec2(112.0, 166.0));
                    let key = format!("zl:{}", b.id);
                    if !b.cover.is_empty() && ui.is_rect_visible(rect) {
                        let (url, px) = (b.cover.clone(), proxy.clone());
                        self.covers.request(&key, || Some(Src::Url { url, proxy: px }), 320);
                    }
                    match self.covers.get(&key) {
                        Some(t) => {
                            ui.painter().image(t.tex.id(), cover, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        None => ui::paint_placeholder_cover(ui.painter(), cover, &b.title, &b.author),
                    }
                    if resp.hovered() {
                        let strip = egui::Rect::from_min_max(egui::pos2(cover.left(), cover.bottom() - 22.0), cover.max);
                        ui.painter().rect_filled(strip, CornerRadius::ZERO, ui::INK);
                        ui.painter().text(strip.left_center() + vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, "+ GET", ui::mono_m(10.5), ui::PAPER);
                    }
                    let y = cover.bottom() + 8.0;
                    ui.painter().text(egui::pos2(rect.left(), y), egui::Align2::LEFT_TOP, format!("Z{:02} — {}", i + 1, b.extension.to_uppercase()), ui::mono(9.5), pal.muted);
                    let mut job = egui::text::LayoutJob::simple(b.title.clone(), ui::ui_m(12.5), pal.fg, 112.0);
                    job.wrap.max_rows = 2;
                    job.wrap.overflow_character = Some('…');
                    let g = ui.painter().layout_job(job);
                    let th = g.size().y;
                    ui.painter().galley(egui::pos2(rect.left(), y + 15.0), g, pal.fg);
                    ui.painter().text(egui::pos2(rect.left(), y + 18.0 + th), egui::Align2::LEFT_TOP, trunc(&b.author, 18), ui::mono(9.5), pal.muted);
                    if resp.clicked() {
                        self.fetch_details(b, app);
                    }
                    ui.add_space(14.0);
                }
            });
        });
        go_source
    }

    /// Right-hand details/download pane, shown wherever the plugin is surfaced.
    pub fn side_details(&mut self, ctx: &egui::Context, app: &mut AppSettings, pal: &Palette) {
        if self.selected.is_none() {
            return;
        }
        egui::SidePanel::right("zl-details").resizable(false).exact_width(320.0).frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin::same(18)).stroke(egui::Stroke::new(1.0, pal.border))).show(ctx, |ui| {
            self.ui_details(ui, app, pal);
        });
    }

    /// Download status block for the navigation sidebar.
    pub fn downloads_ui(&mut self, ui: &mut egui::Ui, pal: &Palette) -> Option<PathBuf> {
        let mut open = None;
        if let Some((t, w, total)) = &self.active_dl {
            ui.label(RichText::new(tr("Downloading")).font(ui::ui_m(12.0)).color(ui::ACCENT));
            ui.add(egui::Label::new(RichText::new(t).size(12.5)).truncate());
            self.progress_bar(ui, pal, *w, *total);
        }
        if !self.dl_error.is_empty() {
            ui.label(RichText::new(&self.dl_error).size(11.5).color(ui::ACCENT));
        }
        if let Some((t, p)) = self.recent.first() {
            ui.label(ui::tag(tr("Downloaded")).color(pal.muted));
            ui.add(egui::Label::new(RichText::new(t).size(12.5)).truncate());
            if ui::text_button(ui, tr("Read now →"), false).clicked() {
                open = Some(p.clone());
            }
        }
        open
    }

    fn progress_bar(&self, ui: &mut egui::Ui, pal: &Palette, w: u64, total: Option<u64>) {
        let frac = total.map(|t| w as f32 / t.max(1) as f32).unwrap_or(0.0).clamp(0.0, 1.0);
        let r = ui.available_rect_before_wrap();
        let bar = egui::Rect::from_min_size(r.min + vec2(0.0, 2.0), vec2(r.width(), 6.0));
        ui.painter().rect_filled(bar, CornerRadius::same(3), pal.border);
        let shown = ui.ctx().animate_value_with_time(egui::Id::new("dl-frac"), frac, 0.25);
        ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * shown, 6.0)), CornerRadius::same(3), ui::ACCENT);
        ui.add_space(12.0);
        let secs = self.dl_started.elapsed().as_secs_f64().max(0.1);
        let speed = w as f64 / secs / 1_048_576.0;
        let size = match total {
            Some(t) => format!("{:.1} of {:.1} MB · {:.0}%", w as f64 / 1_048_576.0, t as f64 / 1_048_576.0, frac * 100.0),
            None => format!("{:.1} MB", w as f64 / 1_048_576.0),
        };
        ui.label(RichText::new(format!("{size} · {speed:.1} MB/s")).font(ui::ui_m(11.5)).color(pal.muted));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }

    pub fn ui(&mut self, ctx: &egui::Context, app: &mut AppSettings, pal: &Palette) -> Option<PathBuf> {
        self.start(app);
        self.drain();
        self.covers.poll();
        let open = None;
        if self.screen == Screen::Search {
            self.side_details(ctx, app, pal);
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 36, right: 28, top: 26, bottom: 10 })).show(ctx, |ui| match self.screen {
            Screen::Connecting => {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    if self.probing {
                        ui.spinner();
                    }
                    ui.heading(if self.probing { tr("Finding a reachable Z-Library mirror…") } else { tr("No mirror reachable") });
                    ui.add_space(12.0);
                    for (d, ok) in &self.probe_results {
                        ui.label(format!("{d}  {}", if *ok { "reachable" } else { "—" }));
                    }
                    if !self.probing && ui.button(tr("Retry")).clicked() {
                        self.start_probe(app);
                    }
                });
            }
            Screen::Login => self.ui_login(ui, app),
            Screen::Search => self.ui_search(ui, app, pal),
        });
        open
    }

    fn ui_login(&mut self, ui: &mut egui::Ui, app: &mut AppSettings) {
        ui.vertical_centered(|ui| {
            ui.set_max_width(440.0);
            ui.add_space(30.0);
            ui.label(ui::tag(tr("Source / plugin")).color(ui.visuals().weak_text_color()));
            ui.label(ui::display(tr("Connect Z-Library"), 34.0));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui::text_button(ui, tr("Sign in"), self.auth_mode == AuthMode::SignIn).clicked() {
                    self.auth_mode = AuthMode::SignIn;
                }
                if ui::text_button(ui, tr("Create account"), self.auth_mode == AuthMode::Register).clicked() {
                    self.auth_mode = AuthMode::Register;
                }
                if ui.small_button(tr("Rescan mirrors")).clicked() {
                    self.start_probe(app);
                }
            });
            ui.add_space(8.0);
            egui::Grid::new("login").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                if self.auth_mode == AuthMode::SignIn && self.use_cookies {
                    ui.label("remix_userid");
                    ui.text_edit_singleline(&mut self.cookie_userid);
                    ui.end_row();
                    ui.label("remix_userkey");
                    ui.text_edit_singleline(&mut self.cookie_userkey);
                    ui.end_row();
                } else {
                    ui.label(tr("Email"));
                    ui.text_edit_singleline(&mut self.email);
                    ui.end_row();
                    ui.label(tr("Password"));
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                    ui.end_row();
                    if self.auth_mode == AuthMode::Register {
                        ui.label(tr("Nickname"));
                        ui.text_edit_singleline(&mut self.reg_nickname);
                        ui.end_row();
                    }
                }
                ui.label(tr("Mirror"));
                ui.text_edit_singleline(&mut self.manual_domain);
                ui.end_row();
                ui.label(tr("Proxy"));
                ui.add(egui::TextEdit::singleline(&mut app.proxy).hint_text(tr("optional, e.g. socks5://127.0.0.1:9050")));
                ui.end_row();
            });
            if self.auth_mode == AuthMode::SignIn {
                ui.checkbox(&mut self.use_cookies, tr("Use session cookies instead of password"));
            }
            ui.add_space(8.0);
            let domain = self.domain();
            let proxy = app.proxy_opt();
            match self.auth_mode {
                AuthMode::SignIn => {
                    if ui.add_enabled(!self.busy, egui::Button::new(if self.busy { tr("Signing in…") } else { tr("Sign in") }).min_size(vec2(160.0, 30.0))).clicked() {
                        self.busy = true;
                        self.login_error.clear();
                        if self.use_cookies {
                            let (id, key) = (self.cookie_userid.clone(), self.cookie_userkey.clone());
                            self.spawn(move || Msg::LoginDone(api::login_with_cookies(&domain, &proxy, &id, &key)));
                        } else {
                            let (e, p) = (self.email.clone(), self.password.clone());
                            self.spawn(move || Msg::LoginDone(api::login(&domain, &proxy, &e, &p)));
                        }
                    }
                }
                AuthMode::Register => {
                    let (e, p, n) = (self.email.clone(), self.password.clone(), self.reg_nickname.clone());
                    if !self.code_sent {
                        if ui.add_enabled(!self.busy, egui::Button::new(tr("Create account")).min_size(vec2(160.0, 30.0))).clicked() {
                            self.busy = true;
                            self.login_error.clear();
                            self.spawn(move || Msg::RegisterDone(api::register(&domain, &proxy, &e, &p, &n)));
                        }
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(tr("Code from email:"));
                            ui.add(egui::TextEdit::singleline(&mut self.reg_code).desired_width(120.0));
                            let code = self.reg_code.trim().to_string();
                            if ui.add_enabled(!self.busy && !code.is_empty(), egui::Button::new(tr("Confirm"))).clicked() {
                                self.busy = true;
                                let (d2, p2, e2, pw2, n2) = (domain.clone(), proxy.clone(), e.clone(), p.clone(), n.clone());
                                self.spawn(move || Msg::ConfirmDone(api::confirm_code(&d2, &p2, &e2, &pw2, &n2, &code)));
                            }
                            if ui.add_enabled(!self.busy, egui::Button::new(tr("Resend"))).clicked() {
                                self.busy = true;
                                self.spawn(move || Msg::ResendDone(api::resend_code(&domain, &proxy, &e, &p, &n)));
                            }
                        });
                    }
                }
            }
            if !self.reg_info.is_empty() {
                ui.colored_label(Color32::from_rgb(60, 170, 90), &self.reg_info);
            }
            if !self.login_error.is_empty() {
                ui.colored_label(Color32::from_rgb(220, 70, 60), &self.login_error);
                ui.small(tr("If password login is disabled for your account, use session cookies: copy remix_userid / remix_userkey from a logged-in browser (DevTools → Storage → Cookies)."));
            }
            let reachable: Vec<String> = self.probe_results.iter().filter(|(_, ok)| *ok).map(|(d, _)| d.clone()).collect();
            if !reachable.is_empty() {
                ui.add_space(6.0);
                ui.small(trf!("Reachable mirrors: {list}", list = reachable.join(", ")));
            }
        });
    }

    fn ui_search(&mut self, ui: &mut egui::Ui, app: &mut AppSettings, pal: &Palette) {
        // header: name on the left, account + quota on the right
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(tr("Source")).font(ui::ui_m(12.0)).color(pal.muted));
                ui.label(ui::display("Z-Library", 30.0));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.session.is_some() && ui::text_button(ui, tr("Sign out"), false).clicked() {
                    self.logout(app);
                }
                if let Some(s) = &self.session {
                    ui.label(RichText::new(if s.email.is_empty() { tr("Signed in").to_string() } else { s.email.clone() }).font(ui::ui_m(12.5)).color(pal.muted));
                }
                if let Some((used, limit)) = self.quota {
                    ui.add_space(12.0);
                    ui.label(RichText::new(trf!("{left} of {limit} downloads left today", left = limit.saturating_sub(used), limit = limit)).font(ui::ui_m(12.5)).color(ui::ACCENT));
                }
            });
        });
        ui.add_space(12.0);
        // search pill
        ui.horizontal(|ui| {
            let (sr, _) = ui.allocate_exact_size(vec2((ui.available_width() - 120.0).max(200.0), 38.0), Sense::hover());
            ui.painter().rect_filled(sr, CornerRadius::same(19), pal.hover);
            ui::paint_icon(ui.painter(), egui::Rect::from_center_size(egui::pos2(sr.left() + 20.0, sr.center().y), vec2(15.0, 15.0)), ui::Icon::Search, pal.muted);
            let inner = egui::Rect::from_min_max(egui::pos2(sr.left() + 38.0, sr.top() + 8.0), egui::pos2(sr.right() - 16.0, sr.bottom() - 6.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
            let r = child.add(egui::TextEdit::singleline(&mut self.query).frame(false).hint_text(RichText::new(tr("Title, author or ISBN")).color(pal.muted)).desired_width(f32::INFINITY).font(ui::ui_m(15.0)));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(8.0);
            if ui::primary_button(ui, if self.searching { tr("Searching") } else { tr("Search") }).clicked() || enter {
                self.do_search(1, app);
            }
        });
        ui.add_space(8.0);
        // filters, quietly
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui::text_button(ui, tr("Exact"), self.filters.exact).on_hover_text(tr("Exact title match")).clicked() {
                self.filters.exact = !self.filters.exact;
            }
            egui::ComboBox::from_id_salt("ext").selected_text(if self.filters.extension.is_empty() { "Any format".to_string() } else { self.filters.extension.clone() }).width(110.0).show_ui(ui, |ui| {
                for e in ["", "EPUB", "PDF", "MOBI", "AZW3", "FB2", "TXT", "DJVU", "CBZ"] {
                    ui.selectable_value(&mut self.filters.extension, e.into(), if e.is_empty() { tr("Any format") } else { e });
                }
            });
            egui::ComboBox::from_id_salt("order").selected_text(order_label(&self.filters.order)).width(130.0).show_ui(ui, |ui| {
                for (v, l) in [("bestmatch", tr("Best match")), ("popular", tr("Popular")), ("date_created", tr("Newest")), ("date_updated", tr("Recently updated")), ("year", tr("Year")), ("titleA", tr("Title A–Z"))] {
                    ui.selectable_value(&mut self.filters.order, v.into(), l);
                }
            });
            ui.add(egui::TextEdit::singleline(&mut self.filters.language).desired_width(96.0).hint_text(tr("Language")).margin(vec2(8.0, 4.0)));
            ui.add(egui::TextEdit::singleline(&mut self.filters.year_from).desired_width(58.0).hint_text(tr("From")).margin(vec2(8.0, 4.0)));
            ui.label(RichText::new("–").color(pal.muted));
            ui.add(egui::TextEdit::singleline(&mut self.filters.year_to).desired_width(58.0).hint_text(tr("To")).margin(vec2(8.0, 4.0)));
        });
        if !self.search_error.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(&self.search_error).color(ui::ACCENT));
        }
        ui.add_space(10.0);
        ui.painter().hline(ui.available_rect_before_wrap().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, pal.border));
        ui.add_space(4.0);
        if self.results.books.is_empty() {
            ui.add_space(40.0);
            if self.searching {
                ui.spinner();
            } else {
                ui.label(RichText::new(tr("Search for a book. Add it to your library, or read it online in your browser.")).font(ui::ui_m(15.0)).color(pal.muted));
            }
            return;
        }
        let books = self.results.books.clone();
        let proxy = app.proxy_opt();
        let mut clicked = None;
        let mut get: Option<api::Book> = None;
        let downloading = self.active_dl.clone();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for b in &books {
                let sel = self.selected.as_ref().is_some_and(|s| s.id == b.id);
                let w = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 92.0), Sense::click());
                let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || sel, 0.12);
                if t > 0.01 {
                    ui.painter().rect_filled(rect, CornerRadius::same(10), if sel { ui::ACCENT.linear_multiply(0.09) } else { pal.hover.linear_multiply(t) });
                }
                let cover = egui::Rect::from_min_size(rect.min + vec2(12.0, 8.0), vec2(50.0, 76.0));
                let key = format!("zl:{}", b.id);
                if !b.cover.is_empty() && ui.is_rect_visible(rect) {
                    let (url, px) = (b.cover.clone(), proxy.clone());
                    self.covers.request(&key, || Some(Src::Url { url, proxy: px }), 240);
                }
                match self.covers.get(&key) {
                    Some(tx) => {
                        ui.painter().image(tx.tex.id(), cover, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => ui::paint_placeholder_cover(ui.painter(), cover, &b.title, ""),
                }
                let actions_w = 96.0;
                let x = cover.right() + 16.0;
                let tw = (rect.right() - actions_w - x - 12.0).max(80.0);
                let mut job = egui::text::LayoutJob::simple(b.title.clone(), ui::ui_m(15.0), pal.fg, tw);
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                ui.painter().galley(egui::pos2(x, rect.top() + 14.0), ui.painter().layout_job(job), pal.fg);
                let mut job = egui::text::LayoutJob::simple(b.author.clone(), egui::FontId::proportional(13.0), pal.muted, tw);
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                ui.painter().galley(egui::pos2(x, rect.top() + 38.0), ui.painter().layout_job(job), pal.muted);
                let meta = [b.year.as_str(), &b.extension.to_uppercase(), &b.size, &b.language].iter().filter(|s| !s.is_empty() && **s != "0").cloned().collect::<Vec<_>>().join("  ·  ");
                ui.painter().text(egui::pos2(x, rect.top() + 62.0), egui::Align2::LEFT_TOP, meta, ui::ui_m(11.5), pal.muted);
                // row actions (or this row's download progress)
                let is_dl = downloading.as_ref().is_some_and(|d| d.0 == b.title) && self.dl_book.as_deref() == Some(b.id.as_str());
                if is_dl {
                    let (_, wr, total) = downloading.clone().unwrap();
                    let frac = total.map(|t| wr as f32 / t.max(1) as f32).unwrap_or(0.0).clamp(0.0, 1.0);
                    let bar = egui::Rect::from_min_size(egui::pos2(rect.right() - actions_w - 4.0, rect.center().y - 3.0), vec2(actions_w - 8.0, 6.0));
                    ui.painter().rect_filled(bar, CornerRadius::same(3), pal.border);
                    ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * frac, 6.0)), CornerRadius::same(3), ui::ACCENT);
                    ui.painter().text(egui::pos2(bar.center().x, bar.bottom() + 12.0), egui::Align2::CENTER_CENTER, format!("{:.0}%", frac * 100.0), ui::ui_m(11.0), pal.muted);
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
                } else {
                    let online = !b.read_online_url.is_empty();
                    for (i, (icon, tip, read)) in [(ui::Icon::Download, tr("Add to library (uses one download from today's quota)"), false), (ui::Icon::Book, if online { tr("Read online in your browser (no download, no quota)") } else { tr("Not available to read online") }, true)].into_iter().enumerate() {
                        if read && !online {
                            continue;
                        }
                        let c = egui::pos2(rect.right() - 28.0 - i as f32 * 42.0, rect.center().y);
                        let br = egui::Rect::from_center_size(c, vec2(36.0, 36.0));
                        let bresp = ui.interact(br, resp.id.with(i), Sense::click());
                        let h = ui.ctx().animate_bool_with_time(bresp.id, bresp.hovered(), 0.12);
                        ui.painter().circle_filled(c, 17.0, if h > 0.01 { ui::ACCENT.linear_multiply(0.15 + 0.85 * h) } else { pal.bg });
                        ui.painter().circle_stroke(c, 17.0, egui::Stroke::new(1.0, pal.border.lerp_to_gamma(ui::ACCENT, h)));
                        ui::paint_icon(ui.painter(), br.shrink(10.0), icon, if h > 0.5 { Color32::WHITE } else { pal.fg });
                        if bresp.on_hover_text(tip).clicked() {
                            if read {
                                crate::platform::open_url(&b.read_online_url);
                            } else if downloading.is_none() {
                                get = Some(b.clone());
                            }
                        }
                    }
                }
                if resp.clicked() {
                    clicked = Some(b.clone());
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if self.page > 1 && !self.searching && ui::text_button(ui, tr("← Previous"), false).clicked() {
                    self.do_search(self.page - 1, app);
                }
                ui.label(RichText::new(trf!("Page {n} of {total}", n = self.results.page, total = self.results.total_pages.max(1))).font(ui::ui_m(12.5)).color(pal.muted));
                if self.page < self.results.total_pages && !self.searching && ui::text_button(ui, tr("Next →"), false).clicked() {
                    self.do_search(self.page + 1, app);
                }
            });
        });
        if let Some(b) = get {
            self.dl_book = Some(b.id.clone());
            self.do_download(b, app);
        }
        if let Some(b) = clicked {
            self.fetch_details(&b, app);
        }
    }

    fn ui_details(&mut self, ui: &mut egui::Ui, app: &mut AppSettings, pal: &Palette) {
        let Some(book) = self.selected.clone() else { return };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui::icon_button(ui, ui::Icon::Close, tr("Close")).clicked() {
                    self.selected = None;
                    self.details = None;
                }
            });
        });
        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(t) = self.covers.get(&format!("zl:{}", book.id)) {
                let w = 150.0;
                let h = w * t.size[1] as f32 / t.size[0].max(1) as f32;
                ui.vertical_centered(|ui| {
                    ui.add(egui::Image::new((t.tex.id(), vec2(w, h))).corner_radius(6));
                });
            }
            ui.add_space(6.0);
            ui.label(RichText::new(&book.title).strong().size(18.0));
            ui.label(RichText::new(&book.author).color(pal.muted));
            ui.add_space(6.0);
            egui::Grid::new("zl-meta").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                for (k, v) in [(tr("Year"), &book.year), (tr("Publisher"), &book.publisher), (tr("Language"), &book.language), (tr("Format"), &book.extension), (tr("Pages"), &book.pages), (tr("Size"), &book.size)] {
                    if !v.is_empty() {
                        ui.label(RichText::new(k).color(pal.muted));
                        ui.label(v.as_str());
                        ui.end_row();
                    }
                }
            });
            ui.add_space(10.0);
            match &self.details {
                None => {
                    if self.fetching_details {
                        ui.spinner();
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(Color32::from_rgb(220, 70, 60), e);
                }
                Some(Ok(d)) => {
                    let d = d.clone();
                    self.quota_ui(ui, pal);
                    if let Some((_, w, total)) = self.active_dl.clone() {
                        self.progress_bar(ui, pal, w, total);
                    } else {
                        ui.horizontal(|ui| {
                            if ui::primary_button(ui, tr("Add to library")).clicked() {
                                self.do_download(d.clone(), app);
                            }
                            if !d.read_online_url.is_empty() && ui::text_button(ui, tr("Read online"), false).on_hover_text(tr("Opens Z-Library's reader in your browser. Nothing is downloaded and no quota is used.")).clicked() {
                                crate::platform::open_url(&d.read_online_url);
                            }
                        });
                    }
                    if !d.description.is_empty() {
                        ui.add_space(10.0);
                        ui.label(RichText::new(crate::book::html::decode_entities(&strip(&d.description)).to_string()).size(13.5));
                    }
                }
            }
        });
    }
}

fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn order_label(o: &str) -> &'static str {
    match o {
        "popular" => tr("Popular"),
        "date_created" => tr("Newest"),
        "date_updated" => tr("Recently updated"),
        "year" => tr("Year"),
        "titleA" => tr("Title A–Z"),
        _ => tr("Best match"),
    }
}

/// Save the catalogue cover as the book's thumbnail and a metadata sidecar.
fn attach_catalogue_data(path: &std::path::Path, book: &api::Book, proxy: &Option<String>) -> bool {
    let Some(id) = store::book_id(path) else { return false };
    let mut ok = false;
    if let (false, Ok(client)) = (book.cover.is_empty(), api::client(proxy, Some(std::time::Duration::from_secs(20)))) {
        // catalogue thumbnails often have a larger sibling without the size suffix
        let big = book.cover.replace("/covers100/", "/covers299/");
        for url in [big.as_str(), book.cover.as_str()] {
            if let Ok(r) = client.get(url).send() {
                if r.status().is_success() {
                    if let Ok(b) = r.bytes() {
                        if crate::library::save_thumbnail(&id, &b).is_some() {
                            ok = true;
                            break;
                        }
                    }
                }
            }
        }
    }
    let meta = serde_json::json!({
        "title": book.title, "author": book.author, "publisher": book.publisher,
        "language": book.language, "year": book.year, "description": book.description,
    });
    let _ = std::fs::create_dir_all(store::covers_dir());
    let _ = std::fs::write(store::covers_dir().join(format!("{id}.meta.json")), meta.to_string());
    ok
}
