//! Reflowable view: geometry, layout cache, painting, pointer interaction.

use crate::i18n::tr;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::layout::{Deco, DecoKind, SectionLayout, Typo};
#[allow(unused_imports)]
use super::layout::Item;
use super::{deco_color, Cover, Env, Reader};
use crate::book::{BlockKind, Loc};
use crate::store::{HlStyle, Layout, ReaderSettings};
use crate::texload::Src;
use crate::ui::{self, Palette, ACCENT};

/// A cover board's placement (see `Reader::cover_geo`).
pub(super) struct CoverGeo {
    /// The closed book, centred on the desk.
    closed: Rect,
    /// The open book's pages (shadows stay on them).
    pages: Rect,
    hinge: f32,
    bw: f32,
    top: f32,
    bot: f32,
    /// +1 left-to-right, -1 right-to-left books.
    d: f32,
    /// Closed position minus open position (x).
    off: f32,
}

pub(super) struct Geo {
    /// Paper sheets (one per visible page; scrolled mode = one tall sheet).
    pub sheets: Vec<Rect>,
    pub spine: Option<f32>,
    pub pad: f32,
    pub x0: f32,
    pub col_w: f32,
    pub gap: f32,
    pub cols: usize,
    pub top: f32,
    pub page_h: f32,
}

impl Geo {
    fn origin(&self, col: usize) -> Pos2 {
        pos2(self.x0 + col as f32 * (self.col_w + self.gap), self.top)
    }
    fn sheet(&self, col: usize) -> Rect {
        self.sheets[col.min(self.sheets.len() - 1)]
    }
}

impl Reader {
    /// The open book on the desk: sheets, spine, text columns.
    fn geometry(&self, rect: Rect, s: &ReaderSettings) -> Geo {
        let desk = vec2((rect.width() * 0.025).clamp(10.0, 36.0), (rect.height() * 0.025).clamp(8.0, 22.0));
        let book = rect.shrink2(desk);
        let header = if s.show_header { 44.0 } else { 26.0 };
        let footer = if s.show_footer { 44.0 } else { 26.0 };
        let paginated = s.layout == Layout::Paginated || self.book.meta.vertical;
        let pad = s.margin.clamp(16.0, 160.0).min(book.width() * 0.09);
        let two = paginated
            && match s.columns {
                crate::store::Columns::One => false,
                crate::store::Columns::Two => book.width() > 760.0,
                crate::store::Columns::Auto => book.width() > 1100.0 && rect.width() > rect.height() * 1.15,
            };
        let (sheets, spine) = if two {
            let sw = (book.width() / 2.0).min(s.max_width + 2.0 * pad).floor();
            let c = book.center().x.round();
            (vec![Rect::from_min_max(pos2(c - sw, book.top()), pos2(c, book.bottom())), Rect::from_min_max(pos2(c, book.top()), pos2(c + sw, book.bottom()))], Some(c))
        } else {
            let sw = book.width().min(s.max_width + 2.0 * pad).floor();
            (vec![Rect::from_center_size(book.center(), vec2(sw, book.height()))], None)
        };
        let col_w = (sheets[0].width() - 2.0 * pad).max(120.0);
        let x0 = sheets[0].left() + pad;
        let top = book.top() + header;
        let page_h = (book.bottom() - footer - top).max(80.0);
        Geo { sheets, spine, pad, x0, col_w, gap: if two { 2.0 * pad } else { 0.0 }, cols: if two { 2 } else { 1 }, top, page_h }
    }

    /// Paper, shadow, page-edge stacks and the spine gutter.
    fn paint_book(&self, painter: &egui::Painter, g: &Geo, pal: &Palette, frac: f32) {
        let all = g.sheets.iter().fold(g.sheets[0], |a, r| a.union(*r));
        // soft stacked shadow (three cheap layers, no blur)
        for (o, a) in [(2.0, 18u8), (5.0, 10), (9.0, 5)] {
            painter.rect_filled(all.translate(vec2(0.0, o)).expand(o * 0.5), CornerRadius::same(4), Color32::from_black_alpha(if pal.dark { a * 3 } else { a }));
        }
        // page edges: thicker on the side you've read / still to read
        let edge = |x: f32, dir: f32, n: usize| {
            for i in 1..=n {
                let xi = x + dir * i as f32 * 1.6;
                let inset = i as f32 * 0.8;
                painter.vline(xi, all.top() + inset..=all.bottom() - inset, Stroke::new(1.0, ui::mix(pal.bg, pal.fg, if i % 2 == 0 { 0.08 } else { 0.2 })));
            }
        };
        let left_n = ((frac * 7.0).round() as usize).min(7);
        let right_n = (((1.0 - frac) * 7.0).round() as usize).min(7);
        if g.spine.is_some() {
            edge(all.left(), -1.0, left_n.max(1));
        }
        edge(all.right(), 1.0, right_n.max(1));
        for sh in &g.sheets {
            painter.rect_filled(*sh, CornerRadius::same(2), pal.bg);
        }
        // gutter: shading into the spine
        let shade = Color32::from_black_alpha(if pal.dark { 70 } else { 26 });
        let clear = Color32::TRANSPARENT;
        let band = |x0: f32, x1: f32, c0: Color32, c1: Color32| {
            let mut m = egui::Mesh::default();
            m.colored_vertex(pos2(x0, all.top()), c0);
            m.colored_vertex(pos2(x1, all.top()), c1);
            m.colored_vertex(pos2(x1, all.bottom()), c1);
            m.colored_vertex(pos2(x0, all.bottom()), c0);
            m.add_triangle(0, 1, 2);
            m.add_triangle(0, 2, 3);
            painter.add(m);
        };
        match g.spine {
            Some(x) => {
                band(x - 46.0, x, clear, shade);
                band(x, x + 46.0, shade, clear);
                painter.vline(x, all.y_range(), Stroke::new(1.0, Color32::from_black_alpha(if pal.dark { 90 } else { 30 })));
            }
            None => band(all.left(), all.left() + 22.0, shade, clear),
        }
    }

    fn ensure_layout(&mut self, ctx: &egui::Context, s: &ReaderSettings, pal: &Palette, env: &Env, g: &Geo) {
        let book = self.book.clone();
        let Some(flow) = book.flow() else { return };
        let si = self.loc.section.min(flow.sections.len() - 1);
        self.ensure_sec_chars(si);
        let sec = &flow.sections[si];
        // request images of this section (and the next one, as prefetch)
        let mut known = 0usize;
        for (k, idx) in sec.blocks.iter().filter_map(|b| if let BlockKind::Image(k) = &b.kind { Some((k, si)) } else { None }) {
            let _ = idx;
            self.tex.request(k, || flow.resources.get(k).cloned().map(Src::Res), 2048);
            if !self.tex.is_pending(k) {
                known += 1;
            }
        }
        if let Some(next) = flow.sections.get(si + 1) {
            for b in next.blocks.iter().take(30) {
                if let BlockKind::Image(k) = &b.kind {
                    self.tex.request(k, || flow.resources.get(k).cloned().map(Src::Res), 2048);
                }
            }
        }
        let typo = Typo {
            size: s.font_size,
            line_height: s.line_height,
            para: s.para_spacing,
            letter: s.letter_spacing,
            justify: s.justify,
            indent: s.indent,
            override_book: s.override_book,
            fg: pal.fg,
            link: pal.link,
            width: g.col_w,
            page_h: g.page_h,
            fake_italic: !env.installed.has_italic,
            // font set + display scale: either changes glyph widths, so line breaks
            font_tag: format!("{}{}#{}{}@{}", env.installed.book_family, env.installed.cjk, env.installed.generation, if ctx.cumulative_pass_nr() <= env.installed.pass { "~" } else { "" }, ctx.pixels_per_point()),
            lang: self.book.meta.language.clone(),
            vertical: self.book.meta.vertical,
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let paged = s.layout == Layout::Paginated || self.book.meta.vertical;
        // a spread opens on the right page, facing the blank inside of the cover
        let lead = paged && si == self.start_sec() && g.cols == 2 && !self.book.meta.vertical;
        (si, typo.key(), known, paged, lead).hash(&mut h);
        let key = h.finish();
        if self.layout.as_ref().is_some_and(|l| l.key == key) {
            return;
        }
        let tex = &self.tex;
        // the cover picture at the top of the text: the hardcover already shows it
        let trimmed;
        let sec = match self.cover_block() {
            Some((cs, cb)) if cs == si => {
                let mut t = sec.clone();
                t.blocks[cb].kind = BlockKind::Para;
                t.blocks[cb].spans.clear();
                trimmed = t;
                &trimmed
            }
            _ => sec,
        };
        let mut lay = SectionLayout::build(ctx, si, sec, &typo, &|k| tex.get(k).map(|l| l.size), key);
        if lead {
            lay.pages.insert(0, super::layout::Page { u0: 0, u1: 0, top: 0.0, bottom: 0.0 });
        }
        self.step = g.cols;
        if self.go_end {
            self.go_end = false;
            let last = lay.pages.len().saturating_sub(1);
            self.page = last - last % self.step;
            self.scroll_y = (lay.height - g.page_h).max(0.0);
            let (b, o) = lay.page_start(self.page);
            self.loc = Loc::new(si, b, o);
        } else {
            self.page = lay.page_for(self.loc.block, self.loc.offset);
            self.page -= self.page % self.step;
            // The layout key changed (section, font, width, mode, images): the old pixel offset is
            // meaningless now, so always re-derive it from the logical location.
            self.scroll_y = (lay.y_of(self.loc.block, self.loc.offset) - 4.0).max(0.0);
        }
        // keep the previous chapter alive so a page can flip across the boundary
        if let Some(old) = self.layout.take() {
            if old.section != si {
                // selection points are block indices of the old section; they mean nothing here
                self.clear_selection();
                self.prev_layout = Some(old);
            }
        }
        self.layout = Some(lay);
    }

    /// Where reading starts. An opening section that holds nothing but the
    /// cover picture is skipped: the hardcover already shows it.
    pub(super) fn start_sec(&self) -> usize {
        let Some(f) = self.book.flow() else { return 0 };
        let Some(first) = f.sections.first() else { return 0 };
        let only_pictures = first.blocks.iter().all(|b| matches!(b.kind, BlockKind::Image(_)) || b.text().trim().is_empty());
        usize::from(f.sections.len() > 1 && self.cover_block().is_some() && only_pictures)
    }

    /// The book's own cover picture at the top of the first section, if it is
    /// there: the same image as the cover, or the opening picture of a section
    /// called "Cover".
    pub(super) fn cover_block(&self) -> Option<(usize, usize)> {
        *self.cover_block.get_or_init(|| {
            let f = self.book.flow()?;
            let sec = f.sections.first()?;
            let (bi, key) = sec.blocks.iter().enumerate().take(6).find_map(|(i, b)| match &b.kind {
                BlockKind::Image(k) => Some(Some((i, k))),
                _ if b.text().trim().is_empty() => None,
                _ => Some(None),
            })??;
            let same = self.book.meta.cover.as_ref().is_some_and(|c| f.resources.get(key).and_then(|r| r.load()).is_some_and(|b| &b == c));
            (same || sec.title.trim().eq_ignore_ascii_case("cover")).then_some((0, bi))
        })
    }

    pub(super) fn next_page(&mut self) {
        if self.book.fixed().is_some() {
            return self.fixed_next();
        }
        let Some(flow) = self.book.flow() else { return };
        let n = flow.sections.len();
        match self.cover {
            Cover::Front => return self.set_cover(Cover::Open, Cover::Front),
            Cover::Back => return,
            Cover::Open => {}
        }
        let Some(l) = &self.layout else { return };
        if self.data.settings.as_ref().map(|s| s.layout).unwrap_or(self.last_layout) == Layout::Scrolled && !self.book.meta.vertical {
            let max = (l.height - self.view_h + 40.0).max(0.0);
            if l.section + 1 >= n && self.scroll_y >= max - 1.0 {
                self.data.finished = true;
                return self.set_cover(Cover::Back, Cover::Back);
            }
            return self.scroll_by(self.view_h * 0.9);
        }
        if self.page + self.step >= l.pages.len() && l.section + 1 >= n {
            // past the last page: the back cover closes over the book
            self.data.finished = true;
            self.dirty = true;
            return self.set_cover(Cover::Back, Cover::Back);
        }
        if self.motion < 2 {
            self.turn = Some(super::Turn { at: Instant::now(), dir: 1, from: self.page, section: l.section });
        }
        if self.page + self.step < l.pages.len() {
            self.page += self.step;
            let (b, o) = l.page_start(self.page);
            self.loc = Loc::new(l.section, b, o);
        } else if l.section + 1 < n {
            self.loc = Loc::new(l.section + 1, 0, 0);
            self.page = 0;
        } else {
            self.data.finished = true;
        }
        self.last_turn = Instant::now();
        self.clear_selection();
        self.dirty = true;
    }

    pub(super) fn prev_page(&mut self) {
        if self.book.fixed().is_some() {
            return self.fixed_prev();
        }
        match self.cover {
            Cover::Back => return self.set_cover(Cover::Open, Cover::Back),
            Cover::Front => return,
            Cover::Open => {}
        }
        let Some(l) = &self.layout else { return };
        let scrolled = self.data.settings.as_ref().map(|s| s.layout).unwrap_or(self.last_layout) == Layout::Scrolled && !self.book.meta.vertical;
        // before the first page: the front cover closes
        if l.section <= self.start_sec() && self.book.flow().is_some() && if scrolled { self.scroll_y <= 1.0 } else { self.page < self.step.max(1) } {
            return self.set_cover(Cover::Front, Cover::Front);
        }
        if scrolled {
            return self.scroll_by(-self.view_h * 0.9);
        }
        if self.motion < 2 && (self.page > 0 || l.section > 0) {
            self.turn = Some(super::Turn { at: Instant::now(), dir: -1, from: self.page, section: l.section });
        }
        if self.page >= self.step {
            self.page -= self.step;
            let (b, o) = l.page_start(self.page);
            self.loc = Loc::new(l.section, b, o);
        } else if l.section > 0 {
            self.loc = Loc::new(l.section - 1, 0, 0);
            self.go_end = true;
        }
        self.last_turn = Instant::now();
        self.clear_selection();
        self.dirty = true;
    }

    /// Show the book open or closed at one cover, animating the board
    /// (`side`) unless page motion is off.
    fn set_cover(&mut self, to: Cover, side: Cover) {
        if to == self.cover {
            return;
        }
        self.cover = to;
        self.back_at = (to == Cover::Back).then_some(self.loc);
        self.cover_anim = (self.motion < 2).then(|| (Instant::now(), to != Cover::Open, side));
        self.turn = None;
        self.last_turn = Instant::now();
        self.clear_selection();
    }

    pub(super) fn scroll_by(&mut self, dy: f32) {
        if self.book.fixed().is_some() {
            self.fx.scroll += dy;
            return;
        }
        let Some(l) = &self.layout else { return };
        let max = (l.height - self.view_h + 40.0).max(0.0);
        let n = self.book.flow().map(|f| f.sections.len()).unwrap_or(1);
        let target = self.scroll_y + dy;
        if target > max + 1.0 {
            if self.scroll_y >= max - 1.0 && l.section + 1 < n {
                self.loc = Loc::new(l.section + 1, 0, 0);
                self.clear_selection();
                self.scroll_y = 0.0;
                self.dirty = true;
                return;
            }
            self.scroll_y = max;
        } else if target < 0.0 {
            if self.scroll_y <= 1.0 && l.section > 0 {
                self.loc = Loc::new(l.section - 1, 0, 0);
                self.clear_selection();
                self.go_end = true;
                self.dirty = true;
                return;
            }
            self.scroll_y = 0.0;
        } else {
            self.scroll_y = target;
        }
        let u = l.unit_at_y(self.scroll_y);
        if let Some(u) = l.units.get(u) {
            self.loc = Loc::new(l.section, u.block, u.c0);
        }
        self.dirty = true;
    }

    pub(super) fn section_edge(&mut self, end: bool) {
        if self.book.fixed().is_some() {
            let n = self.book.fixed().map(|f| f.pages).unwrap_or(1);
            self.fx.page = if end { n - 1 } else { 0 };
            return;
        }
        if let Some(l) = &self.layout {
            if end {
                let last = l.pages.len().saturating_sub(1);
                self.page = last - last % self.step.max(1);
                self.scroll_y = (l.height - self.view_h).max(0.0);
            } else {
                self.page = 0;
                self.scroll_y = 0.0;
            }
            let (b, o) = l.page_start(self.page);
            self.loc = Loc::new(l.section, b, o);
        }
    }

    /// Map a screen point to (column, column-local point, slice top, slice bottom).
    fn locate(&self, g: &Geo, pos: Pos2, paginated: bool) -> Option<(usize, Vec2, f32, f32)> {
        let l = self.layout.as_ref()?;
        let col = if g.cols == 2 && pos.x > g.x0 + g.col_w + g.gap / 2.0 { 1 } else { 0 };
        let o = g.origin(col);
        if paginated {
            let pg = l.pages.get(self.page + self.page_col(g, col))?;
            Some((col, pos - o, pg.top, pg.bottom))
        } else {
            Some((0, pos - o, self.scroll_y, self.scroll_y + g.page_h))
        }
    }

    /// Page offset (from `self.page`) shown in visual column `col`: in
    /// right-to-left books the first page of a spread is the right-hand one.
    fn page_col(&self, g: &Geo, col: usize) -> usize {
        if self.book.meta.rtl && g.cols == 2 {
            1 - col.min(1)
        } else {
            col
        }
    }

    fn hit(&self, g: &Geo, pos: Pos2, paginated: bool, clamp: bool) -> Option<(usize, usize)> {
        let (_, local, top, bottom) = self.locate(g, pos, paginated)?;
        self.layout.as_ref()?.hit(top, bottom, local, clamp)
    }

    fn decos(&self, pal: &Palette) -> Vec<Deco> {
        let mut v = Vec::new();
        let sec = self.loc.section;
        let range = |v: &mut Vec<Deco>, s: Loc, e: Loc, kind: DecoKind| {
            if s.section != sec {
                return;
            }
            for b in s.block..=e.block {
                let c0 = if b == s.block { s.offset } else { 0 };
                let c1 = if b == e.block { e.offset } else { usize::MAX };
                v.push(Deco { block: b, c0, c1, kind });
            }
        };
        for a in &self.data.annotations {
            let c = deco_color(a.color, pal.dark);
            let kind = match a.style {
                HlStyle::Highlight => DecoKind::Fill(c),
                HlStyle::Underline => DecoKind::Underline(c.to_opaque()),
            };
            range(&mut v, a.start, a.end, kind);
        }
        if let Some(i) = self.search_cur {
            if let Some(h) = self.search_hits.get(i) {
                range(&mut v, h.loc, Loc::new(h.loc.section, h.loc.block, h.loc.offset + h.len), DecoKind::Fill(Color32::from_rgba_unmultiplied(255, 150, 0, 120)));
            }
        }
        if let Some((l, e)) = self.tts_at {
            range(&mut v, l, Loc::new(l.section, l.block, e), DecoKind::Fill(ACCENT.linear_multiply(0.22)));
        }
        if let Some(((b0, c0), (b1, c1))) = self.sel {
            range(&mut v, Loc::new(sec, b0, c0), Loc::new(sec, b1, c1), DecoKind::Fill(ACCENT.linear_multiply(if pal.dark { 0.45 } else { 0.3 })));
        }
        v
    }

    pub(super) fn flow_view(&mut self, ui: &mut Ui, rect: Rect, s: &ReaderSettings, pal: &Palette, env: &mut Env) {
        self.last_layout = s.layout;
        let paginated = s.layout == Layout::Paginated || self.book.meta.vertical;
        let rtl = self.book.meta.rtl;
        let g = self.geometry(rect, s);
        self.view_h = g.page_h;
        // the cover-picture section is part of the cover: reading starts after it
        let start = Loc::new(self.start_sec(), 0, 0);
        if self.loc.section < start.section {
            self.loc = start;
        }
        self.ensure_layout(ui.ctx(), s, pal, env, &g);
        self.motion = s.motion;
        if self.opened_at.is_none() {
            let _ = self.cover_tex();
            // no opening animation: a book at its very start rests on its closed
            // front cover until the first page turn; anywhere else opens as is
            self.opened_at = Some(Instant::now());
            if self.loc == start {
                self.cover = Cover::Front;
            }
            // debug aid: ZR_SIDE=toc|notes|marks|find|notebook|settings opens a panel for captures
            #[cfg(debug_assertions)]
            if let Ok(p) = std::env::var("ZR_SIDE") {
                use super::Side;
                match p.as_str() {
                    "toc" => self.side = Some(Side::Toc),
                    "notes" => self.side = Some(Side::Notes),
                    "marks" => self.side = Some(Side::Bookmarks),
                    "find" => self.side = Some(Side::Search),
                    "notebook" => self.notebook = true,
                    _ => self.settings_open = true,
                }
                if let Ok(t) = std::env::var("ZR_SETTINGS_TAB") {
                    self.settings_tab = t.parse().unwrap_or(0);
                }
                if let Ok(q) = std::env::var("ZR_FIND") {
                    self.search_q = q;
                    self.run_search();
                }
            }
        }
        // a jump elsewhere (contents, search, progress) opens a closed book
        if (self.cover == Cover::Front && self.loc != start) || (self.cover == Cover::Back && self.back_at != Some(self.loc)) {
            self.cover = Cover::Open;
            self.cover_anim = None;
        }
        // how closed the book is (1 = shut on `side`), and whether pages show
        let now = Instant::now();
        let full = self.motion == 0;
        let (side, c) = match self.cover_anim {
            Some((at, closing, side)) => {
                #[allow(unused_mut)]
                let mut t = ((now - at).as_secs_f32() / if full { 1.0 } else { 0.25 }).min(1.0);
                // debug aid: ZR_COVER_T=0.5 freezes a cover animation for captures
                #[cfg(debug_assertions)]
                if let Some(f) = std::env::var("ZR_COVER_T").ok().and_then(|v| v.parse::<f32>().ok()) {
                    t = f;
                }
                if t >= 1.0 {
                    self.cover_anim = None;
                }
                ui.ctx().request_repaint();
                (side, if closing { t } else { 1.0 - t })
            }
            None => (self.cover, if self.cover == Cover::Open { 0.0 } else { 1.0 }),
        };
        let cg = self.cover_geo(&g, side);
        let ps = if cg.off.abs() > 1.0 { 0.3 } else { 0.0 };
        // full motion: (swing progress 0 = board on its open side, slide progress)
        // one smoothstep over the whole move, so the slide flows into the swing
        // without stopping in between
        let smooth = |x: f32| x * x * (3.0 - 2.0 * x);
        let (k, slide) = if side == Cover::Front {
            let e = smooth(1.0 - c);
            if ps > 0.0 { (((e - ps) / (1.0 - ps)).clamp(0.0, 1.0), 1.0 - (e / ps).min(1.0)) } else { (e, 0.0) }
        } else {
            let e = smooth(c);
            if ps > 0.0 { ((e / (1.0 - ps)).min(1.0), ((e - (1.0 - ps)) / ps).clamp(0.0, 1.0)) } else { (e, 0.0) }
        };
        let pages_visible = c < 1.0 && if !full || side == Cover::Open { true } else if side == Cover::Front { k > 0.0 } else { k < 1.0 };
        if !pages_visible {
            self.closed_view(ui, rect, &cg, side, pal, cg.closed.translate(vec2(cg.off * (if full { slide } else { 1.0 }) - cg.off, 0.0)));
            return;
        }
        let Some(l) = &self.layout else { return };
        if l.section != self.loc.section {
            return;
        }
        if self.page >= l.pages.len() {
            self.page = l.pages.len().saturating_sub(1);
            self.page -= self.page % self.step.max(1);
        }

        // ----- input -----
        let resp = ui.interact(rect, ui.id().with("page"), Sense::click_and_drag());
        let hover = ui.input(|i| i.pointer.hover_pos());
        if resp.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            self.drag_anchor = origin.and_then(|p| self.hit(&g, p, paginated, false));
            self.sel = None;
            self.sel_popup = None;
            self.hl_popup = None;
            self.footnote = None;
        }
        if resp.dragged() {
            if let (Some(a), Some(p)) = (self.drag_anchor, resp.interact_pointer_pos()) {
                if let Some(h) = self.hit(&g, p, paginated, true) {
                    let (s0, s1) = if h < a { (h, a) } else { (a, h) };
                    self.sel = (s0 != s1).then_some((s0, s1));
                }
                // autoscroll while selecting in scrolled mode
                if !paginated {
                    if p.y > rect.bottom() - 30.0 {
                        self.scroll_by(8.0);
                    } else if p.y < rect.top() + 30.0 {
                        self.scroll_by(-8.0);
                    }
                }
            }
        }
        if resp.drag_stopped() {
            self.drag_anchor = None;
            if self.sel.is_some() {
                match self.hl_mode {
                    Some(c) => self.add_annotation(c, false),
                    None => self.sel_popup = resp.interact_pointer_pos(),
                }
            }
        }
        if resp.double_clicked() {
            self.pending_tap = None;
            if let Some(p) = resp.interact_pointer_pos() {
                if let Some((b, c)) = self.hit(&g, p, paginated, false) {
                    let text = self.book.flow().and_then(|f| f.sections.get(self.loc.section)).and_then(|s| s.blocks.get(b)).map(|b| b.text()).unwrap_or_default();
                    let (ws, we) = super::layout::word_at(&text, c);
                    if we > ws {
                        self.sel = Some(((b, ws), (b, we)));
                        self.sel_popup = Some(p);
                    }
                }
            }
        } else if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.on_click(&g, p, rect, paginated);
            }
        }
        if let Some(fx) = self.due_tap(ui.ctx()) {
            // right-to-left books turn forward from the left edge
            let (back, fwd) = if self.book.meta.rtl { (fx > 0.72, fx < 0.28) } else { (fx < 0.28, fx > 0.72) };
            if s.tap_zones && back {
                self.prev_page();
            } else if s.tap_zones && fwd {
                self.next_page();
            } else {
                self.tools = !self.tools;
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                if self.sel.is_none() {
                    if let Some((b, c)) = self.hit(&g, p, paginated, false) {
                        let text = self.book.flow().and_then(|f| f.sections.get(self.loc.section)).and_then(|s| s.blocks.get(b)).map(|b| b.text()).unwrap_or_default();
                        let (ws, we) = super::layout::word_at(&text, c);
                        if we > ws {
                            self.sel = Some(((b, ws), (b, we)));
                        }
                    }
                }
                if self.sel.is_some() {
                    self.sel_popup = Some(p);
                }
            }
        }
        if hover.is_some_and(|p| rect.contains(p)) {
            let (raw, smooth, cmd) = ui.input(|i| (i.raw_scroll_delta, i.smooth_scroll_delta, i.modifiers.command));
            if cmd {
                // ctrl+wheel = font size
                if raw.y != 0.0 {
                    self.bump_font(env, raw.y.signum());
                }
            } else if paginated {
                if s.wheel_turns_page {
                    self.wheel_acc += raw.y + raw.x;
                    if self.last_turn.elapsed() > Duration::from_millis(180) {
                        if self.wheel_acc <= -40.0 {
                            self.wheel_acc = 0.0;
                            self.next_page();
                        } else if self.wheel_acc >= 40.0 {
                            self.wheel_acc = 0.0;
                            self.prev_page();
                        }
                    } else {
                        self.wheel_acc = 0.0;
                    }
                }
            } else if smooth.y != 0.0 {
                self.scroll_by(-smooth.y);
            }
            // pointer cursor feedback
            if let Some(p) = hover {
                if let Some((_, local, top, _)) = self.locate(&g, p, paginated) {
                    if let Some(l) = &self.layout {
                        if l.over_text(top, local) {
                            let link = self.hit(&g, p, paginated, false).and_then(|(b, c)| self.book.flow()?.sections.get(self.loc.section)?.blocks.get(b)?.link_at(c));
                            ui.ctx().set_cursor_icon(if link.is_some() { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Text });
                        }
                    }
                }
            }
        }

        // ----- paint -----
        let decos = self.decos(pal);
        let Some(l) = &self.layout else { return };
        if l.section != self.loc.section {
            ui.ctx().request_repaint();
            return;
        }
        let mut painter = ui.painter_at(rect);
        if !full && c > 0.0 {
            painter.multiply_opacity(1.0 - c);
        }
        let frac = self.fraction();
        self.paint_book(&painter, &g, pal, frac);
        let prev_ok = |t: &super::Turn| self.prev_layout.as_ref().is_some_and(|p| p.section == t.section);
        let turn = self.turn.filter(|t| paginated && ((t.section == l.section && t.from != self.page) || (t.section != l.section && prev_ok(t))));
        let cross = turn.is_some_and(|t| t.section != l.section);
        let dur = if self.motion == 0 { 0.55 } else { 0.22 };
        let turn_t = turn.map(|t| (now - t.at).as_secs_f32() / dur).unwrap_or(1.0);
        // the 3D leaf is built for left-to-right books; rtl books use the sweep
        let flip3d = self.motion == 0 && turn_t < 1.0 && !rtl;
        if turn_t < 1.0 {
            ui.ctx().request_repaint();
        }
        let mut text_painter = painter.clone();
        if turn_t < 1.0 && !flip3d {
            text_painter.multiply_opacity(0.35 + 0.65 * super::ease(turn_t));
        }
        let tex = &self.tex;
        let texfn = |k: &str| tex.get(k).map(|t| t.tex.id());
        let dim = s.dim_images && pal.dark;
        if paginated {
            // during a 3D flip, the side the leaf starts from still shows the old spread
            // (possibly from the previous chapter's layout)
            let base: Vec<(bool, usize)> = match (flip3d, turn) {
                (true, Some(t)) if g.cols == 2 => {
                    if t.dir > 0 {
                        vec![(cross, t.from), (false, self.page + 1)]
                    } else {
                        vec![(false, self.page), (cross, t.from + 1)]
                    }
                }
                (true, Some(t)) if t.dir < 0 => vec![(cross, t.from)],
                _ => (0..g.cols).map(|c| (false, self.page + self.page_col(&g, c))).collect(),
            };
            for (col, (old, pi)) in base.into_iter().enumerate() {
                let lay = if old { self.prev_layout.as_ref().unwrap_or(l) } else { l };
                let no_decos: Vec<Deco> = Vec::new();
                if let Some(pg) = lay.pages.get(pi).filter(|p| p.u1 > p.u0) {
                    lay.paint(&text_painter, g.origin(col), pg.top, pg.bottom, &texfn, if old { &no_decos } else { &decos }, pal.fg, dim);
                }
            }
        } else {
            let clip = Rect::from_min_max(pos2(rect.left(), g.top - 4.0), pos2(rect.right(), g.top + g.page_h + 4.0));
            l.paint(&text_painter.with_clip_rect(clip), g.origin(0), self.scroll_y, self.scroll_y + g.page_h, &texfn, &decos, pal.fg, dim);
        }

        // chapter numeral hanging in the margin on a chapter's first page
        let lead_col = (paginated && g.cols == 2 && l.pages.len() > 1 && l.pages[0].u0 == l.pages[0].u1).then(|| if self.page_col(&g, 1) == 1 { 1 } else { 0 });
        if paginated && self.page == 0 && g.pad >= 64.0 {
            let n = self.book.toc_index_for(self.loc).map(|i| i + 1).unwrap_or(self.loc.section + 1);
            painter.text(pos2(g.origin(lead_col.unwrap_or(0)).x - 16.0, g.top - 6.0), egui::Align2::RIGHT_TOP, format!("{n:02}"), ui::ui_b(34.0), ACCENT);
        }

        // running heads and folios, set like a printed book
        let small = ui::ui_m(12.0);
        let chapter = self.chapter_title(self.loc);
        // shaped and reordered for display (Arabic/Hebrew titles), after truncating
        let trunc = |t: &str, n: usize| {
            let cut = if t.chars().count() > n { t.chars().take(n).collect::<String>() + "…" } else { t.to_string() };
            crate::book::script::visual(&cut).into_owned()
        };
        // the blank leaf facing the first page of a spread carries no running heads
        let lead = paginated && l.pages.len() > 1 && l.pages[0].u0 == l.pages[0].u1;
        let blank = |col: usize| lead && self.page + self.page_col(&g, col) == 0;
        if s.show_header {
            if g.cols == 2 {
                let (l0, l1) = (g.sheet(0), g.sheet(1));
                if !blank(0) {
                    painter.text(pos2(l0.center().x, l0.top() + 24.0), egui::Align2::CENTER_CENTER, trunc(&self.book.meta.title, 60), small.clone(), pal.muted);
                }
                if !blank(1) {
                    painter.text(pos2(l1.center().x, l1.top() + 24.0), egui::Align2::CENTER_CENTER, trunc(&chapter, 60), small.clone(), pal.muted);
                }
            } else {
                let sh = g.sheet(0);
                painter.text(pos2(sh.center().x, sh.top() + 24.0), egui::Align2::CENTER_CENTER, trunc(&chapter, 80), small.clone(), pal.muted);
            }
        }
        if s.show_footer {
            if self.book.flow().is_some() {
                let pct = (self.fraction() * 100.0).round();
                let left = self.chars_after(self.loc) as f32 / env.app.reading_speed_cpm.max(100.0);
                let left_txt = if left < 1.0 { tr("under a minute left in chapter").to_string() } else { trf!("{n} min left in chapter", n = left.round()) };
                let last = g.sheet(g.cols - 1);
                let first = g.sheet(0);
                let y = first.bottom() - 24.0;
                if paginated {
                    for col in 0..g.cols {
                        let pi = self.page + self.page_col(&g, col);
                        if pi < l.pages.len() && !blank(col) {
                            let sh = g.sheet(col);
                            let (x, al) = if col == 0 && g.cols == 2 { (sh.left() + g.pad, egui::Align2::LEFT_CENTER) } else { (sh.right() - g.pad, egui::Align2::RIGHT_CENTER) };
                            painter.text(pos2(x, y), al, format!("{}", pi + 1 - lead as usize), ui::ui_b(13.0), pal.muted);
                        }
                    }
                }
                if !blank(0) {
                    painter.text(pos2(first.center().x, y), egui::Align2::CENTER_CENTER, left_txt, small.clone(), pal.muted);
                }
                if g.cols == 2 && !blank(g.cols - 1) {
                    painter.text(pos2(last.center().x, y), egui::Align2::CENTER_CENTER, trf!("{pct}% of the book", pct = pct), small, pal.muted);
                }
            }
        }
        // bookmark ribbon hanging from the top edge of the page
        if self.bookmark_here().is_some() {
            let sh = g.sheet(g.cols - 1);
            let r = Rect::from_min_size(pos2(sh.right() - g.pad.min(60.0) - 7.0, sh.top()), vec2(14.0, 34.0));
            painter.add(egui::Shape::convex_polygon(vec![r.left_top(), r.right_top(), r.right_bottom(), pos2(r.center().x, r.bottom() - 7.0), r.left_bottom()], ACCENT, egui::Stroke::NONE));
        }

        if flip3d {
            if let Some(t) = turn {
                self.paint_flip(ui.ctx(), &painter, &g, pal, t, turn_t, dim, cross);
            }
        }
        // simple motion: a sheet sweeping into the spine with a shadowed edge
        if turn_t < 1.0 && paginated && !flip3d {
            let dir = turn.map(|t| t.dir).unwrap_or(1);
            let dir = if rtl { -dir } else { dir };
            let e = super::ease(turn_t);
            let target = if dir > 0 { g.sheet(g.cols - 1) } else { g.sheet(0) };
            let hinge = match (g.spine, dir > 0) {
                (Some(x), _) => x,
                (None, true) => target.left(),
                (None, false) => target.right(),
            };
            let w = target.width() * (1.0 - e);
            let leaf = if dir > 0 { Rect::from_min_max(pos2(hinge, target.top()), pos2(hinge + w, target.bottom())) } else { Rect::from_min_max(pos2(hinge - w, target.top()), pos2(hinge, target.bottom())) };
            if w > 1.0 {
                painter.rect_filled(leaf, CornerRadius::ZERO, ui::mix(pal.bg, pal.fg, 0.03));
                let edge_x = if dir > 0 { leaf.right() } else { leaf.left() };
                let sh = Color32::from_black_alpha((60.0 * (1.0 - e)) as u8);
                let mut m = egui::Mesh::default();
                let (a, b) = if dir > 0 { (edge_x, edge_x + 18.0) } else { (edge_x - 18.0, edge_x) };
                let (ca, cb) = if dir > 0 { (sh, Color32::TRANSPARENT) } else { (Color32::TRANSPARENT, sh) };
                m.colored_vertex(pos2(a, leaf.top()), ca);
                m.colored_vertex(pos2(b, leaf.top()), cb);
                m.colored_vertex(pos2(b, leaf.bottom()), cb);
                m.colored_vertex(pos2(a, leaf.bottom()), ca);
                m.add_triangle(0, 1, 2);
                m.add_triangle(0, 2, 3);
                painter.add(m);
            }
        } else if turn_t >= 1.0 {
            self.turn = None;
        }

        // a cover board on its way open or shut, over the pages
        if c > 0.0 {
            let top = ui.painter_at(rect);
            if full {
                self.paint_swing(&top, &cg, side, k * std::f32::consts::PI, pal);
            } else {
                let mut p = top;
                p.multiply_opacity(c);
                self.paint_closed(&p, cg.closed, side, pal, cg.d);
            }
        }
    }

    /// Where a cover board sits: closed (centred, alone) and open (hinged at
    /// the spine, or at the sheet's edge for single pages).
    fn cover_geo(&self, g: &Geo, side: Cover) -> CoverGeo {
        let all = g.sheets.iter().fold(g.sheets[0], |a, r| a.union(*r));
        let (top, bot) = (all.top(), all.bottom());
        let bw = g.sheet(0).width().min((bot - top) * 0.8);
        let cx = all.center().x;
        let d = if self.book.meta.rtl { -1.0 } else { 1.0 };
        // front: the board lies on the forward side of its hinge; back: behind it
        let hinge = match (g.spine, side) {
            (Some(x), _) => x,
            (None, Cover::Back) => cx + d * bw / 2.0,
            (None, _) => cx - d * bw / 2.0,
        };
        let open_c = if side == Cover::Back { hinge - d * bw / 2.0 } else { hinge + d * bw / 2.0 };
        CoverGeo { closed: Rect::from_min_max(pos2(cx - bw / 2.0, top), pos2(cx + bw / 2.0, bot)), pages: all, hinge, bw, top, bot, d, off: cx - open_c }
    }

    /// The closed book on the desk: click or scroll to turn the cover.
    fn closed_view(&mut self, ui: &mut Ui, rect: Rect, cg: &CoverGeo, side: Cover, pal: &Palette, board: Rect) {
        let resp = ui.interact(rect, ui.id().with("closed"), Sense::click());
        if self.cover_anim.is_none() {
            let wheel = if resp.hovered() { ui.input(|i| i.raw_scroll_delta.y + i.raw_scroll_delta.x) } else { 0.0 };
            let ready = self.last_turn.elapsed() > Duration::from_millis(300);
            let on_board = resp.interact_pointer_pos().is_some_and(|p| board.expand(8.0).contains(p));
            if (resp.clicked() && on_board) || (ready && wheel < -20.0 && side == Cover::Front) || (ready && wheel > 20.0 && side == Cover::Back) {
                if side == Cover::Front {
                    self.next_page();
                } else {
                    self.prev_page();
                }
            }
            if resp.hovered() && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| board.contains(p)) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        self.paint_closed(&ui.painter_at(rect), board, side, pal, cg.d);
    }

    /// A closed hardcover seen from above: board, thickness, hinge groove.
    fn paint_closed(&self, painter: &egui::Painter, r: Rect, side: Cover, pal: &Palette, d: f32) {
        // spine on the left of an LTR front cover, on the right of its back cover
        let spine_left = (side != Cover::Back) == (d > 0.0);
        let inward = if spine_left { 1.0 } else { -1.0 };
        for (o, a) in [(3.0, 26u8), (8.0, 13), (14.0, 6)] {
            painter.rect_filled(r.translate(vec2(o * 0.3 * inward, o)).expand(o * 0.5), CornerRadius::same(6), Color32::from_black_alpha(if pal.dark { a * 3 } else { a }));
        }
        // thickness: the far board and the page block peeking out at the fore edge and foot
        let ink = ui::INK;
        painter.rect_filled(r.translate(vec2(5.0 * inward, 6.0)), CornerRadius::same(4), ui::mix(ink, Color32::BLACK, 0.35));
        let block = Rect::from_min_max(r.min + vec2(if spine_left { 6.0 } else { 3.0 }, 4.0), r.max + vec2(if spine_left { 3.0 } else { -6.0 }, 3.0));
        painter.rect_filled(block, CornerRadius::same(1), ui::mix(ui::PAPER, Color32::BLACK, 0.12));
        for i in 0..3 {
            let y = block.bottom() - 0.8 - i as f32;
            painter.hline(block.x_range(), y, Stroke::new(0.6, ui::mix(ui::PAPER, Color32::BLACK, 0.25 + 0.1 * i as f32)));
        }
        // the board itself
        let cover = if side == Cover::Front { self.tex.get("__cover").map(|t| t.tex.id()) } else { None };
        match (side, cover) {
            (Cover::Front, Some(id)) => {
                painter.rect_filled(r, CornerRadius::same(3), ink);
                painter.image(id, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            (Cover::Front, None) => ui::paint_placeholder_cover(painter, r, &crate::book::script::visual(&self.book.meta.title), &self.book.meta.author),
            // the back board: plain cloth in the front cover's colour
            _ => {
                painter.rect_filled(r, CornerRadius::same(3), self.board_color());
            }
        }
        // rounded spine: shading into the hinge, then the pressed groove
        let sx = if spine_left { r.left() } else { r.right() };
        let mut m = egui::Mesh::default();
        let (x0, x1) = (sx, sx + inward * 26.0);
        let (c0, c1) = (Color32::from_black_alpha(90), Color32::TRANSPARENT);
        m.colored_vertex(pos2(x0, r.top()), c0);
        m.colored_vertex(pos2(x1, r.top()), c1);
        m.colored_vertex(pos2(x1, r.bottom()), c1);
        m.colored_vertex(pos2(x0, r.bottom()), c0);
        m.add_triangle(0, 1, 2);
        m.add_triangle(0, 2, 3);
        painter.add(m);
        let gx = sx + inward * 15.0;
        painter.vline(gx, r.y_range(), Stroke::new(1.5, Color32::from_black_alpha(70)));
        painter.vline(gx + inward * 1.5, r.y_range(), Stroke::new(1.0, Color32::from_white_alpha(28)));
        painter.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, Color32::from_black_alpha(50)), egui::StrokeKind::Inside);
    }

    /// Colour of the binding: the cover image's mean colour, or the generated cover's.
    fn board_color(&self) -> Color32 {
        match self.tex.get("__cover") {
            Some(t) => t.avg,
            None if self.book.meta.cover.is_some() => ui::INK,
            None => ui::placeholder_colors(&crate::book::script::visual(&self.book.meta.title)).0,
        }
    }

    fn board_ink(&self) -> Color32 {
        if self.book.meta.cover.is_some() { ui::PAPER } else { ui::placeholder_colors(&crate::book::script::visual(&self.book.meta.title)).1 }
    }

    /// A cover board rotating `theta` (0..π) about its hinge, from its open
    /// side (front: the forward side) across the spine.
    fn paint_swing(&self, painter: &egui::Painter, cg: &CoverGeo, side: Cover, theta: f32, pal: &Palette) {
        let (hinge, top, bot, d) = (cg.hinge, cg.top, cg.bot, cg.d);
        // front boards start lying forward of the hinge; back boards end lying behind it
        let fx = hinge + d * cg.bw * theta.cos();
        let lift = theta.sin() * (bot - top) * 0.04; // the free edge comes toward you
        let half = theta < std::f32::consts::FRAC_PI_2;
        let outside = if side == Cover::Front { half } else { !half };
        // the cloth side darkens as it turns from the light; the paper side only a little
        let shade = theta.sin() * if outside { 0.35 } else { 0.12 };
        let tint = Color32::from_gray((255.0 * (1.0 - shade)) as u8);
        let cover = if outside && side == Cover::Front { self.tex.get("__cover").map(|t| t.tex.id()) } else { None };
        // shadow the board casts on the pages beneath it
        let sw = (fx - hinge).abs() * 0.25 + 10.0;
        let (a, b) = if fx > hinge { (hinge, fx + sw) } else { (fx - sw, hinge) };
        painter.with_clip_rect(cg.pages).rect_filled(Rect::from_min_max(pos2(a, top), pos2(b, bot)), CornerRadius::ZERO, Color32::from_black_alpha((30.0 * theta.sin()) as u8));
        let quad = [pos2(hinge, top), pos2(fx, top - lift), pos2(fx, bot + lift), pos2(hinge, bot)];
        let mut m = match cover {
            Some(id) => egui::Mesh::with_texture(id),
            None => egui::Mesh::default(),
        };
        // inside of the board: a blank leaf, the same paper as the pages
        let board = if outside { self.board_color() } else { pal.bg };
        // texture u runs from the spine edge of the cover to its fore edge
        let uvs = if d > 0.0 { [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)] } else { [pos2(1.0, 0.0), pos2(0.0, 0.0), pos2(0.0, 1.0), pos2(1.0, 1.0)] };
        for (p, uv) in quad.iter().zip(uvs) {
            m.vertices.push(egui::epaint::Vertex { pos: *p, uv: if cover.is_some() { uv } else { egui::epaint::WHITE_UV }, color: if cover.is_some() { tint } else { ui::mix(board, Color32::BLACK, shade) } });
        }
        m.add_triangle(0, 1, 2);
        m.add_triangle(0, 2, 3);
        painter.add(m);
        // board edge: a thin dark line on the free edge gives it thickness
        painter.line_segment([quad[1], quad[2]], Stroke::new(2.0, Color32::from_black_alpha((60.0 + 60.0 * theta.sin()) as u8)));
        if cover.is_none() && outside && side == Cover::Front && (fx - hinge).abs() > 60.0 {
            let r = Rect::from_two_pos(pos2(hinge, top), pos2(fx, bot));
            painter.with_clip_rect(r).text(r.left_top() + vec2(24.0, 40.0), egui::Align2::LEFT_TOP, crate::book::script::visual(&self.book.meta.title), ui::ui_b(28.0), self.board_ink());
        }
    }

    /// Tessellate one laid-out page into meshes (text + images) so it can be bent.
    fn page_meshes(&self, ctx: &egui::Context, from_prev: bool, page: usize, origin: Pos2, fg: Color32, dim: bool) -> Vec<egui::Mesh> {
        let l = if from_prev { self.prev_layout.as_ref().or(self.layout.as_ref()) } else { self.layout.as_ref() };
        let Some(l) = l else { return Vec::new() };
        let Some(pg) = l.pages.get(page).filter(|p| p.u1 > p.u0) else { return Vec::new() };
        let (top, bottom) = (pg.top, pg.bottom);
        let clip = Rect::from_min_max(pos2(origin.x - 40.0, origin.y - 1.0), pos2(origin.x + l.width + 40.0, origin.y + (bottom - top) + 1.0));
        let mut shapes = Vec::new();
        let start = l.blocks.partition_point(|b| b.y + b.h < top);
        for b in l.blocks.iter().skip(start) {
            if b.y > bottom {
                break;
            }
            let by = origin.y + b.y - top;
            match &b.item {
                super::layout::Item::Text { galley, ax, .. } => shapes.push(egui::Shape::galley(pos2(origin.x + ax, by), ctx.fonts(|f| f.layout_job((*galley.job).clone())), fg)),
                super::layout::Item::Table(lt) => shapes.extend(ctx.fonts(|f| lt.shapes(f, pos2(origin.x, by), fg, clip))),
                super::layout::Item::Image { key, size, x } => {
                    if let Some(t) = self.tex.get(key) {
                        let tint = if dim { Color32::from_gray(225) } else { Color32::WHITE };
                        shapes.push(egui::Shape::image(t.tex.id(), Rect::from_min_size(pos2(origin.x + x, by), *size), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint));
                    }
                }
                _ => {}
            }
        }
        let clipped: Vec<egui::epaint::ClippedShape> = shapes.into_iter().map(|shape| egui::epaint::ClippedShape { clip_rect: clip, shape }).collect();
        let mut out = Vec::new();
        for p in ctx.tessellate(clipped, ctx.pixels_per_point()) {
            if let egui::epaint::Primitive::Mesh(m) = p.primitive {
                // keep only triangles inside this page (galleys may run past it)
                let mut nm = egui::Mesh::with_texture(m.texture_id);
                nm.vertices = m.vertices.clone();
                for tri in m.indices.chunks_exact(3) {
                    let cy = tri.iter().map(|&i| m.vertices[i as usize].pos.y).sum::<f32>() / 3.0;
                    if cy >= clip.top() && cy <= clip.bottom() {
                        nm.indices.extend_from_slice(tri);
                    }
                }
                out.push(nm);
            }
        }
        out
    }

    /// A real page flip: the leaf rotates about the spine with perspective,
    /// carrying the old page's text on its front and the new page's on its back.
    #[allow(clippy::too_many_arguments)]
    fn paint_flip(&self, ctx: &egui::Context, painter: &egui::Painter, g: &Geo, pal: &Palette, t: super::Turn, turn_t: f32, dim: bool, cross: bool) {
        let k = turn_t.clamp(0.0, 1.0);
        let k = k * k * (3.0 - 2.0 * k);
        let pi = std::f32::consts::PI;
        let two = g.cols == 2;
        // (hinge x, front page+origin+sheet, back page+origin+sheet, angle)
        let (hinge, front, back, theta) = if two {
            let h = g.spine.unwrap_or(g.sheet(0).right());
            if t.dir > 0 {
                (h, (t.from + 1, g.origin(1), g.sheet(1)), Some((self.page, g.origin(0), g.sheet(0))), k * pi)
            } else {
                (h, (t.from, g.origin(0), g.sheet(0)), Some((self.page + 1, g.origin(1), g.sheet(1))), k * pi)
            }
        } else if t.dir > 0 {
            (g.sheet(0).left(), (t.from, g.origin(0), g.sheet(0)), None, k * pi / 2.0)
        } else {
            (g.sheet(0).left(), (self.page, g.origin(0), g.sheet(0)), None, (1.0 - k) * pi / 2.0)
        };
        let w = g.sheet(0).width();
        let sheet0 = front.2;
        let cy = sheet0.center().y;
        let half_h = sheet0.height() / 2.0;
        // which side of the hinge the leaf starts on
        let s0 = if sheet0.center().x >= hinge { 1.0 } else { -1.0 };
        // paper, not board: the free edge trails the hinge (strongest mid-turn)
        let bend = 0.75 * (pi * k).sin();
        let flutter = (pi * k).sin() * (1.0 + 0.35 * (k * pi * 3.0).sin());
        let local = |u: f32| theta - bend * u.powf(1.5);
        let along = |a: f32| -> f32 {
            let n = 12;
            let step = a / n as f32;
            (0..n).map(|i| local((step * (i as f32 + 0.5)) / w).cos() * step).sum()
        };
        let warp = |p: Pos2| -> (Pos2, f32) {
            let a = (p.x - hinge).abs();
            let u = (a / w).min(1.2);
            let th = local(u);
            let x = hinge + s0 * along(a);
            let lift = 1.0 + 0.035 * th.sin() * u;
            let droop = 2.5 * flutter * u * u * ((p.y - cy) / half_h);
            (pos2(x, cy + (p.y - cy) * lift + droop), th)
        };
        let mid = local(0.5);
        let show_front = mid < pi / 2.0;
        let (page, origin, sheet) = if show_front { front } else { back.unwrap_or(front) };
        // the front of the leaf is the old page (maybe last chapter); its back is new
        let from_prev = show_front && cross && (two || t.dir > 0);
        // shadow the leaf throws onto the spread
        let edge_x = hinge + s0 * along(w);
        let (sx0, sx1) = if edge_x > hinge { (hinge, edge_x + 26.0 * theta.sin()) } else { (edge_x - 26.0 * theta.sin(), hinge) };
        painter.rect_filled(Rect::from_min_max(pos2(sx0, sheet.top()), pos2(sx1, sheet.bottom())), CornerRadius::ZERO, Color32::from_black_alpha((36.0 * theta.sin()) as u8));
        // the paper itself, as a strip so it can curve
        let mut paper = egui::Mesh::default();
        let cols = 24;
        for i in 0..=cols {
            let a = w * i as f32 / cols as f32;
            let x0 = hinge + if sheet.center().x >= hinge { a } else { -a };
            let (pt, th) = warp(pos2(x0, sheet.top()));
            let (pb, _) = warp(pos2(x0, sheet.bottom()));
            let shade = 1.0 - 0.12 * th.sin().abs();
            let c = Color32::from_rgb((pal.bg.r() as f32 * shade) as u8, (pal.bg.g() as f32 * shade) as u8, (pal.bg.b() as f32 * shade) as u8);
            paper.colored_vertex(pt, c);
            paper.colored_vertex(pb, c);
            if i > 0 {
                let j = (i * 2) as u32;
                paper.add_triangle(j - 2, j - 1, j);
                paper.add_triangle(j - 1, j + 1, j);
            }
        }
        painter.add(egui::Shape::mesh(paper));
        // the page's own text, bent with the paper
        for mut m in self.page_meshes(ctx, from_prev, page, origin, pal.fg, dim) {
            for v in &mut m.vertices {
                let (p, th) = warp(v.pos);
                v.pos = p;
                v.color = v.color.linear_multiply(1.0 - 0.12 * th.sin().abs());
            }
            painter.add(egui::Shape::mesh(m));
        }
        ctx.request_repaint();
    }

    fn on_click(&mut self, g: &Geo, p: Pos2, rect: Rect, paginated: bool) {
        if self.sel_popup.is_some() || self.hl_popup.is_some() || self.footnote.is_some() || self.sel.is_some() {
            self.clear_selection();
            self.footnote = None;
            return;
        }
        if let Some((b, c)) = self.hit(g, p, paginated, false) {
            let sec = self.loc.section;
            let here = Loc::new(sec, b, c);
            if let Some(a) = self.data.annotations.iter().find(|a| a.start <= here && here < a.end) {
                self.hl_popup = Some((a.id, p));
                return;
            }
            let link = self.book.flow().and_then(|f| f.sections.get(sec)).and_then(|s| s.blocks.get(b)).and_then(|blk| {
                let li = blk.link_at(c)?;
                let mut pos = 0;
                let mut text = String::new();
                for sp in &blk.spans {
                    let n = sp.text.chars().count();
                    if sp.link == Some(li) && c >= pos && c < pos + n + 1 {
                        text = sp.text.clone();
                    }
                    pos += n;
                }
                Some((li, text))
            });
            if let Some((li, text)) = link {
                self.follow_link(li, p, &text);
                return;
            }
        }
        // A click on a word might be the first half of a double-click (select
        // word), so its page turn / toolbar toggle waits out the double-click
        // interval. Clicks on margins and blank space act at once: turning
        // pages by clicking must not lag.
        let on_word = self.hit(g, p, paginated, false).is_some_and(|(b, c)| {
            let text = self.book.flow().and_then(|f| f.sections.get(self.loc.section)).and_then(|s| s.blocks.get(b)).map(|b| b.text()).unwrap_or_default();
            let (ws, we) = super::layout::word_at(&text, c);
            we > ws
        });
        let at = if on_word { Instant::now() } else { Instant::now().checked_sub(Duration::from_secs(5)).unwrap_or_else(Instant::now) };
        self.pending_tap = Some(((p.x - rect.left()) / rect.width(), at));
    }

    /// The pending single click, once it's clear it isn't the start of a double-click.
    pub(super) fn due_tap(&mut self, ctx: &egui::Context) -> Option<f32> {
        let (fx, at) = self.pending_tap?;
        let wait = Duration::from_secs_f64(ctx.options(|o| o.input_options.max_double_click_delay)) + Duration::from_millis(30);
        let el = at.elapsed();
        if el >= wait {
            self.pending_tap = None;
            Some(fx)
        } else {
            ctx.request_repaint_after(wait - el);
            None
        }
    }
}
