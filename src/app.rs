use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use eframe::egui;

use crate::api;

// ---------- worker messages ----------

#[derive(Clone, Debug)]
pub struct DownloadEntry {
    pub title: String,
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub enum Msg {
    ProbeDone(Vec<(String, bool)>, Option<String>),
    LoginDone(Result<api::Session, String>),
    RegisterDone(Result<(), String>),
    ConfirmDone(Result<api::Session, String>),
    ResendDone(Result<(), String>),
    SearchDone(Result<api::SearchPage, String>),
    DetailsDone(Result<api::Book, String>),
    DlProgress {
        title: String,
        written: u64,
        total: Option<u64>,
    },
    DlDone(Result<(String, String, u64), String>), // (title, path, size)
}

// ---------- persisted state ----------

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SavedSession {
    domain: String,
    user_id: String,
    user_key: String,
    email: String,
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("zlib-lite")
}

fn load_session() -> Option<api::Session> {
    let p = config_dir().join("session.json");
    let text = std::fs::read_to_string(p).ok()?;
    let s: SavedSession = serde_json::from_str(&text).ok()?;
    if s.user_id.is_empty() || s.user_key.is_empty() {
        return None;
    }
    Some(api::Session {
        domain: s.domain,
        user_id: s.user_id,
        user_key: s.user_key,
        email: s.email,
    })
}

fn save_session(sess: &api::Session) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let s = SavedSession {
        domain: sess.domain.clone(),
        user_id: sess.user_id.clone(),
        user_key: sess.user_key.clone(),
        email: sess.email.clone(),
    };
    if let Ok(text) = serde_json::to_string_pretty(&s) {
        let _ = std::fs::write(dir.join("session.json"), text);
    }
}

fn load_download_dir() -> PathBuf {
    let p = config_dir().join("settings.json");
    if let Ok(text) = std::fs::read_to_string(&p) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(d) = v.get("download_dir").and_then(|d| d.as_str()) {
                return PathBuf::from(d);
            }
        }
    }
    dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn save_download_dir(dir: &std::path::Path) {
    let cfg = config_dir();
    let _ = std::fs::create_dir_all(&cfg);
    let mut v = serde_json::json!({});
    if let Ok(text) = std::fs::read_to_string(cfg.join("settings.json")) {
        v = serde_json::from_str(&text).unwrap_or(v);
    }
    v["download_dir"] = serde_json::Value::String(dir.to_string_lossy().into_owned());
    let _ = std::fs::write(
        cfg.join("settings.json"),
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    );
}

#[derive(PartialEq, Clone, Copy)]
enum AuthMode {
    SignIn,
    Register,
}

#[derive(PartialEq, Clone, Copy)]
enum RegStage {
    Idle,
    CodeSent,
}

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Connecting,
    Login,
    Search,
}

pub struct App {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    screen: Screen,
    session: Option<api::Session>,

    // connecting
    probe_results: Vec<(String, bool)>,
    probing: bool,

    // login form
    email: String,
    password: String,
    cookie_userid: String,
    cookie_userkey: String,
    login_error: String,
    logging_in: bool,
    use_cookies: bool,
    manual_domain: String,

    // registration
    auth_mode: AuthMode,
    reg_nickname: String,
    reg_code: String,
    reg_stage: RegStage,
    reg_busy: bool,
    reg_info: String,

    // search
    query: String,
    filters: api::Filters,
    results: api::SearchPage,
    searching: bool,
    search_error: String,
    page: u32,

    // selection + details
    selected: Option<api::Book>,
    details: Option<Result<api::Book, String>>,
    fetching_details: bool,

    // downloads
    download_dir: PathBuf,
    active_dl: Option<(String, u64, Option<u64>)>,
    downloads: Vec<DownloadEntry>,
    dl_error: String,

    proxy: String,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App {
            tx,
            rx,
            screen: Screen::Connecting,
            session: load_session(),
            probe_results: Vec::new(),
            probing: false,
            email: String::new(),
            password: String::new(),
            cookie_userid: String::new(),
            cookie_userkey: String::new(),
            login_error: String::new(),
            logging_in: false,
            use_cookies: false,
            manual_domain: String::new(),
            auth_mode: AuthMode::SignIn,
            reg_nickname: String::new(),
            reg_code: String::new(),
            reg_stage: RegStage::Idle,
            reg_busy: false,
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
            download_dir: load_download_dir(),
            active_dl: None,
            downloads: Vec::new(),
            dl_error: String::new(),
            proxy: String::new(),
        };
        if app.session.is_some() {
            // still verify the saved session with a quick probe of its domain
            app.screen = Screen::Search;
            let tx = app.tx.clone();
            let domain = app.session.as_ref().unwrap().domain.clone();
            std::thread::spawn(move || {
                let ok = api::probe(&domain, &None);
                let _ = tx.send(Msg::ProbeDone(
                    vec![(domain.clone(), ok)],
                    if ok { Some(domain) } else { None },
                ));
            });
        } else {
            app.start_probe();
        }
        app
    }

    fn start_probe(&mut self) {
        self.screen = Screen::Connecting;
        self.probing = true;
        self.probe_results.clear();
        let tx = self.tx.clone();
        let proxy = if self.proxy.trim().is_empty() {
            None
        } else {
            Some(self.proxy.trim().to_string())
        };
        let mut extra: Vec<String> = Vec::new();
        let md = self.manual_domain.trim().to_string();
        if !md.is_empty() {
            extra.push(api::normalize(&md));
        }
        std::thread::spawn(move || {
            let mut all: Vec<String> = extra;
            all.extend(api::DEFAULT_DOMAINS.iter().map(|s| s.to_string()));
            let mut statuses: Vec<(String, bool)> = Vec::new();
            let mut first_ok: Option<String> = None;
            // probe concurrently
            let handles: Vec<_> = all
                .clone()
                .into_iter()
                .map(|d| {
                    let proxy = proxy.clone();
                    std::thread::spawn(move || {
                        let ok = api::probe(&d, &proxy);
                        (d, ok)
                    })
                })
                .collect();
            for h in handles {
                if let Ok((d, ok)) = h.join() {
                    if ok && first_ok.is_none() {
                        first_ok = Some(d.clone());
                    }
                    statuses.push((d.clone(), ok));
                }
            }
            let _ = tx.send(Msg::ProbeDone(statuses, first_ok));
        });
    }

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::ProbeDone(results, domain) => {
                    self.probing = false;
                    self.probe_results = results;
                    match domain {
                        Some(d) if self.session.is_none() => {
                            self.session = None;
                            self.screen = Screen::Login;
                            self.manual_domain = d;
                        }
                        _ => {
                            if self.session.is_none() {
                                self.screen = Screen::Login;
                            }
                        }
                    }
                }
                Msg::LoginDone(res) => {
                    self.logging_in = false;
                    match res {
                        Ok(sess) => {
                            save_session(&sess);
                            self.session = Some(sess);
                            self.screen = Screen::Search;
                            self.password.clear();
                        }
                        Err(e) => self.login_error = e,
                    }
                }
                Msg::RegisterDone(res) => {
                    self.reg_busy = false;
                    match res {
                        Ok(()) => {
                            self.reg_stage = RegStage::CodeSent;
                            self.reg_info = "Check your email for the confirmation code.".into();
                        }
                        Err(e) => self.login_error = e,
                    }
                }
                Msg::ConfirmDone(res) => {
                    self.reg_busy = false;
                    match res {
                        Ok(sess) => {
                            save_session(&sess);
                            self.session = Some(sess);
                            self.screen = Screen::Search;
                            self.password.clear();
                            self.reg_stage = RegStage::Idle;
                        }
                        Err(e) => self.login_error = e,
                    }
                }
                Msg::ResendDone(res) => {
                    self.reg_busy = false;
                    match res {
                        Ok(()) => self.reg_info = "Code resent — check your email.".into(),
                        Err(e) => self.login_error = e,
                    }
                }
                Msg::SearchDone(res) => {
                    self.searching = false;
                    match res {
                        Ok(page) => {
                            self.results = page;
                            self.search_error.clear();
                        }
                        Err(e) => self.search_error = e,
                    }
                }
                Msg::DetailsDone(res) => {
                    self.fetching_details = false;
                    self.details = Some(res);
                }
                Msg::DlProgress { title, written, total } => {
                    self.active_dl = Some((title, written, total));
                }
                Msg::DlDone(res) => {
                    self.active_dl = None;
                    match res {
                        Ok((title, path, size)) => {
                            self.dl_error.clear();
                            self.downloads.insert(0, DownloadEntry { title, path, size });
                        }
                        Err(e) => self.dl_error = e,
                    }
                }
            }
        }
    }

    fn chosen_domain(&self) -> String {
        if !self.manual_domain.trim().is_empty() {
            api::normalize(self.manual_domain.trim())
        } else {
            self.probe_results
                .iter()
                .find(|(_, ok)| *ok)
                .map(|(d, _)| d.clone())
                .unwrap_or_else(|| "https://z-lib.gd".into())
        }
    }

    fn proxy_opt(&self) -> Option<String> {
        if self.proxy.trim().is_empty() { None } else { Some(self.proxy.trim().to_string()) }
    }

    fn do_register(&mut self) {
        if self.reg_busy {
            return;
        }
        self.reg_busy = true;
        self.login_error.clear();
        let tx = self.tx.clone();
        let domain = self.chosen_domain();
        let proxy = self.proxy_opt();
        let email = self.email.clone();
        let password = self.password.clone();
        let name = self.reg_nickname.clone();
        std::thread::spawn(move || {
            let res = api::register(&domain, &proxy, &email, &password, &name);
            let _ = tx.send(Msg::RegisterDone(res));
        });
    }

    fn do_confirm(&mut self) {
        if self.reg_busy {
            return;
        }
        self.reg_busy = true;
        self.login_error.clear();
        let tx = self.tx.clone();
        let domain = self.chosen_domain();
        let proxy = self.proxy_opt();
        let email = self.email.clone();
        let password = self.password.clone();
        let name = self.reg_nickname.clone();
        let code = self.reg_code.trim().to_string();
        std::thread::spawn(move || {
            let res = api::confirm_code(&domain, &proxy, &email, &password, &name, &code);
            let _ = tx.send(Msg::ConfirmDone(res));
        });
    }

    fn do_resend(&mut self) {
        if self.reg_busy {
            return;
        }
        self.reg_busy = true;
        let tx = self.tx.clone();
        let domain = self.chosen_domain();
        let proxy = self.proxy_opt();
        let email = self.email.clone();
        let password = self.password.clone();
        let name = self.reg_nickname.clone();
        std::thread::spawn(move || {
            let res = api::resend_code(&domain, &proxy, &email, &password, &name);
            let _ = tx.send(Msg::ResendDone(res));
        });
    }

    fn do_login(&mut self) {
        if self.logging_in {
            return;
        }
        self.logging_in = true;
        self.login_error.clear();
        let tx = self.tx.clone();
        let domain = self.chosen_domain();
        let proxy = self.proxy_opt();
        if self.use_cookies {
            let id = self.cookie_userid.clone();
            let key = self.cookie_userkey.clone();
            std::thread::spawn(move || {
                let res = api::login_with_cookies(&domain, &proxy, &id, &key);
                let _ = tx.send(Msg::LoginDone(res));
            });
        } else {
            let email = self.email.clone();
            let password = self.password.clone();
            std::thread::spawn(move || {
                let res = api::login(&domain, &proxy, &email, &password);
                let _ = tx.send(Msg::LoginDone(res));
            });
        }
    }

    fn do_search(&mut self, page: u32) {
        if self.searching || self.query.trim().is_empty() {
            return;
        }
        self.searching = true;
        self.search_error.clear();
        self.page = page;
        let tx = self.tx.clone();
        let sess = self.session.clone().expect("search requires session");
        let proxy = if self.proxy.trim().is_empty() { None } else { Some(self.proxy.trim().to_string()) };
        let query = self.query.trim().to_string();
        let filters = self.filters.clone();
        std::thread::spawn(move || {
            let res = api::search(&sess, &proxy, &query, page, &filters);
            let _ = tx.send(Msg::SearchDone(res));
        });
    }

    fn do_download(&mut self, book: api::Book) {
        if self.active_dl.is_some() {
            self.dl_error = "a download is already in progress".into();
            return;
        }
        self.dl_error.clear();
        let tx = self.tx.clone();
        let sess = self.session.clone().expect("download requires session");
        let proxy = if self.proxy.trim().is_empty() { None } else { Some(self.proxy.trim().to_string()) };
        let dest = self.download_dir.clone();
        let title = book.title.clone();
        self.active_dl = Some((title.clone(), 0, None));
        std::thread::spawn(move || {
            let tx2 = tx.clone();
            let title2 = title.clone();
            let res = api::download_book(&sess, &proxy, &book, &dest, move |written, total| {
                let _ = tx2.send(Msg::DlProgress {
                    title: title2.clone(),
                    written,
                    total,
                });
            });
            let msg = res.map(|p| (title, p.to_string_lossy().into_owned(), std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)));
            let _ = tx.send(Msg::DlDone(msg));
        });
    }

    fn fetch_details(&mut self, book: &api::Book) {
        if self.fetching_details {
            return;
        }
        self.fetching_details = true;
        self.selected = Some(book.clone());
        self.details = None;
        let tx = self.tx.clone();
        let sess = self.session.clone().expect("details requires session");
        let proxy = if self.proxy.trim().is_empty() { None } else { Some(self.proxy.trim().to_string()) };
        let id = book.id.clone();
        let hash = book.hash.clone();
        std::thread::spawn(move || {
            let res = api::book_details(&sess, &proxy, &id, &hash);
            let _ = tx.send(Msg::DetailsDone(res));
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_messages();
        ctx.request_repaint_after(Duration::from_millis(200));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("zlib-lite");
                ui.separator();
                match &self.session {
                    Some(s) => {
                        ui.label(format!("{}  •  {}", s.domain, if s.email.is_empty() { "logged in" } else { &s.email }));
                        if ui.small_button("logout").clicked() {
                            self.session = None;
                            self.results = api::SearchPage::default();
                            self.selected = None;
                            self.details = None;
                            let _ = std::fs::remove_file(config_dir().join("session.json"));
                            self.start_probe();
                        }
                    }
                    None => {
                        ui.label(if self.probing { "connecting…" } else { "not connected" });
                    }
                }
            });
        });

        // downloads bar
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Save to:");
                ui.monospace(self.download_dir.to_string_lossy());
                if ui.small_button("change…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().set_directory(&self.download_dir).pick_folder() {
                        self.download_dir = d;
                        save_download_dir(&self.download_dir);
                    }
                }
            });
            if !self.downloads.is_empty() {
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(90.0)
                    .show(ui, |ui| {
                        for dl in &self.downloads {
                            ui.horizontal(|ui| {
                                ui.strong(&dl.title);
                                ui.monospace(format!("{} MB", dl.size / 1_048_576));
                                if ui.small_button("open folder").clicked() {
                                    open_containing(&dl.path);
                                }
                            });
                        }
                    });
            }
            if let Some((title, written, total)) = &self.active_dl {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Downloading:");
                    ui.strong(title);
                    let frac = total.map(|t| *written as f32 / t as f32).unwrap_or(0.0);
                    ui.add(
                        egui::ProgressBar::new(frac.max(0.01))
                            .show_percentage()
                            .text(format!("{} MB", written / 1_048_576)),
                    );
                });
            }
            if !self.dl_error.is_empty() {
                ui.colored_label(egui::Color32::RED, &self.dl_error);
            }
        });

        match self.screen {
            Screen::Connecting => self.ui_connecting(ctx),
            Screen::Login => self.ui_login(ctx),
            Screen::Search => self.ui_search(ctx),
        }
    }
}

impl App {
    fn ui_connecting(&self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.heading(if self.probing { "Finding a reachable Z-Library mirror…" } else { "No mirror reachable" });
                ui.add_space(16.0);
            });
            for (domain, ok) in &self.probe_results {
                ui.horizontal(|ui| {
                    ui.monospace(domain);
                    ui.label(if *ok {
                        "✅ reachable"
                    } else {
                        "—"
                    });
                });
            }
        });
    }

    fn ui_login(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.heading("Z-Library");
                ui.add_space(8.0);
            });

            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.auth_mode, AuthMode::SignIn, "Sign in");
                ui.selectable_value(&mut self.auth_mode, AuthMode::Register, "Create account");
                ui.separator();
                if ui.small_button("rescan mirrors").clicked() {
                    self.start_probe();
                }
            });
            ui.separator();

            match self.auth_mode {
                AuthMode::SignIn => self.ui_signin(ui),
                AuthMode::Register => self.ui_register(ui),
            }
        });
    }

    fn ui_signin(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("login").num_columns(2).show(ui, |ui| {
            ui.checkbox(&mut self.use_cookies, "Use session cookies instead");
            ui.label("(if password login is disabled for your account)");
            ui.end_row();

            if self.use_cookies {
                ui.label("remix_userid:");
                ui.text_edit_singleline(&mut self.cookie_userid);
                ui.end_row();
                ui.label("remix_userkey:");
                ui.text_edit_singleline(&mut self.cookie_userkey);
                ui.end_row();
            } else {
                ui.label("Email:");
                ui.text_edit_singleline(&mut self.email);
                ui.end_row();
                ui.label("Password:");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                ui.end_row();
            }
            ui.label("Domain:");
            ui.text_edit_singleline(&mut self.manual_domain);
            ui.end_row();
            ui.label("Proxy (optional):");
            ui.text_edit_singleline(&mut self.proxy);
            ui.end_row();
        });

        let reachable: Vec<String> = self.probe_results.iter().filter(|(_, ok)| *ok).map(|(d, _)| d.clone()).collect();
        if !reachable.is_empty() {
            ui.small(format!("reachable mirrors: {}", reachable.join(", ")));
        }

        ui.add_space(10.0);
        let busy = self.logging_in;
        let btn = egui::Button::new(if busy { "Signing in…" } else { "Sign in" });
        if ui.add_enabled(!busy, btn).clicked() {
            self.do_login();
        }
        if !self.login_error.is_empty() {
            ui.colored_label(egui::Color32::RED, &self.login_error);
            ui.small("If your account cannot use API password login, enable 'Use session cookies' and paste remix_userid / remix_userkey from a logged-in browser session (F12 → Application/Storage → Cookies on z-lib.ws).");
        }
    }

    fn ui_register(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("register").num_columns(2).show(ui, |ui| {
            ui.label("Email:");
            ui.text_edit_singleline(&mut self.email);
            ui.end_row();
            ui.label("Password:");
            ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
            ui.end_row();
            ui.label("Nickname (optional):");
            ui.text_edit_singleline(&mut self.reg_nickname);
            ui.end_row();
            ui.label("Domain:");
            ui.text_edit_singleline(&mut self.manual_domain);
            ui.end_row();
            ui.label("Proxy (optional):");
            ui.text_edit_singleline(&mut self.proxy);
            ui.end_row();
        });

        match self.reg_stage {
            RegStage::Idle => {
                let busy = self.reg_busy;
                if ui.add_enabled(!busy, egui::Button::new("Create account")).clicked() {
                    self.do_register();
                }
            }
            RegStage::CodeSent => {
                ui.label("Confirmation code from email:");
                let mut submit = false;
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.reg_code).desired_width(160.0),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                let busy = self.reg_busy;
                if ui.add_enabled(!busy && !self.reg_code.trim().is_empty(), egui::Button::new("Confirm")).clicked() {
                    submit = true;
                }
                if submit {
                    self.do_confirm();
                }
                if ui.add_enabled(!busy, egui::Button::new("Resend code")).clicked() {
                    self.do_resend();
                }
            }
        }
        if !self.reg_info.is_empty() {
            ui.colored_label(egui::Color32::GREEN, &self.reg_info);
        }
        if !self.login_error.is_empty() {
            ui.colored_label(egui::Color32::RED, &self.login_error);
        }
    }

    fn ui_search(&mut self, ctx: &egui::Context) {
        // details side panel
        let show_details = self.selected.is_some();
        if show_details {
            egui::SidePanel::right("details")
                .min_width(320.0)
                .default_width(360.0)
                .show(ctx, |ui| self.ui_details(ui));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // search bar
            ui.horizontal(|ui| {
                let mut submit = false;
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search title, author, ISBN…")
                        .desired_width(ui.available_width() - 260.0),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                let busy = self.searching;
                if ui
                    .add_enabled(!busy, egui::Button::new("Search"))
                    .clicked()
                {
                    submit = true;
                }
                if submit {
                    self.do_search(1);
                }
            });
            ui.add_space(4.0);

            // filters row
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.filters.exact, "exact");
                ui.label("year");
                ui.add(
                    egui::TextEdit::singleline(&mut self.filters.year_from)
                        .desired_width(50.0)
                        .hint_text("from"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.filters.year_to)
                        .desired_width(50.0)
                        .hint_text("to"),
                );
                egui::ComboBox::from_id_salt("ext")
                    .selected_text(if self.filters.extension.is_empty() { "any format".into() } else { self.filters.extension.clone() })
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for ext in ["", "EPUB", "PDF", "MOBI", "AZW3", "FB2", "TXT", "RTF", "DJVU"] {
                            ui.selectable_value(&mut self.filters.extension, ext.into(), if ext.is_empty() { "any format" } else { ext });
                        }
                    });
                ui.text_edit_singleline(&mut self.filters.language)
                    .on_hover_text("language, e.g. english");
                egui::ComboBox::from_id_salt("order")
                    .selected_text(order_label(&self.filters.order))
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for (v, label) in [
                            ("bestmatch", "best match"),
                            ("popular", "popular"),
                            ("date_created", "newest"),
                            ("date_updated", "recently updated"),
                            ("year", "year"),
                            ("titleA", "title A–Z"),
                        ] {
                            ui.selectable_value(&mut self.filters.order, v.into(), label);
                        }
                    });
            });
            ui.add_space(6.0);

            if !self.search_error.is_empty() {
                ui.colored_label(egui::Color32::RED, &self.search_error);
                ui.add_space(4.0);
            }

            let results = self.results.clone();
            if results.books.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(if self.searching {
                        "Searching…"
                    } else {
                        "Enter a query and press Search."
                    });
                });
            } else {
                ui.label(format!(
                    "Page {} of {} — {} results",
                    results.page, results.total_pages, results.books.len()
                ));
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for book in &results.books {
                        let is_sel = self
                            .selected
                            .as_ref()
                            .map(|s| s.id == book.id && s.hash == book.hash)
                            .unwrap_or(false);
                        let row = egui::SelectableLabel::new(
                            is_sel,
                            format!(
                                "📘 {} — {} ({}) [{}] {}",
                                book.title,
                                book.author,
                                book.year,
                                book.extension.to_uppercase(),
                                book.size
                            ),
                        );
                        if ui.add(row).clicked() {
                            self.fetch_details(book);
                        }
                    }
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.page > 1 && !self.searching, egui::Button::new("← prev"))
                        .clicked()
                    {
                        self.do_search(self.page - 1);
                    }
                    if ui
                        .add_enabled(
                            self.page < self.results.total_pages && !self.searching,
                            egui::Button::new("next →"),
                        )
                        .clicked()
                    {
                        self.do_search(self.page + 1);
                    }
                });
            }
        });
    }

    fn ui_details(&mut self, ui: &mut egui::Ui) {
        let book = self.selected.clone().unwrap();
        ui.heading(&book.title);
        ui.label(book.author.clone());
        ui.add_space(6.0);
        egui::Grid::new("book").num_columns(2).show(ui, |ui| {
            ui.label("Year:"); ui.monospace(&book.year); ui.end_row();
            ui.label("Language:"); ui.monospace(&book.language); ui.end_row();
            ui.label("Format:"); ui.monospace(&book.extension); ui.end_row();
            ui.label("Pages:"); ui.monospace(&book.pages); ui.end_row();
            ui.label("Size:"); ui.monospace(&book.size); ui.end_row();
            ui.label("ID:"); ui.monospace(format!("{}:{}", book.id, book.hash)); ui.end_row();
        });
        ui.add_space(8.0);

        match &self.details {
            None => {
                if self.fetching_details {
                    ui.label("Loading details…");
                }
            }
            Some(Err(e)) => {
                ui.colored_label(egui::Color32::RED, e);
            }
            Some(Ok(d)) => {
                if !d.description.is_empty() {
                    ui.collapsing("Description", |ui| {
                        ui.label(&d.description);
                    });
                }
                ui.add_space(8.0);
                let busy = self.active_dl.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new("⬇ Download").min_size(egui::vec2(ui.available_width(), 28.0)))
                    .clicked()
                {
                    self.do_download(d.clone());
                }
            }
        }
        ui.add_space(10.0);
        if ui.button("close").clicked() {
            self.selected = None;
            self.details = None;
        }
    }
}

fn order_label(order: &str) -> String {
    match order {
        "popular" => "popular".into(),
        "date_created" => "newest".into(),
        "date_updated" => "recently updated".into(),
        "year" => "year".into(),
        "titleA" => "title A–Z".into(),
        _ => "best match".into(),
    }
}

#[cfg(unix)]
fn open_containing(path: &str) {
    use std::os::unix::process::CommandExt;
    let _ = std::process::Command::new("xdg-open")
        .arg(std::path::Path::new(path).parent().unwrap_or(std::path::Path::new(".")))
        .process_group(0)
        .spawn();
}

#[cfg(windows)]
fn open_containing(path: &str) {
    let _ = std::process::Command::new("explorer")
        .arg(std::path::Path::new(path).parent().unwrap_or(std::path::Path::new(".")))
        .spawn();
}
