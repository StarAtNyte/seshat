//! Minimal pure-Rust EPUB reader: ZIP container + OPF spine + XHTML text extraction.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Clone)]
pub struct Chapter {
    pub title: String,
    /// Rendered text: paragraphs and headings, ready to display.
    pub blocks: Vec<Block>,
}

#[derive(Clone)]
pub enum Block {
    Heading(String),
    Paragraph(String),
}

pub struct Epub {
    pub chapters: Vec<Chapter>,
    pub cover: Option<Vec<u8>>,
    pub title: String,
    pub author: String,
}

pub fn open(path: &Path) -> Result<Epub, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("bad epub file: {e}"))?;

    // 1. container.xml -> rootfile (OPF path)
    let container = read_entry(&mut zip, "META-INF/container.xml")?;
    let opf_path = parse_container(&container)?;

    // 2. OPF: metadata (title), manifest (id -> href), spine order
    let opf = read_entry(&mut zip, &opf_path)?;
    let opf_dir = parent_of(&opf_path);
    let (title, author, manifest, spine) = parse_opf(&opf)?;

    // 3. cover image (first manifest item with properties="cover-image" or id containing "cover")
    let cover = manifest
        .values()
        .find(|m| m.properties.contains("cover-image"))
        .and_then(|m| read_entry(&mut zip, &join_path(&opf_dir, &m.href)).ok());

    // 4. chapters: each spine item that is HTML
    let mut chapters = Vec::new();
    for idref in &spine {
        let Some(item) = manifest.get(idref) else { continue };
        let mtype = item.media_type.clone();
        if !(mtype.contains("xhtml") || mtype.contains("html")) {
            continue;
        }
        let href = join_path(&opf_dir, &item.href);
        let Ok(html) = read_entry(&mut zip, &href) else { continue };
        let text = String::from_utf8_lossy(&html).into_owned();
        let (title2, blocks) = extract_text(&text);
        let title = if title2.trim().is_empty() {
            format!("Chapter {}", chapters.len() + 1)
        } else {
            title2
        };
        chapters.push(Chapter { title, blocks });
    }

    Ok(Epub {
        chapters,
        cover,
        title: if title.is_empty() {
            path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            title
        },
        author,
    })
}

// ---------- helpers ----------

fn read_entry<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>, String> {
    let mut f = zip.by_name(name).map_err(|_| format!("missing {name}"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

fn join_path(base: &str, href: &str) -> String {
    // strip fragment
    let href = href.split('#').next().unwrap_or(href);
    // URL-decode common escapes
    let href = href.replace("%20", " ");
    if href.starts_with('/') {
        return href.trim_start_matches('/').to_string();
    }
    if base.is_empty() {
        return href;
    }
    // resolve relative path segments
    let mut parts: Vec<&str> = base.split('/').collect();
    for seg in href.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn parse_container(xml: &[u8]) -> Result<String, String> {
    let xml = String::from_utf8_lossy(xml);
    let mut reader = Reader::from_str(&xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"rootfile" => {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"full-path" {
                        return Ok(String::from_utf8_lossy(&attr.value).into_owned());
                    }
                }
            }
            Ok(Event::Eof) => return Err("no rootfile in container.xml".into()),
            _ => {}
        }
    }
}

struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

fn parse_opf(xml: &[u8]) -> Result<(String, String, HashMap<String, ManifestItem>, Vec<String>), String> {
    let xml = String::from_utf8_lossy(xml);
    let mut reader = Reader::from_str(&xml);
    reader.trim_text(true);
    let mut title = String::new();
    let mut author = String::new();
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut section = String::new(); // metadata | manifest | spine
    let mut in_title = false;
    let mut in_creator = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"metadata" => section = "metadata".into(),
                    b"manifest" => section = "manifest".into(),
                    b"spine" => section = "spine".into(),
                    b"title" if section == "metadata" => in_title = true,
                    _ => {
                        let n = name.as_ref();
                        if section == "metadata" && (n.ends_with(b"title") || n == b"dc:title") {
                            in_title = true;
                        }
                        if section == "metadata" && (n.ends_with(b"creator") || n == b"dc:creator") {
                            in_creator = true;
                        }
                    }
                    _ => {}
                }
                if section == "manifest" && name.as_ref() == b"item" {
                    let mut id = String::new();
                    let mut href = String::new();
                    let mut mt = String::new();
                    let mut props = String::new();
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"id" => id = String::from_utf8_lossy(&attr.value).into_owned(),
                            b"href" => href = String::from_utf8_lossy(&attr.value).into_owned(),
                            b"media-type" => mt = String::from_utf8_lossy(&attr.value).into_owned(),
                            b"properties" => props = String::from_utf8_lossy(&attr.value).into_owned(),
                            _ => {}
                        }
                    }
                    if !id.is_empty() && !href.is_empty() {
                        manifest.insert(id, ManifestItem { href, media_type: mt, properties: props });
                    }
                }
                if section == "spine" && name.as_ref() == b"itemref" {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"idref" {
                            spine.push(String::from_utf8_lossy(&attr.value).into_owned());
                        }
                    }
                }
            }
            Ok(Event::Text(t)) if in_title => {
                title = t.unescape().map(|s| s.into_owned()).unwrap_or_default();
                in_title = false;
            }
            Ok(Event::Text(t)) if in_creator => {
                author = t.unescape().map(|s| s.into_owned()).unwrap_or_default();
                in_creator = false;
            }
            Ok(Event::End(e)) => {
                match e.name().as_ref() {
                    b"metadata" | b"manifest" | b"spine" => section = String::new(),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("opf parse error: {e}")),
            _ => {}
        }
    }
    Ok((title, author, manifest, spine))
}

/// Extract display blocks from XHTML. Returns (page title, blocks).
/// Keeps headings, paragraphs and line breaks; drops scripts/styles.
fn extract_text(html: &str) -> (String, Vec<Block>) {
    let mut reader = Reader::from_str(html);
    reader.trim_text(false);
    let mut blocks: Vec<Block> = Vec::new();
    let mut cur = String::new();
    let mut skip_depth = 0usize;
    let mut tag_stack: Vec<String> = Vec::new();
    let mut first_heading = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                if matches!(name.as_str(), "style" | "script" | "head") {
                    skip_depth += 1;
                }
                if skip_depth > 0 {
                    continue;
                }
                match name.as_str() {
                    "p" | "div" | "blockquote" | "li" | "td" => {
                        flush_with_marker(&mut blocks, &mut cur);
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush_with_marker(&mut blocks, &mut cur);
                        cur.push_str("\u{1D}H"); // marker: next flush is a heading
                    }
                    "br" => {
                        cur.push(' ');
                    }
                    "img" => {}
                    _ => {}
                }
                tag_stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                if name == "br" {
                    cur.push(' ');
                }
            }
            Ok(Event::Text(t)) => {
                if skip_depth > 0 {
                    continue;
                }
                let text = t.unescape().unwrap_or_default();
                cur.push_str(&text);
                cur.push(' ');
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                if matches!(name.as_str(), "style" | "script" | "head") && skip_depth > 0 {
                    skip_depth -= 1;
                }
                if skip_depth > 0 {
                    continue;
                }
                match name.as_str() {
                    "p" | "div" | "blockquote" | "li" | "td" => {
                        flush_with_marker(&mut blocks, &mut cur);
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        let text = collapse_ws(&cur);
                        cur.clear();
                        if !text.is_empty() {
                            if first_heading.is_empty() {
                                first_heading = text.clone();
                            }
                            blocks.push(Block::Heading(text));
                        }
                    }
                    _ => {}
                }
                tag_stack.pop();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    // flush trailing text
    flush_with_marker(&mut blocks, &mut cur);
    (first_heading, blocks)
}

fn flush_with_marker(blocks: &mut Vec<Block>, cur: &mut String) {
    let text = collapse_ws(cur);
    cur.clear();
    if text.is_empty() {
        return;
    }
    if text.starts_with('\u{1D}') || text.contains('\u{1D}') {
        let cleaned = text.replace('\u{1D}', "");
        let cleaned = collapse_ws(&cleaned);
        if !cleaned.is_empty() {
            blocks.push(Block::Heading(cleaned));
        }
        return;
    }
    blocks.push(Block::Paragraph(text));
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Block {
    pub fn text(&self) -> &str {
        match self {
            Block::Heading(t) => t,
            Block::Paragraph(t) => t,
        }
    }
}
