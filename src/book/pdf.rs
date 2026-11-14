//! PDF via hayro (pure Rust): metadata, outline, page rendering and text
//! extraction for search. No external tools, works on every platform.

use std::path::Path;

use hayro::hayro_interpret::font::GlyphRun;
use hayro::hayro_interpret::hayro_syntax::object::{Array, Dict, Object};
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterCache, InterpreterSettings,
    SoftMask, TransformExt,
};
use hayro::kurbo::BezPath;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

use super::{Book, Content, Fixed, FixedKind, Loc, Meta, TocEntry};

pub fn load(path: &Path) -> Result<Pdf, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    Pdf::new(data).map_err(|e| format!("cannot open PDF: {e:?}"))
}

fn pdf_string(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&b[3..]).into_owned();
    }
    super::util::decode_cp1252(b)
}

fn meta_of(pdf: &Pdf) -> Meta {
    let m = pdf.metadata();
    let s = |v: &Option<Box<[u8]>>| v.as_ref().map(|b| pdf_string(b)).unwrap_or_default().trim().to_string();
    let mut title = s(&m.title);
    // Many PDFs carry junk titles like "1_Prelims.pdf" or "Microsoft Word - x.doc"
    let tl = title.to_lowercase();
    if tl.ends_with(".pdf") || tl.ends_with(".doc") || tl.ends_with(".docx") || tl.starts_with("microsoft word") || tl == "untitled" {
        title.clear();
    }
    Meta { title, author: s(&m.author), description: s(&m.subject), ..Default::default() }
}

pub fn read_meta(path: &Path) -> Result<Meta, String> {
    let pdf = load(path)?;
    let mut m = meta_of(&pdf);
    m.cover = render_png(&pdf, 0, 360);
    Ok(m)
}

pub fn open(path: &Path) -> Result<Book, String> {
    let pdf = load(path)?;
    let pages = pdf.pages().len();
    if pages == 0 {
        return Err("PDF has no pages".into());
    }
    let meta = meta_of(&pdf);
    let toc = outline(&pdf);
    Ok(Book { meta, toc, content: Content::Fixed(Fixed { kind: FixedKind::Pdf, path: path.to_path_buf(), pages }) })
}

/// Render page `idx` scaled to `width` px wide: (w, h, premultiplied RGBA).
pub fn render(pdf: &Pdf, idx: usize, width: u32) -> Option<(u32, u32, Vec<u8>)> {
    let pages = pdf.pages();
    let page = pages.get(idx)?;
    let (w, _h) = page.render_dimensions();
    if w <= 0.0 {
        return None;
    }
    let scale = (width as f32 / w).clamp(0.05, 8.0);
    let cache = RenderCache::new();
    let pix = hayro::render(
        page,
        &cache,
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: hayro::vello_cpu::color::palette::css::WHITE },
    );
    let (pw, ph) = (pix.width() as u32, pix.height() as u32);
    Some((pw, ph, pix.data_as_u8_slice().to_vec()))
}

fn render_png(pdf: &Pdf, idx: usize, width: u32) -> Option<Vec<u8>> {
    let (w, h, rgba) = render(pdf, idx, width)?;
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img).to_rgb8().write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Page aspect ratios (h / w) for layout before rendering.
pub fn page_aspects(pdf: &Pdf) -> Vec<f32> {
    pdf.pages()
        .iter()
        .map(|p| {
            let (w, h) = p.render_dimensions();
            if w > 0.0 {
                h / w
            } else {
                1.414
            }
        })
        .collect()
}

// ---------- outline ----------

fn outline(pdf: &Pdf) -> Vec<TocEntry> {
    let xref = pdf.xref();
    let Some(root) = xref.get::<Dict>(xref.root_id()) else { return Vec::new() };
    let Some(outlines) = root.get::<Dict>(b"Outlines") else { return Vec::new() };
    let page_ids: Vec<Option<(u32, i32)>> = pdf.pages().iter().map(|p| p.raw().obj_id().map(|i| (i.obj_number, i.gen_number))).collect();
    let dests = root.get::<Dict>(b"Dests");
    let names_dests = root.get::<Dict>(b"Names").and_then(|n| n.get::<Dict>(b"Dests"));
    let mut out = Vec::new();
    let mut guard = 0;
    walk(&outlines, 0, &page_ids, dests.as_ref(), names_dests.as_ref(), &mut out, &mut guard);
    out
}

fn page_of_dest(arr: &Array, page_ids: &[Option<(u32, i32)>]) -> Option<usize> {
    let first = arr.raw_iter().next()?;
    if let Some(r) = first.as_obj_ref() {
        return page_ids.iter().position(|p| *p == Some((r.obj_number, r.gen_number)));
    }
    // Some producers use a page number instead of a reference.
    arr.iter::<i32>().next().map(|n| n.max(0) as usize)
}

fn lookup_name_tree<'a>(tree: &Dict<'a>, name: &[u8], depth: usize) -> Option<Object<'a>> {
    if depth > 20 {
        return None;
    }
    if let Some(names) = tree.get::<Array>(b"Names") {
        let mut it = names.iter::<Object>();
        while let (Some(k), Some(v)) = (it.next(), it.next()) {
            if let Some(s) = k.clone().into_string() {
                if s.as_bytes() == name {
                    return Some(v);
                }
            }
        }
    }
    if let Some(kids) = tree.get::<Array>(b"Kids") {
        for kid in kids.iter::<Dict>() {
            if let Some(v) = lookup_name_tree(&kid, name, depth + 1) {
                return Some(v);
            }
        }
    }
    None
}

fn resolve_dest(obj: Object, page_ids: &[Option<(u32, i32)>], dests: Option<&Dict>, names: Option<&Dict>) -> Option<usize> {
    match obj {
        Object::Array(a) => page_of_dest(&a, page_ids),
        Object::Name(n) => {
            let d = dests?;
            let v = d.get::<Object>(n.as_ref())?;
            match v {
                Object::Array(a) => page_of_dest(&a, page_ids),
                Object::Dict(dd) => page_of_dest(&dd.get::<Array>(b"D")?, page_ids),
                _ => None,
            }
        }
        Object::String(s) => {
            let v = lookup_name_tree(names?, s.as_bytes(), 0)?;
            match v {
                Object::Array(a) => page_of_dest(&a, page_ids),
                Object::Dict(dd) => page_of_dest(&dd.get::<Array>(b"D")?, page_ids),
                _ => None,
            }
        }
        _ => None,
    }
}

fn walk(node: &Dict, depth: usize, page_ids: &[Option<(u32, i32)>], dests: Option<&Dict>, names: Option<&Dict>, out: &mut Vec<TocEntry>, guard: &mut usize) {
    let mut cur = node.get::<Dict>(b"First");
    while let Some(item) = cur {
        *guard += 1;
        if *guard > 5000 {
            return;
        }
        let title = item.get::<hayro::hayro_interpret::hayro_syntax::object::String>(b"Title").map(|s| pdf_string(s.as_bytes())).unwrap_or_default();
        let dest = item
            .get::<Object>(b"Dest")
            .or_else(|| item.get::<Dict>(b"A").and_then(|a| a.get::<Object>(b"D")))
            .and_then(|d| resolve_dest(d, page_ids, dests, names));
        if let Some(page) = dest {
            let t = title.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                out.push(TocEntry { title: t, depth, loc: Loc::new(page, 0, 0) });
            }
        }
        walk(&item, depth + 1, page_ids, dests, names, out, guard);
        cur = item.get::<Dict>(b"Next");
    }
}

// ---------- text extraction ----------

/// A page's text with where each character sits, for selection and
/// highlights. `boxes[i]` is the box of `chars[i]` as page fractions
/// (l, t, r, b); spaces and line breaks the extractor inserted have none.
#[derive(Default)]
pub struct PageText {
    pub chars: Vec<char>,
    pub boxes: Vec<Option<[f32; 4]>>,
}

impl PageText {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }
}

struct TextDevice {
    out: PageText,
    /// Pen position after the previous glyph and its font size (points).
    last: Option<(f64, f64, f64)>,
    page_w: f64,
    page_h: f64,
}

impl TextDevice {
    fn push(&mut self, c: char, b: Option<[f32; 4]>) {
        self.out.chars.push(c);
        self.out.boxes.push(b);
    }
}

impl<'a> Device<'a> for TextDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for g in run.glyphs() {
            // glyph space is 1/1000 em, so the transform's scale is size / 1000
            let t = props.transform * g.transform();
            let [_, _, c, d, _, _] = t.as_coeffs();
            let size = ((c * c + d * d).sqrt() * 1000.0).max(1.0);
            let p = t.translation();
            if let Some((lx, ly, lsize)) = self.last {
                let em = size.max(lsize);
                let dy = (p.y - ly).abs();
                if dy > em * 0.6 {
                    self.push('\n', None);
                    // a clearly larger gap marks a paragraph break (used by the reflow view)
                    if dy > em * 1.9 {
                        self.push('\n', None);
                    }
                } else if (p.x - lx > em * 0.15 || p.x < lx - em * 2.0) && !self.out.chars.last().is_some_and(|c| c.is_whitespace()) {
                    self.push(' ', None);
                }
            }
            let adv = match &**g {
                hayro::hayro_interpret::font::Glyph::Outline(o) => o.advance_width(),
                hayro::hayro_interpret::font::Glyph::Type3(t3) => t3.advance_width(),
            }
            .filter(|w| *w > 0.0)
            .unwrap_or(500.0) as f64;
            // corners of the glyph cell (advance × descender..ascender) in page space
            let pts = [(0.0, -200.0), (adv, -200.0), (0.0, 800.0), (adv, 800.0)].map(|(x, y)| t * hayro::kurbo::Point::new(x, y));
            let (mut l, mut tp, mut r, mut bt) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for q in pts {
                l = l.min(q.x);
                r = r.max(q.x);
                tp = tp.min(q.y);
                bt = bt.max(q.y);
            }
            let bx = [(l / self.page_w) as f32, (tp / self.page_h) as f32, (r / self.page_w) as f32, (bt / self.page_h) as f32];
            let text: Vec<char> = match g.as_unicode() {
                Some(hayro::hayro_interpret::hayro_cmap::BfString::Char(c)) => vec![c],
                Some(hayro::hayro_interpret::hayro_cmap::BfString::String(s)) => s.chars().collect(),
                None => Vec::new(),
            };
            // ligatures: share the cell between their letters
            let n = text.len().max(1) as f32;
            for (k, ch) in text.into_iter().enumerate() {
                let w = (bx[2] - bx[0]) / n;
                let x0 = bx[0] + w * k as f32;
                self.push(ch, Some([x0, bx[1], x0 + w, bx[3]]));
            }
            let end = t * hayro::kurbo::Point::new(adv, 0.0);
            self.last = Some((end.x, p.y, size));
        }
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

/// Text of page `idx` with a box per character.
pub fn page_glyphs(pdf: &Pdf, idx: usize) -> PageText {
    let pages = pdf.pages();
    let Some(page) = pages.get(idx) else { return PageText::default() };
    let (w, h) = page.render_dimensions();
    let cache = InterpreterCache::new();
    let mut ctx = Context::new(page.initial_transform(true).to_kurbo(), hayro::kurbo::Rect::new(0.0, 0.0, 1e6, 1e6), &cache, page.xref(), InterpreterSettings::default());
    let mut dev = TextDevice { out: PageText::default(), last: None, page_w: (w as f64).max(1.0), page_h: (h as f64).max(1.0) };
    interpret_page(page, &mut ctx, &mut dev);
    dev.out
}

/// Plain text of page `idx`; char offsets match [`page_glyphs`].
pub fn page_text(pdf: &Pdf, idx: usize) -> String {
    page_glyphs(pdf, idx).text()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extraction stats on a real PDF: `PDF=path cargo test pdf_words -- --ignored`.
    #[test]
    #[ignore]
    fn pdf_words() {
        let pdf = load(Path::new(&std::env::var("PDF").unwrap())).unwrap();
        for i in [5, 20, 40] {
            let t = page_glyphs(&pdf, i);
            let s = t.text();
            let words = s.split_whitespace().count().max(1);
            let letters = s.chars().filter(|c| c.is_alphabetic()).count();
            let inside = t.boxes.iter().flatten().filter(|b| b[0] >= -0.01 && b[2] <= 1.01 && b[1] >= -0.05 && b[3] <= 1.05 && b[0] < b[2] && b[1] < b[3]).count();
            eprintln!("page {i}: letters/word {:.1}, boxes ok {}/{}", letters as f32 / words as f32, inside, t.boxes.iter().flatten().count());
            // with OUT=dir, also save the page with every character box outlined
            if let (Ok(dir), Some((w, h, mut px))) = (std::env::var("OUT"), render(&pdf, i, 800)) {
                for b in t.boxes.iter().flatten() {
                    let (x0, y0, x1, y1) = ((b[0] * w as f32) as i64, (b[1] * h as f32) as i64, (b[2] * w as f32) as i64, (b[3] * h as f32) as i64);
                    for x in x0..=x1 {
                        for y in [y0, y1] {
                            if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                                let k = ((y as u32 * w + x as u32) * 4) as usize;
                                px[k..k + 3].copy_from_slice(&[255, 0, 0]);
                            }
                        }
                    }
                }
                let img = image::RgbaImage::from_raw(w, h, px).unwrap();
                img.save(format!("{dir}/page{i}.png")).unwrap();
            }
        }
    }
}
