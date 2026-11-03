//! Z-Library tab: mirror discovery, sign-in/registration, search, details, downloads.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense};

use crate::api;
use crate::store::{self, AppSettings};
use crate::texload::{Src, TexLoader};
use crate::ui::{self, chip, Palette};

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
    pub recent: Vec<(String, PathBuf)>,
    covers: TexLoader,
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
            recent: Vec::new(),
            covers: TexLoader::new(ctx, 3),
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
                            self.reg_info = "Check your email for the confirmation code.".into();
                        }
                        Err(e) => self.login_error = e,
                    }
                }
                Msg::ResendDone(r) => {
                    self.busy = false;
                    match r {
                        Ok(()) => self.reg_info = "Code resent — check your email.".into(),
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
                Msg::DlDone(r) => {
                    self.active_dl = None;
                    match r {
                        Ok((title, p)) => {
                            self.dl_error.clear();
                            self.downloaded.push((title.clone(), p.clone()));
                            self.recent.insert(0, (title, p));
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
            let _ = tx.send(Msg::DlDone(res.map(|p| (title, p))));
            ctx.request_repaint();
        });
    }

    pub fn ui(&mut self, ctx: &egui::Context, app: &mut AppSettings, pal: &Palette) -> Option<PathBuf> {
        self.start(app);
        self.drain();
        self.covers.poll();
        let mut open = None;
        if self.screen == Screen::Search && self.selected.is_some() {
            egui::SidePanel::right("zl-details").resizable(true).default_width(360.0).frame(egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin::same(14))).show(ctx, |ui| {
                self.ui_details(ui, app, pal);
            });
        }
        if self.active_dl.is_some() || !self.dl_error.is_empty() || !self.recent.is_empty() {
            egui::TopBottomPanel::bottom("zl-dl").frame(egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin::symmetric(14, 8))).show(ctx, |ui| {
                if let Some((t, w, total)) = &self.active_dl {
                    ui.horizontal(|ui| {
                        ui.label("Downloading");
                        ui.strong(t);
                        let frac = total.map(|t| *w as f32 / t.max(1) as f32).unwrap_or(0.0);
                        ui.add(egui::ProgressBar::new(frac).desired_width(260.0).text(format!("{:.1} MB", *w as f64 / 1_048_576.0)));
                    });
                }
                if !self.dl_error.is_empty() {
                    ui.colored_label(Color32::from_rgb(220, 70, 60), &self.dl_error);
                }
                for (t, p) in self.recent.iter().take(3) {
                    ui.horizontal(|ui| {
                        ui.label("✓");
                        ui.label(RichText::new(t).strong());
                        if ui::primary_button(ui, "Read now").clicked() {
                            open = Some(p.clone());
                        }
                        if ui.small_button("Show in folder").clicked() {
                            crate::platform::reveal(p);
                        }
                    });
                }
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin::symmetric(20, 14))).show(ctx, |ui| match self.screen {
            Screen::Connecting => {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    if self.probing {
                        ui.spinner();
                    }
                    ui.heading(if self.probing { "Finding a reachable Z-Library mirror…" } else { "No mirror reachable" });
                    ui.add_space(12.0);
                    for (d, ok) in &self.probe_results {
                        ui.label(format!("{d}  {}", if *ok { "✓ reachable" } else { "—" }));
                    }
                    if !self.probing && ui.button("Retry").clicked() {
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
            ui.heading("Connect to Z-Library");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if chip(ui, self.auth_mode == AuthMode::SignIn, "Sign in").clicked() {
                    self.auth_mode = AuthMode::SignIn;
                }
                if chip(ui, self.auth_mode == AuthMode::Register, "Create account").clicked() {
                    self.auth_mode = AuthMode::Register;
                }
                if ui.small_button("Rescan mirrors").clicked() {
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
                    ui.label("Email");
                    ui.text_edit_singleline(&mut self.email);
                    ui.end_row();
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                    ui.end_row();
                    if self.auth_mode == AuthMode::Register {
                        ui.label("Nickname");
                        ui.text_edit_singleline(&mut self.reg_nickname);
                        ui.end_row();
                    }
                }
                ui.label("Mirror");
                ui.text_edit_singleline(&mut self.manual_domain);
                ui.end_row();
                ui.label("Proxy");
                ui.add(egui::TextEdit::singleline(&mut app.proxy).hint_text("optional, e.g. socks5://127.0.0.1:9050"));
                ui.end_row();
            });
            if self.auth_mode == AuthMode::SignIn {
                ui.checkbox(&mut self.use_cookies, "Use session cookies instead of password");
            }
            ui.add_space(8.0);
            let domain = self.domain();
            let proxy = app.proxy_opt();
            match self.auth_mode {
                AuthMode::SignIn => {
                    if ui.add_enabled(!self.busy, egui::Button::new(if self.busy { "Signing in…" } else { "Sign in" }).min_size(vec2(160.0, 30.0))).clicked() {
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
                        if ui.add_enabled(!self.busy, egui::Button::new("Create account").min_size(vec2(160.0, 30.0))).clicked() {
                            self.busy = true;
                            self.login_error.clear();
                            self.spawn(move || Msg::RegisterDone(api::register(&domain, &proxy, &e, &p, &n)));
                        }
                    } else {
                        ui.horizontal(|ui| {
                            ui.label("Code from email:");
                            ui.add(egui::TextEdit::singleline(&mut self.reg_code).desired_width(120.0));
                            let code = self.reg_code.trim().to_string();
                            if ui.add_enabled(!self.busy && !code.is_empty(), egui::Button::new("Confirm")).clicked() {
                                self.busy = true;
                                let (d2, p2, e2, pw2, n2) = (domain.clone(), proxy.clone(), e.clone(), p.clone(), n.clone());
                                self.spawn(move || Msg::ConfirmDone(api::confirm_code(&d2, &p2, &e2, &pw2, &n2, &code)));
                            }
                            if ui.add_enabled(!self.busy, egui::Button::new("Resend")).clicked() {
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
                ui.small("If password login is disabled for your account, use session cookies: copy remix_userid / remix_userkey from a logged-in browser (DevTools → Storage → Cookies).");
            }
            let reachable: Vec<String> = self.probe_results.iter().filter(|(_, ok)| *ok).map(|(d, _)| d.clone()).collect();
            if !reachable.is_empty() {
                ui.add_space(6.0);
                ui.small(format!("Reachable mirrors: {}", reachable.join(", ")));
            }
        });
    }

    fn ui_search(&mut self, ui: &mut egui::Ui, app: &mut AppSettings, pal: &Palette) {
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search Z-Library — title, author, ISBN…").desired_width(ui.available_width() - 110.0).margin(vec2(10.0, 7.0)));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui::primary_button(ui, if self.searching { "Searching…" } else { "Search" }).clicked() || enter {
                self.do_search(1, app);
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.filters.exact, "Exact match");
            ui.label("Year");
            ui.add(egui::TextEdit::singleline(&mut self.filters.year_from).desired_width(48.0).hint_text("from"));
            ui.add(egui::TextEdit::singleline(&mut self.filters.year_to).desired_width(48.0).hint_text("to"));
            egui::ComboBox::from_id_salt("ext").selected_text(if self.filters.extension.is_empty() { "Any format".to_string() } else { self.filters.extension.clone() }).width(110.0).show_ui(ui, |ui| {
                for e in ["", "EPUB", "PDF", "MOBI", "AZW3", "FB2", "TXT", "DJVU", "CBZ"] {
                    ui.selectable_value(&mut self.filters.extension, e.into(), if e.is_empty() { "Any format" } else { e });
                }
            });
            ui.add(egui::TextEdit::singleline(&mut self.filters.language).desired_width(90.0).hint_text("language"));
            egui::ComboBox::from_id_salt("order").selected_text(order_label(&self.filters.order)).width(130.0).show_ui(ui, |ui| {
                for (v, l) in [("bestmatch", "Best match"), ("popular", "Popular"), ("date_created", "Newest"), ("date_updated", "Recently updated"), ("year", "Year"), ("titleA", "Title A–Z")] {
                    ui.selectable_value(&mut self.filters.order, v.into(), l);
                }
            });
            if let Some(s) = &self.session {
                ui.label(RichText::new(format!("{} · {}", s.domain.trim_start_matches("https://"), if s.email.is_empty() { "signed in" } else { &s.email })).small().color(pal.muted));
                if ui.small_button("Sign out").clicked() {
                    self.logout(app);
                }
            }
        });
        if !self.search_error.is_empty() {
            ui.colored_label(Color32::from_rgb(220, 70, 60), &self.search_error);
        }
        ui.separator();
        if self.results.books.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                if self.searching {
                    ui.spinner();
                } else {
                    ui.label(RichText::new("Search millions of books and download them straight into your library.").color(pal.muted));
                }
            });
            return;
        }
        let books = self.results.books.clone();
        let proxy = app.proxy_opt();
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for b in &books {
                let sel = self.selected.as_ref().is_some_and(|s| s.id == b.id);
                let w = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 84.0), Sense::click());
                if sel {
                    ui.painter().rect_filled(rect, CornerRadius::same(8), ui::ACCENT.linear_multiply(0.12));
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(8), pal.hover);
                }
                let cover = egui::Rect::from_min_size(rect.min + vec2(8.0, 6.0), vec2(48.0, 72.0));
                let key = format!("zl:{}", b.id);
                if !b.cover.is_empty() && ui.is_rect_visible(rect) {
                    let url = b.cover.clone();
                    let px = proxy.clone();
                    self.covers.request(&key, || Some(Src::Url { url, proxy: px }), 240);
                }
                match self.covers.get(&key) {
                    Some(t) => {
                        ui.painter().image(t.tex.id(), cover, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => ui::paint_placeholder_cover(ui.painter(), cover, &b.title, ""),
                }
                let x = cover.right() + 12.0;
                let tw = w - (x - rect.left()) - 10.0;
                let g = ui.painter().layout(b.title.clone(), egui::FontId::proportional(15.0), pal.fg, tw);
                let g = if g.rows.len() > 1 { ui.painter().layout_no_wrap(trunc(&b.title, (tw / 7.5) as usize), egui::FontId::proportional(15.0), pal.fg) } else { g };
                ui.painter().galley(egui::pos2(x, rect.top() + 10.0), g, pal.fg);
                ui.painter().text(egui::pos2(x, rect.top() + 34.0), egui::Align2::LEFT_TOP, trunc(&b.author, (tw / 6.5) as usize), egui::FontId::proportional(13.0), pal.muted);
                let meta = [b.year.as_str(), &b.extension.to_uppercase(), &b.size, &b.language].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
                ui.painter().text(egui::pos2(x, rect.top() + 56.0), egui::Align2::LEFT_TOP, meta, egui::FontId::proportional(12.0), pal.muted);
                if resp.clicked() {
                    clicked = Some(b.clone());
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(self.page > 1 && !self.searching, egui::Button::new("← Previous")).clicked() {
                    self.do_search(self.page - 1, app);
                }
                ui.label(format!("Page {} of {}", self.results.page, self.results.total_pages.max(1)));
                if ui.add_enabled(self.page < self.results.total_pages && !self.searching, egui::Button::new("Next →")).clicked() {
                    self.do_search(self.page + 1, app);
                }
            });
        });
        if let Some(b) = clicked {
            self.fetch_details(&b, app);
        }
    }

    fn ui_details(&mut self, ui: &mut egui::Ui, app: &mut AppSettings, pal: &Palette) {
        let Some(book) = self.selected.clone() else { return };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui::icon_button(ui, ui::Icon::Close, "Close").clicked() {
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
                for (k, v) in [("Year", &book.year), ("Publisher", &book.publisher), ("Language", &book.language), ("Format", &book.extension), ("Pages", &book.pages), ("Size", &book.size)] {
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
                    let busy = self.active_dl.is_some();
                    if ui.add_enabled(!busy, egui::Button::new(RichText::new("⬇  Download to library").color(Color32::WHITE).strong()).fill(ui::ACCENT).min_size(vec2(ui.available_width(), 34.0))).clicked() {
                        self.do_download(d.clone(), app);
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
        "popular" => "Popular",
        "date_created" => "Newest",
        "date_updated" => "Recently updated",
        "year" => "Year",
        "titleA" => "Title A–Z",
        _ => "Best match",
    }
}
