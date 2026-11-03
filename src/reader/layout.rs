//! Typesetting engine: blocks → egui galleys → line units → pages.
//!
//! One section is laid out as a continuous column. Every text row, image and
//! rule becomes an atomic "unit"; pages are greedy runs of units that fit the
//! page height, so page breaks always fall between lines (never through one).
//! Scrolled mode uses the same column with a free viewport.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{self, pos2, vec2, Align as EAlign, Color32, CornerRadius, FontFamily, FontId, Galley, Painter, Pos2, Rect, Stroke, TextureId, Vec2};
use egui::text::{LayoutJob, TextFormat, TextWrapping};

// declared here (not in reader/mod.rs) to keep this engine self-contained
#[path = "typeset.rs"]
pub mod typeset;
#[path = "vertical.rs"]
pub mod vertical;

use self::typeset::Disp;
use crate::book::{script, Align, Block, BlockKind, Section, Span, Style, Table};

#[derive(Clone, Debug)]
pub struct Typo {
    pub size: f32,
    pub line_height: f32,
    pub para: f32,
    pub letter: f32,
    pub justify: bool,
    pub indent: bool,
    pub override_book: bool,
    pub fg: Color32,
    pub link: Color32,
    pub width: f32,
    pub page_h: f32,
    pub fake_italic: bool,
    pub font_tag: String,
    /// Book language (hyphenation patterns).
    pub lang: String,
    /// Vertical CJK (columns right to left).
    pub vertical: bool,
}

impl Typo {
    pub fn key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for f in [self.size, self.line_height, self.para, self.letter, self.width, self.page_h] {
            f.to_bits().hash(&mut h);
        }
        (self.justify, self.indent, self.override_book, self.fake_italic).hash(&mut h);
        self.fg.hash(&mut h);
        self.link.hash(&mut h);
        self.font_tag.hash(&mut h);
        self.lang.hash(&mut h);
        self.vertical.hash(&mut h);
        h.finish()
    }
}

pub enum Item {
    /// `disp` maps display chars to logical offsets when the galley text was
    /// transformed (shaping, bidi, hyphens); `rows` = logical range per row.
    Text { galley: Arc<Galley>, ax: f32, disp: Option<Arc<Disp>>, rows: Vec<(usize, usize)> },
    Table(Box<LTable>),
    /// Vertical text: columns, and the block's indent from the column top.
    Vertical(Vec<vertical::VCol>, f32),
    Image { key: String, size: Vec2, x: f32 },
    Rule,
    Empty,
}

pub struct LBlock {
    pub y: f32,
    pub h: f32,
    pub item: Item,
}

#[derive(Clone, Copy, Debug)]
pub struct Unit {
    pub top: f32,
    pub bottom: f32,
    pub block: usize,
    pub c0: usize,
    pub c1: usize,
    pub keep_next: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Page {
    pub u0: usize,
    pub u1: usize,
    pub top: f32,
    pub bottom: f32,
}

pub struct SectionLayout {
    pub section: usize,
    pub key: u64,
    pub blocks: Vec<LBlock>,
    pub units: Vec<Unit>,
    pub pages: Vec<Page>,
    pub height: f32,
    pub width: f32,
    /// Vertical layout: unit/page `top..bottom` run along the horizontal axis
    /// (from the right edge) and pages are `width` long; see `vertical`.
    pub vertical: bool,
    /// Column height of a vertical layout.
    pub col_h: f32,
}

#[derive(Clone, Copy)]
pub enum DecoKind {
    Fill(Color32),
    Underline(Color32),
}

#[derive(Clone, Copy)]
pub struct Deco {
    pub block: usize,
    pub c0: usize,
    pub c1: usize,
    pub kind: DecoKind,
}

fn scale_for(kind: &BlockKind) -> f32 {
    match kind {
        BlockKind::Heading(1) => 1.6,
        BlockKind::Heading(2) => 1.4,
        BlockKind::Heading(3) => 1.22,
        BlockKind::Heading(4) => 1.1,
        BlockKind::Heading(_) => 1.0,
        BlockKind::Pre => 0.85,
        _ => 1.0,
    }
}

fn family(st: Style) -> FontFamily {
    let name = if st.has(Style::MONO) {
        "mono"
    } else {
        match (st.has(Style::BOLD), st.has(Style::ITALIC)) {
            (true, true) => "book-bi",
            (true, false) => "book-b",
            (false, true) => "book-i",
            _ => "book",
        }
    };
    FontFamily::Name(name.into())
}

/// Text format of a span in a block set at `base` px with leading `lh`.
fn fmt_for(sp: &Span, base: f32, lh: f32, t: &Typo) -> TextFormat {
    let rel = if t.override_book && !sp.style.has(Style::SUP) && !sp.style.has(Style::SUB) { 1.0 } else { sp.size };
    let size = (base * rel).max(6.0);
    let color = if sp.link.is_some() { t.link } else { t.fg };
    let mut f = TextFormat::simple(FontId::new(size, family(sp.style)), color);
    f.line_height = Some(lh.max(size * 1.2));
    f.extra_letter_spacing = t.letter;
    f.italics = sp.style.has(Style::ITALIC) && t.fake_italic && !sp.style.has(Style::MONO);
    if sp.style.has(Style::UNDERLINE) {
        f.underline = Stroke::new(1.0, color);
    }
    if sp.style.has(Style::STRIKE) {
        f.strikethrough = Stroke::new(1.0, color);
    }
    if sp.style.has(Style::SUP) {
        f.valign = EAlign::TOP;
    } else if sp.style.has(Style::SUB) {
        f.valign = EAlign::BOTTOM;
    }
    f
}

/// Whether a block's text needs right-to-left setting (and so its base direction).
fn block_rtl(b: &Block) -> bool {
    match b.rtl {
        Some(r) => r,
        None => {
            let text = b.text();
            script::has_rtl(&text) && typeset::base_rtl(&text, None)
        }
    }
}

fn job_for(block: &Block, prev: Option<&Block>, t: &Typo, avail: f32, rtl: bool) -> (LayoutJob, f32, bool) {
    let base = t.size * scale_for(&block.kind) * if block.note { 0.88 } else { 1.0 };
    // headings set solid on their own size; body uses the reader's leading
    let lh = base * match block.kind {
        BlockKind::Pre => 1.3,
        BlockKind::Heading(_) => 1.25,
        _ => t.line_height,
    };
    let mut indent_px = (block.indent * t.size).min(avail * 0.3);
    if let BlockKind::ListItem(d) = block.kind {
        indent_px += (d as f32 - 1.0) * 1.4 * t.size + 0.4 * t.size;
    }
    let book_align = if t.override_book && block.kind == BlockKind::Para { None } else { block.align };
    let align = match (&block.kind, book_align) {
        (BlockKind::Pre, _) => Align::Start,
        (_, Some(Align::Justify)) | (BlockKind::Para | BlockKind::Quote | BlockKind::ListItem(_), None | Some(Align::Start)) => {
            if t.justify && block.kind != BlockKind::Heading(1) {
                Align::Justify
            } else {
                Align::Start
            }
        }
        (_, Some(a)) => a,
        (_, None) => Align::Start,
    };
    let first_indent = t.indent
        && !rtl
        && block.kind == BlockKind::Para
        && matches!(align, Align::Justify | Align::Start)
        && prev.is_some_and(|p| p.kind == BlockKind::Para && !matches!(p.align, Some(Align::Center) | Some(Align::End)))
        && !block.spans.first().is_some_and(|s| s.text.starts_with('\n'));
    let mut job = LayoutJob::default();
    let mut first = true;
    for sp in &block.spans {
        let f = fmt_for(sp, base, lh, t);
        let lead = if first && first_indent { 1.5 * t.size } else { 0.0 };
        first = false;
        job.append(&sp.text, lead, f);
    }
    let w = (avail - indent_px).max(40.0);
    job.wrap = TextWrapping { max_width: w, ..Default::default() };
    // egui's built-in justify letter-spaces loose lines; we justify by word spacing instead.
    job.justify = false;
    // right-to-left: the indent sits on the right and start/end swap sides
    let x_left = if rtl { (avail - indent_px - w).max(0.0) } else { indent_px };
    let visual_right = matches!((align, rtl), (Align::End, false) | (Align::Start | Align::Justify, true));
    let ax = if align == Align::Center {
        job.halign = EAlign::Center;
        x_left + w / 2.0
    } else if visual_right {
        job.halign = EAlign::RIGHT;
        x_left + w
    } else {
        x_left
    };
    (job, ax, align == Align::Justify)
}

/// A laid-out table: cell galleys placed in block coordinates, plus rules.
pub struct LTable {
    pub cells: Vec<LCell>,
    pub lines: Vec<[Pos2; 2]>,
    pub fills: Vec<Rect>,
}

pub struct LCell {
    pub galley: Arc<Galley>,
    pub disp: Option<Arc<Disp>>,
    pub rows: Vec<(usize, usize)>,
    /// Galley origin (block-local).
    pub pos: Pos2,
    /// Cell box (block-local), padding included.
    pub rect: Rect,
    /// Char offset of the cell's text within the block.
    pub start: usize,
    pub row: usize,
}

impl LTable {
    /// Shapes for painting at `origin` (the block's top-left), galleys re-fetched
    /// through egui's layout cache so they survive font-atlas rebuilds.
    pub fn shapes(&self, fonts: &egui::text::Fonts, origin: Pos2, fg: Color32, clip: Rect) -> Vec<egui::Shape> {
        let mut out = Vec::new();
        let off = origin.to_vec2();
        for r in &self.fills {
            let r = r.translate(off);
            if r.intersects(clip) {
                out.push(egui::Shape::rect_filled(r, CornerRadius::ZERO, fg.linear_multiply(0.06)));
            }
        }
        let stroke = Stroke::new(1.0, fg.linear_multiply(0.35));
        for l in &self.lines {
            out.push(egui::Shape::line_segment([l[0] + off, l[1] + off], stroke));
        }
        for c in &self.cells {
            if c.rect.translate(off).intersects(clip) {
                out.push(egui::Shape::galley(c.pos + off, fonts.layout_job((*c.galley.job).clone()), fg));
            }
        }
        out
    }
}

/// Lay out a table block of width `avail`; returns the table, its height and
/// the pagination bands (top, bottom, c0, c1) in block coordinates.
fn build_table(fonts: &egui::text::Fonts, block: &Block, tb: &Table, t: &Typo, avail: f32) -> (LTable, f32, Vec<(f32, f32, usize, usize)>) {
    let base = t.size * 0.9 * if block.note { 0.88 } else { 1.0 };
    let lh = base * t.line_height.min(1.45);
    let pad_x = (0.45 * base).round();
    let pad_y = (0.25 * base).round();
    let bw = 1.0;
    let indent = (block.indent * t.size).min(avail * 0.3);
    let avail = (avail - indent).max(80.0);
    let rtl = block_rtl(block);
    let ncols = tb.rows.iter().map(|r| r.iter().map(|c| c.colspan as usize).sum::<usize>()).max().unwrap_or(1).max(1);
    let job_of = |c: &crate::book::Cell, width: f32| {
        let mut job = LayoutJob::default();
        for sp in block.slice_spans(c.start, c.end) {
            let mut sp = sp;
            if c.header {
                sp.style = sp.style.with(Style::BOLD);
            }
            job.append(&sp.text, 0.0, fmt_for(&sp, base, lh, t));
        }
        job.wrap = TextWrapping { max_width: width, ..Default::default() };
        job
    };
    // column widths from content: (min = widest word, max = unwrapped)
    let mut cmin = vec![0.0f32; ncols];
    let mut cmax = vec![0.0f32; ncols];
    let mut spans_multi = Vec::new();
    let mut measured: Vec<Vec<(f32, f32)>> = Vec::with_capacity(tb.rows.len());
    for row in &tb.rows {
        let mut col = 0;
        let mut mrow = Vec::with_capacity(row.len());
        for c in row {
            let g = fonts.layout_job(job_of(c, f32::INFINITY));
            let mx = g.size().x.ceil();
            let mut mn = 0.0f32;
            for r in &g.rows {
                let mut ws = None;
                for gl in &r.glyphs {
                    if gl.chr.is_whitespace() {
                        if let Some(x0) = ws.take() {
                            mn = mn.max(gl.pos.x - x0);
                        }
                    } else if ws.is_none() {
                        ws = Some(gl.pos.x);
                    }
                }
                if let (Some(x0), Some(last)) = (ws, r.glyphs.last()) {
                    mn = mn.max(last.max_x() - x0);
                }
            }
            let mn = (mn.ceil() + 1.0).min(avail * 0.5);
            let span = (c.colspan as usize).min(ncols - col).max(1);
            if span == 1 {
                cmin[col] = cmin[col].max(mn);
                cmax[col] = cmax[col].max(mx);
            } else {
                spans_multi.push((col, span, mn, mx));
            }
            mrow.push((mn, mx));
            col += span;
            if col >= ncols {
                break;
            }
        }
        measured.push(mrow);
    }
    for (col, span, mn, mx) in spans_multi {
        let extra = (span - 1) as f32 * (2.0 * pad_x + bw);
        let have_min: f32 = cmin[col..col + span].iter().sum::<f32>() + extra;
        if mn > have_min {
            let d = (mn - have_min) / span as f32;
            cmin[col..col + span].iter_mut().for_each(|w| *w += d);
        }
        let have_max: f32 = cmax[col..col + span].iter().sum::<f32>() + extra;
        if mx > have_max {
            let d = (mx - have_max) / span as f32;
            cmax[col..col + span].iter_mut().for_each(|w| *w += d);
        }
    }
    let chrome = (ncols as f32 + 1.0) * bw + ncols as f32 * 2.0 * pad_x;
    let room = (avail - chrome).max(ncols as f32 * 12.0);
    if ncols > 1 {
        let cap = (room * 0.6).max(1.0);
        for i in 0..ncols {
            cmax[i] = cmax[i].min(cap).max(cmin[i]);
        }
    }
    for i in 0..ncols {
        cmax[i] = cmax[i].max(cmin[i]).max(8.0);
    }
    let (smin, smax): (f32, f32) = (cmin.iter().sum(), cmax.iter().sum());
    let widths: Vec<f32> = if smax <= room {
        cmax.clone()
    } else if smin >= room {
        cmin.iter().map(|w| (w * room / smin.max(1.0)).max(8.0)).collect()
    } else {
        let k = (room - smin) / (smax - smin).max(1.0);
        (0..ncols).map(|i| cmin[i] + (cmax[i] - cmin[i]) * k).collect()
    };
    let table_w = widths.iter().sum::<f32>() + chrome;
    let x0 = if rtl { indent + (avail - table_w).max(0.0) } else { indent };
    // column left edges (box incl. padding), visual order
    let mut col_x = Vec::with_capacity(ncols + 1);
    let mut x = bw;
    for wdt in &widths {
        col_x.push(x);
        x += wdt + 2.0 * pad_x + bw;
    }
    col_x.push(x);
    let mut cells = Vec::new();
    let mut lines = Vec::new();
    let mut fills = Vec::new();
    let mut bands = Vec::new();
    let mut y = bw;
    lines.push([pos2(x0, 0.5), pos2(x0 + table_w, 0.5)]);
    for (ri, row) in tb.rows.iter().enumerate() {
        let mut col = 0;
        let mut row_cells = Vec::new();
        let mut row_h = 0.0f32;
        for c in row {
            if col >= ncols {
                break;
            }
            let span = (c.colspan as usize).min(ncols - col).max(1);
            let inner = col_x[col + span] - col_x[col] - bw - 2.0 * pad_x;
            let mut job = job_of(c, inner.max(8.0));
            let crtl = {
                let text: String = job.text.clone();
                block.rtl.unwrap_or(false) || (script::has_rtl(&text) && typeset::base_rtl(&text, None))
            };
            let al = c.align.unwrap_or(if c.header { Align::Center } else { Align::Start });
            let right = matches!((al, crtl), (Align::End, false) | (Align::Start | Align::Justify, true));
            let gx = if al == Align::Center {
                job.halign = EAlign::Center;
                inner / 2.0
            } else if right {
                job.halign = EAlign::RIGHT;
                inner
            } else {
                0.0
            };
            let set = typeset::set(fonts, job, &typeset::Opts { width: inner.max(8.0), justify: false, hyph: None, rtl: block.rtl });
            row_h = row_h.max(set.galley.size().y);
            // visual column: mirrored in right-to-left tables
            let (l, r) = if rtl { (table_w - col_x[col + span], table_w - col_x[col] - bw + bw) } else { (col_x[col], col_x[col + span] - bw) };
            row_cells.push((set, gx, l, r, c.start, c.header));
            col += span;
        }
        let h = row_h + 2.0 * pad_y;
        let mut row_rows: Vec<(f32, f32)> = Vec::new();
        for (set, gx, l, r, start, header) in row_cells {
            let rect = Rect::from_min_max(pos2(x0 + l, y), pos2(x0 + r, y + h));
            if header {
                fills.push(rect);
            }
            // right edge of each cell, and the table's outer left edge
            let edge = if rtl { rect.left() - 0.5 } else { rect.right() + 0.5 };
            lines.push([pos2(edge, y - bw), pos2(edge, y + h)]);
            for gr in &set.galley.rows {
                row_rows.push((y + pad_y + gr.min_y(), y + pad_y + gr.max_y()));
            }
            cells.push(LCell { galley: set.galley, disp: set.disp, rows: set.rows, pos: pos2(rect.left() + pad_x + gx, y + pad_y), rect, start, row: ri });
        }
        let outer = if rtl { x0 + table_w - 0.5 } else { x0 + 0.5 };
        lines.push([pos2(outer, y - bw), pos2(outer, y + h)]);
        lines.push([pos2(x0, y + h + 0.5), pos2(x0 + table_w, y + h + 0.5)]);
        // pagination: one band per row; split very tall rows between text lines
        let first = row.first().map(|c| c.start).unwrap_or(0);
        let last = row.last().map(|c| c.end + 1).unwrap_or(first);
        if h <= t.page_h * 0.9 || row_rows.is_empty() {
            bands.push((y - bw, y + h + bw, first, last));
        } else {
            let mut cuts: Vec<f32> = row_rows.iter().map(|r| r.1).filter(|&b| !row_rows.iter().any(|r| r.0 < b - 0.5 && r.1 > b + 0.5)).collect();
            cuts.sort_by(|a, b| a.total_cmp(b));
            cuts.dedup_by(|a, b| (*a - *b).abs() < 0.5);
            let mut top = y - bw;
            let n = cuts.len();
            for (i, cut) in cuts.into_iter().enumerate() {
                let bottom = if i + 1 == n { y + h + bw } else { cut };
                if bottom > top + 0.5 {
                    bands.push((top, bottom, if i == 0 { first } else { first + 1 }, last));
                    top = bottom;
                }
            }
        }
        y += h + bw;
    }
    // c0 of later pieces of a split row only needs to be inside the row
    (LTable { cells, lines, fills }, y, bands)
}

fn spacing(kind: &BlockKind, t: &Typo) -> (f32, f32) {
    let s = t.size;
    match kind {
        BlockKind::Heading(l) => (if *l <= 2 { 1.6 * s } else { 1.1 * s }, 0.7 * s),
        BlockKind::Image(_) => (0.6 * s, 0.6 * s),
        BlockKind::Rule => (0.9 * s, 0.9 * s),
        BlockKind::Pre => (0.3 * s, 0.6 * s),
        _ => (0.0, t.para * s),
    }
}

impl SectionLayout {
    /// Lay out `section`. `img_size` reports natural pixel sizes of decoded images.
    pub fn build(ctx: &egui::Context, section_idx: usize, section: &Section, t: &Typo, img_size: &dyn Fn(&str) -> Option<[usize; 2]>, key: u64) -> SectionLayout {
        if t.vertical {
            return vertical::build(ctx, section_idx, section, t, img_size, key);
        }
        let mut blocks = Vec::with_capacity(section.blocks.len());
        let mut units = Vec::new();
        let mut y = 0.0f32;
        let mut prev_after = 0.0f32;
        let only_image = section.blocks.len() <= 2 && section.blocks.iter().any(|b| matches!(b.kind, BlockKind::Image(_)));
        let hyph = script::hyph_lang(&t.lang);
        ctx.fonts(|fonts| {
            for (bi, b) in section.blocks.iter().enumerate() {
                let (before, after) = spacing(&b.kind, t);
                if bi > 0 {
                    y += prev_after.max(before);
                }
                let prev = if bi > 0 { section.blocks.get(bi - 1) } else { None };
                let lb = match &b.kind {
                    BlockKind::Image(key) => {
                        let nat = img_size(key).map(|s| vec2(s[0] as f32, s[1] as f32)).unwrap_or(vec2(t.width, t.width * 0.6));
                        // SVGs are rasterised at a multiple of their size for crispness
                        let nat = if crate::book::svg::is_svg_key(key) { nat / crate::book::svg::SCALE } else { nat };
                        let max_h = t.page_h * if only_image { 1.0 } else { 0.92 };
                        let mut scale = (t.width / nat.x).min(max_h / nat.y);
                        if !only_image {
                            scale = scale.min(1.0f32.max(t.size / 18.0));
                        }
                        let size = nat * scale.max(0.01);
                        let x = (t.width - size.x) / 2.0;
                        units.push(Unit { top: y, bottom: y + size.y, block: bi, c0: 0, c1: 0, keep_next: false });
                        LBlock { y, h: size.y, item: Item::Image { key: key.clone(), size, x } }
                    }
                    BlockKind::Table(tb) => {
                        let (lt, h, bands) = build_table(fonts, b, tb, t, t.width);
                        for (top, bottom, c0, c1) in bands {
                            units.push(Unit { top: y + top, bottom: y + bottom, block: bi, c0, c1, keep_next: false });
                        }
                        LBlock { y, h, item: Item::Table(Box::new(lt)) }
                    }
                    BlockKind::Rule => {
                        let h = 1.0;
                        units.push(Unit { top: y, bottom: y + h, block: bi, c0: 0, c1: 0, keep_next: false });
                        LBlock { y, h, item: Item::Rule }
                    }
                    _ => {
                        let rtl = block_rtl(b);
                        let (job, ax, just) = job_for(b, prev, t, t.width, rtl);
                        if job.text.is_empty() {
                            LBlock { y, h: 0.0, item: Item::Empty }
                        } else {
                            let hyph = if matches!(b.kind, BlockKind::Para | BlockKind::Quote | BlockKind::ListItem(_)) { hyph } else { None };
                            let set = typeset::set(fonts, job.clone(), &typeset::Opts { width: job.wrap.max_width, justify: just, hyph, rtl: b.rtl.or(if rtl { Some(true) } else { None }) });
                            let g = set.galley;
                            let heading = matches!(b.kind, BlockKind::Heading(_));
                            let n = g.rows.len();
                            for (ri, row) in g.rows.iter().enumerate() {
                                let (c0, c1) = set.rows.get(ri).copied().unwrap_or((0, 0));
                                units.push(Unit { top: y + row.min_y(), bottom: y + row.max_y(), block: bi, c0, c1, keep_next: heading && ri + 1 == n });
                            }
                            let h = g.size().y;
                            LBlock { y, h, item: Item::Text { galley: g, ax, disp: set.disp, rows: set.rows } }
                        }
                    }
                };
                y += lb.h;
                prev_after = after;
                blocks.push(lb);
            }
        });
        let pages = paginate(&units, t.page_h);
        SectionLayout { section: section_idx, key, blocks, units, pages, height: y, width: t.width, vertical: false, col_h: t.page_h }
    }

    pub fn unit_for(&self, block: usize, offset: usize) -> usize {
        // first unit of the block containing offset, else first unit of a later block
        let mut best = None;
        for (i, u) in self.units.iter().enumerate() {
            if u.block == block {
                if offset >= u.c0 && offset < u.c1.max(u.c0 + 1) {
                    return i;
                }
                best = Some(i);
            } else if u.block > block {
                return best.unwrap_or(i);
            }
        }
        best.unwrap_or(self.units.len().saturating_sub(1))
    }

    pub fn page_for(&self, block: usize, offset: usize) -> usize {
        let u = self.unit_for(block, offset);
        self.pages.iter().position(|p| u >= p.u0 && u < p.u1).unwrap_or(0)
    }

    /// Location at the start of a page: (block, char offset).
    pub fn page_start(&self, page: usize) -> (usize, usize) {
        self.pages.get(page).and_then(|p| self.units.get(p.u0)).map(|u| (u.block, u.c0)).unwrap_or((0, 0))
    }

    /// Location at the end of a page (exclusive).
    pub fn page_end(&self, page: usize) -> (usize, usize) {
        self.pages.get(page).and_then(|p| p.u1.checked_sub(1)).and_then(|i| self.units.get(i)).map(|u| (u.block, u.c1)).unwrap_or((0, 0))
    }

    /// First unit whose bottom is below `y` (scrolled mode).
    pub fn unit_at_y(&self, y: f32) -> usize {
        self.units.partition_point(|u| u.bottom <= y + 1.0).min(self.units.len().saturating_sub(1))
    }

    pub fn y_of(&self, block: usize, offset: usize) -> f32 {
        self.units.get(self.unit_for(block, offset)).map(|u| u.top).unwrap_or(0.0)
    }

    /// Paint the slice [top, bottom) of the column with its top-left at `origin`.
    pub fn paint(&self, painter: &Painter, origin: Pos2, top: f32, bottom: f32, tex: &dyn Fn(&str) -> Option<TextureId>, decos: &[Deco], fg: Color32, dim_images: bool) {
        if self.vertical {
            return vertical::paint(self, painter, origin, top, bottom, tex, decos, fg, dim_images);
        }
        let clip = Rect::from_min_max(pos2(origin.x - 40.0, origin.y - 1.0), pos2(origin.x + self.width + 40.0, origin.y + (bottom - top) + 1.0));
        let p = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
        let start = self.blocks.partition_point(|b| b.y + b.h < top);
        for (bi, b) in self.blocks.iter().enumerate().skip(start) {
            if b.y > bottom {
                break;
            }
            let by = origin.y + b.y - top;
            match &b.item {
                Item::Text { galley, ax, disp, .. } => {
                    let gpos = pos2(origin.x + ax, by);
                    for d in decos.iter().filter(|d| d.block == bi) {
                        paint_deco(&p, galley, disp.as_deref(), gpos, d.c0, d.c1, d.kind);
                    }
                    // re-fetch through egui's cache: survives font-atlas rebuilds
                    p.galley(gpos, p.layout_job((*galley.job).clone()), fg);
                }
                Item::Table(lt) => {
                    let bpos = pos2(origin.x, by);
                    for d in decos.iter().filter(|d| d.block == bi) {
                        for c in &lt.cells {
                            let n = c.rows.last().map(|r| r.1).unwrap_or(0);
                            let (c0, c1) = (d.c0.max(c.start), d.c1.min(c.start + n));
                            if c0 < c1 {
                                paint_deco(&p, &c.galley, c.disp.as_deref(), c.pos + bpos.to_vec2(), c0 - c.start, c1 - c.start, d.kind);
                            }
                        }
                    }
                    let clip = p.clip_rect();
                    let shapes = p.fonts(|f| lt.shapes(f, bpos, fg, clip));
                    p.extend(shapes);
                }
                Item::Image { key, size, x } => {
                    let r = Rect::from_min_size(pos2(origin.x + x, by), *size);
                    match tex(key) {
                        Some(id) => {
                            let tint = if dim_images { Color32::from_gray(225) } else { Color32::WHITE };
                            p.image(id, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
                        }
                        None => {
                            p.rect_filled(r, CornerRadius::same(6), fg.linear_multiply(0.06));
                        }
                    }
                }
                Item::Rule => {
                    let cx = origin.x + self.width / 2.0;
                    let w = (self.width * 0.25).min(120.0);
                    p.line_segment([pos2(cx - w, by), pos2(cx + w, by)], Stroke::new(1.0, fg.linear_multiply(0.35)));
                }
                Item::Empty | Item::Vertical(..) => {}
            }
        }
    }

    /// Hit-test a point in column-local coordinates (relative to `origin` used
    /// in `paint` with the same `top`). Returns (block, char offset).
    pub fn hit(&self, top: f32, bottom: f32, local: Vec2, clamp: bool) -> Option<(usize, usize)> {
        if self.vertical {
            return vertical::hit(self, top, bottom, local, clamp);
        }
        let y = top + local.y;
        let mut best: Option<(usize, f32)> = None;
        let start = self.blocks.partition_point(|b| b.y + b.h < top);
        for (bi, b) in self.blocks.iter().enumerate().skip(start) {
            if b.y > bottom {
                break;
            }
            if let Item::Text { .. } | Item::Table(_) = b.item {
                let d = if y < b.y {
                    b.y - y
                } else if y > b.y + b.h {
                    y - b.y - b.h
                } else {
                    0.0
                };
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((bi, d));
                }
            }
        }
        let (bi, d) = best?;
        if !clamp && d > 4.0 {
            return None;
        }
        let b = &self.blocks[bi];
        match &b.item {
            Item::Text { galley, ax, disp, rows } => Some((bi, typeset::hit(galley, disp.as_deref(), rows, vec2(local.x - ax, (y - b.y).clamp(0.0, b.h - 0.5))))),
            Item::Table(lt) => {
                let p = pos2(local.x, (y - b.y).clamp(0.0, b.h - 0.5));
                // the cell under the point, else the nearest one
                let c = lt.cells.iter().min_by(|a, c| a.rect.distance_sq_to_pos(p).total_cmp(&c.rect.distance_sq_to_pos(p)))?;
                let off = typeset::hit(&c.galley, c.disp.as_deref(), &c.rows, p - c.pos);
                Some((bi, c.start + off))
            }
            _ => None,
        }
    }

    /// Whether `local` is actually over text (not margin/gap).
    pub fn over_text(&self, top: f32, local: Vec2) -> bool {
        if self.vertical {
            return vertical::over_text(self, top, local);
        }
        let y = top + local.y;
        let i = self.blocks.partition_point(|b| b.y + b.h < y);
        let Some(b) = self.blocks.get(i) else { return false };
        if y < b.y {
            return false;
        }
        match &b.item {
            Item::Text { galley, ax, .. } => {
                let gl = vec2(local.x - ax, y - b.y);
                galley.rows.iter().any(|r| r.rect.expand(2.0).contains(pos2(gl.x, gl.y)))
            }
            Item::Table(lt) => {
                let p = pos2(local.x, y - b.y);
                lt.cells.iter().any(|c| c.galley.rows.iter().any(|r| r.rect.translate(c.pos.to_vec2()).expand(2.0).contains(p)))
            }
            _ => false,
        }
    }

    /// Screen rect (column-local) of a char in a block, for anchoring popups.
    pub fn char_rect(&self, block: usize, offset: usize) -> Option<Rect> {
        let b = self.blocks.get(block)?;
        match &b.item {
            Item::Text { galley, ax, disp, .. } => Some(typeset::char_rect(galley, disp.as_deref(), offset).translate(vec2(*ax, b.y))),
            Item::Table(lt) => {
                let c = lt.cells.iter().rev().find(|c| c.start <= offset)?;
                Some(typeset::char_rect(&c.galley, c.disp.as_deref(), offset - c.start).translate(c.pos.to_vec2() + vec2(0.0, b.y)))
            }
            _ => None,
        }
    }
}

fn paint_deco(p: &Painter, g: &Galley, disp: Option<&Disp>, gpos: Pos2, c0: usize, c1: usize, kind: DecoKind) {
    for r in typeset::range_rects(g, disp, c0, c1) {
        let r = r.translate(gpos.to_vec2());
        match kind {
            DecoKind::Fill(c) => {
                p.rect_filled(r.expand2(vec2(1.0, 0.0)), CornerRadius::same(2), c);
            }
            DecoKind::Underline(c) => {
                p.line_segment([pos2(r.left(), r.bottom() - 2.0), pos2(r.right(), r.bottom() - 2.0)], Stroke::new(2.0, c));
            }
        }
    }
}

fn paginate(units: &[Unit], page_h: f32) -> Vec<Page> {
    let mut pages = Vec::new();
    let mut i = 0;
    while i < units.len() {
        let top = units[i].top;
        let limit = top + page_h.max(40.0);
        let mut j = i + 1;
        while j < units.len() && units[j].bottom <= limit + 0.5 {
            j += 1;
        }
        // keep headings with the following line
        if j < units.len() {
            let mut k = j;
            while k > i + 1 && units[k - 1].keep_next {
                k -= 1;
            }
            if k > i + 1 {
                j = k;
            }
        }
        pages.push(Page { u0: i, u1: j, top, bottom: units[j - 1].bottom });
        i = j;
    }
    if pages.is_empty() {
        pages.push(Page { u0: 0, u1: 0, top: 0.0, bottom: 0.0 });
    }
    pages
}

/// Galley-local rects covering chars [c0, c1).
pub fn range_rects(g: &Galley, c0: usize, c1: usize) -> Vec<Rect> {
    let mut out = Vec::new();
    if c1 <= c0 {
        return out;
    }
    let mut pos = 0;
    for row in &g.rows {
        let n = row.char_count_excluding_newline();
        let rs = pos;
        let re = pos + n;
        let s = c0.max(rs);
        let e = c1.min(re);
        if s < e {
            let x0 = row.x_offset(s - rs);
            let x1 = row.x_offset(e - rs);
            out.push(Rect::from_min_max(pos2(x0, row.min_y()), pos2(x1, row.max_y())));
        }
        pos += row.char_count_including_newline();
        if pos >= c1 {
            break;
        }
    }
    out
}

/// Word boundaries around a char offset.
pub fn word_at(text: &str, offset: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let is_w = |c: char| c.is_alphanumeric() || c == '\'' || c == '’' || c == '-';
    let mut s = offset.min(chars.len());
    let mut e = s;
    while s > 0 && is_w(chars[s - 1]) {
        s -= 1;
    }
    while e < chars.len() && is_w(chars[e]) {
        e += 1;
    }
    // trim trailing apostrophes/hyphens
    while e > s && matches!(chars[e - 1], '\'' | '’' | '-') {
        e -= 1;
    }
    (s, e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::html::{convert, Hooks, Options};
    use crate::book::css::Sheet;

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let mut defs = egui::FontDefinitions::default();
        let prop = defs.families[&FontFamily::Proportional].clone();
        for n in ["book", "book-b", "book-i", "book-bi", "mono"] {
            defs.families.insert(FontFamily::Name(n.into()), prop.clone());
        }
        ctx.set_fonts(defs);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn typo(width: f32, page_h: f32) -> Typo {
        Typo { size: 16.0, line_height: 1.5, para: 0.6, letter: 0.0, justify: true, indent: false, override_book: false, fg: Color32::BLACK, link: Color32::BLUE, width, page_h, fake_italic: false, font_tag: String::new(), lang: "en".into(), vertical: false }
    }

    fn section(src: &str) -> Section {
        let mut link = |_: &str| 0u32;
        let mut image = |_: &str| None;
        let mut css = |_: &str| None;
        let mut hooks = Hooks { link: &mut link, image: &mut image, stylesheet: &mut css };
        let o = convert(src, Sheet::default(), &mut hooks, &Options::default());
        Section { title: String::new(), blocks: o.blocks, chars: 0 }.finish()
    }

    #[test]
    fn table_layout_cells_rows_and_hits() {
        let sec = section("<table><tr><th>Name</th><th>Notes</th></tr><tr><td>Venus</td><td>Hottest planet thanks to a runaway greenhouse effect that traps heat beneath thick clouds.</td></tr><tr><td>Earth</td><td>Home.</td></tr></table>");
        let c = ctx();
        let t = typo(300.0, 120.0);
        let l = SectionLayout::build(&c, 0, &sec, &t, &|_| None, 1);
        let Item::Table(lt) = &l.blocks[0].item else { panic!("table item") };
        assert_eq!(lt.cells.len(), 6);
        for a in &lt.cells {
            assert!(a.rect.right() <= 300.5, "fits the measure: {:?}", a.rect);
            for b in &lt.cells {
                if !std::ptr::eq(a, b) {
                    assert!(!a.rect.shrink(1.0).intersects(b.rect.shrink(1.0)), "cells overlap");
                }
            }
        }
        // the long cell wrapped
        assert!(lt.cells[3].galley.rows.len() > 1);
        // one unit per row; rows never split across pages
        assert_eq!(l.units.len(), 3);
        for p in &l.pages {
            for u in &l.units[p.u0..p.u1] {
                assert!(u.top >= p.top - 0.01 && u.bottom <= p.bottom + 0.01);
            }
        }
        // hit the middle of every cell: offset inside that cell's text
        let text: Vec<char> = sec.blocks[0].text().chars().collect();
        let BlockKind::Table(tb) = &sec.blocks[0].kind else { panic!() };
        for (cell, model) in lt.cells.iter().zip(tb.rows.iter().flatten()) {
            let p = cell.rect.center();
            let (bi, off) = l.hit(0.0, l.height, p.to_vec2(), true).unwrap();
            assert_eq!(bi, 0);
            assert!(off >= model.start && off <= model.end, "{off} in {}..{} ({:?})", model.start, model.end, text[model.start..model.end].iter().collect::<String>());
        }
    }

    #[test]
    fn rtl_paragraph_is_right_aligned() {
        let sec = section("<p>שלום עולם וכל יושביו, זהו משפט ארוך מספיק כדי להישבר על פני כמה שורות בעמוד.</p>");
        let c = ctx();
        let l = SectionLayout::build(&c, 0, &sec, &typo(200.0, 600.0), &|_| None, 1);
        let Item::Text { galley, ax, disp, rows } = &l.blocks[0].item else { panic!() };
        assert!(disp.is_some());
        assert!((*ax - 200.0).abs() < 0.5, "anchored right: {ax}");
        assert_eq!(rows.len(), galley.rows.len());
        assert!(rows.len() > 1);
        assert_eq!(l.units.first().map(|u| u.c0), Some(0));
        assert_eq!(l.units.last().map(|u| u.c1), Some(sec.blocks[0].char_len()));
    }

    #[test]
    fn vertical_columns_paginate_horizontally() {
        let sec = section(&format!("<h1>吾輩は猫である</h1>{}", "<p>吾輩は猫である。名前はまだ無い。どこで生れたかとんと見当がつかぬ。「何でも薄暗いじめじめした所で泣いていた。」第12章。</p>".repeat(8)));
        let c = ctx();
        let mut t = typo(300.0, 200.0);
        t.vertical = true;
        let l = SectionLayout::build(&c, 0, &sec, &t, &|_| None, 1);
        assert!(l.vertical);
        assert!(l.pages.len() > 1, "several pages: {}", l.pages.len());
        for p in &l.pages {
            assert!(p.bottom - p.top <= 300.5);
        }
        // units cover each block's text contiguously
        for (bi, b) in sec.blocks.iter().enumerate() {
            let us: Vec<&Unit> = l.units.iter().filter(|u| u.block == bi).collect();
            assert_eq!(us.first().unwrap().c0, 0);
            assert_eq!(us.last().unwrap().c1, b.char_len());
            for w in us.windows(2) {
                assert_eq!(w[0].c1, w[1].c0);
            }
        }
        // top of the rightmost column = first char of the heading
        let pg = l.pages[0];
        let hit = l.hit(pg.top, pg.bottom, vec2(300.0 - 4.0, 2.0), true);
        assert_eq!(hit, Some((0, 0)));
        // no column is taller than the page
        if let Item::Vertical(cols, _) = &l.blocks[1].item {
            for col in cols {
                assert!(col.galley.size().y <= 200.0 + 40.0, "{}", col.galley.size().y);
            }
        }
    }
}
