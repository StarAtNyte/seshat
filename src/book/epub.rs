//! EPUB 2/3: OPF metadata + spine, nav/NCX table of contents, CSS, images, links.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::Path;

use super::css::Sheet;
use super::html::{self, decode_entities, tokenize, Hooks, Options, Tok};
use super::util::{join_path, parent_of, remap, split_long_sections, zip_read};
use super::{Book, Content, Flow, LinkTarget, Loc, Meta, ResRef, Section, TocEntry};

struct Item {
    href: String,
    media_type: String,
    properties: String,
}

struct Opf {
    meta: Meta,
    manifest: HashMap<String, Item>,
    spine: Vec<String>,
    toc_id: String,
    cover_id: String,
    guide_cover: String,
}

type Zip = zip::ZipArchive<std::io::BufReader<std::fs::File>>;

fn open_zip(path: &Path) -> Result<Zip, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("not a valid EPUB (zip): {e}"))
}

fn find_opf<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Result<String, String> {
    if let Ok(c) = zip_read(zip, "META-INF/container.xml") {
        let s = String::from_utf8_lossy(&c).into_owned();
        let mut found = None;
        tokenize(&s, |_, t| {
            if let Tok::Start { name, attrs, .. } = t {
                if name == "rootfile" && found.is_none() {
                    found = attrs.iter().find(|(k, _)| k == "full-path").map(|(_, v)| v.clone());
                }
            }
        });
        if let Some(f) = found {
            return Ok(f);
        }
    }
    zip.file_names().find(|n| n.to_lowercase().ends_with(".opf")).map(|s| s.to_string()).ok_or_else(|| "no OPF package file".into())
}

fn parse_opf(xml: &str) -> Opf {
    let mut o = Opf { meta: Meta::default(), manifest: HashMap::new(), spine: Vec::new(), toc_id: String::new(), cover_id: String::new(), guide_cover: String::new() };
    let mut field: Option<String> = None;
    let mut buf = String::new();
    let mut in_meta = false;
    let mut series_meta = false;
    tokenize(xml, |_, t| match t {
        Tok::Start { name, attrs, self_closing } => {
            let a = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
            match name.as_str() {
                "metadata" => in_meta = true,
                "title" | "creator" | "language" | "description" | "publisher" | "subject" if in_meta && !self_closing => {
                    field = Some(name.clone());
                    buf.clear();
                }
                "meta" if in_meta => {
                    let n = a("name");
                    if n == "cover" {
                        o.cover_id = a("content");
                    } else if n == "calibre:series" {
                        o.meta.series = a("content");
                    } else if a("property") == "belongs-to-collection" && !self_closing {
                        series_meta = true;
                        buf.clear();
                    }
                }
                "item" => {
                    let id = a("id");
                    if !id.is_empty() {
                        o.manifest.insert(id, Item { href: a("href"), media_type: a("media-type"), properties: a("properties") });
                    }
                }
                "spine" => o.toc_id = a("toc"),
                "itemref" => {
                    if a("linear") != "no" || true {
                        o.spine.push(a("idref"));
                    }
                }
                "reference" => {
                    if a("type") == "cover" {
                        o.guide_cover = a("href");
                    }
                }
                _ => {}
            }
        }
        Tok::Text(t) => {
            if field.is_some() || series_meta {
                buf.push_str(&decode_entities(t));
            }
        }
        Tok::Raw(t) => {
            if field.is_some() {
                buf.push_str(t);
            }
        }
        Tok::End { name } => {
            if name == "metadata" {
                in_meta = false;
            }
            if series_meta && name == "meta" {
                if o.meta.series.is_empty() {
                    o.meta.series = buf.trim().to_string();
                }
                series_meta = false;
            }
            if field.as_deref() == Some(name.as_str()) {
                let v = buf.split_whitespace().collect::<Vec<_>>().join(" ");
                let m = &mut o.meta;
                match name.as_str() {
                    "title" if m.title.is_empty() => m.title = v,
                    "creator" => {
                        if m.author.is_empty() {
                            m.author = v
                        } else if !v.is_empty() && m.author.matches(", ").count() < 3 && !m.author.split(", ").any(|a| a == v) {
                            m.author = format!("{}, {}", m.author, v)
                        }
                    }
                    "language" if m.language.is_empty() => m.language = v,
                    "publisher" if m.publisher.is_empty() => m.publisher = v,
                    "description" if m.description.is_empty() => m.description = strip_tags(&v),
                    "subject" if !v.is_empty() && m.subjects.len() < 8 => m.subjects.push(v),
                    _ => {}
                }
                field = None;
            }
        }
    });
    o
}

fn strip_tags(s: &str) -> String {
    let s = decode_entities(s).into_owned();
    let mut out = String::new();
    tokenize(&s, |_, t| {
        if let Tok::Text(t) = t {
            out.push_str(&decode_entities(t));
        }
        if let Tok::Start { name, .. } = t {
            if name == "p" || name == "br" {
                out.push('\n');
            }
        }
    });
    out.trim().to_string()
}

fn cover_bytes(zip: &mut Zip, o: &Opf, opf_dir: &str) -> Option<Vec<u8>> {
    let by_prop = o.manifest.values().find(|m| m.properties.split_whitespace().any(|p| p == "cover-image"));
    let by_meta = o.manifest.get(&o.cover_id).filter(|m| m.media_type.starts_with("image"));
    let by_name = o.manifest.iter().find(|(id, m)| m.media_type.starts_with("image") && (id.to_lowercase().contains("cover") || m.href.to_lowercase().contains("cover"))).map(|(_, m)| m);
    if let Some(m) = by_prop.or(by_meta).or(by_name) {
        if let Ok(b) = zip_read(zip, &join_path(opf_dir, &m.href)) {
            return Some(b);
        }
    }
    // guide cover page → first image in it
    let page = if !o.guide_cover.is_empty() { Some(join_path(opf_dir, &o.guide_cover)) } else { None };
    let page = page.or_else(|| o.spine.first().and_then(|id| o.manifest.get(id)).map(|m| join_path(opf_dir, &m.href)))?;
    let html = String::from_utf8_lossy(&zip_read(zip, &page).ok()?).into_owned();
    let mut src = None;
    tokenize(&html, |_, t| {
        if let Tok::Start { name, attrs, .. } = t {
            if src.is_none() && (name == "img" || name == "image") {
                src = attrs.iter().find(|(k, _)| k == "src" || k.ends_with("href")).map(|(_, v)| v.clone());
            }
        }
    });
    zip_read(zip, &join_path(&parent_of(&page), &src?)).ok()
}

pub fn read_meta(path: &Path) -> Result<Meta, String> {
    let mut zip = open_zip(path)?;
    let opf_path = find_opf(&mut zip)?;
    let opf = String::from_utf8_lossy(&zip_read(&mut zip, &opf_path)?).into_owned();
    let o = parse_opf(&opf);
    let dir = parent_of(&opf_path);
    let cover = cover_bytes(&mut zip, &o, &dir);
    let mut m = o.meta;
    m.cover = cover;
    Ok(m)
}

struct RawLink {
    base: String,
    href: String,
}

pub fn open(path: &Path) -> Result<Book, String> {
    let mut zip = open_zip(path)?;
    if zip.by_name("META-INF/encryption.xml").is_ok() {
        let enc = String::from_utf8_lossy(&zip_read(&mut zip, "META-INF/encryption.xml").unwrap_or_default()).into_owned();
        // Font obfuscation is harmless; anything else means DRM.
        if enc.contains("EncryptedData") && !enc.contains("http://www.idpf.org/2008/embedding") && !enc.contains("http://ns.adobe.com/pdf/enc#RC") {
            return Err("this EPUB is DRM-protected".into());
        }
    }
    let opf_path = find_opf(&mut zip)?;
    let opf = String::from_utf8_lossy(&zip_read(&mut zip, &opf_path)?).into_owned();
    let o = parse_opf(&opf);
    let opf_dir = parent_of(&opf_path);
    let mut meta = o.meta.clone();
    meta.cover = cover_bytes(&mut zip, &o, &opf_dir);

    let mut links: Vec<RawLink> = Vec::new();
    let mut resources: HashMap<String, ResRef> = HashMap::new();
    let mut sheets: HashMap<String, Option<Sheet>> = HashMap::new();
    let mut anchors: HashMap<String, (usize, usize)> = HashMap::new();
    let mut sections: Vec<Section> = Vec::new();
    let mut spine_raw: Vec<usize> = Vec::new();
    let pathbuf = path.to_path_buf();

    for idref in &o.spine {
        spine_raw.push(sections.len());
        let Some(item) = o.manifest.get(idref) else { continue };
        if !(item.media_type.contains("html") || item.media_type.contains("xml") || item.href.ends_with("htm") || item.href.ends_with("html")) {
            continue;
        }
        let file = join_path(&opf_dir, &item.href);
        let Ok(bytes) = zip_read(&mut zip, &file) else { continue };
        let src = super::util::decode_text(&bytes);
        let dir = parent_of(&file);
        let si = sections.len();

        let out = {
            let file_c = file.clone();
            let mut link_fn = |href: &str| {
                links.push(RawLink { base: file_c.clone(), href: href.to_string() });
                (links.len() - 1) as u32
            };
            let mut image_fn = |src: &str| {
                if src.starts_with("data:") {
                    let data = src.split_once(',').map(|(_, d)| super::util::base64_decode(d))?;
                    let key = format!("data:{}", resources.len());
                    resources.insert(key.clone(), ResRef::Bytes(std::sync::Arc::new(data)));
                    return Some(key);
                }
                if src.contains("://") {
                    return None;
                }
                let key = join_path(&dir, src);
                resources.entry(key.clone()).or_insert_with(|| ResRef::Zip { path: pathbuf.clone(), entry: key.clone() });
                Some(key)
            };
            let zip_ref = &mut zip;
            let mut css_fn = |href: &str| {
                let p = join_path(&dir, href);
                sheets
                    .entry(p.clone())
                    .or_insert_with(|| zip_read(zip_ref, &p).ok().map(|b| Sheet::parse(&String::from_utf8_lossy(&b))))
                    .clone()
            };
            let mut hooks = Hooks { link: &mut link_fn, image: &mut image_fn, stylesheet: &mut css_fn };
            html::convert(&src, Sheet::default(), &mut hooks, &Options::default())
        };
        anchors.entry(file.clone()).or_insert((si, 0));
        for (id, b) in &out.anchors {
            anchors.entry(format!("{file}#{id}")).or_insert((si, *b));
        }
        let title = out
            .blocks
            .iter()
            .find(|b| matches!(b.kind, super::BlockKind::Heading(_)))
            .map(|b| b.text())
            .filter(|t| t.chars().count() < 100)
            .unwrap_or_else(|| out.title.clone());
        sections.push(Section { title, blocks: out.blocks, chars: 0 });
    }

    let (sections, map) = split_long_sections(sections, 400);
    let resolve = |base: &str, href: &str| -> LinkTarget {
        let h = href.trim();
        if h.contains("://") || h.starts_with("mailto:") {
            return LinkTarget::External(h.to_string());
        }
        let (file_part, frag) = match h.split_once('#') {
            Some((f, fr)) => (f, Some(fr)),
            None => (h, None),
        };
        let file = if file_part.is_empty() { base.to_string() } else { join_path(&parent_of(base), file_part) };
        let key = match frag {
            Some(fr) if !fr.is_empty() => format!("{file}#{}", super::util::percent_decode(fr)),
            _ => file.clone(),
        };
        anchors
            .get(&key)
            .or_else(|| anchors.get(&file))
            .map(|&(s, b)| LinkTarget::Internal(remap(&map, s, b)))
            .unwrap_or(LinkTarget::Unresolved)
    };
    let links: Vec<LinkTarget> = links.iter().map(|l| resolve(&l.base, &l.href)).collect();

    // ---- table of contents ----
    let mut toc: Vec<TocEntry> = Vec::new();
    let nav = o.manifest.values().find(|m| m.properties.split_whitespace().any(|p| p == "nav"));
    if let Some(nav) = nav {
        let file = join_path(&opf_dir, &nav.href);
        if let Ok(b) = zip_read(&mut zip, &file) {
            for (title, depth, href) in parse_nav(&String::from_utf8_lossy(&b)) {
                if let LinkTarget::Internal(loc) = resolve(&file, &href) {
                    toc.push(TocEntry { title, depth, loc });
                }
            }
        }
    }
    if toc.is_empty() {
        let ncx = o.manifest.get(&o.toc_id).or_else(|| o.manifest.values().find(|m| m.media_type.contains("dtbncx")));
        if let Some(ncx) = ncx {
            let file = join_path(&opf_dir, &ncx.href);
            if let Ok(b) = zip_read(&mut zip, &file) {
                for (title, depth, href) in parse_ncx(&String::from_utf8_lossy(&b)) {
                    if let LinkTarget::Internal(loc) = resolve(&file, &href) {
                        toc.push(TocEntry { title, depth, loc });
                    }
                }
            }
        }
    }
    // Section titles from TOC where better.
    let mut sections = sections;
    for e in &toc {
        if e.loc.block == 0 {
            if let Some(s) = sections.get_mut(e.loc.section) {
                if s.title.is_empty() || s.title.chars().count() > 80 {
                    s.title = e.title.clone();
                }
            }
        }
    }
    let spine_sections = spine_raw.iter().map(|&s| map.get(s).map(|m| m.0).unwrap_or(sections.len().saturating_sub(1))).collect();

    Ok(Book {
        meta,
        toc,
        content: Content::Flow(Flow { sections, links, resources, spine_sections }),
    })
}

/// EPUB3 nav document → (title, depth, href)
fn parse_nav(src: &str) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    let mut nav_depth = 0usize; // >0 inside the toc nav
    let mut in_toc = false;
    let mut ol = 0usize;
    let mut cur: Option<(String, String)> = None;
    tokenize(src, |_, t| match t {
        Tok::Start { name, attrs, .. } => match name.as_str() {
            "nav" => {
                nav_depth += 1;
                let ty = attrs.iter().find(|(k, _)| k.ends_with("type")).map(|(_, v)| v.as_str()).unwrap_or("");
                if ty.contains("toc") || (!in_toc && out.is_empty() && ty.is_empty()) {
                    in_toc = true;
                }
            }
            "ol" | "ul" if in_toc => ol += 1,
            "a" | "span" if in_toc => {
                let href = attrs.iter().find(|(k, _)| k == "href").map(|(_, v)| v.clone()).unwrap_or_default();
                cur = Some((String::new(), href));
            }
            _ => {}
        },
        Tok::Text(t) => {
            if let Some((title, _)) = cur.as_mut() {
                title.push_str(&decode_entities(t));
            }
        }
        Tok::End { name } => match name.as_str() {
            "nav" => {
                nav_depth = nav_depth.saturating_sub(1);
                if in_toc {
                    in_toc = false;
                }
            }
            "ol" | "ul" if in_toc => ol = ol.saturating_sub(1),
            "a" | "span" => {
                if let Some((title, href)) = cur.take() {
                    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !title.is_empty() && !href.is_empty() {
                        out.push((title, ol.saturating_sub(1), href));
                    }
                }
            }
            _ => {}
        },
        _ => {}
    });
    out
}

/// EPUB2 NCX → (title, depth, href)
fn parse_ncx(src: &str) -> Vec<(String, usize, String)> {
    let mut out: Vec<(String, usize, String)> = Vec::new();
    let mut depth = 0usize;
    let mut in_text = false;
    let mut in_map = false;
    let mut title = String::new();
    tokenize(src, |_, t| match t {
        Tok::Start { name, attrs, .. } => match name.as_str() {
            "navmap" => in_map = true,
            "navpoint" if in_map => {
                depth += 1;
                title.clear();
            }
            "text" if in_map => in_text = true,
            "content" if in_map => {
                if let Some(src) = attrs.iter().find(|(k, _)| k == "src").map(|(_, v)| v.clone()) {
                    let t = title.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() {
                        out.push((t, depth.saturating_sub(1), src));
                    }
                    title.clear();
                }
            }
            _ => {}
        },
        Tok::Text(t) if in_text => title.push_str(&decode_entities(t)),
        Tok::End { name } => match name.as_str() {
            "navmap" => in_map = false,
            "navpoint" => depth = depth.saturating_sub(1),
            "text" => in_text = false,
            _ => {}
        },
        _ => {}
    });
    out
}

#[allow(dead_code)]
pub fn loc_zero() -> Loc {
    Loc::default()
}
