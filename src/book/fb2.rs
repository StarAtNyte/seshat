//! FictionBook 2 (.fb2 and .fb2.zip).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use super::css::Sheet;
use super::html::{self, decode_entities, tokenize, Hooks, Options, Tok};
use super::util::{base64_decode, decode_text, remap, split_long_sections};
use super::{Book, Content, Flow, LinkTarget, Meta, ResRef, Section};

fn load(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let bytes = if bytes.starts_with(b"PK") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        let name = zip.file_names().find(|n| n.to_lowercase().ends_with(".fb2")).map(|s| s.to_string()).ok_or("no .fb2 inside zip")?;
        let mut f = zip.by_name(&name).map_err(|e| e.to_string())?;
        super::util::read_entry_capped(&mut f)?
    } else {
        bytes
    };
    // Honour the XML declaration's encoding (any label encoding_rs knows).
    if !bytes.starts_with(&[0xEF, 0xBB, 0xBF]) && !bytes.starts_with(&[0xFF, 0xFE]) && !bytes.starts_with(&[0xFE, 0xFF]) {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).to_lowercase();
        let label = head.split_once("encoding").and_then(|(_, r)| {
            let r = r.trim_start_matches(|c: char| c == '=' || c == '"' || c == '\'' || c.is_whitespace());
            let l: String = r.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')).collect();
            (!l.is_empty()).then_some(l)
        });
        if let Some(enc) = label.and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes())) {
            if enc != encoding_rs::UTF_8 {
                return Ok(enc.decode_without_bom_handling(&bytes).0.into_owned());
            }
        }
    }
    Ok(decode_text(&bytes))
}

struct Parsed {
    meta: Meta,
    cover_id: String,
    binaries: HashMap<String, Vec<u8>>,
}

fn parse_head(src: &str, want_binaries: bool, only_cover: bool) -> Parsed {
    let mut p = Parsed { meta: Meta::default(), cover_id: String::new(), binaries: HashMap::new() };
    let mut stack: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut first = String::new();
    let mut last = String::new();
    let mut authors: Vec<String> = Vec::new();
    let mut bin_id: Option<String> = None;
    let desc_end = src.find("</description>").unwrap_or(src.len());
    let bin_start = if want_binaries { src.find("<binary").unwrap_or(src.len()) } else { src.len() };
    let mut handle = |part: &str, offset_ok: bool, p: &mut Parsed| {
        let _ = offset_ok;
        stack.clear();
        tokenize(part, |_, t| match t {
            Tok::Start { name, attrs, self_closing } => {
                if name == "image" && stack.iter().any(|s| s == "coverpage") {
                    if let Some((_, v)) = attrs.iter().find(|(k, _)| k.ends_with("href")) {
                        p.cover_id = v.trim_start_matches('#').to_string();
                    }
                }
                if name == "binary" {
                    bin_id = attrs.iter().find(|(k, _)| k == "id").map(|(_, v)| v.clone());
                }
                if !self_closing {
                    stack.push(name);
                }
                buf.clear();
            }
            Tok::Text(t) => buf.push_str(&decode_entities(t)),
            Tok::Raw(t) => buf.push_str(t),
            Tok::End { name } => {
                let in_title_info = stack.iter().any(|s| s == "title-info");
                let v = buf.trim().to_string();
                if in_title_info {
                    match name.as_str() {
                        "book-title" => p.meta.title = v.clone(),
                        "first-name" if stack.iter().any(|s| s == "author") => first = v.clone(),
                        "last-name" if stack.iter().any(|s| s == "author") => last = v.clone(),
                        "author" => {
                            let a = format!("{first} {last}").trim().to_string();
                            if !a.is_empty() {
                                authors.push(a);
                            }
                            first.clear();
                            last.clear();
                        }
                        "lang" => p.meta.language = v.clone(),
                        "genre" if !v.is_empty() => p.meta.subjects.push(v.replace('_', " ")),
                        "p" if stack.iter().any(|s| s == "annotation") => {
                            if !p.meta.description.is_empty() {
                                p.meta.description.push('\n');
                            }
                            p.meta.description.push_str(&v);
                        }
                        _ => {}
                    }
                }
                if name == "publisher" {
                    p.meta.publisher = v.clone();
                }
                if name == "binary" {
                    if let Some(id) = bin_id.take() {
                        if !only_cover || id == p.cover_id {
                            p.binaries.insert(id, base64_decode(&buf));
                        }
                    }
                }
                if let Some(i) = stack.iter().rposition(|s| *s == name) {
                    stack.truncate(i);
                }
                buf.clear();
            }
        });
    };
    handle(&src[..desc_end.min(src.len())], true, &mut p);
    if want_binaries && bin_start < src.len() {
        handle(&src[bin_start..], true, &mut p);
    }
    p.meta.author = authors.join(", ");
    p
}

pub fn read_meta(path: &Path) -> Result<Meta, String> {
    let src = load(path)?;
    let mut p = parse_head(&src, true, true);
    p.meta.cover = p.binaries.remove(&p.cover_id);
    Ok(p.meta)
}

pub fn open(path: &Path) -> Result<Book, String> {
    let src = load(path)?;
    let mut parsed = parse_head(&src, true, false);
    let mut meta = parsed.meta.clone();
    meta.cover = parsed.binaries.get(&parsed.cover_id).cloned();

    let body_start = src.find("<body").ok_or("FB2 has no <body>")?;
    let body_end = src.find("<binary").unwrap_or(src.len()).max(body_start);
    let body = &src[body_start..body_end];

    let mut resources: HashMap<String, ResRef> = HashMap::new();
    let mut raw_links: Vec<String> = Vec::new();
    let out = {
        let mut link_fn = |h: &str| {
            raw_links.push(h.to_string());
            (raw_links.len() - 1) as u32
        };
        let bins = &mut parsed.binaries;
        let mut image_fn = |s: &str| {
            let id = s.trim_start_matches('#').to_string();
            if !resources.contains_key(&id) {
                let b = bins.remove(&id)?;
                resources.insert(id.clone(), ResRef::Bytes(Arc::new(b)));
            }
            Some(id)
        };
        let mut css_fn = |_: &str| None;
        let mut hooks = Hooks { link: &mut link_fn, image: &mut image_fn, stylesheet: &mut css_fn };
        let opts = Options { section_break_depth: Some(1), break_tags: &["body"], fb2: true, ..Default::default() };
        html::convert(body, Sheet::default(), &mut hooks, &opts)
    };

    let mut starts = vec![0];
    starts.extend(out.breaks.iter().copied());
    starts.push(out.blocks.len());
    starts.dedup();
    let mut blocks = out.blocks;
    let mut parts = Vec::new();
    for w in starts.windows(2).rev() {
        parts.push(blocks.split_off(w[0]));
    }
    parts.reverse();
    let mut gmap = Vec::new();
    let mut secs = Vec::new();
    for bl in parts {
        if bl.is_empty() {
            continue;
        }
        let si = secs.len();
        for bi in 0..bl.len() {
            gmap.push((si, bi));
        }
        let title = bl.iter().find(|b| matches!(b.kind, super::BlockKind::Heading(_))).map(|b| b.text()).unwrap_or_default();
        secs.push(Section { title, blocks: bl, chars: 0 });
    }
    let (sections, map) = split_long_sections(secs, 400);
    let anchors: HashMap<String, usize> = out.anchors.into_iter().collect();
    let links = raw_links
        .iter()
        .map(|h| {
            if h.contains("://") {
                return LinkTarget::External(h.clone());
            }
            match anchors.get(h.trim_start_matches('#')).and_then(|&g| gmap.get(g)) {
                Some(&(s, b)) => LinkTarget::Internal(remap(&map, s, b)),
                None => LinkTarget::Unresolved,
            }
        })
        .collect();
    let n = sections.len();
    Ok(Book { meta, toc: Vec::new(), content: Content::Flow(Flow { sections, links, resources, spine_sections: (0..n).collect() }) })
}
