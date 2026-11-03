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

struct TextDevice {
    out: String,
    last: Option<(f64, f64)>,
}

impl<'a> Device<'a> for TextDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for g in run.glyphs() {
            let t = props.transform * g.transform();
            let p = t.translation();
            if let Some((lx, ly)) = self.last {
                let size = t.as_coeffs()[3].abs().max(1.0);
                let dy = (p.y - ly).abs();
                if dy > size * 0.6 {
                    self.out.push('\n');
                    // a clearly larger gap marks a paragraph break (used by the reflow view)
                    if dy > size * 1.9 {
                        self.out.push('\n');
                    }
                } else if p.x - lx > size * 0.25 && !self.out.ends_with(' ') {
                    self.out.push(' ');
                }
            }
            match g.as_unicode() {
                Some(hayro::hayro_interpret::hayro_cmap::BfString::Char(c)) => self.out.push(c),
                Some(hayro::hayro_interpret::hayro_cmap::BfString::String(s)) => self.out.push_str(&s),
                None => {}
            }
            let adv = 0.5 * t.as_coeffs()[0].abs();
            self.last = Some((p.x + adv, p.y));
        }
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

pub fn page_text(pdf: &Pdf, idx: usize) -> String {
    let pages = pdf.pages();
    let Some(page) = pages.get(idx) else { return String::new() };
    let cache = InterpreterCache::new();
    let mut ctx = Context::new(page.initial_transform(true).to_kurbo(), hayro::kurbo::Rect::new(0.0, 0.0, 1e6, 1e6), &cache, page.xref(), InterpreterSettings::default());
    let mut dev = TextDevice { out: String::new(), last: None };
    interpret_page(page, &mut ctx, &mut dev);
    dev.out
}
