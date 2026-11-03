//! Paragraph setting beyond what egui does by itself: Arabic shaping,
//! bidirectional reordering, hyphenation and word-space justification.
//!
//! egui lays text out in logical order, left to right, wrapping at spaces.
//! For plain left-to-right text without hyphenation we let it do exactly that
//! (fast path, galley text == `Block::text()`). Otherwise we:
//!
//! 1. shape Arabic letters into presentation forms (logical order),
//! 2. measure the shaped text unwrapped and break it into lines ourselves
//!    (greedy, with hyphenation for justified text),
//! 3. reorder every line visually (UAX #9 levels from `unicode-bidi`, L2 run
//!    reversal, mirrored brackets),
//! 4. lay the display text out with an explicit `\n` per line.
//!
//! The display galley's text then differs from the logical text, so a `Disp`
//! map translates between display chars and logical offsets; hit-testing,
//! highlight rectangles and line ranges all go through it. Selection, search,
//! TTS and stored positions stay in logical offsets.

use std::sync::Arc;

use eframe::egui::{self, pos2, Align as EAlign, Galley, Rect, Vec2};
use egui::text::{Fonts, LayoutJob, TextWrapping};

use crate::book::script;

/// Display ↔ logical char mapping for a galley whose text was transformed.
pub struct Disp {
    /// Display char → logical char offset (len = display chars + 1).
    pub v2l: Vec<u32>,
    /// Logical char → a display char showing it (len = logical chars + 1).
    pub l2v: Vec<u32>,
    /// Display char sits in a right-to-left run.
    pub rtl: Vec<bool>,
}

pub struct Set {
    pub galley: Arc<Galley>,
    pub disp: Option<Arc<Disp>>,
    /// Logical char range [c0, c1) of each galley row.
    pub rows: Vec<(usize, usize)>,
    /// The paragraph's base direction is right-to-left.
    pub rtl: bool,
}

pub struct Opts {
    /// Measure (wrap width).
    pub width: f32,
    pub justify: bool,
    pub hyph: Option<hypher::Lang>,
    /// Explicit base direction (None = first strong char).
    pub rtl: Option<bool>,
}

/// Base direction of a paragraph: explicit, else its first strong char.
pub fn base_rtl(text: &str, explicit: Option<bool>) -> bool {
    if let Some(r) = explicit {
        return r;
    }
    for c in text.chars() {
        if script::is_rtl_char(c) {
            return true;
        }
        if c.is_alphabetic() {
            return false;
        }
    }
    false
}

fn needs_bidi(text: &str, explicit: Option<bool>) -> bool {
    explicit == Some(true) || script::has_rtl(text)
}

/// Lay out a logical job (`job.wrap.max_width` = measure, `job.halign` set by the
/// caller for the paragraph's direction).
pub fn set(fonts: &Fonts, job: LayoutJob, o: &Opts) -> Set {
    let bidi = needs_bidi(&job.text, o.rtl);
    let rtl = bidi && base_rtl(&job.text, o.rtl);
    let hyph = if o.justify && !bidi { o.hyph } else { None };
    if !bidi && hyph.is_none() {
        let g = fonts.layout_job(job.clone());
        let g = if o.justify { justify_wrapped(fonts, job, g) } else { g };
        let mut rows = Vec::with_capacity(g.rows.len());
        let mut c = 0;
        for r in &g.rows {
            let n = r.char_count_including_newline();
            rows.push((c, c + n));
            c += n;
        }
        return Set { galley: g, disp: None, rows, rtl: false };
    }
    complex(fonts, job, o, bidi, hyph, rtl)
}

/// Section index of every char of a job.
fn sections_of(job: &LayoutJob) -> Vec<u32> {
    let mut v = Vec::with_capacity(job.text.len());
    for (si, s) in job.sections.iter().enumerate() {
        let n = job.text[s.byte_range.clone()].chars().count();
        v.extend(std::iter::repeat_n(si as u32, n));
    }
    v
}

/// Build a job from display chars, each taking the format of a logical char.
fn rebuild(job: &LayoutJob, chars: &[(char, usize)], sec_of: &[u32], lead: f32) -> LayoutJob {
    let mut nj = LayoutJob { text: String::new(), sections: Vec::new(), ..job.clone() };
    let mut buf = String::new();
    let mut cur: Option<u32> = None;
    let mut first = true;
    let last_sec = sec_of.last().copied().unwrap_or(0);
    for &(c, l) in chars {
        let s = sec_of.get(l).copied().unwrap_or(last_sec);
        if cur != Some(s) {
            if let Some(cs) = cur {
                nj.append(&buf, if first { lead } else { 0.0 }, job.sections[cs as usize].format.clone());
                first = false;
                buf.clear();
            }
            cur = Some(s);
        }
        buf.push(c);
    }
    if let Some(cs) = cur {
        nj.append(&buf, if first { lead } else { 0.0 }, job.sections[cs as usize].format.clone());
    }
    nj
}

fn is_space(c: char) -> bool {
    c.is_whitespace() && c != '\u{a0}' && c != '\u{202f}'
}

fn no_line_start(c: char) -> bool {
    ")]｝〕〉》」』】〙〗〟'\"｠»ヽヾーァィゥェォッャュョヮヵヶぁぃぅぇぉっゃゅょゎゕゖ々〻‐゠–〜?!‼⁇⁈⁉・、:;,。.）！？：；，".contains(c)
}

fn no_line_end(c: char) -> bool {
    "([｛〔〈《「『【〘〖〝｟«（".contains(c)
}

fn complex(fonts: &Fonts, job: LayoutJob, o: &Opts, bidi: bool, hyph: Option<hypher::Lang>, rtl: bool) -> Set {
    let w = o.width;
    let logical: Vec<char> = job.text.chars().collect();
    let nl = logical.len();
    let sec_of = sections_of(&job);
    let lead = job.sections.first().map(|s| s.leading_space).unwrap_or(0.0);
    // 1. shaping
    let shaped: Vec<(char, usize)> = if logical.iter().any(|&c| script::is_arabic_joining(c)) { script::shape_arabic(&logical) } else { logical.iter().copied().enumerate().map(|(i, c)| (c, i)).collect() };
    let n = shaped.len();
    let log_at = |s: usize| if s < n { shaped[s].1 } else { nl };
    // 2. measure unwrapped
    let mut mj = rebuild(&job, &shaped, &sec_of, if rtl { 0.0 } else { lead });
    mj.wrap = TextWrapping { max_width: f32::INFINITY, ..Default::default() };
    mj.halign = EAlign::LEFT;
    let g1 = fonts.layout_job(mj);
    let mut xs = vec![0.0f32; n + 1];
    let mut xe = vec![0.0f32; n + 1];
    let mut hard: Vec<(usize, usize)> = Vec::new(); // [a, b) without the newline
    let mut i = 0;
    for row in &g1.rows {
        let a = i;
        for gl in &row.glyphs {
            if i < n {
                xs[i] = gl.pos.x;
                xe[i] = gl.max_x();
            }
            i += 1;
        }
        hard.push((a, i));
        if row.ends_with_newline {
            i += 1;
        }
    }
    let ch = |k: usize| shaped[k].0;
    let width = |a: usize, ls: usize, e: usize| -> f32 {
        let mut e = e;
        while e > ls && is_space(ch(e - 1)) {
            e -= 1;
        }
        if e <= ls {
            0.0
        } else {
            xe[e - 1] - if ls == a { 0.0 } else { xs[ls] }
        }
    };
    let font_of = |k: usize| job.sections[sec_of.get(log_at(k)).copied().unwrap_or(0) as usize].format.font_id.clone();
    // 3. line breaking: (start, end, soft break, hyphen)
    let mut lines: Vec<(usize, usize, bool, bool)> = Vec::new();
    for &(a, b) in &hard {
        let mut opps = Vec::new();
        for p in a + 1..b {
            let (pc, c) = (ch(p - 1), ch(p));
            let cjk = (script::is_cjk(pc) || script::is_cjk(c)) && !no_line_start(c) && !no_line_end(pc) && !is_space(c);
            let dash = matches!(pc, '-' | '–' | '—' | '/') && c.is_alphanumeric() && p >= a + 2 && ch(p - 2).is_alphanumeric();
            if (is_space(pc) && !is_space(c)) || cjk || dash {
                opps.push(p);
            }
        }
        opps.push(b);
        let mut ls = a;
        let mut last: Option<usize> = None;
        let mut k = 0;
        while k < opps.len() {
            let p = opps[k];
            if width(a, ls, p) <= w + 0.5 {
                last = Some(p);
                k += 1;
                continue;
            }
            let ws = last.unwrap_or(ls);
            if let Some(h) = hyph.and_then(|lang| hyph_point(&shaped, ws, p, lang, |h| width(a, ls, h) + fonts.glyph_width(&font_of(h - 1), '-') <= w + 0.5)) {
                if h > ls {
                    lines.push((ls, h, true, true));
                    ls = h;
                    last = None;
                    continue;
                }
            }
            if let Some(o) = last {
                if o > ls {
                    lines.push((ls, o, true, false));
                    ls = o;
                    last = None;
                    continue;
                }
            }
            // a single unbreakable run wider than the measure: break between chars
            let mut q = ls + 1;
            while q + 1 < p && width(a, ls, q + 1) <= w + 0.5 {
                q += 1;
            }
            lines.push((ls, q, true, false));
            ls = q;
            last = None;
        }
        lines.push((ls, b, false, false));
    }
    // hard line ends own their newline char
    let newline_after: Vec<bool> = {
        let mut v = vec![false; lines.len()];
        let mut li = 0;
        for (hi, &(_, b)) in hard.iter().enumerate() {
            while li < lines.len() && lines[li].1 < b {
                li += 1;
            }
            if li < lines.len() && hi + 1 < hard.len() {
                v[li] = true;
            }
        }
        v
    };
    // 4. bidi levels on the shaped text
    let levels: Vec<u8> = if bidi {
        let s: String = shaped.iter().map(|c| c.0).collect();
        let lvl = o.rtl.map(|r| if r { unicode_bidi::Level::rtl() } else { unicode_bidi::Level::ltr() });
        let info = unicode_bidi::BidiInfo::new(&s, lvl);
        s.char_indices().map(|(b, _)| info.levels[b].number()).collect()
    } else {
        Vec::new()
    };
    let para_level = if rtl { 1u8 } else { 0 };
    // 5. display chars
    let mut disp: Vec<(char, usize)> = Vec::with_capacity(n + lines.len() * 2);
    let mut drtl: Vec<bool> = Vec::with_capacity(n + lines.len() * 2);
    let mut rows = Vec::with_capacity(lines.len());
    let mut justify_row = Vec::with_capacity(lines.len());
    for (li, &(s0, s1, soft, hy)) in lines.iter().enumerate() {
        let mut e = s1;
        while e > s0 && is_space(ch(e - 1)) {
            e -= 1;
        }
        let mut order: Vec<usize> = (s0..e).collect();
        let lv: Vec<u8> = if bidi { (s0..e).map(|k| levels[k]).collect() } else { vec![0; e - s0] };
        if bidi {
            let max = lv.iter().copied().max().unwrap_or(0);
            let min_odd = lv.iter().copied().filter(|l| l % 2 == 1).min();
            if let Some(min_odd) = min_odd {
                for lev in (min_odd..=max).rev() {
                    let mut j = 0;
                    while j < order.len() {
                        if lv[order[j] - s0] >= lev {
                            let st = j;
                            while j < order.len() && lv[order[j] - s0] >= lev {
                                j += 1;
                            }
                            order[st..j].reverse();
                        } else {
                            j += 1;
                        }
                    }
                }
            }
        }
        let trail: Vec<(char, usize)> = (e..s1).map(|k| (ch(k), log_at(k))).collect();
        if para_level == 1 {
            for &t in trail.iter().rev() {
                disp.push(t);
                drtl.push(true);
            }
        }
        for &k in &order {
            let odd = bidi && lv[k - s0] % 2 == 1;
            disp.push((if odd { script::mirror(ch(k)) } else { ch(k) }, log_at(k)));
            drtl.push(odd);
        }
        if para_level == 0 {
            for &t in &trail {
                disp.push(t);
                drtl.push(false);
            }
        }
        if hy {
            disp.push(('-', log_at(s1)));
            drtl.push(false);
        }
        let last = li + 1 == lines.len();
        let (l0, mut l1) = (log_at(s0), log_at(s1));
        if newline_after[li] {
            l1 = log_at(s1) + 1; // the logical '\n' after this hard line
        }
        if !last {
            disp.push(('\n', log_at(s1)));
            drtl.push(false);
        } else {
            l1 = nl;
        }
        rows.push((l0, l1.max(l0)));
        justify_row.push(soft && !last);
    }
    let mut dj = rebuild(&job, &disp, &sec_of, if rtl { 0.0 } else { lead });
    dj.wrap = TextWrapping { max_width: f32::INFINITY, ..Default::default() };
    let g = fonts.layout_job(dj.clone());
    let g = if o.justify { justify_rows(fonts, dj, g, w, &|ri| justify_row.get(ri).copied().unwrap_or(false)) } else { g };
    let mut v2l: Vec<u32> = disp.iter().map(|d| d.1 as u32).collect();
    v2l.push(nl as u32);
    let mut l2v = vec![u32::MAX; nl + 1];
    for (v, d) in disp.iter().enumerate() {
        if d.0 != '\n' || logical.get(d.1) == Some(&'\n') {
            let slot = &mut l2v[d.1.min(nl)];
            if *slot == u32::MAX {
                *slot = v as u32;
            }
        }
    }
    l2v[nl] = disp.len() as u32;
    // logical chars without a glyph of their own (ligature tails): the previous one's
    for i in 1..=nl {
        if l2v[i] == u32::MAX {
            l2v[i] = l2v[i - 1];
        }
    }
    if l2v[0] == u32::MAX {
        l2v[0] = 0;
    }
    drtl.push(false);
    // the galley must have one row per line for `rows` to line up
    debug_assert_eq!(g.rows.len(), rows.len());
    Set { galley: g, disp: Some(Arc::new(Disp { v2l, l2v, rtl: drtl })), rows, rtl }
}

/// The latest hyphenation point in the word at [ws, p) that `fits`.
fn hyph_point(s: &[(char, usize)], ws: usize, p: usize, lang: hypher::Lang, fits: impl Fn(usize) -> bool) -> Option<usize> {
    let mut e = p;
    while e > ws && !s[e - 1].0.is_alphabetic() {
        e -= 1;
    }
    let mut b = ws;
    while b < e && !s[b].0.is_alphabetic() {
        b += 1;
    }
    if e < b + 5 || (b..e).any(|k| !s[k].0.is_alphabetic()) {
        return None;
    }
    let word: String = s[b..e].iter().map(|c| c.0).collect();
    let mut cuts = Vec::new();
    let mut at = b;
    let syl: Vec<&str> = hypher::hyphenate(&word, lang).collect();
    for sy in syl.iter().take(syl.len().saturating_sub(1)) {
        at += sy.chars().count();
        cuts.push(at);
    }
    cuts.into_iter().rev().find(|&h| fits(h))
}

/// egui-wrapped paragraph: justify all rows but the last of each paragraph.
fn justify_wrapped(fonts: &Fonts, job: LayoutJob, g: Arc<Galley>) -> Arc<Galley> {
    let w = job.wrap.max_width;
    let n = g.rows.len();
    let ends: Vec<bool> = g.rows.iter().map(|r| r.ends_with_newline).collect();
    justify_rows(fonts, job, g, w, &|ri| !(ri + 1 == n || ends[ri]))
}

/// Re-layout so every chosen row fills the measure, with the slack spread over
/// that row's inter-word spaces.
pub fn justify_rows(fonts: &Fonts, job: LayoutJob, g: Arc<Galley>, w: f32, pick: &dyn Fn(usize) -> bool) -> Arc<Galley> {
    if g.rows.len() < 2 {
        return g;
    }
    let left = job.halign == EAlign::LEFT;
    let mut inserts: Vec<(usize, f32)> = Vec::new();
    let mut start = 0usize;
    for (ri, row) in g.rows.iter().enumerate() {
        let cc = row.char_count_including_newline();
        if pick(ri) {
            let glyphs = &row.glyphs;
            if let Some(end) = glyphs.iter().rposition(|gl| !gl.chr.is_whitespace()) {
                let first = glyphs.iter().position(|gl| !gl.chr.is_whitespace()).unwrap_or(0);
                let spaces: Vec<usize> = (first..end).filter(|&j| glyphs[j].chr == ' ' && glyphs[j + 1].chr != ' ').collect();
                let x0 = if left { 0.0 } else { glyphs.first().map(|g| g.pos.x).unwrap_or(0.0) };
                let slack = w - (glyphs[end].max_x() - x0) - 0.75;
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

/// Galley-local rects covering logical chars [c0, c1).
pub fn range_rects(g: &Galley, disp: Option<&Disp>, c0: usize, c1: usize) -> Vec<Rect> {
    let Some(d) = disp else { return super::range_rects(g, c0, c1) };
    let mut out = Vec::new();
    if c1 <= c0 {
        return out;
    }
    let mut v = 0usize;
    for row in &g.rows {
        let mut run: Option<(f32, f32)> = None;
        for (j, gl) in row.glyphs.iter().enumerate() {
            let l = d.v2l.get(v + j).copied().unwrap_or(u32::MAX) as usize;
            if l >= c0 && l < c1 {
                run = Some(match run {
                    Some((a, _)) => (a, gl.max_x()),
                    None => (gl.pos.x, gl.max_x()),
                });
            } else if let Some((a, b)) = run.take() {
                out.push(Rect::from_min_max(pos2(a, row.min_y()), pos2(b, row.max_y())));
            }
        }
        if let Some((a, b)) = run {
            out.push(Rect::from_min_max(pos2(a, row.min_y()), pos2(b, row.max_y())));
        }
        v += row.char_count_including_newline();
    }
    out
}

/// Logical char offset at a galley-local point.
pub fn hit(g: &Galley, disp: Option<&Disp>, rows: &[(usize, usize)], p: Vec2) -> usize {
    let Some(d) = disp else { return g.cursor_from_pos(p).ccursor.index };
    // nearest row by y
    let mut ri = 0;
    let mut best = f32::INFINITY;
    for (i, r) in g.rows.iter().enumerate() {
        let dy = if p.y < r.min_y() { r.min_y() - p.y } else if p.y > r.max_y() { p.y - r.max_y() } else { 0.0 };
        if dy < best {
            best = dy;
            ri = i;
        }
    }
    let v0: usize = g.rows.iter().take(ri).map(|r| r.char_count_including_newline()).sum();
    let row = &g.rows[ri];
    let (rl0, rl1) = rows.get(ri).copied().unwrap_or((0, 0));
    if row.glyphs.is_empty() {
        return rl0;
    }
    let mut j = row.glyphs.iter().position(|gl| p.x < gl.max_x()).unwrap_or(row.glyphs.len() - 1);
    // skip the display-only line-end newline
    if row.glyphs[j].chr == '\n' && j > 0 {
        j -= 1;
    }
    let gl = &row.glyphs[j];
    let l = d.v2l.get(v0 + j).copied().unwrap_or(0) as usize;
    let mut after = p.x > gl.pos.x + gl.advance_width / 2.0;
    if d.rtl.get(v0 + j).copied().unwrap_or(false) {
        after = !after;
    }
    let l = if after { l + 1 } else { l };
    // stay on this line (the last line's end is the block end)
    let hi = if ri + 1 == g.rows.len() { rl1 } else { rl1.saturating_sub(1).max(rl0) };
    l.clamp(rl0, hi.max(rl0))
}

/// Galley-local rect of a logical char.
pub fn char_rect(g: &Galley, disp: Option<&Disp>, offset: usize) -> Rect {
    let v = match disp {
        Some(d) => d.l2v.get(offset).copied().unwrap_or(0) as usize,
        None => offset,
    };
    g.pos_from_ccursor(egui::text::CCursor::new(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, FontId, TextFormat};

    fn with_fonts<R>(f: impl FnOnce(&Fonts) -> R) -> R {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        ctx.fonts(f)
    }

    fn job(text: &str, width: f32) -> LayoutJob {
        let mut j = LayoutJob::default();
        j.append(text, 0.0, TextFormat::simple(FontId::proportional(16.0), Color32::BLACK));
        j.wrap = TextWrapping { max_width: width, ..Default::default() };
        j
    }

    fn check_rows(set: &Set, n: usize) {
        assert_eq!(set.rows.first().map(|r| r.0), Some(0));
        assert_eq!(set.rows.last().map(|r| r.1), Some(n));
        for w in set.rows.windows(2) {
            assert_eq!(w[0].1, w[1].0, "rows contiguous: {:?}", set.rows);
        }
        assert_eq!(set.rows.len(), set.galley.rows.len());
    }

    #[test]
    fn hyphenates_without_touching_text() {
        let text = "Extraordinarily incomprehensible characteristically internationalization representative understanding";
        with_fonts(|f| {
            let set = set(f, job(text, 170.0), &Opts { width: 170.0, justify: true, hyph: hypher::Lang::from_iso(*b"en"), rtl: None });
            let shown = set.galley.text();
            assert!(shown.contains("-\n"), "hyphen at a line end: {shown:?}");
            let d = set.disp.as_ref().unwrap();
            // every display char maps back to the logical char it shows
            let logical: Vec<char> = text.chars().collect();
            for (v, c) in shown.chars().enumerate() {
                if c != '-' && c != '\n' {
                    assert_eq!(logical[d.v2l[v] as usize], c);
                }
            }
            check_rows(&set, logical.len());
            let n = set.galley.rows.len();
            for (i, r) in set.galley.rows.iter().enumerate() {
                let right = r.glyphs.iter().rev().find(|g| !g.chr.is_whitespace()).map(|g| g.max_x()).unwrap_or(0.0);
                assert!(right <= 171.0, "row fits: {right}");
                if i + 1 < n && r.glyphs.iter().filter(|g| g.chr == ' ').count() > 0 {
                    assert!(right > 165.0, "row {i} justified: {right} {shown:?}");
                }
            }
        });
    }

    #[test]
    fn rtl_line_is_reversed_and_hit_maps_back() {
        let text = "שלום עולם (abc) סוף";
        with_fonts(|f| {
            let mut j = job(text, 1000.0);
            j.halign = EAlign::RIGHT;
            let set = set(f, j, &Opts { width: 1000.0, justify: false, hyph: None, rtl: None });
            assert!(set.rtl);
            let shown = set.galley.text();
            // visual order: the last logical word comes first (leftmost); latin stays ltr
            assert!(shown.starts_with("ףוס"), "{shown:?}");
            assert!(shown.contains("(abc)"), "{shown:?}");
            check_rows(&set, text.chars().count());
            // the rightmost glyph is logical char 0
            let row = &set.galley.rows[0];
            let last = row.glyphs.last().unwrap();
            let off = hit(&set.galley, set.disp.as_deref(), &set.rows, egui::vec2(last.pos.x + last.advance_width * 0.8, row.min_y() + 2.0));
            assert_eq!(off, 0);
            let rects = range_rects(&set.galley, set.disp.as_deref(), 0, 4);
            assert_eq!(rects.len(), 1);
            assert!((rects[0].right() - last.max_x()).abs() < 0.01);
        });
    }

    #[test]
    fn arabic_is_shaped_for_display_only() {
        let text = "سلام عليكم";
        with_fonts(|f| {
            let set = set(f, job(text, 1000.0), &Opts { width: 1000.0, justify: false, hyph: None, rtl: None });
            let shown = set.galley.text();
            assert!(shown.chars().all(|c| c == ' ' || (0xFB50..=0xFEFF).contains(&(c as u32))), "{shown:?}");
            check_rows(&set, text.chars().count());
        });
    }

    #[test]
    fn plain_text_takes_fast_path() {
        with_fonts(|f| {
            let set = set(f, job("plain words here", 60.0), &Opts { width: 60.0, justify: true, hyph: None, rtl: None });
            assert!(set.disp.is_none());
            assert_eq!(set.galley.text(), "plain words here");
        });
    }
}
