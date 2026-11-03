//! Vertical CJK layout (`writing-mode: vertical-rl`).
//!
//! Characters run top to bottom in columns; columns advance right to left.
//! The section is still one continuous strip so the rest of the engine
//! (units, greedy pagination, page starts, locations) is unchanged: in a
//! vertical layout a unit's `top..bottom` is its span along the *horizontal*
//! progression axis (distance from the right edge of the strip), and pages
//! are cut every `width` instead of every `page_h`.
//!
//! Each column is a narrow egui galley with one character per row
//! (halign centre), so painting still goes through egui's galley cache.
//! Upright glyphs only: Latin runs stay upright, one letter per row, except
//! runs of 1–2 ASCII digits, which are set horizontally in one cell
//! (tate-chū-yoko). Punctuation and brackets use the Unicode vertical
//! presentation forms when the font has them.

use std::sync::Arc;

use eframe::egui::{self, pos2, vec2, Align as EAlign, Color32, CornerRadius, FontId, Galley, Painter, Pos2, Rect, Stroke, TextureId, Vec2};
use egui::text::{LayoutJob, TextWrapping};

use super::typeset::{self, Disp};
use super::{family, scale_for, Deco, Item, LBlock, SectionLayout, Typo, Unit};
use crate::book::{BlockKind, Section, Style};

pub struct VCol {
    pub galley: Arc<Galley>,
    pub disp: Arc<Disp>,
    pub rows: Vec<(usize, usize)>,
    /// Offset of the column's start along the progression axis, within the block.
    pub v: f32,
    pub pitch: f32,
}

fn vertical_form(c: char) -> char {
    match c {
        '、' => '︑',
        '。' => '︒',
        '，' | ',' => '︐',
        '：' => '︓',
        '；' => '︔',
        '！' => '︕',
        '？' => '︖',
        '「' => '﹁',
        '」' => '﹂',
        '『' => '﹃',
        '』' => '﹄',
        '（' | '(' => '︵',
        '）' | ')' => '︶',
        '｛' => '︷',
        '｝' => '︸',
        '〔' => '︹',
        '〕' => '︺',
        '【' => '︻',
        '】' => '︼',
        '《' => '︽',
        '》' => '︾',
        '〈' => '︿',
        '〉' => '﹀',
        '［' | '[' => '﹇',
        '］' | ']' => '﹈',
        '…' => '︙',
        '‥' => '︰',
        '—' | '―' => '︱',
        '–' => '︲',
        'ー' | '〜' | '～' => '丨',
        _ => c,
    }
}

fn no_col_start(c: char) -> bool {
    "、。，．・：；！？）」』】〕〉》｝ーぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ々".contains(c)
}

/// Cells of a block: (display text, logical start, logical len, span index).
fn cells(text: &[char]) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        // tate-chu-yoko: 1-2 ascii digits standing alone
        if c.is_ascii_digit() {
            let mut j = i;
            while j < text.len() && text[j].is_ascii_digit() {
                j += 1;
            }
            if j - i <= 2 {
                out.push((text[i..j].iter().collect(), i, j - i));
                i = j;
                continue;
            }
        }
        out.push((c.to_string(), i, 1));
        i += 1;
    }
    out
}

pub fn build(ctx: &egui::Context, section_idx: usize, section: &Section, t: &Typo, img_size: &dyn Fn(&str) -> Option<[usize; 2]>, key: u64) -> SectionLayout {
    let mut blocks = Vec::with_capacity(section.blocks.len());
    let mut units = Vec::new();
    let col_h = t.page_h.max(80.0);
    let mut v = 0.0f32;
    ctx.fonts(|fonts| {
        for (bi, b) in section.blocks.iter().enumerate() {
            if bi > 0 {
                v += match b.kind {
                    BlockKind::Heading(_) | BlockKind::Image(_) => 1.2 * t.size,
                    _ => t.para * t.size * 0.6,
                };
            }
            let lb = match &b.kind {
                BlockKind::Image(k) => {
                    let nat = img_size(k).map(|s| vec2(s[0] as f32, s[1] as f32)).unwrap_or(vec2(col_h * 0.6, col_h));
                    let nat = if crate::book::svg::is_svg_key(k) { nat / crate::book::svg::SCALE } else { nat };
                    let scale = (t.width / nat.x).min(col_h / nat.y).min(1.0f32.max(t.size / 18.0));
                    let size = nat * scale.max(0.01);
                    units.push(Unit { top: v, bottom: v + size.x, block: bi, c0: 0, c1: 0, keep_next: false });
                    // x = offset down the column (centred)
                    LBlock { y: v, h: size.x, item: Item::Image { key: k.clone(), size, x: (col_h - size.y) / 2.0 } }
                }
                BlockKind::Rule => {
                    units.push(Unit { top: v, bottom: v + t.size, block: bi, c0: 0, c1: 0, keep_next: false });
                    LBlock { y: v, h: t.size, item: Item::Rule }
                }
                _ => {
                    let text: Vec<char> = b.text().chars().collect();
                    if text.iter().all(|c| c.is_whitespace()) {
                        blocks.push(LBlock { y: v, h: 0.0, item: Item::Empty });
                        continue;
                    }
                    let base = t.size * scale_for(&b.kind) * if b.note { 0.88 } else { 1.0 };
                    let pitch = (base * t.line_height.max(1.4)).round();
                    let adv = (base * 1.04 + t.letter).max(6.0);
                    // style of each logical char
                    let mut st: Vec<(Style, f32, bool)> = Vec::with_capacity(text.len());
                    for sp in &b.spans {
                        for _ in sp.text.chars() {
                            st.push((sp.style, sp.size, sp.link.is_some()));
                        }
                    }
                    let per_col = ((col_h / adv).floor() as usize).max(1);
                    let cs = cells(&text);
                    // indent of every column (list depth, quotes) from the top
                    let mut indent = (b.indent * t.size).min(col_h * 0.3);
                    if let BlockKind::ListItem(d) = b.kind {
                        indent += (d as f32 - 1.0) * 1.4 * t.size;
                    }
                    let per_col = ((per_col as f32 - indent / adv).floor() as usize).max(1);
                    let mut cols = Vec::new();
                    let mut i = 0;
                    let mut cv = 0.0f32;
                    let start_v = v;
                    while i < cs.len() {
                        let mut j = (i + per_col).min(cs.len());
                        // hard line breaks end a column
                        if let Some(k) = (i..j).find(|&k| cs[k].0 == "\n") {
                            j = k + 1;
                        }
                        // kinsoku: hang closing punctuation on this column
                        while j < cs.len() && j < i + per_col + 2 && cs[j].0.chars().next().is_some_and(no_col_start) {
                            j += 1;
                        }
                        let mut job = LayoutJob::default();
                        let mut v2l: Vec<u32> = Vec::new();
                        let mut rows = Vec::new();
                        for (k, (s, l0, n)) in cs[i..j].iter().enumerate() {
                            if s == "\n" {
                                continue;
                            }
                            let (style, rel, link) = st.get(*l0).copied().unwrap_or_default();
                            let rel = if t.override_book && !style.has(Style::SUP) && !style.has(Style::SUB) { 1.0 } else { rel };
                            let size = (base * rel).max(6.0) * if s.chars().count() == 2 { 0.8 } else { 1.0 };
                            let font = FontId::new(size, family(style));
                            let c0 = s.chars().next().unwrap_or(' ');
                            let shown = if s.chars().count() == 1 {
                                let vf = vertical_form(c0);
                                if vf != c0 && fonts.has_glyph(&font, vf) { vf.to_string() } else { s.clone() }
                            } else {
                                s.clone()
                            };
                            let mut f = egui::text::TextFormat::simple(font, if link { t.link } else { t.fg });
                            f.line_height = Some(adv);
                            f.italics = style.has(Style::ITALIC) && t.fake_italic;
                            let text = if k + 1 < j - i { format!("{shown}\n") } else { shown.clone() };
                            for _ in shown.chars() {
                                v2l.push(*l0 as u32);
                            }
                            if k + 1 < j - i {
                                v2l.push((*l0 + *n) as u32);
                            }
                            job.append(&text, 0.0, f);
                            rows.push((*l0, *l0 + *n));
                        }
                        let col_end = if j < cs.len() { cs[j].1 } else { text.len() };
                        if let Some(r) = rows.last_mut() {
                            r.1 = col_end;
                        }
                        if !job.text.is_empty() {
                            if job.text.ends_with('\n') {
                                job.text.pop();
                                if let Some(sec) = job.sections.last_mut() {
                                    sec.byte_range.end -= 1;
                                }
                                v2l.pop();
                            }
                            job.halign = EAlign::Center;
                            job.wrap = TextWrapping { max_width: f32::INFINITY, ..Default::default() };
                            v2l.push(col_end as u32);
                            let mut l2v = vec![0u32; text.len() + 1];
                            for (vi, &l) in v2l.iter().enumerate().rev() {
                                if (l as usize) < l2v.len() {
                                    l2v[l as usize] = vi as u32;
                                }
                            }
                            let nv = v2l.len();
                            let g = fonts.layout_job(job);
                            let c0 = rows.first().map(|r| r.0).unwrap_or(cs[i].1);
                            units.push(Unit { top: start_v + cv, bottom: start_v + cv + pitch, block: bi, c0, c1: col_end, keep_next: matches!(b.kind, BlockKind::Heading(_)) && j >= cs.len() });
                            let g_rows = g.rows.len();
                            rows.truncate(g_rows);
                            cols.push(VCol { galley: g, disp: Arc::new(Disp { v2l, l2v, rtl: vec![false; nv] }), rows, v: cv, pitch });
                            cv += pitch;
                        }
                        i = j;
                    }
                    LBlock { y: v, h: cv, item: Item::Vertical(cols, indent) }
                }
            };
            v += lb.h;
            blocks.push(lb);
        }
    });
    let pages = super::paginate(&units, t.width);
    SectionLayout { section: section_idx, key, blocks, units, pages, height: v, width: t.width, vertical: true, col_h }
}

/// Screen x of the right edge of progression offset `v` on a page starting at `top`.
fn x_of(l: &SectionLayout, origin: Pos2, top: f32, v: f32) -> f32 {
    origin.x + l.width - (v - top)
}

pub fn paint(l: &SectionLayout, painter: &Painter, origin: Pos2, top: f32, bottom: f32, tex: &dyn Fn(&str) -> Option<TextureId>, decos: &[Deco], fg: Color32, dim_images: bool) {
    let clip = Rect::from_min_max(pos2(origin.x - 2.0, origin.y - 4.0), pos2(origin.x + l.width + 2.0, origin.y + l.col_h + 4.0));
    let p = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
    for (bi, b) in l.blocks.iter().enumerate() {
        if b.y + b.h <= top + 0.5 || b.y >= bottom - 0.5 {
            continue;
        }
        match &b.item {
            Item::Vertical(cols, indent) => {
                for c in cols {
                    let cv = b.y + c.v;
                    if cv + c.pitch <= top + 0.5 || cv >= bottom - 0.5 {
                        continue;
                    }
                    let gpos = pos2(x_of(l, origin, top, cv) - c.pitch / 2.0, origin.y + indent);
                    for d in decos.iter().filter(|d| d.block == bi) {
                        for r in typeset::range_rects(&c.galley, Some(&c.disp), d.c0, d.c1) {
                            let r = r.translate(gpos.to_vec2());
                            match d.kind {
                                super::DecoKind::Fill(col) => {
                                    p.rect_filled(r.expand2(vec2(0.0, 1.0)), CornerRadius::same(2), col);
                                }
                                super::DecoKind::Underline(col) => {
                                    p.line_segment([pos2(r.right() + 1.0, r.top()), pos2(r.right() + 1.0, r.bottom())], Stroke::new(2.0, col));
                                }
                            }
                        }
                    }
                    p.galley(gpos, p.layout_job((*c.galley.job).clone()), fg);
                }
            }
            Item::Image { key, size, x } => {
                let r = Rect::from_min_size(pos2(x_of(l, origin, top, b.y) - size.x, origin.y + x), *size);
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
                let x = x_of(l, origin, top, b.y + b.h / 2.0);
                let h = (l.col_h * 0.25).min(120.0);
                let cy = origin.y + l.col_h / 2.0;
                p.line_segment([pos2(x, cy - h), pos2(x, cy + h)], Stroke::new(1.0, fg.linear_multiply(0.35)));
            }
            _ => {}
        }
    }
}

/// (block, column) nearest to progression offset `v`, within the page.
fn col_at(l: &SectionLayout, top: f32, bottom: f32, v: f32) -> Option<(usize, usize, f32)> {
    let mut best: Option<(usize, usize, f32)> = None;
    for (bi, b) in l.blocks.iter().enumerate() {
        if b.y + b.h <= top || b.y >= bottom {
            continue;
        }
        if let Item::Vertical(cols, _) = &b.item {
            for (ci, c) in cols.iter().enumerate() {
                let c0 = b.y + c.v;
                if c0 + c.pitch <= top + 0.5 || c0 >= bottom - 0.5 {
                    continue;
                }
                let d = if v < c0 { c0 - v } else if v > c0 + c.pitch { v - c0 - c.pitch } else { 0.0 };
                if best.is_none_or(|(_, _, bd)| d < bd) {
                    best = Some((bi, ci, d));
                }
            }
        }
    }
    best
}

pub fn hit(l: &SectionLayout, top: f32, bottom: f32, local: Vec2, clamp: bool) -> Option<(usize, usize)> {
    let v = top + (l.width - local.x);
    let (bi, ci, d) = col_at(l, top, bottom, v)?;
    if !clamp && d > 4.0 {
        return None;
    }
    let b = &l.blocks[bi];
    let Item::Vertical(cols, indent) = &b.item else { return None };
    let c = &cols[ci];
    let cx = l.width - (b.y + c.v - top) - c.pitch / 2.0;
    // vertical: the row is chosen by y; within a cell, upper half = before
    let gp = vec2(local.x - cx, (local.y - indent).clamp(0.0, c.galley.size().y - 0.5));
    let row = c.galley.rows.iter().position(|r| gp.y < r.max_y()).unwrap_or(c.galley.rows.len().saturating_sub(1));
    let (r0, r1) = c.rows.get(row).copied().unwrap_or((0, 0));
    let mid = c.galley.rows.get(row).map(|r| (r.min_y() + r.max_y()) / 2.0).unwrap_or(0.0);
    Some((bi, if gp.y > mid { r1.min(r0 + 1).max(r0) } else { r0 }))
}

pub fn over_text(l: &SectionLayout, top: f32, local: Vec2) -> bool {
    let v = top + (l.width - local.x);
    col_at(l, top, top + l.width, v).is_some_and(|(bi, ci, d)| {
        d <= 0.0
            && match &l.blocks[bi].item {
                Item::Vertical(cols, indent) => local.y >= *indent && local.y <= indent + cols[ci].galley.size().y,
                _ => false,
            }
    })
}
