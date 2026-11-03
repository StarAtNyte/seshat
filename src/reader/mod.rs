//! The reading view: paginated/scrolled reflowable books and fixed-layout
//! PDF/CBZ, with TOC, search, bookmarks, highlights + notes, lookups, TTS,
//! footnote popups, navigation history, reading stats and KOSync.

pub mod layout;

mod chrome;
mod fixed;
mod flow;
mod panels;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Align2, Color32, Key, Pos2, Rect, RichText};

use crate::book::{Book, Content, LinkTarget, Loc};
use crate::fonts::Installed;
use crate::lookup;
use crate::store::{self, AppSettings, Annotation, BookData, Bookmark, HlColor, HlStyle, ReaderSettings};
use crate::texload::TexLoader;
use crate::tts;
use crate::ui::{self, icon_button, icon_toggle, Icon, Palette, ACCENT};

use layout::SectionLayout;

#[derive(Clone, Debug)]
pub struct Hit {
    pub loc: Loc,
    pub len: usize,
    pub excerpt: String,
    pub mstart: usize,
    pub chapter: String,
}

pub enum Msg {
    Search { gen: u64, hits: Vec<Hit>, done: bool },
    PdfTexts(Arc<Vec<String>>),
    PdfAspects(Vec<f32>),
    Tts(tts::Event),
    Lookup { gen: u64, res: Result<lookup::Answer, String> },
    SyncPulled(Result<Option<crate::kosync::Progress>, String>),
    SyncPushed(Result<(), String>),
}

pub struct Env<'a> {
    pub app: &'a mut AppSettings,
    pub families: &'a [String],
    pub installed: &'a Installed,
}

pub enum Action {
    Close,
    InstallFont { family: String, cjk: bool },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Toc,
    Notes,
    Bookmarks,
    Search,
}

struct LookupUi {
    kind: lookup::Kind,
    query: String,
    gen: u64,
    res: Option<Result<lookup::Answer, String>>,
}

type Pt = (usize, usize);

pub struct Reader {
    pub id: String,
    pub path: PathBuf,
    pub book: Arc<Book>,
    pub data: BookData,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    dirty: bool,
    last_save: Instant,

    // flow state
    pub loc: Loc,
    layout: Option<SectionLayout>,
    page: usize,
    scroll_y: f32,
    overscroll: f32,
    go_end: bool,
    history: Vec<Loc>,
    wheel_acc: f32,
    last_turn: Instant,
    step: usize,
    view_h: f32,
    last_layout: store::Layout,
    tex: TexLoader,

    // chrome / panels
    side: Option<Side>,
    settings_open: bool,
    chrome: bool,
    fullscreen: bool,
    focus_search: bool,
    toc_scroll: bool,
    font_filter: String,

    // selection & popups
    drag_anchor: Option<Pt>,
    sel: Option<(Pt, Pt)>,
    sel_popup: Option<Pos2>,
    hl_popup: Option<(u64, Pos2)>,
    note_edit: Option<(u64, String)>,
    footnote: Option<(Loc, String, Pos2)>,
    lookup: Option<LookupUi>,
    lookup_gen: u64,

    // search
    search_q: String,
    search_hits: Vec<Hit>,
    search_running: bool,
    search_gen: u64,
    search_cur: Option<usize>,
    search_stop: Arc<AtomicBool>,

    // tts
    speaker: Option<tts::Speaker>,
    tts_at: Option<(Loc, usize)>,
    tts_error: String,

    // stats
    last_active: Instant,
    last_tick: Instant,
    turn_mark: (Instant, Loc),

    // sync
    sync_prompt: Option<crate::kosync::Progress>,
    pub toast: Option<(String, Instant)>,

    fx: fixed::FixedState,
    cjk_checked: bool,
    goto_text: String,
    hl_mode: Option<HlColor>,
    auto: bool,
    auto_last: Instant,
    opened_at: Option<Instant>,
    turn: Option<Turn>,
    motion: u8,
    motion_hint: bool,
    notebook: bool,
    quick_note: String,
    /// Toolbar pinned open (otherwise it slides in on demand).
    tools: bool,
}

impl Reader {
    pub fn new(ctx: &egui::Context, id: String, path: PathBuf, book: Arc<Book>, app: &AppSettings) -> Reader {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut data = BookData::load(&id);
        data.path = path.to_string_lossy().into_owned();
        let loc = data.loc.unwrap_or_default();
        let mut fx = fixed::FixedState::default();
        if let Content::Fixed(f) = &book.content {
            fx.page = loc.section.min(f.pages.saturating_sub(1));
            fx.zoom = data.fixed_zoom;
            fx.rtl = data.fixed_rtl;
            fx.aspects = vec![1.414; f.pages];
            if matches!(f.kind, crate::book::FixedKind::Pdf) {
                let tx2 = tx.clone();
                let p = f.path.clone();
                let ctx2 = ctx.clone();
                std::thread::spawn(move || {
                    if let Ok(pdf) = crate::book::pdf::load(&p) {
                        let _ = tx2.send(Msg::PdfAspects(crate::book::pdf::page_aspects(&pdf)));
                        ctx2.request_repaint();
                    }
                });
            }
        }
        let mut r = Reader {
            id,
            path,
            book,
            data,
            tx,
            rx,
            ctx: ctx.clone(),
            dirty: false,
            last_save: Instant::now(),
            loc,
            layout: None,
            page: 0,
            scroll_y: 0.0,
            overscroll: 0.0,
            go_end: false,
            history: Vec::new(),
            wheel_acc: 0.0,
            last_turn: Instant::now(),
            step: 1,
            view_h: 600.0,
            last_layout: app.reader.layout,
            tex: TexLoader::new(ctx, 2),
            side: None,
            settings_open: false,
            chrome: false,
            fullscreen: false,
            focus_search: false,
            toc_scroll: false,
            font_filter: String::new(),
            drag_anchor: None,
            sel: None,
            sel_popup: None,
            hl_popup: None,
            note_edit: None,
            footnote: None,
            lookup: None,
            lookup_gen: 0,
            search_q: String::new(),
            search_hits: Vec::new(),
            search_running: false,
            search_gen: 0,
            search_cur: None,
            search_stop: Arc::new(AtomicBool::new(false)),
            speaker: None,
            tts_at: None,
            tts_error: String::new(),
            last_active: Instant::now(),
            last_tick: Instant::now(),
            turn_mark: (Instant::now(), loc),
            sync_prompt: None,
            toast: None,
            fx,
            cjk_checked: false,
            goto_text: String::new(),
            hl_mode: None,
            auto: false,
            auto_last: Instant::now(),
            opened_at: None,
            turn: None,
            motion: 0,
            motion_hint: false,
            notebook: false,
            quick_note: String::new(),
            tools: false,
        };
        r.data.last_read = store::now();
        r.clamp_loc();
        if app.kosync.enabled && !app.kosync.username.is_empty() {
            let (cfg, proxy, id, tx, ctx) = (app.kosync.clone(), app.proxy_opt(), r.id.clone(), r.tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Msg::SyncPulled(crate::kosync::pull(&cfg, &proxy, &id)));
                ctx.request_repaint();
            });
        }
        r
    }

    fn clamp_loc(&mut self) {
        if let Some(f) = self.book.flow() {
            if self.loc.section >= f.sections.len() {
                self.loc = Loc::default();
            }
            let n = f.sections[self.loc.section].blocks.len();
            if self.loc.block >= n {
                self.loc.block = n.saturating_sub(1);
                self.loc.offset = 0;
            }
        }
    }

    pub fn effective(&self, global: &ReaderSettings) -> ReaderSettings {
        self.data.settings.clone().unwrap_or_else(|| global.clone())
    }

    pub fn fraction(&self) -> f32 {
        match &self.book.content {
            Content::Flow(f) => f.fraction(self.loc),
            Content::Fixed(f) => (self.fx.page + 1) as f32 / f.pages.max(1) as f32,
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn save(&mut self) {
        self.data.loc = Some(match &self.book.content {
            Content::Flow(_) => self.loc,
            Content::Fixed(_) => Loc::new(self.fx.page, 0, 0),
        });
        self.data.fraction = self.fraction();
        self.data.last_read = store::now();
        self.data.fixed_zoom = self.fx.zoom;
        self.data.fixed_rtl = self.fx.rtl;
        self.data.save(&self.id);
        self.dirty = false;
        self.last_save = Instant::now();
    }

    /// Called when leaving the book.
    pub fn close(&mut self, app: &AppSettings) {
        if let Some(s) = self.speaker.take() {
            s.stop();
        }
        self.search_stop.store(true, Ordering::Relaxed);
        self.save();
        if app.kosync.enabled && !app.kosync.username.is_empty() {
            let (cfg, proxy, id) = (app.kosync.clone(), app.proxy_opt(), self.id.clone());
            let (prog, pct) = self.sync_progress();
            std::thread::spawn(move || {
                let _ = crate::kosync::push(&cfg, &proxy, &id, &prog, pct);
            });
        }
    }

    fn sync_progress(&self) -> (String, f32) {
        match &self.book.content {
            Content::Flow(f) => {
                let spine = f.spine_sections.iter().rposition(|&s| s <= self.loc.section).unwrap_or(0);
                (crate::kosync::encode_flow(spine, self.loc.section, self.loc.block, self.loc.offset), f.fraction(self.loc))
            }
            Content::Fixed(f) => ((self.fx.page + 1).to_string(), (self.fx.page + 1) as f32 / f.pages.max(1) as f32),
        }
    }

    fn apply_remote(&mut self, p: &crate::kosync::Progress) {
        use crate::kosync::Decoded;
        let target = match (&self.book.content, crate::kosync::decode(&p.progress)) {
            (Content::Flow(f), Some(Decoded::Exact(s, b, o))) if s < f.sections.len() => Loc::new(s, b, o),
            (Content::Flow(f), Some(Decoded::Spine(i))) if (p.percentage - f.fraction(Loc::new(f.spine_sections.get(i).copied().unwrap_or(0), 0, 0))).abs() > 0.05 => f.loc_at_fraction(p.percentage),
            (Content::Flow(f), Some(Decoded::Spine(i))) => Loc::new(f.spine_sections.get(i).copied().unwrap_or(0), 0, 0),
            (Content::Flow(f), _) => f.loc_at_fraction(p.percentage),
            (Content::Fixed(f), Some(Decoded::Page(pg))) => Loc::new(pg.min(f.pages - 1), 0, 0),
            (Content::Fixed(f), _) => Loc::new(((p.percentage * f.pages as f32) as usize).min(f.pages - 1), 0, 0),
        };
        self.jump(target, true);
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Search { gen, hits, done } => {
                    if gen == self.search_gen {
                        self.search_hits.extend(hits);
                        if done {
                            self.search_running = false;
                        }
                    }
                }
                Msg::PdfTexts(t) => self.fx.texts = Some(t),
                Msg::PdfAspects(a) => {
                    if !a.is_empty() {
                        self.fx.aspects = a;
                    }
                }
                Msg::Tts(ev) => match ev {
                    tts::Event::At(loc, end) => {
                        self.tts_at = Some((loc, end));
                        self.follow_tts(loc);
                    }
                    tts::Event::Done => {
                        self.speaker = None;
                        self.tts_at = None;
                    }
                    tts::Event::Error(e) => {
                        self.speaker = None;
                        self.tts_error = e;
                    }
                },
                Msg::Lookup { gen, res } => {
                    if let Some(l) = self.lookup.as_mut() {
                        if l.gen == gen {
                            l.res = Some(res);
                        }
                    }
                }
                Msg::SyncPulled(r) => match r {
                    Ok(Some(p)) => {
                        let local = self.fraction();
                        if (p.percentage - local).abs() > 0.003 {
                            self.sync_prompt = Some(p);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => self.toast = Some((format!("Sync: {e}"), Instant::now())),
                },
                Msg::SyncPushed(r) => {
                    if let Err(e) = r {
                        self.toast = Some((format!("Sync: {e}"), Instant::now()));
                    }
                }
            }
        }
    }

    pub fn jump(&mut self, loc: Loc, push_history: bool) {
        let cur = match &self.book.content {
            Content::Flow(_) => self.loc,
            Content::Fixed(_) => Loc::new(self.fx.page, 0, 0),
        };
        if push_history && cur != loc {
            self.history.push(cur);
            if self.history.len() > 60 {
                self.history.remove(0);
            }
        }
        match &self.book.content {
            Content::Flow(_) => {
                self.loc = loc;
                self.clamp_loc();
                if let Some(l) = &self.layout {
                    if l.section == self.loc.section {
                        self.page = l.page_for(self.loc.block, self.loc.offset);
                        self.page -= self.page % self.step.max(1);
                        self.scroll_y = (l.y_of(self.loc.block, self.loc.offset) - 8.0).max(0.0);
                    }
                }
            }
            Content::Fixed(f) => {
                self.fx.page = loc.section.min(f.pages.saturating_sub(1));
                self.fx.scroll = 0.0;
            }
        }
        self.clear_selection();
        self.mark_dirty();
    }

    fn back(&mut self) {
        if let Some(l) = self.history.pop() {
            self.jump(l, false);
        }
    }

    fn clear_selection(&mut self) {
        self.sel = None;
        self.sel_popup = None;
        self.hl_popup = None;
        self.drag_anchor = None;
    }

    fn chapter_title(&self, loc: Loc) -> String {
        if let Some(i) = self.book.toc_index_for(loc) {
            return self.book.toc[i].title.clone();
        }
        self.book.flow().and_then(|f| f.sections.get(loc.section)).map(|s| s.title.clone()).unwrap_or_default()
    }

    fn selection_text(&self) -> Option<String> {
        let ((b0, c0), (b1, c1)) = self.sel?;
        let flow = self.book.flow()?;
        let sec = flow.sections.get(self.loc.section)?;
        let mut out = String::new();
        for b in b0..=b1 {
            let t = sec.blocks.get(b)?.text();
            let s = if b == b0 { c0 } else { 0 };
            let e = if b == b1 { c1 } else { usize::MAX };
            let part: String = t.chars().skip(s).take(e.saturating_sub(s)).collect();
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&part);
        }
        Some(out.trim().to_string())
    }

    fn bookmark_here(&self) -> Option<usize> {
        match &self.book.content {
            Content::Flow(_) => {
                let l = self.layout.as_ref()?;
                if l.section != self.loc.section {
                    return None;
                }
                let (sb, so) = l.page_start(self.page);
                let (eb, eo) = l.page_end((self.page + self.step - 1).min(l.pages.len() - 1));
                let s = Loc::new(self.loc.section, sb, so);
                let e = Loc::new(self.loc.section, eb, eo);
                self.data.bookmarks.iter().position(|b| b.loc >= s && b.loc < e)
            }
            Content::Fixed(_) => self.data.bookmarks.iter().position(|b| b.loc.section == self.fx.page),
        }
    }

    fn toggle_bookmark(&mut self) {
        if let Some(i) = self.bookmark_here() {
            self.data.bookmarks.remove(i);
        } else {
            let (loc, excerpt) = match &self.book.content {
                Content::Flow(f) => {
                    let t = f.sections.get(self.loc.section).and_then(|s| s.blocks.get(self.loc.block)).map(|b| b.text()).unwrap_or_default();
                    (self.loc, t.chars().skip(self.loc.offset).take(120).collect::<String>())
                }
                Content::Fixed(_) => (Loc::new(self.fx.page, 0, 0), format!("Page {}", self.fx.page + 1)),
            };
            let chapter = self.chapter_title(loc);
            self.data.bookmarks.push(Bookmark { loc, excerpt, chapter, created: store::now() });
            self.data.bookmarks.sort_by_key(|b| b.loc);
        }
        self.mark_dirty();
    }

    fn add_annotation(&mut self, color: HlColor, with_note: bool) {
        let Some(((b0, c0), (b1, c1))) = self.sel else { return };
        let text = self.selection_text().unwrap_or_default();
        let id = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) as u64).max(self.data.annotations.iter().map(|a| a.id + 1).max().unwrap_or(1));
        let start = Loc::new(self.loc.section, b0, c0);
        let a = Annotation { id, start, end: Loc::new(self.loc.section, b1, c1), color, style: HlStyle::Highlight, note: String::new(), text, chapter: self.chapter_title(start), created: store::now() };
        self.data.annotations.push(a);
        self.data.annotations.sort_by_key(|a| a.start);
        if with_note {
            self.note_edit = Some((id, String::new()));
        }
        self.clear_selection();
        self.mark_dirty();
    }

    fn start_lookup(&mut self, kind: lookup::Kind, text: String, app: &AppSettings) {
        self.lookup_gen += 1;
        let gen = self.lookup_gen;
        self.lookup = Some(LookupUi { kind, query: text.clone(), gen, res: None });
        let (tx, ctx, lang, to, proxy) = (self.tx.clone(), self.ctx.clone(), self.book.meta.language.clone(), app.translate_to.clone(), app.proxy_opt());
        std::thread::spawn(move || {
            let res = lookup::run(kind, &text, &lang, &to, &proxy);
            let _ = tx.send(Msg::Lookup { gen, res });
            ctx.request_repaint();
        });
        self.sel_popup = None;
    }

    pub fn start_tts(&mut self, from: Loc, rate: f32) {
        if let Some(s) = self.speaker.take() {
            s.stop();
        }
        self.tts_error.clear();
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.speaker = tts::start(self.book.clone(), from, rate, move |ev| {
            let _ = tx.send(Msg::Tts(ev));
            ctx.request_repaint();
        });
        if self.speaker.is_none() {
            self.tts_error = if cfg!(target_os = "linux") { "No speech engine found. Install speech-dispatcher (spd-say) or espeak-ng.".into() } else { "No speech engine available.".into() };
        }
    }

    fn stop_tts(&mut self) {
        if let Some(s) = self.speaker.take() {
            s.stop();
        }
        self.tts_at = None;
    }

    fn follow_tts(&mut self, loc: Loc) {
        if loc.section != self.loc.section {
            self.loc = loc;
            return;
        }
        if let Some(l) = &self.layout {
            let p = l.page_for(loc.block, loc.offset);
            let p = p - p % self.step.max(1);
            if p != self.page {
                self.page = p;
                let (b, o) = l.page_start(p);
                self.loc = Loc::new(loc.section, b, o);
            }
            let y = l.y_of(loc.block, loc.offset);
            if y < self.scroll_y || y > self.scroll_y + self.view_h * 0.8 {
                self.scroll_y = (y - self.view_h * 0.2).max(0.0);
            }
        }
    }

    fn run_search(&mut self) {
        self.search_stop.store(true, Ordering::Relaxed);
        self.search_stop = Arc::new(AtomicBool::new(false));
        self.search_gen += 1;
        self.search_hits.clear();
        self.search_cur = None;
        let q = self.search_q.trim().to_string();
        if q.chars().count() < 2 {
            self.search_running = false;
            return;
        }
        self.search_running = true;
        let (tx, ctx, gen, stop, book) = (self.tx.clone(), self.ctx.clone(), self.search_gen, self.search_stop.clone(), self.book.clone());
        let texts = self.fx.texts.clone();
        std::thread::spawn(move || {
            let lower = |s: &str| -> Vec<char> { s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect() };
            let needle = lower(&q);
            let find_all = |hay: &[char]| -> Vec<usize> {
                let mut v = Vec::new();
                if hay.len() < needle.len() {
                    return v;
                }
                let mut i = 0;
                while i + needle.len() <= hay.len() {
                    if hay[i..i + needle.len()] == needle[..] {
                        v.push(i);
                        i += needle.len();
                    } else {
                        i += 1;
                    }
                }
                v
            };
            let excerpt = |orig: &[char], at: usize, len: usize| -> (String, usize) {
                let s = at.saturating_sub(40);
                let e = (at + len + 60).min(orig.len());
                let mut ex: String = orig[s..e].iter().collect();
                ex = ex.replace('\n', " ");
                let pre = if s > 0 { "…" } else { "" };
                (format!("{pre}{ex}{}", if e < orig.len() { "…" } else { "" }), at - s + pre.chars().count())
            };
            let mut total = 0;
            match &book.content {
                Content::Flow(f) => {
                    for (si, sec) in f.sections.iter().enumerate() {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        let mut hits = Vec::new();
                        for (bi, b) in sec.blocks.iter().enumerate() {
                            let t = b.text();
                            let orig: Vec<char> = t.chars().collect();
                            for at in find_all(&lower(&t)) {
                                let (ex, ms) = excerpt(&orig, at, needle.len());
                                let loc = Loc::new(si, bi, at);
                                let chapter = book.toc_index_for(loc).map(|i| book.toc[i].title.clone()).unwrap_or_else(|| sec.title.clone());
                                hits.push(Hit { loc, len: needle.len(), excerpt: ex, mstart: ms, chapter });
                            }
                        }
                        total += hits.len();
                        if !hits.is_empty() {
                            let _ = tx.send(Msg::Search { gen, hits, done: false });
                            ctx.request_repaint();
                        }
                        if total > 2000 {
                            break;
                        }
                    }
                }
                Content::Fixed(fx) => {
                    let texts = match texts {
                        Some(t) => t,
                        None => {
                            let Ok(pdf) = crate::book::pdf::load(&fx.path) else {
                                let _ = tx.send(Msg::Search { gen, hits: vec![], done: true });
                                return;
                            };
                            let mut v = Vec::with_capacity(fx.pages);
                            for p in 0..fx.pages {
                                if stop.load(Ordering::Relaxed) {
                                    return;
                                }
                                v.push(crate::book::pdf::page_text(&pdf, p));
                            }
                            let t = Arc::new(v);
                            let _ = tx.send(Msg::PdfTexts(t.clone()));
                            t
                        }
                    };
                    for (p, t) in texts.iter().enumerate() {
                        let orig: Vec<char> = t.chars().collect();
                        let hits: Vec<Hit> = find_all(&lower(t))
                            .into_iter()
                            .map(|at| {
                                let (ex, ms) = excerpt(&orig, at, needle.len());
                                Hit { loc: Loc::new(p, 0, at), len: needle.len(), excerpt: ex, mstart: ms, chapter: format!("Page {}", p + 1) }
                            })
                            .collect();
                        if !hits.is_empty() {
                            let _ = tx.send(Msg::Search { gen, hits, done: false });
                        }
                    }
                }
            }
            let _ = tx.send(Msg::Search { gen, hits: vec![], done: true });
            ctx.request_repaint();
        });
    }

    fn tick_stats(&mut self, ctx: &egui::Context, app: &mut AppSettings) {
        let now = Instant::now();
        let active_input = ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving());
        if active_input || self.speaker.is_some() {
            self.last_active = now;
        }
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(5.0);
        self.last_tick = now;
        if now.duration_since(self.last_active) < Duration::from_secs(120) {
            self.data.seconds += dt;
            *self.data.daily.entry(store::date_string(store::now())).or_insert(0.0) += dt;
        }
        // measure reading speed on forward page turns
        if self.turn_mark.1 != self.loc {
            let secs = now.duration_since(self.turn_mark.0).as_secs_f32();
            if let Some(f) = self.book.flow() {
                if self.turn_mark.1 < self.loc && self.turn_mark.1.section == self.loc.section && (4.0..300.0).contains(&secs) {
                    let chars = f.section_chars_after(self.turn_mark.1).saturating_sub(f.section_chars_after(self.loc)) as f32;
                    let cpm = chars / secs * 60.0;
                    if (200.0..6000.0).contains(&cpm) {
                        app.reading_speed_cpm = app.reading_speed_cpm * 0.9 + cpm * 0.1;
                    }
                }
            }
            self.turn_mark = (now, self.loc);
        }
        if self.dirty && self.last_save.elapsed() > Duration::from_secs(4) {
            self.save();
        }
    }

    // ---------- main entry ----------

    pub fn ui(&mut self, ctx: &egui::Context, env: &mut Env) -> Vec<Action> {
        let mut actions = Vec::new();
        self.poll();
        if self.tex.poll() {
            ctx.request_repaint();
        }
        let s = self.effective(&env.app.reader);
        let pal = ui::palette(&s);

        if !self.cjk_checked {
            self.cjk_checked = true;
            if let Some(f) = self.book.flow() {
                let sample: String = f.sections.iter().flat_map(|s| s.blocks.iter()).take(60).map(|b| b.text()).collect();
                if crate::fonts::needs_cjk(&sample) && !env.installed.cjk {
                    actions.push(Action::InstallFont { family: s.font.clone(), cjk: true });
                }
            }
        }

        self.keyboard(ctx, &s, env, &mut actions);
        self.tick_auto(ctx, &s);

        if !self.fullscreen {
            self.title_bar(ctx, &pal, &mut actions);
            if let Some(side) = self.side {
                panels::sidebar(self, ctx, side, &pal, env);
            }
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(ui::desk(&pal))).show(ctx, |ui| {
            let rect = ui.max_rect();
            match &self.book.content {
                Content::Flow(_) => self.flow_view(ui, rect, &s, &pal, env),
                Content::Fixed(_) => self.fixed_view(ui, rect, &s, &pal),
            }
        });

        self.floating_toolbar(ctx, &s, &pal, env);
        let screen = ctx.screen_rect();
        let near_bottom = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y > screen.bottom() - 64.0);
        if near_bottom {
            self.bottom_bar(ctx, &pal, env);
        }
        if !self.fullscreen {
            ui::resize_edges(ctx);
        }
        if self.speaker.is_some() || !self.tts_error.is_empty() {
            self.tts_bar(ctx, &pal, env);
        }
        if self.settings_open {
            let mut open = true;
            panels::settings_window(self, ctx, &mut open, env, &mut actions);
            self.settings_open = open;
        }
        self.popups(ctx, &pal, env);
        self.motion_hint_bar(ctx, &pal, env);
        self.sync_bar(ctx, &pal);
        if let Some((t, at)) = &self.toast {
            if at.elapsed() < Duration::from_secs(4) {
                egui::Area::new("toast".into()).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -90.0)).order(egui::Order::Tooltip).show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(t));
                });
                ctx.request_repaint_after(Duration::from_millis(500));
            } else {
                self.toast = None;
            }
        }
        self.tick_stats(ctx, env.app);
        actions
    }

    fn keyboard(&mut self, ctx: &egui::Context, s: &ReaderSettings, env: &mut Env, actions: &mut Vec<Action>) {
        let typing = ctx.wants_keyboard_input();
        let (mods, keys): (egui::Modifiers, Vec<Key>) = ctx.input(|i| {
            (i.modifiers, i.events.iter().filter_map(|e| if let egui::Event::Key { key, pressed: true, .. } = e { Some(*key) } else { None }).collect())
        });
        let copy_requested = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        if copy_requested && !typing {
            if let Some(t) = self.selection_text() {
                ctx.copy_text(t);
            }
        }
        for k in keys {
            if k == Key::Escape {
                if self.lookup.is_some() || self.footnote.is_some() || self.sel.is_some() || self.hl_popup.is_some() || self.note_edit.is_some() {
                    self.lookup = None;
                    self.footnote = None;
                    self.note_edit = None;
                    self.clear_selection();
                } else if self.settings_open {
                    self.settings_open = false;
                } else if self.side.is_some() {
                    self.side = None;
                } else if self.fullscreen {
                    self.toggle_fullscreen(ctx);
                } else if self.hl_mode.is_some() {
                    self.hl_mode = None;
                } else {
                    actions.push(Action::Close);
                }
                continue;
            }
            if typing {
                continue;
            }
            let rtl = self.fx.rtl && self.book.fixed().is_some();
            match k {
                Key::ArrowRight | Key::PageDown | Key::Space | Key::J if !(k == Key::Space && mods.shift) => {
                    if k == Key::ArrowRight && rtl {
                        self.prev_page()
                    } else {
                        self.next_page()
                    }
                }
                Key::ArrowLeft | Key::PageUp | Key::K => {
                    if k == Key::ArrowLeft && rtl {
                        self.next_page()
                    } else {
                        self.prev_page()
                    }
                }
                Key::Space => self.prev_page(),
                Key::ArrowDown => {
                    if s.layout == store::Layout::Scrolled && self.book.flow().is_some() {
                        self.scroll_by(60.0)
                    } else {
                        self.next_page()
                    }
                }
                Key::ArrowUp => {
                    if s.layout == store::Layout::Scrolled && self.book.flow().is_some() {
                        self.scroll_by(-60.0)
                    } else {
                        self.prev_page()
                    }
                }
                Key::Home => self.section_edge(false),
                Key::End => self.section_edge(true),
                Key::F | Key::K if mods.command => {
                    self.side = Some(Side::Search);
                    self.focus_search = true;
                }
                Key::B | Key::D => self.toggle_bookmark(),
                Key::T => self.tools = !self.tools,
                Key::C | Key::N => {
                    let want = if k == Key::C { Side::Toc } else { Side::Notes };
                    self.side = if self.side == Some(want) { None } else { Some(want) };
                    self.toc_scroll = true;
                }
                Key::F11 | Key::F => self.toggle_fullscreen(ctx),
                Key::H if self.book.flow().is_some() => {
                    self.hl_mode = if self.hl_mode.is_some() { None } else { Some(HlColor::Yellow) };
                }
                Key::A => {
                    self.auto = !self.auto;
                    self.auto_last = Instant::now();
                }
                Key::Plus | Key::Equals if mods.command => self.bump_font(env, 1.0),
                Key::Minus if mods.command => self.bump_font(env, -1.0),
                Key::Backspace if mods.alt || !mods.any() => self.back(),
                _ => {}
            }
        }
    }

    fn bump_font(&mut self, env: &mut Env, d: f32) {
        if let Some(bs) = self.data.settings.as_mut() {
            bs.font_size = (bs.font_size + d).clamp(10.0, 48.0);
        } else {
            env.app.reader.font_size = (env.app.reader.font_size + d).clamp(10.0, 48.0);
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context, pal: &Palette, env: &mut Env, actions: &mut Vec<Action>) {
        let left = ctx.available_rect().left();
        let w = ctx.screen_rect().width() - left;
        let chapter = self.chapter_title(match &self.book.content {
            Content::Flow(_) => self.loc,
            Content::Fixed(_) => Loc::new(self.fx.page, 0, 0),
        });
        egui::Area::new("reader-top".into()).fixed_pos(pos2(left, 0.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin { left: 16, right: 16, top: 8, bottom: 8 }).show(ui, |ui| {
                ui.set_width(w - 32.0);
                ui.horizontal(|ui| {
                    if ui::text_button(ui, "← Library", false).on_hover_text("Esc").clicked() {
                        actions.push(Action::Close);
                    }
                    if ui::text_button(ui, "Contents", self.side.is_some()).on_hover_text("C").clicked() {
                        self.side = if self.side.is_some() { None } else { Some(Side::Toc) };
                        self.toc_scroll = true;
                    }
                    if !self.history.is_empty() && ui::text_button(ui, "← Back", false).on_hover_text("Backspace").clicked() {
                        self.back();
                    }
                    let rest = ui.available_width() - 330.0;
                    ui.allocate_ui(vec2(rest.max(80.0), 26.0), |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.add_space(12.0);
                            ui.add(egui::Label::new(RichText::new(&self.book.meta.title).font(ui::ui_m(14.0))).truncate());
                            if !chapter.is_empty() && chapter != self.book.meta.title {
                                ui.label(RichText::new("/").font(ui::ui_m(14.0)).color(ACCENT));
                                ui.add(egui::Label::new(RichText::new(&chapter).size(14.0).color(pal.muted)).truncate());
                            }
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::text_button(ui, "Full", self.fullscreen).on_hover_text("F").clicked() {
                            self.fullscreen = !self.fullscreen;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
                        }
                        if self.book.flow().is_some() {
                            let playing = self.speaker.is_some();
                            if ui::text_button(ui, "Listen", playing).clicked() {
                                if playing {
                                    self.stop_tts();
                                } else {
                                    self.start_tts(self.loc, env.app.tts_rate);
                                }
                            }
                        }
                        if ui::text_button(ui, "Aa", self.settings_open).clicked() {
                            self.settings_open = !self.settings_open;
                        }
                        let bm = self.bookmark_here().is_some();
                        if ui::text_button(ui, "Mark", bm).on_hover_text("B").clicked() {
                            self.toggle_bookmark();
                        }
                        if ui::text_button(ui, "Find", self.side == Some(Side::Search)).on_hover_text("Ctrl+K").clicked() {
                            self.side = if self.side == Some(Side::Search) { None } else { Some(Side::Search) };
                            self.focus_search = true;
                        }
                    });
                });
            });
            let r = ui.min_rect();
            ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, pal.border));
        });
    }

    fn bottom_bar(&mut self, ctx: &egui::Context, pal: &Palette, env: &mut Env) {
        let left = ctx.available_rect().left();
        let w = ctx.screen_rect().width() - left;
        let bottom = ctx.screen_rect().bottom();
        let frac = self.fraction();
        let info = self.position_text(env.app.reading_speed_cpm);
        egui::Area::new("reader-bottom".into()).fixed_pos(pos2(left, bottom - 50.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            let r0 = ui.max_rect();
            ui.painter().hline(r0.left()..=r0.left() + w, r0.top(), egui::Stroke::new(1.0, pal.border));
            egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
                ui.set_width(w - 32.0);
                ui.set_height(34.0);
                ui.horizontal_centered(|ui| {
                    if ui::text_button(ui, "← Ch", false).on_hover_text("Previous chapter").clicked() {
                        self.chapter_step(-1);
                    }
                    let info_w = 300.0;
                    let mut f = frac * 100.0;
                    ui.spacing_mut().slider_width = (ui.available_width() - info_w - 70.0).max(80.0);
                    let r = ui.add(egui::Slider::new(&mut f, 0.0..=100.0).show_value(false).trailing_fill(true));
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        let target = match &self.book.content {
                            Content::Flow(fl) => fl.loc_at_fraction(f / 100.0),
                            Content::Fixed(fx) => Loc::new(((f / 100.0) * fx.pages as f32) as usize, 0, 0),
                        };
                        self.jump(target, true);
                    }
                    if r.dragged() {
                        r.on_hover_text(format!("{f:.0}%"));
                    }
                    if ui::text_button(ui, "Ch →", false).on_hover_text("Next chapter").clicked() {
                        self.chapter_step(1);
                    }
                    ui.add_sized(vec2(info_w, 20.0), egui::Label::new(RichText::new(info.to_uppercase()).font(ui::mono(10.5)).color(pal.muted)).truncate());
                });
            });
        });
    }

    fn position_text(&self, cpm: f32) -> String {
        match &self.book.content {
            Content::Flow(f) => {
                let pct = (f.fraction(self.loc) * 100.0).round();
                let mut s = String::new();
                if let Some(l) = &self.layout {
                    let step = self.step.max(1);
                    let n = l.pages.len().div_ceil(step);
                    s.push_str(&format!("{} of {} in chapter", self.page / step + 1, n));
                }
                let left = f.section_chars_after(self.loc) as f32 / cpm.max(100.0);
                if left >= 1.0 {
                    s.push_str(&format!(" · {} min left", left.round()));
                }
                format!("{s} · {pct}%")
            }
            Content::Fixed(fx) => format!("Page {} of {} · {}%", self.fx.page + 1, fx.pages, ((self.fx.page + 1) * 100 / fx.pages.max(1))),
        }
    }

    fn chapter_step(&mut self, d: i32) {
        let cur = match &self.book.content {
            Content::Flow(_) => self.loc,
            Content::Fixed(_) => Loc::new(self.fx.page, 0, 0),
        };
        if !self.book.toc.is_empty() {
            let i = self.book.toc_index_for(cur);
            let target = match (d > 0, i) {
                (true, Some(i)) => self.book.toc.get(i + 1).map(|e| e.loc),
                (true, None) => self.book.toc.first().map(|e| e.loc),
                (false, Some(i)) => {
                    // go to start of current chapter first, then previous
                    let e = self.book.toc[i].loc;
                    if cur > e && (cur.section != e.section || cur.block > e.block + 2) {
                        Some(e)
                    } else {
                        i.checked_sub(1).map(|j| self.book.toc[j].loc)
                    }
                }
                (false, None) => None,
            };
            if let Some(t) = target {
                self.jump(t, false);
            }
            return;
        }
        match &self.book.content {
            Content::Flow(f) => {
                let s = (self.loc.section as i64 + d as i64).clamp(0, f.sections.len() as i64 - 1) as usize;
                self.jump(Loc::new(s, 0, 0), false);
            }
            Content::Fixed(fx) => {
                let p = (self.fx.page as i64 + d as i64 * 10).clamp(0, fx.pages as i64 - 1) as usize;
                self.jump(Loc::new(p, 0, 0), false);
            }
        }
    }

    fn tts_bar(&mut self, ctx: &egui::Context, pal: &Palette, env: &mut Env) {
        egui::Area::new("tts".into()).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -64.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(pal.panel).corner_radius(20).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if !self.tts_error.is_empty() {
                        ui.colored_label(Color32::from_rgb(220, 80, 60), &self.tts_error);
                        if icon_button(ui, Icon::Close, "Dismiss").clicked() {
                            self.tts_error.clear();
                        }
                        return;
                    }
                    ui.label(RichText::new("Reading aloud").color(pal.muted));
                    if icon_button(ui, Icon::Stop, "Stop").clicked() {
                        self.stop_tts();
                    }
                    let mut rate = env.app.tts_rate;
                    ui.spacing_mut().slider_width = 90.0;
                    let r = ui.add(egui::Slider::new(&mut rate, 0.5..=2.5).step_by(0.1).suffix("×"));
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        env.app.tts_rate = rate;
                        if let Some((loc, _)) = self.tts_at {
                            self.start_tts(loc, rate);
                        }
                    } else if r.changed() {
                        env.app.tts_rate = rate;
                    }
                });
            });
        });
    }

    /// Once, after the first animated opening: let people pick how much motion they want.
    fn motion_hint_bar(&mut self, ctx: &egui::Context, pal: &Palette, env: &mut Env) {
        if env.app.motion_hint_shown || self.book.flow().is_none() {
            return;
        }
        let Some(t0) = self.opened_at else { return };
        if t0.elapsed() < Duration::from_millis(1400) {
            ctx.request_repaint_after(Duration::from_millis(300));
            return;
        }
        let cur = self.effective(&env.app.reader).motion as usize;
        let mut pick = None;
        let mut done = false;
        egui::Area::new("motion-hint".into()).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -28.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::new().fill(pal.bg).stroke(egui::Stroke::new(1.0, pal.border)).corner_radius(22).inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Page motion").font(ui::ui_m(13.5)));
                    if let Some(i) = ui::segmented(ui, "motion-hint", &["Full", "Simple", "Off"], cur) {
                        pick = Some(i as u8);
                    }
                    if ui::text_button(ui, "Keep it", false).clicked() {
                        done = true;
                    }
                });
            });
        });
        if let Some(m) = pick {
            self.set(env, |s| s.motion = m);
        }
        if done {
            env.app.motion_hint_shown = true;
        }
    }

    fn sync_bar(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(p) = self.sync_prompt.clone() else { return };
        egui::Area::new("sync".into()).anchor(Align2::CENTER_TOP, vec2(0.0, 60.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(pal.panel).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("{} is at {:.0}% (you're at {:.0}%).", if p.device.is_empty() { "Another device" } else { &p.device }, p.percentage * 100.0, self.fraction() * 100.0));
                    if ui::primary_button(ui, "Jump there").clicked() {
                        self.apply_remote(&p);
                        self.sync_prompt = None;
                    }
                    if ui.button("Keep mine").clicked() {
                        self.sync_prompt = None;
                    }
                });
            });
        });
    }

    fn popups(&mut self, ctx: &egui::Context, pal: &Palette, env: &mut Env) {
        // selection toolbar
        if let (Some(pos), Some(_)) = (self.sel_popup, self.sel) {
            let mut act: Option<&str> = None;
            let mut color = None;
            egui::Area::new("sel-pop".into()).fixed_pos(clamp_popup(ctx, pos, vec2(560.0, 44.0))).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::new().fill(ui::INK).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                    ui.visuals_mut().override_text_color = Some(ui::PAPER);
                    ui.visuals_mut().widgets.hovered.weak_bg_fill = Color32::from_gray(40);
                    ui.horizontal(|ui| {
                        for c in store::HL_COLORS {
                            let [r, g, b] = c.rgb();
                            let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 26.0), egui::Sense::click());
                            let t = ui.ctx().animate_bool_with_time(ui.id().with(("sw", *c as u8)), resp.hovered(), 0.12);
                            let sq = egui::Rect::from_center_size(rect.center() - vec2(0.0, 3.0 * t), vec2(12.0 + 2.0 * t, 12.0 + 2.0 * t));
                            ui.painter().rect_filled(sq, egui::CornerRadius::ZERO, Color32::from_rgb(r, g, b));
                            if resp.on_hover_text("Highlight").clicked() {
                                color = Some(*c);
                            }
                        }
                        ui.add_space(6.0);
                        for (k, label) in [("note", "Note"), ("copy", "Copy"), ("dict", "Define"), ("wiki", "Wiki"), ("translate", "Translate"), ("speak", "Listen"), ("search", "Find")] {
                            if ui::text_button(ui, label, false).clicked() {
                                act = Some(k);
                            }
                        }
                    });
                });
            });
            if let Some(c) = color {
                self.add_annotation(c, false);
            }
            match act {
                Some("note") => self.add_annotation(HlColor::Yellow, true),
                Some("copy") => {
                    if let Some(t) = self.selection_text() {
                        ctx.copy_text(t);
                    }
                    self.clear_selection();
                }
                Some(k @ ("dict" | "wiki" | "translate")) => {
                    let kind = match k {
                        "dict" => lookup::Kind::Dictionary,
                        "wiki" => lookup::Kind::Wikipedia,
                        _ => lookup::Kind::Translate,
                    };
                    if let Some(t) = self.selection_text() {
                        self.start_lookup(kind, t, env.app);
                    }
                }
                Some("speak") => {
                    if let Some(((b, c), _)) = self.sel {
                        let l = Loc::new(self.loc.section, b, c);
                        self.clear_selection();
                        self.start_tts(l, env.app.tts_rate);
                    }
                }
                Some("search") => {
                    if let Some(t) = self.selection_text() {
                        self.search_q = t.chars().take(80).collect();
                        self.side = Some(Side::Search);
                        self.run_search();
                    }
                    self.clear_selection();
                }
                _ => {}
            }
        }

        // existing highlight
        if let Some((id, pos)) = self.hl_popup {
            let mut close = false;
            egui::Area::new("hl-pop".into()).fixed_pos(clamp_popup(ctx, pos, vec2(300.0, 50.0))).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).fill(pal.panel).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let Some(idx) = self.data.annotations.iter().position(|a| a.id == id) else {
                            close = true;
                            return;
                        };
                        for c in store::HL_COLORS {
                            let [r, g, b] = c.rgb();
                            let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), egui::Sense::click());
                            let sel = self.data.annotations[idx].color == *c;
                            ui.painter().circle_filled(rect.center(), if sel || resp.hovered() { 10.0 } else { 8.0 }, Color32::from_rgb(r, g, b));
                            if resp.clicked() {
                                self.data.annotations[idx].color = *c;
                                self.dirty = true;
                            }
                        }
                        ui.separator();
                        let ul = self.data.annotations[idx].style == HlStyle::Underline;
                        if icon_toggle(ui, Icon::Pen, "Underline style", ul).clicked() {
                            self.data.annotations[idx].style = if ul { HlStyle::Highlight } else { HlStyle::Underline };
                            self.dirty = true;
                        }
                        if icon_button(ui, Icon::Note, "Edit note").clicked() {
                            self.note_edit = Some((id, self.data.annotations[idx].note.clone()));
                            close = true;
                        }
                        if icon_button(ui, Icon::Copy, "Copy text").clicked() {
                            ctx.copy_text(self.data.annotations[idx].text.clone());
                            close = true;
                        }
                        if icon_button(ui, Icon::Trash, "Delete highlight").clicked() {
                            self.data.annotations.remove(idx);
                            self.dirty = true;
                            close = true;
                        }
                    });
                });
            });
            if close {
                self.hl_popup = None;
            }
        }

        // note editor
        if let Some((id, mut text)) = self.note_edit.take() {
            let mut keep = true;
            let quote = self.data.annotations.iter().find(|a| a.id == id).map(|a| a.text.clone()).unwrap_or_default();
            egui::Window::new("Note").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(RichText::new(format!("“{}”", quote.chars().take(240).collect::<String>())).italics().color(pal.muted));
                ui.add_space(6.0);
                let r = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(5).desired_width(f32::INFINITY).hint_text("Write a note…"));
                r.request_focus();
                ui.horizontal(|ui| {
                    if ui::primary_button(ui, "Save").clicked() || (ui.input(|i| i.key_pressed(Key::Enter) && i.modifiers.command)) {
                        if let Some(a) = self.data.annotations.iter_mut().find(|a| a.id == id) {
                            a.note = text.trim().to_string();
                        }
                        self.dirty = true;
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                self.note_edit = Some((id, text));
            }
        }

        // footnote
        if let Some((loc, text, pos)) = self.footnote.clone() {
            let mut close = false;
            egui::Area::new("fn-pop".into()).fixed_pos(clamp_popup(ctx, pos, vec2(440.0, 260.0))).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).fill(pal.panel).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.label(RichText::new(text).size(14.5));
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Go to note").clicked() {
                            self.jump(loc, true);
                            close = true;
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
            });
            if close {
                self.footnote = None;
            }
        }

        // lookup results
        if self.lookup.is_some() {
            let mut open = true;
            let l = self.lookup.as_ref().unwrap();
            let title = l.kind.title();
            let mut retry = false;
            egui::Window::new(title).open(&mut open).collapsible(false).resizable(true).default_width(420.0).default_height(320.0).anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -70.0)).show(ctx, |ui| {
                let l = self.lookup.as_ref().unwrap();
                match &l.res {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Looking up “{}”…", l.query.chars().take(40).collect::<String>()));
                        });
                    }
                    Some(Err(e)) => {
                        ui.label(e);
                        if ui.button("Retry").clicked() {
                            retry = true;
                        }
                    }
                    Some(Ok(a)) => {
                        ui.label(RichText::new(&a.heading).strong().size(17.0));
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            ui.label(RichText::new(&a.body).size(14.0));
                        });
                        ui.horizontal(|ui| {
                            if let Some(u) = &a.url {
                                if ui.link("Open in browser").clicked() {
                                    crate::platform::open_url(u);
                                }
                            }
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text(format!("{}\n{}", a.heading, a.body));
                            }
                        });
                    }
                }
                if l.kind == lookup::Kind::Translate {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Target language:");
                        let mut t = env.app.translate_to.clone();
                        egui::ComboBox::from_id_salt("tl").selected_text(&t).width(90.0).show_ui(ui, |ui| {
                            for code in ["en", "es", "fr", "de", "it", "pt", "ru", "zh-CN", "ja", "ko", "ar", "hi", "tr", "nl", "pl", "uk", "vi", "id"] {
                                ui.selectable_value(&mut t, code.to_string(), code);
                            }
                        });
                        if t != env.app.translate_to {
                            env.app.translate_to = t;
                            retry = true;
                        }
                    });
                }
            });
            if retry {
                if let Some(l) = self.lookup.take() {
                    self.start_lookup(l.kind, l.query, env.app);
                }
            } else if !open {
                self.lookup = None;
            }
        }
    }

    fn follow_link(&mut self, link: u32, pos: Pos2, link_text: &str) {
        let Some(flow) = self.book.flow() else { return };
        match flow.links.get(link as usize) {
            Some(LinkTarget::Internal(loc)) => {
                let loc = *loc;
                let target_block = flow.sections.get(loc.section).and_then(|s| s.blocks.get(loc.block));
                let short_ref = link_text.trim().chars().count() <= 5 || link_text.trim().starts_with('[');
                let is_note = target_block.is_some_and(|b| b.note) || short_ref;
                if is_note {
                    let sec = &flow.sections[loc.section];
                    let mut text = String::new();
                    for b in sec.blocks.iter().skip(loc.block).take(4) {
                        if !text.is_empty() && matches!(b.kind, crate::book::BlockKind::Heading(_)) {
                            break;
                        }
                        if !text.is_empty() {
                            text.push_str("\n\n");
                        }
                        text.push_str(&b.text());
                        if text.chars().count() > 40 {
                            break;
                        }
                    }
                    self.footnote = Some((loc, text, pos));
                } else {
                    self.jump(loc, true);
                }
            }
            Some(LinkTarget::External(u)) => crate::platform::open_url(u),
            _ => {}
        }
    }
}

fn clamp_popup(ctx: &egui::Context, pos: Pos2, size: egui::Vec2) -> Pos2 {
    let s = ctx.screen_rect();
    let x = (pos.x - size.x / 2.0).clamp(s.left() + 8.0, (s.right() - size.x - 8.0).max(s.left() + 8.0));
    let y = if pos.y + 16.0 + size.y > s.bottom() { (pos.y - size.y - 24.0).max(s.top() + 8.0) } else { pos.y + 16.0 };
    pos2(x, y)
}

pub fn deco_color(c: HlColor, dark: bool) -> Color32 {
    let [r, g, b] = c.rgb();
    Color32::from_rgba_unmultiplied(r, g, b, if dark { 95 } else { 110 })
}

#[allow(dead_code)]
fn _unused(_: Rect) {
    let _ = ACCENT;
}

#[derive(Clone, Copy)]
pub(crate) struct Turn {
    pub at: Instant,
    pub dir: i8,
    pub from: usize,
    pub section: usize,
}

pub(crate) fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}
