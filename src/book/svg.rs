//! SVG images: rasterised with resvg (shapes, gradients, paths; no SVG text
//! and no embedded raster images, to keep the binary small) into PNG bytes,
//! so the normal image pipeline decodes them.
//!
//! Inline `<svg>` elements are captured by the HTML converter into a small
//! registry and adopted into the book's resources by `book::open`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{BlockKind, Flow, ResRef};

/// SVGs are rendered at this multiple of their CSS size; layout divides the
/// reported size back so they appear at their intended size, but crisp.
pub const SCALE: f32 = 2.0;
const MAX_DIM: f32 = 2400.0;

/// Resource keys of inline SVGs start with this.
pub const INLINE_PREFIX: &str = "svg-inline:";

static INLINE: Mutex<Option<HashMap<String, Arc<Vec<u8>>>>> = Mutex::new(None);

/// Whether a resource key names an SVG (layout uses it to undo `SCALE`).
pub fn is_svg_key(key: &str) -> bool {
    key.starts_with(INLINE_PREFIX) || {
        let k = key.to_ascii_lowercase();
        let k = k.split(['?', '#']).next().unwrap_or("");
        k.ends_with(".svg") || k.ends_with(".svgz") || k.starts_with("data:image/svg")
    }
}

fn looks_like_svg(b: &[u8]) -> bool {
    if b.starts_with(&[0x1f, 0x8b]) {
        return false; // svgz is rare in books; skip rather than guess
    }
    let head = &b[..b.len().min(2048)];
    let head = String::from_utf8_lossy(head);
    let t = head.trim_start_matches('\u{feff}').trim_start();
    (t.starts_with("<?xml") || t.starts_with("<!--") || t.starts_with("<!DOCTYPE") || t.starts_with("<svg")) && head.contains("<svg")
}

/// Rasterise SVG bytes to PNG; anything else is returned unchanged.
pub fn rasterize_if_svg(bytes: Vec<u8>) -> Vec<u8> {
    if !looks_like_svg(&bytes) {
        return bytes;
    }
    rasterize(&bytes).unwrap_or(bytes)
}

pub fn rasterize(data: &[u8]) -> Option<Vec<u8>> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(data, &opt).ok()?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let s = SCALE.min(MAX_DIM / w.max(h));
    let (pw, ph) = ((w * s).ceil().max(1.0) as u32, (h * s).ceil().max(1.0) as u32);
    let mut pm = resvg::tiny_skia::Pixmap::new(pw, ph)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    let mut rgba = Vec::with_capacity((pw * ph * 4) as usize);
    for p in pm.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    let img = image::RgbaImage::from_raw(pw, ph, rgba)?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Register an inline `<svg>` element's source; returns its resource key.
pub fn register_inline(src: &str) -> String {
    let mut s = src.to_string();
    // HTML (not XHTML) documents may omit the namespaces usvg needs
    if let Some(open_end) = s.find('>') {
        let open = &s[..open_end];
        let mut add = String::new();
        if !open.contains("xmlns=") && !open.contains("xmlns:svg") {
            add.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
        }
        if s.contains("xlink:") && !open.contains("xmlns:xlink") {
            add.push_str(" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
        }
        if !add.is_empty() {
            let at = if open.ends_with('/') { open_end - 1 } else { open_end };
            let name_end = s[1..].find(|c: char| c.is_whitespace() || c == '>' || c == '/').map(|e| e + 1).unwrap_or(at);
            s.insert_str(name_end.min(at), &add);
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&s, &mut h);
    let key = format!("{INLINE_PREFIX}{:016x}.svg", std::hash::Hasher::finish(&h));
    let mut g = INLINE.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if m.len() > 4096 {
        m.clear(); // never-adopted leftovers (conversions outside book::open)
    }
    m.insert(key.clone(), Arc::new(s.into_bytes()));
    key
}

/// Move the inline SVGs this book references into its resources.
pub fn adopt_inline(f: &mut Flow) {
    let mut g = INLINE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = g.as_mut() else { return };
    if m.is_empty() {
        return;
    }
    for s in &f.sections {
        for b in &s.blocks {
            if let BlockKind::Image(k) = &b.kind {
                if k.starts_with(INLINE_PREFIX) {
                    if let Some(d) = m.remove(k) {
                        f.resources.insert(k.clone(), ResRef::Bytes(d));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_circle() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><circle cx="5" cy="5" r="4" fill="red"/></svg>"#;
        let png = super::rasterize(svg).expect("rendered");
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (40, 20));
    }

    #[test]
    fn inline_gets_namespace() {
        let k = super::register_inline("<svg width=\"4\" height=\"4\"><rect width=\"4\" height=\"4\"/></svg>");
        let g = super::INLINE.lock().unwrap();
        let d = g.as_ref().unwrap().get(&k).unwrap();
        assert!(super::rasterize(d).is_some());
    }
}
