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

use crate::book::{Align, Block, BlockKind, Section, Style};

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
        h.finish()
    }
}

pub enum Item {
    Text { galley: Arc<Galley>, ax: f32 },
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

fn job_for(block: &Block, prev: Option<&Block>, t: &Typo, avail: f32) -> (LayoutJob, f32, bool) {
    let base = t.size * scale_for(&block.kind) * if block.note { 0.88 } else { 1.0 };
    let lh = base * if block.kind == BlockKind::Pre { 1.3 } else { t.line_height };
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
        && block.kind == BlockKind::Para
        && matches!(align, Align::Justify | Align::Start)
        && prev.is_some_and(|p| p.kind == BlockKind::Para && !matches!(p.align, Some(Align::Center) | Some(Align::End)))
        && !block.spans.first().is_some_and(|s| s.text.starts_with('\n'));
    let mut job = LayoutJob::default();
    let mut first = true;
    for sp in &block.spans {
        let rel = if t.override_book && !sp.style.has(Style::SUP) && !sp.style.has(Style::SUB) { 1.0 } else { sp.size };
        let size = (base * rel).max(6.0);
        let color = if sp.link.is_some() { t.link } else { t.fg };
        let mut f = TextFormat::simple(FontId::new(size, family(sp.style)), color);
        f.line_height = Some(lh);
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
        }
        let lead = if first && first_indent { 1.5 * t.size } else { 0.0 };
        first = false;
        job.append(&sp.text, lead, f);
    }
    let w = (avail - indent_px).max(40.0);
    job.wrap = TextWrapping { max_width: w, ..Default::default() };
    // egui's built-in justify letter-spaces loose lines; we justify by word spacing instead.
    job.justify = false;
    let ax = match align {
        Align::Center => {
            job.halign = EAlign::Center;
            indent_px + w / 2.0
        }
        Align::End => {
            job.halign = EAlign::RIGHT;
            indent_px + w
        }
        _ => indent_px,
    };
    (job, ax, align == Align::Justify)
}

/// Re-layout a paragraph so every line but the last fills the measure, with
/// the slack distributed over the inter-word spaces of that line.
fn justify(fonts: &egui::text::Fonts, job: LayoutJob, g: Arc<Galley>) -> Arc<Galley> {
    let w = job.wrap.max_width;
    if g.rows.len() < 2 {
        return g;
    }
    // char index → extra leading space before it
    let mut inserts: Vec<(usize, f32)> = Vec::new();
    let mut start = 0usize;
    let n = g.rows.len();
    for (ri, row) in g.rows.iter().enumerate() {
        let cc = row.char_count_including_newline();
        let last_line = ri + 1 == n || row.ends_with_newline;
        if !last_line {
            let glyphs = &row.glyphs;
            let end = glyphs.iter().rposition(|gl| !gl.chr.is_whitespace());
            if let Some(end) = end {
                let first = glyphs.iter().position(|gl| !gl.chr.is_whitespace()).unwrap_or(0);
                let spaces: Vec<usize> = (first..end).filter(|&j| glyphs[j].chr == ' ' && j + 1 <= end && glyphs[j + 1].chr != ' ').collect();
                let slack = w - glyphs[end].max_x() - 0.75;
                if !spaces.is_empty() && slack > 0.0 && slack < w * 0.5 {
                    let per = slack / spaces.len() as f32;
                    for j in spaces {
                        inserts.push((start + j + 1, per));
                    }
                }
            }
        }
        start += cc;
    }
    if inserts.is_empty() {
        return g;
    }
    // rebuild the job with leading space before the chosen words
    let text = job.text.clone();
    let char_byte: Vec<usize> = text.char_indices().map(|(b, _)| b).chain(std::iter::once(text.len())).collect();
    let mut cuts: Vec<(usize, f32)> = inserts.iter().filter_map(|&(c, e)| char_byte.get(c).map(|&b| (b, e))).collect();
    cuts.sort_by_key(|c| c.0);
    let mut nj = LayoutJob { text: String::new(), sections: Vec::new(), ..job.clone() };
    let mut ci = 0;
    for sec in &job.sections {
        let mut pos = sec.byte_range.start;
        let mut lead = sec.leading_space;
        while ci < cuts.len() && cuts[ci].0 <= pos {
            if cuts[ci].0 == pos {
                lead += cuts[ci].1;
            }
            ci += 1;
        }
        loop {
            let next = cuts.get(ci).filter(|c| c.0 < sec.byte_range.end).copied();
            let end = next.map(|c| c.0).unwrap_or(sec.byte_range.end);
            nj.append(&text[pos..end], lead, sec.format.clone());
            match next {
                Some((b, e)) => {
                    pos = b;
                    lead = e;
                    ci += 1;
                }
                None => break,
            }
        }
    }
    let ng = fonts.layout_job(nj);
    if ng.rows.len() == g.rows.len() && ng.rows.iter().zip(g.rows.iter()).all(|(a, b)| a.char_count_including_newline() == b.char_count_including_newline()) {
        ng
    } else {
        g
    }
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
        let mut blocks = Vec::with_capacity(section.blocks.len());
        let mut units = Vec::new();
        let mut y = 0.0f32;
        let mut prev_after = 0.0f32;
        let only_image = section.blocks.len() <= 2 && section.blocks.iter().any(|b| matches!(b.kind, BlockKind::Image(_)));
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
                    BlockKind::Rule => {
                        let h = 1.0;
                        units.push(Unit { top: y, bottom: y + h, block: bi, c0: 0, c1: 0, keep_next: false });
                        LBlock { y, h, item: Item::Rule }
                    }
                    _ => {
                        let (job, ax, just) = job_for(b, prev, t, t.width);
                        if job.text.is_empty() {
                            LBlock { y, h: 0.0, item: Item::Empty }
                        } else {
                            let g = fonts.layout_job(job.clone());
                            let g = if just { justify(fonts, job, g) } else { g };
                            let mut c = 0;
                            let heading = matches!(b.kind, BlockKind::Heading(_));
                            let n = g.rows.len();
                            for (ri, row) in g.rows.iter().enumerate() {
                                let cc = row.char_count_including_newline();
                                units.push(Unit { top: y + row.min_y(), bottom: y + row.max_y(), block: bi, c0: c, c1: c + cc, keep_next: heading && ri + 1 == n });
                                c += cc;
                            }
                            let h = g.size().y;
                            LBlock { y, h, item: Item::Text { galley: g, ax } }
                        }
                    }
                };
                y += lb.h;
                prev_after = after;
                blocks.push(lb);
            }
        });
        let pages = paginate(&units, t.page_h);
        SectionLayout { section: section_idx, key, blocks, units, pages, height: y, width: t.width }
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
        let clip = Rect::from_min_max(pos2(origin.x - 40.0, origin.y - 1.0), pos2(origin.x + self.width + 40.0, origin.y + (bottom - top) + 1.0));
        let p = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
        let start = self.blocks.partition_point(|b| b.y + b.h < top);
        for (bi, b) in self.blocks.iter().enumerate().skip(start) {
            if b.y > bottom {
                break;
            }
            let by = origin.y + b.y - top;
            match &b.item {
                Item::Text { galley, ax } => {
                    let gpos = pos2(origin.x + ax, by);
                    for d in decos.iter().filter(|d| d.block == bi) {
                        for r in range_rects(galley, d.c0, d.c1) {
                            let r = r.translate(gpos.to_vec2());
                            match d.kind {
                                DecoKind::Fill(c) => {
                                    p.rect_filled(r.expand2(vec2(1.0, 0.0)), CornerRadius::same(2), c);
                                }
                                DecoKind::Underline(c) => {
                                    p.line_segment([pos2(r.left(), r.bottom() - 2.0), pos2(r.right(), r.bottom() - 2.0)], Stroke::new(2.0, c));
                                }
                            }
                        }
                    }
                    p.galley(gpos, galley.clone(), fg);
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
                Item::Empty => {}
            }
        }
    }

    /// Hit-test a point in column-local coordinates (relative to `origin` used
    /// in `paint` with the same `top`). Returns (block, char offset).
    pub fn hit(&self, top: f32, bottom: f32, local: Vec2, clamp: bool) -> Option<(usize, usize)> {
        let y = top + local.y;
        let mut best: Option<(usize, f32)> = None;
        let start = self.blocks.partition_point(|b| b.y + b.h < top);
        for (bi, b) in self.blocks.iter().enumerate().skip(start) {
            if b.y > bottom {
                break;
            }
            if let Item::Text { .. } = b.item {
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
        let Item::Text { galley, ax } = &b.item else { return None };
        let c = galley.cursor_from_pos(vec2(local.x - ax, (y - b.y).clamp(0.0, b.h - 0.5)));
        Some((bi, c.ccursor.index))
    }

    /// Whether `local` is actually over text (not margin/gap).
    pub fn over_text(&self, top: f32, local: Vec2) -> bool {
        let y = top + local.y;
        let i = self.blocks.partition_point(|b| b.y + b.h < y);
        let Some(b) = self.blocks.get(i) else { return false };
        if let Item::Text { galley, ax } = &b.item {
            if y < b.y {
                return false;
            }
            let gl = vec2(local.x - ax, y - b.y);
            return galley.rows.iter().any(|r| r.rect.expand(2.0).contains(pos2(gl.x, gl.y)));
        }
        false
    }

    /// Screen rect (column-local) of a char in a block, for anchoring popups.
    pub fn char_rect(&self, block: usize, offset: usize) -> Option<Rect> {
        let b = self.blocks.get(block)?;
        let Item::Text { galley, ax } = &b.item else { return None };
        let r = galley.pos_from_ccursor(egui::text::CCursor::new(offset));
        Some(r.translate(vec2(*ax, b.y)))
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
