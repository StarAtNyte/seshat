//! Fixed-layout view for PDF and CBZ: fit page / fit width, two-page spreads,
//! right-to-left (manga) order, zoom & pan, progressive-resolution rendering
//! with prefetch and bounded texture memory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Rect, Sense, Ui};

use super::Reader;
use crate::book::{FixedKind, ResRef};
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
}

impl Reader {
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
        if self.fx.scale == 0.0 {
            self.fx.scale = 1.0;
        }
        let area = Rect::from_min_max(rect.min + vec2(12.0, 30.0), rect.max - vec2(12.0, 30.0));
        self.view_h = area.height();
        self.fx.spread = self.fx.zoom == 0 && area.width() > area.height() * 1.15 && n > 2;
        let pages = self.spread_pages();
        let aspect = |p: usize, me: &Reader| -> f32 {
            let (k, _, _) = me.src_for(p, 0);
            if !is_pdf {
                if let Some(l) = me.tex.get(&k) {
                    return l.size[1] as f32 / l.size[0].max(1) as f32;
                }
            }
            me.fx.aspects.get(p).copied().unwrap_or(1.414)
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

        // input
        let resp = ui.interact(rect, ui.id().with("fixed"), Sense::click_and_drag());
        if resp.dragged() {
            let d = resp.drag_delta();
            self.fx.scroll -= d.y;
            self.fx.pan_x += d.x;
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let fx = (p.x - rect.left()) / rect.width();
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
        if resp.double_clicked() {
            self.fx.zoom = 1 - self.fx.zoom.min(1);
            self.fx.scale = 1.0;
        }
        if resp.hovered() {
            let (raw, smooth, cmd) = ui.input(|i| (i.raw_scroll_delta, i.smooth_scroll_delta, i.modifiers.command));
            let zoom_delta = ui.input(|i| i.zoom_delta());
            if cmd && raw.y != 0.0 {
                self.fx.scale = (self.fx.scale * if raw.y > 0.0 { 1.1 } else { 1.0 / 1.1 }).clamp(0.3, 5.0);
            } else if zoom_delta != 1.0 {
                self.fx.scale = (self.fx.scale * zoom_delta).clamp(0.3, 5.0);
            } else if max_scroll > 1.0 {
                let before = self.fx.scroll;
                self.fx.scroll = (self.fx.scroll - smooth.y).clamp(0.0, max_scroll);
                if raw.y < -30.0 && before >= max_scroll - 0.5 && self.last_turn.elapsed() > Duration::from_millis(350) {
                    self.fixed_next();
                } else if raw.y > 30.0 && before <= 0.5 && self.last_turn.elapsed() > Duration::from_millis(350) && self.fx.page > 0 {
                    self.fixed_prev();
                    self.fx.scroll = f32::MAX;
                }
            } else {
                self.wheel_acc += raw.y + raw.x;
                if self.last_turn.elapsed() > Duration::from_millis(180) {
                    if self.wheel_acc <= -40.0 {
                        self.wheel_acc = 0.0;
                        self.fixed_next();
                    } else if self.wheel_acc >= 40.0 {
                        self.wheel_acc = 0.0;
                        self.fixed_prev();
                    }
                } else {
                    self.wheel_acc = 0.0;
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
            let bucket = (((sz.x * ppp) / 256.0).ceil() as u32 * 256).clamp(256, 4096);
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
                    painter.image(l.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    painter.text(r.center(), egui::Align2::CENTER_CENTER, "Rendering…", egui::FontId::proportional(14.0), Color32::GRAY);
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                }
            }
            painter.rect_stroke(r, CornerRadius::same(2), egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Outside);
        }
        // prefetch neighbours at the same resolution
        let bucket = sizes.first().map(|s| (((s.x * ppp) / 256.0).ceil() as u32 * 256).clamp(256, 4096)).unwrap_or(1024);
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
