//! Reflowable view: geometry, layout cache, painting, pointer interaction.

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::layout::{Deco, DecoKind, SectionLayout, Typo};
use super::{deco_color, Env, Reader};
use crate::book::{BlockKind, Loc};
use crate::store::{HlStyle, Layout, ReaderSettings};
use crate::texload::Src;
use crate::ui::{self, Palette, ACCENT};

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
        let paginated = s.layout == Layout::Paginated;
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
            font_tag: format!("{}{}", env.installed.book_family, env.installed.cjk),
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (si, typo.key(), known, s.layout == Layout::Paginated).hash(&mut h);
        let key = h.finish();
        if self.layout.as_ref().is_some_and(|l| l.key == key) {
            return;
        }
        let tex = &self.tex;
        let lay = SectionLayout::build(ctx, si, sec, &typo, &|k| tex.get(k).map(|l| l.size), key);
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
            if self.layout.as_ref().map(|l| l.section) != Some(si) || s.layout == Layout::Paginated {
                self.scroll_y = (lay.y_of(self.loc.block, self.loc.offset) - 4.0).max(0.0);
            }
        }
        self.layout = Some(lay);
    }

    pub(super) fn next_page(&mut self) {
        if self.book.fixed().is_some() {
            return self.fixed_next();
        }
        let Some(flow) = self.book.flow() else { return };
        let n = flow.sections.len();
        let Some(l) = &self.layout else { return };
        if self.data.settings.as_ref().map(|s| s.layout).unwrap_or(self.last_layout) == Layout::Scrolled {
            return self.scroll_by(self.view_h * 0.9);
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
        let Some(l) = &self.layout else { return };
        if self.data.settings.as_ref().map(|s| s.layout).unwrap_or(self.last_layout) == Layout::Scrolled {
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
                self.scroll_y = 0.0;
                self.dirty = true;
                return;
            }
            self.scroll_y = max;
        } else if target < 0.0 {
            if self.scroll_y <= 1.0 && l.section > 0 {
                self.loc = Loc::new(l.section - 1, 0, 0);
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
            let pg = l.pages.get(self.page + col)?;
            Some((col, pos - o, pg.top, pg.bottom))
        } else {
            Some((0, pos - o, self.scroll_y, self.scroll_y + g.page_h))
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
        let paginated = s.layout == Layout::Paginated;
        let g = self.geometry(rect, s);
        self.view_h = g.page_h;
        self.ensure_layout(ui.ctx(), s, pal, env, &g);
        self.motion = s.motion;
        if self.opened_at.is_none() {
            let _ = self.cover_tex();
            if self.tex.get("__cover").is_some() || self.book.meta.cover.is_none() || self.tex.state("__cover").is_some_and(|s| matches!(s, crate::texload::State::Failed)) {
                self.opened_at = Some(std::time::Instant::now());
            } else {
                ui.ctx().request_repaint();
            }
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
                self.on_click(&g, p, rect, s, paginated);
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
        let painter = ui.painter_at(rect);
        let frac = self.book.flow().map(|f| f.fraction(self.loc)).unwrap_or(0.0);
        self.paint_book(&painter, &g, pal, frac);
        // opening: the cover swings away toward the spine
        let now = std::time::Instant::now();
        let open_t = if self.motion == 0 { self.opened_at.map(|t| (now - t).as_secs_f32() / 0.95).unwrap_or(0.0) } else { 1.0 };
        let turn = self.turn.filter(|t| paginated && t.section == l.section && t.from != self.page);
        let dur = if self.motion == 0 { 0.55 } else { 0.22 };
        let turn_t = turn.map(|t| (now - t.at).as_secs_f32() / dur).unwrap_or(1.0);
        let flip3d = self.motion == 0 && turn_t < 1.0;
        if open_t < 1.0 || turn_t < 1.0 {
            ui.ctx().request_repaint();
        }
        let mut text_painter = painter.clone();
        if open_t < 1.0 {
            text_painter.multiply_opacity(super::ease((open_t - 0.3) / 0.6));
        } else if turn_t < 1.0 && !flip3d {
            text_painter.multiply_opacity(0.35 + 0.65 * super::ease(turn_t));
        }
        let tex = &self.tex;
        let texfn = |k: &str| tex.get(k).map(|t| t.tex.id());
        let dim = s.dim_images && pal.dark;
        if paginated {
            // during a 3D flip, the side the leaf starts from still shows the old spread
            let base: Vec<usize> = match (flip3d, turn) {
                (true, Some(t)) if g.cols == 2 => {
                    if t.dir > 0 {
                        vec![t.from, self.page + 1]
                    } else {
                        vec![self.page, t.from + 1]
                    }
                }
                (true, Some(t)) if t.dir < 0 => vec![t.from],
                _ => (0..g.cols).map(|c| self.page + c).collect(),
            };
            for (col, pi) in base.into_iter().enumerate() {
                if let Some(pg) = l.pages.get(pi) {
                    l.paint(&text_painter, g.origin(col), pg.top, pg.bottom, &texfn, &decos, pal.fg, dim);
                }
            }
        } else {
            let clip = Rect::from_min_max(pos2(rect.left(), g.top - 4.0), pos2(rect.right(), g.top + g.page_h + 4.0));
            l.paint(&text_painter.with_clip_rect(clip), g.origin(0), self.scroll_y, self.scroll_y + g.page_h, &texfn, &decos, pal.fg, dim);
        }

        // chapter numeral hanging in the margin on a chapter's first page
        if paginated && self.page == 0 && g.pad >= 64.0 {
            let n = self.book.toc_index_for(self.loc).map(|i| i + 1).unwrap_or(self.loc.section + 1);
            painter.text(pos2(g.origin(0).x - 16.0, g.top - 6.0), egui::Align2::RIGHT_TOP, format!("{n:02}"), ui::ui_b(34.0), ACCENT);
        }

        // running heads and folios, set like a printed book
        let small = ui::ui_m(12.0);
        let chapter = self.chapter_title(self.loc);
        let trunc = |t: &str, n: usize| if t.chars().count() > n { t.chars().take(n).collect::<String>() + "…" } else { t.to_string() };
        if s.show_header {
            if g.cols == 2 {
                let (l0, l1) = (g.sheet(0), g.sheet(1));
                painter.text(pos2(l0.center().x, l0.top() + 24.0), egui::Align2::CENTER_CENTER, trunc(&self.book.meta.title, 60), small.clone(), pal.muted);
                painter.text(pos2(l1.center().x, l1.top() + 24.0), egui::Align2::CENTER_CENTER, trunc(&chapter, 60), small.clone(), pal.muted);
            } else {
                let sh = g.sheet(0);
                painter.text(pos2(sh.center().x, sh.top() + 24.0), egui::Align2::CENTER_CENTER, trunc(&chapter, 80), small.clone(), pal.muted);
            }
        }
        if s.show_footer {
            if let Some(flow) = self.book.flow() {
                let pct = (flow.fraction(self.loc) * 100.0).round();
                let left = flow.section_chars_after(self.loc) as f32 / env.app.reading_speed_cpm.max(100.0);
                let left_txt = if left < 1.0 { "under a minute left in chapter".to_string() } else { format!("{} min left in chapter", left.round()) };
                let last = g.sheet(g.cols - 1);
                let first = g.sheet(0);
                let y = first.bottom() - 24.0;
                if paginated {
                    for col in 0..g.cols {
                        let pi = self.page + col;
                        if pi < l.pages.len() {
                            let sh = g.sheet(col);
                            let (x, al) = if col == 0 && g.cols == 2 { (sh.left() + g.pad, egui::Align2::LEFT_CENTER) } else { (sh.right() - g.pad, egui::Align2::RIGHT_CENTER) };
                            painter.text(pos2(x, y), al, format!("{}", pi + 1), ui::ui_b(13.0), pal.muted);
                        }
                    }
                }
                painter.text(pos2(first.center().x, y), egui::Align2::CENTER_CENTER, left_txt, small.clone(), pal.muted);
                if g.cols == 2 {
                    painter.text(pos2(last.center().x, y), egui::Align2::CENTER_CENTER, format!("{pct}% of the book"), small, pal.muted);
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
                self.paint_flip(ui.ctx(), &painter, &g, pal, t, turn_t, dim);
            }
        }
        // simple motion: a sheet sweeping into the spine with a shadowed edge
        if turn_t < 1.0 && paginated && !flip3d {
            let dir = turn.map(|t| t.dir).unwrap_or(1);
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

        // the cover swinging open: a hinged board rotating 180° about the spine
        if open_t < 1.0 {
            let k = open_t.clamp(0.0, 1.0);
            let e = k * k * (3.0 - 2.0 * k); // smoothstep: slow start, slow landing
            let theta = e * std::f32::consts::PI;
            let target = g.sheet(g.cols - 1);
            let hinge = g.spine.unwrap_or(target.left());
            let (top, bot) = (target.top(), target.bottom());
            let w = target.width();
            let fx = hinge + w * theta.cos();
            let lift = theta.sin() * target.height() * 0.07; // the free edge comes toward you
            let front = theta < std::f32::consts::FRAC_PI_2;
            let shade = (theta.sin() * 0.35 + if front { 0.0 } else { 0.1 }).min(0.5);
            let tint = Color32::from_gray((255.0 * (1.0 - shade)) as u8);
            let quad = [pos2(hinge, top), pos2(fx, top - lift), pos2(fx, bot + lift), pos2(hinge, bot)];
            let cover = if front { self.tex.get("__cover").map(|t| t.tex.id()) } else { None };
            let mut m = match cover {
                Some(id) => egui::Mesh::with_texture(id),
                None => egui::Mesh::default(),
            };
            let board = if front { ui::INK } else { ui::mix(pal.bg, pal.fg, 0.1) };
            let uvs = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
            for (p, uv) in quad.iter().zip(uvs) {
                m.vertices.push(egui::epaint::Vertex { pos: *p, uv: if cover.is_some() { uv } else { egui::epaint::WHITE_UV }, color: if cover.is_some() { tint } else { board.linear_multiply(1.0 - shade) } });
            }
            m.add_triangle(0, 1, 2);
            m.add_triangle(0, 2, 3);
            // shadow the board casts on the page it is leaving
            if front {
                let sw = (fx - hinge).abs() * 0.25 + 10.0;
                painter.rect_filled(Rect::from_min_max(pos2(fx.min(hinge), top), pos2((fx + sw).max(hinge), bot)), CornerRadius::ZERO, Color32::from_black_alpha((50.0 * theta.sin()) as u8));
            }
            painter.add(m);
            if cover.is_none() && front && (fx - hinge) > 60.0 {
                let r = Rect::from_min_max(pos2(hinge, top), pos2(fx, bot));
                painter.with_clip_rect(r).text(r.left_top() + vec2(24.0, 40.0), egui::Align2::LEFT_TOP, &self.book.meta.title, ui::ui_b(28.0), ui::PAPER);
            }
        }
    }

    /// Tessellate one laid-out page into meshes (text + images) so it can be bent.
    fn page_meshes(&self, ctx: &egui::Context, page: usize, origin: Pos2, fg: Color32, dim: bool) -> Vec<egui::Mesh> {
        let Some(l) = &self.layout else { return Vec::new() };
        let Some(pg) = l.pages.get(page) else { return Vec::new() };
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
                super::layout::Item::Text { galley, ax } => shapes.push(egui::Shape::galley(pos2(origin.x + ax, by), galley.clone(), fg)),
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
    fn paint_flip(&self, ctx: &egui::Context, painter: &egui::Painter, g: &Geo, pal: &Palette, t: super::Turn, turn_t: f32, dim: bool) {
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
        let cy = g.sheet(0).center().y;
        let show_front = theta < pi / 2.0;
        let (page, origin, sheet) = if show_front { front } else { back.unwrap_or(front) };
        let sign = if show_front { 1.0 } else { -1.0 };
        let (c, s) = (theta.cos(), theta.sin());
        let warp = |p: Pos2| -> Pos2 {
            let rel = p.x - hinge;
            let x = hinge + sign * rel * c;
            let persp = 1.0 + 0.09 * s * (rel.abs() / w);
            pos2(x, cy + (p.y - cy) * persp)
        };
        // shadow the leaf throws onto the spread
        let reach = (w * c.abs()).max(12.0);
        let side = if (t.dir > 0) == show_front { 1.0 } else { -1.0 };
        let sh_rect = if side > 0.0 { Rect::from_min_max(pos2(hinge, sheet.top()), pos2(hinge + reach + 30.0 * s, sheet.bottom())) } else { Rect::from_min_max(pos2(hinge - reach - 30.0 * s, sheet.top()), pos2(hinge, sheet.bottom())) };
        painter.rect_filled(sh_rect, CornerRadius::ZERO, Color32::from_black_alpha((40.0 * s) as u8));
        // the paper of the leaf
        let shade = 1.0 - 0.22 * s;
        let paper = Color32::from_rgb((pal.bg.r() as f32 * shade) as u8, (pal.bg.g() as f32 * shade) as u8, (pal.bg.b() as f32 * shade) as u8);
        let corners = [sheet.left_top(), sheet.right_top(), sheet.right_bottom(), sheet.left_bottom()].map(warp);
        painter.add(egui::Shape::convex_polygon(corners.to_vec(), paper, Stroke::new(1.0, Color32::from_black_alpha(30))));
        // the page's own text, bent with the leaf
        for mut m in self.page_meshes(ctx, page, origin, pal.fg, dim) {
            for v in &mut m.vertices {
                v.pos = warp(v.pos);
                v.color = v.color.linear_multiply(shade);
            }
            painter.add(egui::Shape::mesh(m));
        }
        ctx.request_repaint();
    }

    fn on_click(&mut self, g: &Geo, p: Pos2, rect: Rect, s: &ReaderSettings, paginated: bool) {
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
        let fx = (p.x - rect.left()) / rect.width();
        if s.tap_zones && fx < 0.28 {
            self.prev_page();
        } else if s.tap_zones && fx > 0.72 {
            self.next_page();
        } else {
            self.tools = !self.tools;
        }
    }
}
