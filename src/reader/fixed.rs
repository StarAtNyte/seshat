//! Fixed-layout view for PDF and CBZ: fit page / fit width, two-page spreads,
//! right-to-left (manga) order, zoom & pan, progressive-resolution rendering
//! with prefetch and bounded texture memory.

use crate::i18n::tr;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Rect, Sense, Ui};

use super::Reader;
use crate::book::pdf::PageText;
use crate::book::{FixedKind, ResRef};
use crate::store::HlStyle;
use crate::store::ReaderSettings;
use crate::texload::Src;
use crate::ui::Palette;

#[derive(Default)]
pub struct FixedState {
    pub page: usize,
    /// 0 = fit page, 1 = fit width
    pub zoom: u8,
    pub scale: f32,
    pub scroll: f32,
    pub pan_x: f32,
    pub rtl: bool,
    pub aspects: Vec<f32>,
    pub texts: Option<Arc<Vec<String>>>,
    best: HashMap<usize, (u32, String)>,
    spread: bool,
    /// Crop white margins (per book).
    pub crop: bool,
    crop_started: bool,
    /// Common content box for even / odd pages (page fractions l, t, r, b).
    crop_common: [[f32; 4]; 2],
    crop_ready: bool,
    /// Show the PDF text as a flowing document.
    pub reflow: bool,
    pub texts_loading: bool,
    paras: Option<Arc<Vec<Vec<(bool, String)>>>>,
    heights: Vec<f32>,
    reflow_page: usize,
    /// Scroll correction queued for the next frame (pages above the view changed size).
    reflow_adjust: f32,
    /// Character boxes of nearby PDF pages (selection, highlights).
    pub glyphs: HashMap<usize, Arc<PageText>>,
    pub glyphs_pending: HashSet<usize>,
}

const FULL: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

impl FixedState {
    pub(super) fn set_crops(&mut self, boxes: Vec<Option<[f32; 4]>>) {
        self.crop_common = common_crop(&boxes);
        self.crop_ready = true;
    }

    fn crop_of(&self, page: usize) -> [f32; 4] {
        if self.crop && self.crop_ready {
            self.crop_common[page % 2]
        } else {
            FULL
        }
    }
}

/// Content box (l, t, r, b as fractions) of a rendered page: rows/columns whose pixels differ
/// from the background (taken from the corners) mark the content. None for blank pages.
pub fn detect_margins(w: usize, h: usize, rgba: &[u8]) -> Option<[f32; 4]> {
    if w < 8 || h < 8 || rgba.len() < w * h * 4 {
        return None;
    }
    let px = |x: usize, y: usize| -> [i32; 3] {
        let i = (y * w + x) * 4;
        [rgba[i] as i32, rgba[i + 1] as i32, rgba[i + 2] as i32]
    };
    let corners = [px(0, 0), px(w - 1, 0), px(0, h - 1), px(w - 1, h - 1)];
    let bg: [i32; 3] = std::array::from_fn(|c| corners.iter().map(|p| p[c]).sum::<i32>() / 4);
    let ink = |x: usize, y: usize| -> bool {
        let p = px(x, y);
        (p[0] - bg[0]).abs() + (p[1] - bg[1]).abs() + (p[2] - bg[2]).abs() > 90
    };
    let mut rows = vec![0usize; h];
    let mut cols = vec![0usize; w];
    for y in 0..h {
        for x in 0..w {
            if ink(x, y) {
                rows[y] += 1;
                cols[x] += 1;
            }
        }
    }
    let rmin = (w / 120).max(2);
    let cmin = (h / 120).max(2);
    let top = rows.iter().position(|&n| n >= rmin)?;
    let bottom = rows.iter().rposition(|&n| n >= rmin)?;
    let left = cols.iter().position(|&n| n >= cmin)?;
    let right = cols.iter().rposition(|&n| n >= cmin)?;
    if right <= left || bottom <= top {
        return None;
    }
    Some([left as f32 / w as f32, top as f32 / h as f32, (right + 1) as f32 / w as f32, (bottom + 1) as f32 / h as f32])
}

/// One crop box per page parity (so odd/even pages don't jump): robust extremes over all pages.
pub fn common_crop(boxes: &[Option<[f32; 4]>]) -> [[f32; 4]; 2] {
    let mut out = [FULL; 2];
    let mut per: [Vec<[f32; 4]>; 2] = [Vec::new(), Vec::new()];
    for (i, b) in boxes.iter().enumerate() {
        if let Some(b) = b {
            per[i % 2].push(*b);
        }
    }
    let all: Vec<[f32; 4]> = per.iter().flatten().copied().collect();
    for par in 0..2 {
        let v = if per[par].len() >= 3 { &per[par] } else { &all };
        if v.is_empty() {
            continue;
        }
        let pick = |k: usize, hi: bool| -> f32 {
            let mut xs: Vec<f32> = v.iter().map(|b| b[k]).collect();
            xs.sort_by(|a, b| a.total_cmp(b));
            let n = xs.len();
            xs[if hi { (n * 97 / 100).min(n - 1) } else { n * 3 / 100 }]
        };
        const PAD: f32 = 0.015;
        let b = [(pick(0, false) - PAD).max(0.0), (pick(1, false) - PAD).max(0.0), (pick(2, true) + PAD).min(1.0), (pick(3, true) + PAD).min(1.0)];
        if b[2] - b[0] >= 0.25 && b[3] - b[1] >= 0.25 {
            out[par] = b;
        }
    }
    out
}

fn is_end(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | ':' | ';' | '"' | '”' | '’' | ')' | '…' | '»')
}

/// Rebuild paragraphs from extracted page text: join lines, undo hyphenation, and split on blank
/// lines or short lines that end a sentence. The flag marks a first paragraph that continues the
/// previous page's last one.
pub fn build_paragraphs(texts: &[String]) -> Vec<Vec<(bool, String)>> {
    let mut pages: Vec<Vec<(bool, String)>> = Vec::with_capacity(texts.len());
    for t in texts {
        let lines: Vec<&str> = t.lines().map(|l| l.trim()).collect();
        let maxlen = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(1);
        let mut paras: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut prev_len = 0usize;
        for l in lines {
            if l.is_empty() {
                if !cur.is_empty() {
                    paras.push(std::mem::take(&mut cur));
                }
                continue;
            }
            // lone page numbers
            if l.len() <= 4 && l.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let len = l.chars().count();
            if !cur.is_empty() {
                let last = cur.chars().last().unwrap_or(' ');
                let first = l.chars().next().unwrap_or(' ');
                let bullet = matches!(first, '•' | '–' | '—' | '-' | '*') || (first.is_ascii_digit() && l.chars().nth(1).is_some_and(|c| c == '.' || c == ')'));
                let short_end = is_end(last) && (prev_len as f32) < maxlen as f32 * 0.75;
                if short_end || bullet {
                    paras.push(std::mem::take(&mut cur));
                }
            }
            if cur.is_empty() {
                cur.push_str(l);
            } else {
                let last = cur.chars().last().unwrap_or(' ');
                let before = cur.chars().rev().nth(1).unwrap_or(' ');
                let first = l.chars().next().unwrap_or(' ');
                if last == '-' && before.is_alphabetic() && first.is_lowercase() {
                    cur.pop();
                } else {
                    cur.push(' ');
                }
                cur.push_str(l);
            }
            prev_len = len;
        }
        if !cur.is_empty() {
            paras.push(cur);
        }
        let mut v: Vec<(bool, String)> = paras.into_iter().map(|p| (false, p)).collect();
        // continuation of the previous page's last paragraph
        if let (Some(prev), Some(first)) = (pages.last_mut().and_then(|p| p.last_mut()), v.first_mut()) {
            let last = prev.1.chars().last().unwrap_or('.');
            let starts_lower = first.1.chars().next().is_some_and(|c| c.is_lowercase());
            if (!is_end(last) && starts_lower) || (last == '-' && starts_lower) {
                if last == '-' {
                    prev.1.pop();
                }
                first.0 = true;
            }
        }
        pages.push(v);
    }
    pages
}

impl Reader {
    /// Detect margins for every page in the background (small renders), then crop.
    pub(super) fn start_crop(&mut self) {
        if self.fx.crop_started {
            return;
        }
        let Some(f) = self.book.fixed() else { return };
        self.fx.crop_started = true;
        let (tx, ctx, path, pages) = (self.tx.clone(), self.ctx.clone(), f.path.clone(), f.pages);
        let entries = match &f.kind {
            FixedKind::Cbz { entries } => Some(entries.clone()),
            FixedKind::Pdf => None,
        };
        std::thread::spawn(move || {
            let mut out = Vec::with_capacity(pages);
            match entries {
                None => {
                    if let Ok(pdf) = crate::book::pdf::load(&path) {
                        for p in 0..pages {
                            out.push(crate::book::pdf::render(&pdf, p, 200).and_then(|(w, h, d)| detect_margins(w as usize, h as usize, &d)));
                        }
                    }
                }
                Some(entries) => {
                    for e in entries {
                        let r = ResRef::Zip { path: path.clone(), entry: e };
                        out.push(r.load().and_then(|b| image::load_from_memory(&b).ok()).and_then(|img| {
                            let t = img.thumbnail(240, 240).to_rgba8();
                            detect_margins(t.width() as usize, t.height() as usize, t.as_raw())
                        }));
                    }
                }
            }
            let _ = tx.send(super::Msg::Crops(out));
            ctx.request_repaint();
        });
    }

    /// Extract every page's text in the background (reflow, accessibility).
    pub(super) fn start_texts(&mut self) {
        if self.fx.texts.is_some() || self.fx.texts_loading {
            return;
        }
        let Some(f) = self.book.fixed() else { return };
        if !matches!(f.kind, FixedKind::Pdf) {
            return;
        }
        self.fx.texts_loading = true;
        let (tx, ctx, path, pages) = (self.tx.clone(), self.ctx.clone(), f.path.clone(), f.pages);
        std::thread::spawn(move || {
            let mut v = Vec::with_capacity(pages);
            if let Ok(pdf) = crate::book::pdf::load(&path) {
                for p in 0..pages {
                    v.push(crate::book::pdf::page_text(&pdf, p));
                }
            } else {
                v.resize(pages, String::new());
            }
            let _ = tx.send(super::Msg::PdfTexts(Arc::new(v)));
            ctx.request_repaint();
        });
    }

    pub(super) fn set_crop(&mut self, on: bool) {
        self.fx.crop = on;
        self.data.fixed_crop = on;
        self.dirty = true;
        if on {
            self.start_crop();
        }
    }

    pub(super) fn set_reflow(&mut self, on: bool) {
        self.fx.reflow = on;
        self.fx.reflow_page = usize::MAX;
        self.fx.scroll = 0.0;
        if on {
            self.start_texts();
        }
    }

    fn reflow_view(&mut self, ui: &mut Ui, rect: Rect, s: &ReaderSettings, pal: &Palette) {
        let n = self.book.fixed().map(|f| f.pages).unwrap_or(0);
        self.fx.spread = false;
        let Some(texts) = self.fx.texts.clone() else {
            self.start_texts();
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tr("Extracting text…"), egui::FontId::proportional(15.0), pal.muted);
            ui.ctx().request_repaint_after(Duration::from_millis(200));
            return;
        };
        if self.fx.paras.is_none() {
            self.fx.paras = Some(Arc::new(build_paragraphs(&texts)));
        }
        let paras = self.fx.paras.clone().unwrap();
        if self.fx.heights.len() != n {
            self.fx.heights = vec![0.0; n];
        }
        let font = egui::FontId::new(s.font_size, egui::FontFamily::Name("book".into()));
        let col_w = (rect.width() - 48.0).min(s.max_width).max(200.0);
        let gap = s.para_spacing.max(0.3) * s.font_size;
        let line_h = s.font_size * s.line_height;
        let estimate = |i: usize| -> f32 {
            let chars: usize = paras.get(i).map(|p| p.iter().map(|(_, t)| t.chars().count()).sum()).unwrap_or(0);
            let lines = (chars as f32 * s.font_size * 0.5 / col_w).ceil().max(1.0);
            lines * line_h + gap * paras.get(i).map(|p| p.len()).unwrap_or(1) as f32 + 56.0
        };
        let jump = self.fx.page != self.fx.reflow_page;
        let mut area = egui::ScrollArea::vertical().id_salt("reflow").auto_shrink(false);
        if jump {
            let off: f32 = (0..self.fx.page).map(|i| if self.fx.heights[i] > 0.0 { self.fx.heights[i] } else { estimate(i) }).sum();
            area = area.vertical_scroll_offset(off);
        } else if self.fx.reflow_adjust != 0.0 {
            // keep the text under the eye still when pages above were measured
            let cur = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::new("reflow-off"))).unwrap_or(0.0);
            area = area.vertical_scroll_offset((cur + self.fx.reflow_adjust).max(0.0));
        }
        self.fx.reflow_adjust = 0.0;
        let mut cur_page = self.fx.page;
        let mut adjust = 0.0f32;
        let mut heights = std::mem::take(&mut self.fx.heights);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let out = area.show_viewport(&mut child, |ui, vp| {
            let origin = ui.cursor().top();
            let x0 = ui.max_rect().center().x - col_w / 2.0;
            let mut found = false;
            for i in 0..n {
                let y = ui.cursor().top() - origin;
                let est = if heights[i] > 0.0 { heights[i] } else { estimate(i) };
                if y + est < vp.min.y || y > vp.max.y {
                    ui.allocate_exact_size(vec2(1.0, est), Sense::hover());
                    if y + est >= vp.min.y {
                        found = true;
                    }
                    continue;
                }
                if !found && y + est > vp.min.y + 30.0 {
                    cur_page = i;
                    found = true;
                }
                let top = ui.cursor().top();
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x0, top), vec2(col_w, 100_000.0))));
                c.label(egui::RichText::new(format!("{}", i + 1)).size(11.0).color(pal.muted));
                for (k, (cont, text)) in paras[i].iter().enumerate() {
                    if k > 0 || !*cont {
                        c.add_space(gap);
                    }
                    c.add(egui::Label::new(egui::RichText::new(text).font(font.clone()).color(pal.fg)));
                }
                let h = c.min_rect().height() + gap;
                if (h - heights[i]).abs() > 0.5 {
                    if y < vp.min.y && heights[i] > 0.0 {
                        adjust += h - heights[i];
                    }
                    heights[i] = h;
                }
                ui.allocate_exact_size(vec2(1.0, h), Sense::hover());
            }
        });
        self.fx.heights = heights;
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("reflow-off"), out.state.offset.y));
        self.fx.reflow_adjust = adjust;
        if jump {
            self.fx.reflow_page = self.fx.page;
            ui.ctx().request_repaint();
        } else if cur_page != self.fx.page {
            self.fx.page = cur_page;
            self.fx.reflow_page = cur_page;
            self.dirty = true;
        }
        let small = egui::FontId::proportional(12.5);
        ui.painter().text(pos2(rect.center().x, rect.bottom() - 14.0), egui::Align2::CENTER_CENTER, format!("{} / {}", self.fx.page + 1, n), small, pal.muted);
    }

    fn spread_pages(&self) -> Vec<usize> {
        let n = self.book.fixed().map(|f| f.pages).unwrap_or(1);
        let p = self.fx.page.min(n.saturating_sub(1));
        if self.fx.spread && p > 0 {
            // pairs (1,2), (3,4)... with the cover alone
            let a = if p % 2 == 1 { p } else { p - 1 };
            let mut v = vec![a];
            if a + 1 < n {
                v.push(a + 1);
            }
            if self.fx.rtl {
                v.reverse();
            }
            v
        } else {
            vec![p]
        }
    }

    pub(super) fn fixed_next(&mut self) {
        self.clear_selection();
        let n = self.book.fixed().map(|f| f.pages).unwrap_or(1);
        let p = self.fx.page;
        let next = if self.fx.spread && p > 0 { if p % 2 == 1 { p + 2 } else { p + 1 } } else { p + 1 };
        if next < n {
            self.fx.page = next;
            self.fx.scroll = 0.0;
        } else {
            self.data.finished = true;
        }
        self.last_turn = Instant::now();
        self.dirty = true;
    }

    pub(super) fn fixed_prev(&mut self) {
        self.clear_selection();
        let p = self.fx.page;
        if p == 0 {
            return;
        }
        self.fx.page = if self.fx.spread {
            let start = if p % 2 == 1 { p } else { p - 1 };
            if start <= 1 { 0 } else { start - 2 }
        } else {
            p - 1
        };
        self.fx.scroll = 0.0;
        self.last_turn = Instant::now();
        self.dirty = true;
    }

    /// Load character boxes for these PDF pages in the background.
    fn want_glyphs(&mut self, pages: &[usize]) {
        let need: Vec<usize> = pages.iter().copied().filter(|p| !self.fx.glyphs.contains_key(p) && !self.fx.glyphs_pending.contains(p)).collect();
        if need.is_empty() {
            return;
        }
        let Some(f) = self.book.fixed() else { return };
        self.fx.glyphs_pending.extend(need.iter().copied());
        // bound memory: keep pages near the reader
        let cur = self.fx.page;
        if self.fx.glyphs.len() > 16 {
            self.fx.glyphs.retain(|p, _| p.abs_diff(cur) <= 6);
        }
        let (tx, ctx, path) = (self.tx.clone(), self.ctx.clone(), f.path.clone());
        std::thread::spawn(move || {
            let v = match crate::book::pdf::load(&path) {
                Ok(pdf) => need.into_iter().map(|p| (p, Arc::new(crate::book::pdf::page_glyphs(&pdf, p)))).collect(),
                Err(_) => need.into_iter().map(|p| (p, Arc::new(PageText::default()))).collect(),
            };
            let _ = tx.send(super::Msg::PdfGlyphs(v));
            ctx.request_repaint();
        });
    }

    /// Highlight under a point of page `p` (page fractions).
    fn highlight_at(&self, p: usize, f: egui::Pos2) -> Option<u64> {
        let g = self.fx.glyphs.get(&p)?;
        self.data.annotations.iter().rev().find(|a| a.start.section == p && a.end > a.start && range_rects(g, a.start.offset, a.end.offset).iter().any(|b| f.x >= b[0] && f.x <= b[2] && f.y >= b[1] && f.y <= b[3])).map(|a| a.id)
    }

    /// Highlights, the search hit and the selection over a PDF page.
    fn paint_marks(&self, painter: &egui::Painter, p: usize, r: Rect, cb: [f32; 4]) {
        let Some(g) = self.fx.glyphs.get(&p) else { return };
        let dark = false; // PDF pages are always drawn on white
        let fill = |c0: usize, c1: usize, color: Color32, underline: bool| {
            for b in range_rects(g, c0, c1) {
                let sr = to_screen(r, cb, b);
                if underline {
                    painter.hline(sr.x_range(), sr.bottom(), egui::Stroke::new(2.0, color.to_opaque()));
                } else {
                    painter.rect_filled(sr, CornerRadius::same(2), color);
                }
            }
        };
        for a in &self.data.annotations {
            if a.start.section == p && a.end > a.start && a.end.section == p {
                fill(a.start.offset, a.end.offset, super::deco_color(a.color, dark), a.style == HlStyle::Underline);
            }
        }
        if let Some(h) = self.search_cur.and_then(|i| self.search_hits.get(i)) {
            if h.loc.section == p {
                fill(h.loc.offset, h.loc.offset + h.len, Color32::from_rgba_unmultiplied(255, 150, 0, 110), false);
            }
        }
        if let Some(((sp, c0), (_, c1))) = self.sel {
            if sp == p {
                fill(c0, c1, crate::ui::ACCENT.linear_multiply(0.3), false);
            }
        }
    }

    fn src_for(&self, page: usize, width: u32) -> (String, Option<Src>, u32) {
        let Some(f) = self.book.fixed() else { return (String::new(), None, 0) };
        match &f.kind {
            FixedKind::Pdf => (format!("pdf{page}w{width}"), Some(Src::Pdf { path: f.path.clone(), page, width }), width),
            FixedKind::Cbz { entries } => (
                format!("cbz{page}"),
                entries.get(page).map(|e| Src::Res(ResRef::Zip { path: f.path.clone(), entry: e.clone() })),
                3200,
            ),
        }
    }

    pub(super) fn fixed_view(&mut self, ui: &mut Ui, rect: Rect, s: &ReaderSettings, pal: &Palette) {
        let _ = s;
        let Some(f) = self.book.fixed() else { return };
        let n = f.pages;
        let is_pdf = matches!(f.kind, FixedKind::Pdf);
        if self.fx.crop && !self.fx.crop_started {
            self.start_crop();
        }
        if self.fx.reflow && is_pdf {
            self.reflow_view(ui, rect, s, pal);
            return;
        }
        if self.fx.scale == 0.0 {
            self.fx.scale = 1.0;
        }
        let area = Rect::from_min_max(rect.min + vec2(12.0, 30.0), rect.max - vec2(12.0, 30.0));
        self.view_h = area.height();
        self.fx.spread = self.fx.zoom == 0 && area.width() > area.height() * 1.15 && n > 2;
        let pages = self.spread_pages();
        let base_aspect = |p: usize, me: &Reader| -> f32 {
            let (k, _, _) = me.src_for(p, 0);
            if !is_pdf {
                if let Some(l) = me.tex.get(&k) {
                    return l.size[1] as f32 / l.size[0].max(1) as f32;
                }
            }
            me.fx.aspects.get(p).copied().unwrap_or(1.414)
        };
        let aspect = |p: usize, me: &Reader| -> f32 {
            let c = me.fx.crop_of(p);
            base_aspect(p, me) * (c[3] - c[1]) / (c[2] - c[0])
        };
        let gap = 6.0;
        let count = pages.len() as f32;
        // compute display size of each page
        let mut sizes = Vec::new();
        for &p in &pages {
            let a = aspect(p, self);
            let (w, h) = if self.fx.zoom == 0 {
                let mut h = area.height() * self.fx.scale;
                let mut w = h / a;
                let maxw = (area.width() - gap * (count - 1.0)) / count * self.fx.scale;
                if w > maxw {
                    w = maxw;
                    h = w * a;
                }
                (w, h)
            } else {
                let w = area.width().min(1400.0) * self.fx.scale;
                (w, w * a)
            };
            sizes.push(vec2(w, h));
        }
        let total_w: f32 = sizes.iter().map(|s| s.x).sum::<f32>() + gap * (count - 1.0);
        let max_h = sizes.iter().map(|s| s.y).fold(0.0, f32::max);
        let max_scroll = (max_h - area.height()).max(0.0);
        self.fx.scroll = self.fx.scroll.clamp(0.0, max_scroll);
        let max_pan = ((total_w - area.width()) / 2.0).max(0.0);
        self.fx.pan_x = self.fx.pan_x.clamp(-max_pan, max_pan);

        // where each shown page sits on screen (also used for paint below)
        let place = |me: &Reader| -> Vec<(usize, Rect)> {
            let mut x = area.center().x - total_w / 2.0 + me.fx.pan_x;
            pages
                .iter()
                .enumerate()
                .map(|(i, &p)| {
                    let sz = sizes.get(i).copied().unwrap_or(vec2(100.0, 140.0));
                    let y = if sz.y <= area.height() { area.center().y - sz.y / 2.0 } else { area.top() - me.fx.scroll };
                    let r = Rect::from_min_size(pos2(x, y), sz);
                    x += sz.x + gap;
                    (p, r)
                })
                .collect()
        };
        if is_pdf {
            self.want_glyphs(&pages);
        }

        // input
        let resp = ui.interact(rect, ui.id().with("fixed"), Sense::click_and_drag());
        let placed = place(self);
        // (page, page rect) under a screen point, and the point in page fractions
        let at = |me: &Reader, pos: egui::Pos2| -> Option<(usize, egui::Pos2)> {
            let (p, r) = placed.iter().find(|(_, r)| r.contains(pos)).copied()?;
            Some((p, to_page(r, me.fx.crop_of(p), pos)))
        };
        let text_at = |me: &Reader, pos: egui::Pos2| -> Option<(usize, usize)> {
            let (p, f) = at(me, pos)?;
            Some((p, glyph_under(me.fx.glyphs.get(&p)?, f)?))
        };
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
        if let Some(h) = hover {
            if self.drag_anchor.is_some() || text_at(self, h).is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
            }
        }
        if resp.drag_started() {
            // a drag that starts on text selects it; anywhere else it pans
            let origin = ui.input(|i| i.pointer.press_origin());
            if let Some((p, c)) = origin.and_then(|o| text_at(self, o)) {
                self.clear_selection();
                self.drag_anchor = Some((p, c));
            }
        }
        if resp.dragged() {
            if let Some((ap, ac)) = self.drag_anchor {
                if let (Some(pos), Some(r)) = (resp.interact_pointer_pos(), placed.iter().find(|(p, _)| *p == ap).map(|(_, r)| *r)) {
                    if let Some(g) = self.fx.glyphs.get(&ap) {
                        // the anchor's own character counts whichever way the drag goes
                        let c = caret_at(g, to_page(r, self.fx.crop_of(ap), pos));
                        let (s0, s1) = if c <= ac { (c, ac + 1) } else { (ac, c) };
                        self.sel = (s1 > s0).then_some(((ap, s0), (ap, s1)));
                    }
                }
            } else {
                let d = resp.drag_delta();
                self.fx.scroll -= d.y;
                self.fx.pan_x += d.x;
            }
        }
        if resp.drag_stopped() && self.drag_anchor.take().is_some() && self.sel.is_some() {
            match self.hl_mode {
                Some(c) => self.add_annotation(c, false),
                None => self.sel_popup = resp.interact_pointer_pos(),
            }
        }
        if resp.double_clicked() {
            // the first click's pending page turn is cancelled
            self.pending_tap = None;
            let word = resp.interact_pointer_pos().and_then(|pos| {
                let (p, c) = text_at(self, pos)?;
                let (ws, we) = super::layout::word_at(&self.fx.glyphs.get(&p)?.text(), c);
                (we > ws).then_some((p, ws, we, pos))
            });
            match word {
                Some((p, ws, we, pos)) => {
                    self.sel = Some(((p, ws), (p, we)));
                    self.sel_popup = Some(pos);
                }
                None => {
                    self.fx.zoom = 1 - self.fx.zoom.min(1);
                    self.fx.scale = 1.0;
                }
            }
        } else if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let on_hl = at(self, pos).and_then(|(p, f)| self.highlight_at(p, f));
                if self.sel.is_some() || self.hl_popup.is_some() {
                    // a click away from a selection only dismisses it
                    self.clear_selection();
                } else if let Some(id) = on_hl {
                    self.hl_popup = Some((id, pos));
                } else {
                    // on text the click may start a double-click (select word), so it waits
                    let when = if text_at(self, pos).is_some() { Instant::now() } else { Instant::now().checked_sub(Duration::from_secs(5)).unwrap_or_else(Instant::now) };
                    self.pending_tap = Some(((pos.x - rect.left()) / rect.width(), when));
                }
            }
        }
        if resp.secondary_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if self.sel.is_none() {
                    if let Some((p, c)) = text_at(self, pos) {
                        if let Some(g) = self.fx.glyphs.get(&p) {
                            let (ws, we) = super::layout::word_at(&g.text(), c);
                            if we > ws {
                                self.sel = Some(((p, ws), (p, we)));
                            }
                        }
                    }
                }
                if self.sel.is_some() {
                    self.sel_popup = Some(pos);
                }
            }
        }
        if let Some(fx) = self.due_tap(ui.ctx()) {
            {
                let (left, right) = if self.fx.rtl { (true, false) } else { (false, true) };
                if fx < 0.28 {
                    if left { self.fixed_next() } else { self.fixed_prev() }
                } else if fx > 0.72 {
                    if right { self.fixed_next() } else { self.fixed_prev() }
                } else {
                    self.chrome = !self.chrome;
                }
            }
        }
        if resp.hovered() {
            let (raw, smooth, cmd) = ui.input(|i| (i.raw_scroll_delta, i.smooth_scroll_delta, i.modifiers.command));
            let zoom_delta = ui.input(|i| i.zoom_delta());
            let (lines, points) = Self::wheel_input(ui.ctx());
            if cmd && raw.y != 0.0 {
                self.fx.scale = (self.fx.scale * if raw.y > 0.0 { 1.1 } else { 1.0 / 1.1 }).clamp(0.3, 5.0);
            } else if zoom_delta != 1.0 {
                self.fx.scale = (self.fx.scale * zoom_delta).clamp(0.3, 5.0);
            } else if max_scroll > 1.0 {
                // scroll within the page; past its edge, a fresh gesture turns it
                let before = self.fx.scroll;
                let at_end = before >= max_scroll - 0.5 && lines + points < 0.0;
                let at_start = before <= 0.5 && lines + points > 0.0;
                if at_end || at_start {
                    match self.wheel_turn(lines, points) {
                        1 if at_end => self.fixed_next(),
                        -1 if at_start && self.fx.page > 0 => {
                            self.fixed_prev();
                            self.fx.scroll = f32::MAX;
                        }
                        _ => {}
                    }
                } else {
                    self.fx.scroll = (self.fx.scroll - smooth.y).clamp(0.0, max_scroll);
                    self.wheel_spent(lines, points);
                }
            } else {
                match self.wheel_turn(lines, points) {
                    1 => self.fixed_next(),
                    -1 => self.fixed_prev(),
                    _ => {}
                }
            }
        }
        let pages = self.spread_pages();

        // render requests + paint
        let ppp = ui.ctx().pixels_per_point();
        let painter = ui.painter_at(rect);
        let mut x = area.center().x - total_w / 2.0 + self.fx.pan_x;
        let mut wanted: Vec<String> = Vec::new();
        for (i, &p) in pages.iter().enumerate() {
            let sz = sizes.get(i).copied().unwrap_or(vec2(100.0, 140.0));
            let y = if sz.y <= area.height() { area.center().y - sz.y / 2.0 } else { area.top() - self.fx.scroll };
            let r = Rect::from_min_size(pos2(x, y), sz);
            x += sz.x + gap;
            let cb = self.fx.crop_of(p);
            let bucket = ((((sz.x / (cb[2] - cb[0])) * ppp) / 256.0).ceil() as u32 * 256).clamp(256, 4096);
            let (key, src, max_dim) = self.src_for(p, bucket);
            wanted.push(key.clone());
            self.tex.request(&key, || src, max_dim);
            painter.rect_filled(r, CornerRadius::same(2), if pal.dark { Color32::from_gray(235) } else { Color32::WHITE });
            let shown = if self.tex.get(&key).is_some() {
                self.fx.best.insert(p, (bucket, key.clone()));
                Some(key.clone())
            } else {
                self.fx.best.get(&p).map(|(_, k)| k.clone())
            };
            match shown.and_then(|k| self.tex.get(&k)) {
                Some(l) => {
                    painter.image(l.tex.id(), r, Rect::from_min_max(pos2(cb[0], cb[1]), pos2(cb[2], cb[3])), Color32::WHITE);
                }
                None => {
                    painter.text(r.center(), egui::Align2::CENTER_CENTER, tr("Rendering…"), egui::FontId::proportional(14.0), Color32::GRAY);
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                }
            }
            painter.rect_stroke(r, CornerRadius::same(2), egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Outside);
            self.paint_marks(&painter, p, r, cb);
        }
        // prefetch neighbours at the same resolution
        let cw0 = pages.first().map(|&p| { let c = self.fx.crop_of(p); c[2] - c[0] }).unwrap_or(1.0);
        let bucket = sizes.first().map(|s| (((s.x / cw0 * ppp) / 256.0).ceil() as u32 * 256).clamp(256, 4096)).unwrap_or(1024);
        let lo = pages.iter().min().copied().unwrap_or(0);
        let hi = pages.iter().max().copied().unwrap_or(0);
        for p in [hi + 1, hi + 2, lo.wrapping_sub(1)] {
            if p < n {
                let (key, src, max_dim) = self.src_for(p, bucket);
                wanted.push(key.clone());
                self.tex.request(&key, || src, max_dim);
            }
        }
        // bound memory: keep only nearby pages
        let keep_lo = lo.saturating_sub(3);
        let keep_hi = hi + 4;
        let best_keys: Vec<String> = self.fx.best.iter().filter(|(p, _)| **p >= keep_lo && **p <= keep_hi).map(|(_, (_, k))| k.clone()).collect();
        self.fx.best.retain(|p, _| *p >= keep_lo && *p <= keep_hi);
        self.tex.cancel_queued(|k| wanted.iter().any(|w| w == k));
        if self.tex.len() > 24 {
            self.tex.evict(|k| wanted.iter().any(|w| w == k) || best_keys.iter().any(|b| b == k));
        }

        // footer
        let small = egui::FontId::proportional(12.5);
        painter.text(pos2(rect.center().x, rect.bottom() - 14.0), egui::Align2::CENTER_CENTER, format!("{} / {}", self.fx.page + 1, n), small, pal.muted);
    }
}

/// Screen point → page fractions, for a page drawn in `r` showing crop box `cb`.
fn to_page(r: Rect, cb: [f32; 4], pos: egui::Pos2) -> egui::Pos2 {
    pos2(cb[0] + (pos.x - r.left()) / r.width().max(1.0) * (cb[2] - cb[0]), cb[1] + (pos.y - r.top()) / r.height().max(1.0) * (cb[3] - cb[1]))
}

/// Page-fraction box → screen rect.
fn to_screen(r: Rect, cb: [f32; 4], b: [f32; 4]) -> Rect {
    let x = |f: f32| r.left() + (f - cb[0]) / (cb[2] - cb[0]) * r.width();
    let y = |f: f32| r.top() + (f - cb[1]) / (cb[3] - cb[1]) * r.height();
    Rect::from_min_max(pos2(x(b[0]), y(b[1])), pos2(x(b[2]), y(b[3])))
}

/// The character whose box holds `f` (page fractions).
pub fn glyph_under(pt: &PageText, f: egui::Pos2) -> Option<usize> {
    pt.boxes.iter().position(|b| {
        b.is_some_and(|b| {
            let pad = (b[3] - b[1]) * 0.15;
            f.x >= b[0] && f.x <= b[2] && f.y >= b[1] - pad && f.y <= b[3] + pad
        })
    })
}

/// The caret (gap 0..=len between characters) nearest to `f`; the line wins over the column.
pub fn caret_at(pt: &PageText, f: egui::Pos2) -> usize {
    let mut best = (f32::MAX, 0usize);
    for (i, b) in pt.boxes.iter().enumerate() {
        let Some(b) = b else { continue };
        let dy = if f.y < b[1] { b[1] - f.y } else if f.y > b[3] { f.y - b[3] } else { 0.0 };
        let dx = if f.x < b[0] { b[0] - f.x } else if f.x > b[2] { f.x - b[2] } else { 0.0 };
        let d = dy * 8.0 + dx;
        if d < best.0 {
            best = (d, if f.x > (b[0] + b[2]) / 2.0 { i + 1 } else { i });
        }
    }
    best.1
}

/// Boxes covering characters `c0..c1`, merged into one per line.
pub fn range_rects(pt: &PageText, c0: usize, c1: usize) -> Vec<[f32; 4]> {
    let n = pt.boxes.len();
    let mut out: Vec<[f32; 4]> = Vec::new();
    for b in pt.boxes[c0.min(n)..c1.min(n)].iter().flatten() {
        if let Some(l) = out.last_mut() {
            let h = (l[3] - l[1]).max(b[3] - b[1]);
            let same_line = (b[1] - l[1]).abs() < h * 0.5 && b[0] >= l[0] && b[0] - l[2] < h * 3.0;
            if same_line {
                l[0] = l[0].min(b[0]);
                l[1] = l[1].min(b[1]);
                l[2] = l[2].max(b[2]);
                l[3] = l[3].max(b[3]);
                continue;
            }
        }
        out.push(*b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(w: usize, h: usize, inner: (usize, usize, usize, usize)) -> Vec<u8> {
        let mut d = vec![255u8; w * h * 4];
        for y in inner.1..inner.3 {
            for x in inner.0..inner.2 {
                let i = (y * w + x) * 4;
                d[i..i + 3].copy_from_slice(&[10, 10, 10]);
            }
        }
        d
    }

    #[test]
    fn margins_detected_on_synthetic_bitmap() {
        let d = page(200, 300, (40, 30, 160, 270));
        let b = detect_margins(200, 300, &d).unwrap();
        assert!((b[0] - 0.2).abs() < 0.01 && (b[2] - 0.8).abs() < 0.01, "{b:?}");
        assert!((b[1] - 0.1).abs() < 0.01 && (b[3] - 0.9).abs() < 0.01, "{b:?}");
        assert!(detect_margins(200, 300, &vec![255u8; 200 * 300 * 4]).is_none());
    }

    #[test]
    fn common_crop_is_shared_per_parity() {
        let boxes = vec![Some([0.2, 0.1, 0.8, 0.9]), Some([0.1, 0.1, 0.7, 0.9]), Some([0.22, 0.12, 0.78, 0.9]), Some([0.12, 0.1, 0.72, 0.9]), None];
        let c = common_crop(&boxes);
        assert!(c[0][0] < 0.2 && c[0][2] > 0.8 - 1e-3);
        assert!(c[1][0] < 0.12);
        assert_ne!(c[0], FULL);
        assert_eq!(common_crop(&[None, None]), [FULL; 2]);
    }

    #[test]
    fn pdf_selection_geometry() {
        // "ab cd" on one line, "ef" on the next
        let bx = |l: f32, t: f32| Some([l, t, l + 0.05, t + 0.03]);
        let pt = PageText { chars: "ab cd\nef".chars().collect(), boxes: vec![bx(0.1, 0.1), bx(0.15, 0.1), None, bx(0.25, 0.1), bx(0.3, 0.1), None, bx(0.1, 0.2), bx(0.15, 0.2)] };
        assert_eq!(glyph_under(&pt, pos2(0.16, 0.11)), Some(1));
        assert_eq!(glyph_under(&pt, pos2(0.5, 0.5)), None);
        assert_eq!(caret_at(&pt, pos2(0.9, 0.11)), 5, "past the line end");
        assert_eq!(caret_at(&pt, pos2(0.11, 0.21)), 6);
        let r = range_rects(&pt, 0, 8);
        assert_eq!(r.len(), 2, "one box per line: {r:?}");
        assert!((r[0][2] - 0.35).abs() < 1e-5);
    }

    #[test]
    fn paragraphs_join_dehyphenate_and_split() {
        let page1 = "The quick brown fox jumps over the lazy dog and keeps on run-\nning through the long field of grass.\nA new paragraph starts here and goes on\n\n12".to_string();
        let page2 = "and continues here on the next page.".to_string();
        let r = build_paragraphs(&[page1, page2]);
        assert_eq!(r[0][0].1, "The quick brown fox jumps over the lazy dog and keeps on running through the long field of grass.");
        assert!(r[0].len() == 2);
        assert!(r[1][0].0, "page 2 continues page 1");
    }
}
