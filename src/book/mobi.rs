//! MOBI / AZW / AZW3 (KF8) / PRC reader.
//!
//! PalmDB container → PalmDOC (LZ77) text records → HTML, which then goes
//! through the shared HTML converter. Handles combo MOBI6+KF8 files (prefers
//! KF8), trailing-entry stripping, EXTH metadata, cover, `recindex` and
//! `kindle:embed` images, and MOBI6 `filepos` links. HUFF/CDIC compression
//! and DRM are reported as unsupported.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use super::css::Sheet;
use super::html::{self, Hooks, Options};
use super::util::{decode_cp1252, remap, split_long_sections};
use super::{Book, Content, Flow, LinkTarget, Meta, ResRef, Section};

struct Pdb<'a> {
    data: &'a [u8],
    offsets: Vec<usize>,
}

impl<'a> Pdb<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, String> {
        if data.len() < 78 {
            return Err("file too small for MOBI".into());
        }
        let n = u16::from_be_bytes([data[76], data[77]]) as usize;
        let mut offsets = Vec::with_capacity(n);
        for i in 0..n {
            let p = 78 + i * 8;
            if p + 4 > data.len() {
                break;
            }
            offsets.push(be32(data, p) as usize);
        }
        Ok(Pdb { data, offsets })
    }
    fn record(&self, i: usize) -> &'a [u8] {
        let Some(&s) = self.offsets.get(i) else { return &[] };
        let e = self.offsets.get(i + 1).copied().unwrap_or(self.data.len());
        if s >= e || e > self.data.len() {
            return &[];
        }
        &self.data[s..e]
    }
    fn len(&self) -> usize {
        self.offsets.len()
    }
}

fn be32(d: &[u8], p: usize) -> u32 {
    if p + 4 > d.len() {
        return 0;
    }
    u32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
}
fn be16(d: &[u8], p: usize) -> u16 {
    if p + 2 > d.len() {
        return 0;
    }
    u16::from_be_bytes([d[p], d[p + 1]])
}

struct Header {
    compression: u16,
    text_records: usize,
    encryption: u16,
    utf8: bool,
    first_image: usize,
    extra_flags: u16,
    fdst: usize,
    exth: HashMap<u32, Vec<Vec<u8>>>,
    full_name: String,
}

fn parse_header(rec0: &[u8]) -> Header {
    let compression = be16(rec0, 0);
    let text_records = be16(rec0, 8) as usize;
    let encryption = be16(rec0, 12);
    let mut h = Header { compression, text_records, encryption, utf8: true, first_image: usize::MAX, extra_flags: 0, fdst: usize::MAX, exth: HashMap::new(), full_name: String::new() };
    if rec0.len() >= 24 && &rec0[16..20] == b"MOBI" {
        let hlen = be32(rec0, 20) as usize;
        h.utf8 = be32(rec0, 28) == 65001;
        h.first_image = be32(rec0, 108) as usize;
        if hlen >= 0xE4 {
            h.extra_flags = be16(rec0, 16 + 0xE2);
        }
        if hlen >= 0xC4 {
            h.fdst = be32(rec0, 16 + 0xC0) as usize;
        }
        let name_off = be32(rec0, 84) as usize;
        let name_len = be32(rec0, 88) as usize;
        if name_off + name_len <= rec0.len() {
            h.full_name = decode(&rec0[name_off..name_off + name_len], h.utf8);
        }
        let exth_flags = be32(rec0, 128);
        let ep = 16 + hlen;
        if exth_flags & 0x40 != 0 && ep + 12 <= rec0.len() && &rec0[ep..ep + 4] == b"EXTH" {
            let count = be32(rec0, ep + 8) as usize;
            let mut p = ep + 12;
            for _ in 0..count {
                let ty = be32(rec0, p);
                let len = be32(rec0, p + 4) as usize;
                if len < 8 || p + len > rec0.len() {
                    break;
                }
                h.exth.entry(ty).or_default().push(rec0[p + 8..p + len].to_vec());
                p += len;
            }
        }
    }
    h
}

fn decode(b: &[u8], utf8: bool) -> String {
    if utf8 {
        String::from_utf8_lossy(b).into_owned()
    } else {
        decode_cp1252(b)
    }
}

fn exth_str(h: &Header, ty: u32) -> String {
    h.exth.get(&ty).and_then(|v| v.first()).map(|b| decode(b, h.utf8)).unwrap_or_default()
}

fn exth_u32(h: &Header, ty: u32) -> Option<usize> {
    h.exth.get(&ty).and_then(|v| v.first()).filter(|b| b.len() >= 4).map(|b| be32(b, 0) as usize)
}

fn trailing_size(data: &[u8], flags: u16) -> usize {
    let mut num = 0usize;
    let mut f = flags >> 1;
    while f != 0 {
        if f & 1 != 0 {
            // backward-encoded varint at end
            let mut size = data.len().saturating_sub(num);
            let mut result = 0usize;
            let mut bitpos = 0;
            while size > 0 {
                let v = data[size - 1];
                result |= ((v & 0x7F) as usize) << bitpos;
                bitpos += 7;
                size -= 1;
                if v & 0x80 != 0 || bitpos >= 28 {
                    break;
                }
            }
            num += result;
        }
        f >>= 1;
    }
    if flags & 1 != 0 {
        let p = data.len().saturating_sub(num);
        if p > 0 {
            num += (data[p - 1] & 3) as usize + 1;
        }
    }
    num.min(data.len())
}

fn palmdoc(input: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        i += 1;
        match c {
            1..=8 => {
                let end = (i + c as usize).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            0 | 9..=0x7F => out.push(c),
            0xC0..=0xFF => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
            _ => {
                if i >= input.len() {
                    break;
                }
                let d = ((c as usize) << 8) | input[i] as usize;
                i += 1;
                let dist = (d >> 3) & 0x7FF;
                let len = (d & 7) + 3;
                if dist == 0 || dist > out.len() {
                    continue;
                }
                let start = out.len() - dist;
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
            }
        }
    }
}

fn is_image(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8]) || b.starts_with(b"\x89PNG") || b.starts_with(b"GIF8") || b.starts_with(b"BM")
}

struct Part {
    header: Header,
    base: usize,
}

fn select_part(pdb: &Pdb) -> Result<Part, String> {
    let h0 = parse_header(pdb.record(0));
    if let Some(boundary) = exth_u32(&h0, 121) {
        if boundary > 0 && boundary < pdb.len() {
            let rec = pdb.record(boundary);
            if rec.len() > 20 && &rec[16..20] == b"MOBI" {
                let mut h = parse_header(rec);
                // EXTH metadata of the KF8 part is usually complete; fall back to MOBI6's.
                for (k, v) in &h0.exth {
                    h.exth.entry(*k).or_insert_with(|| v.clone());
                }
                return Ok(Part { header: h, base: boundary });
            }
        }
    }
    Ok(Part { header: h0, base: 0 })
}

fn first_image_index(pdb: &Pdb, part: &Part, h0_first: usize) -> Option<usize> {
    let cands = [part.header.first_image, part.header.first_image.wrapping_add(part.base), h0_first];
    cands.into_iter().find(|&c| c < pdb.len() && is_image(pdb.record(c)))
}

fn extract_text(pdb: &Pdb, part: &Part) -> Result<Vec<u8>, String> {
    let h = &part.header;
    if h.encryption != 0 {
        return Err("this book is DRM-protected and cannot be opened".into());
    }
    if h.compression == 17480 {
        return Err("HUFF/CDIC-compressed MOBI files are not supported yet".into());
    }
    let mut text = Vec::new();
    for i in 1..=h.text_records {
        let rec = pdb.record(part.base + i);
        let n = trailing_size(rec, h.extra_flags);
        let rec = &rec[..rec.len() - n];
        match h.compression {
            2 => palmdoc(rec, &mut text),
            _ => text.extend_from_slice(rec),
        }
    }
    Ok(text)
}

fn meta_from(h: &Header) -> Meta {
    let mut m = Meta {
        title: exth_str(h, 503),
        author: h.exth.get(&100).map(|v| v.iter().map(|b| decode(b, h.utf8)).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
        publisher: exth_str(h, 101),
        description: exth_str(h, 103),
        language: exth_str(h, 524),
        subjects: h.exth.get(&105).map(|v| v.iter().map(|b| decode(b, h.utf8)).collect()).unwrap_or_default(),
        ..Default::default()
    };
    if m.title.is_empty() {
        m.title = h.full_name.clone();
    }
    m
}

fn cover_of(pdb: &Pdb, part: &Part, first_img: Option<usize>) -> Option<Vec<u8>> {
    let fi = first_img?;
    let off = exth_u32(&part.header, 201).or_else(|| exth_u32(&part.header, 202))?;
    let rec = pdb.record(fi + off);
    is_image(rec).then(|| rec.to_vec())
}

pub fn read_meta(path: &Path) -> Result<Meta, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let pdb = Pdb::parse(&data)?;
    let part = select_part(&pdb)?;
    let h0 = parse_header(pdb.record(0));
    let fi = first_image_index(&pdb, &part, h0.first_image);
    let mut m = meta_from(&part.header);
    m.cover = cover_of(&pdb, &part, fi);
    Ok(m)
}

fn base32(s: &str) -> Option<usize> {
    let mut v = 0usize;
    for c in s.chars() {
        let d = c.to_digit(32)?;
        v = v * 32 + d as usize;
    }
    Some(v)
}

pub fn open(path: &Path) -> Result<Book, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let pdb = Pdb::parse(&data)?;
    let part = select_part(&pdb)?;
    let h0 = parse_header(pdb.record(0));
    let first_img = first_image_index(&pdb, &part, h0.first_image);
    let mut meta = meta_from(&part.header);
    meta.cover = cover_of(&pdb, &part, first_img);
    let kf8 = part.base > 0 || be32(pdb.record(0), 36) >= 8;

    let raw = extract_text(&pdb, &part)?;
    // KF8: keep only flow 0 (the HTML), other flows are CSS/SVG.
    let mut flows: Vec<Vec<u8>> = Vec::new();
    let mut text = raw.clone();
    let fdst_rec = part.header.fdst.checked_add(part.base).filter(|&i| i < pdb.len()).map(|i| pdb.record(i));
    if let Some(f) = fdst_rec.filter(|f| f.starts_with(b"FDST")) {
        let count = (be32(f, 8) as usize).min(f.len().saturating_sub(12) / 8);
        for k in 0..count {
            let s = be32(f, 12 + k * 8) as usize;
            let e = be32(f, 16 + k * 8) as usize;
            if s <= e && e <= raw.len() {
                flows.push(raw[s..e].to_vec());
            }
        }
        if let Some(f0) = flows.first() {
            text = f0.clone();
        }
    }
    let src = decode(&text, part.header.utf8);
    // Raw byte offset → offset in `src` (identity for UTF-8; CP1252 chars may be multi-byte).
    let offset_map: Option<Vec<usize>> = if part.header.utf8 {
        None
    } else {
        let mut m = Vec::with_capacity(text.len() + 1);
        let mut acc = 0;
        for &c in &text {
            m.push(acc);
            acc += if c < 0x80 { 1 } else { decode_cp1252(&[c]).len() };
        }
        m.push(acc);
        Some(m)
    };

    // MOBI6 filepos anchors: collect targets up front.
    let mut pos_anchors: Vec<usize> = Vec::new();
    {
        let mut rest = src.as_str();
        while let Some(i) = rest.find("filepos=") {
            rest = &rest[i + 8..];
            let digits: String = rest.trim_start_matches(['"', '\'']).chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<usize>() {
                pos_anchors.push(n);
            }
        }
        pos_anchors.sort_unstable();
        pos_anchors.dedup();
    }
    // filepos values are raw byte offsets; translate to offsets in the decoded string.
    let pos_anchors: Vec<(usize, usize)> = pos_anchors
        .iter()
        .map(|&raw| (offset_map.as_ref().map_or(raw, |m| m[raw.min(m.len() - 1)]), raw))
        .collect();
    let mut resources: HashMap<String, ResRef> = HashMap::new();
    let mut raw_links: Vec<String> = Vec::new();
    let out = {
        let mut link_fn = |href: &str| {
            raw_links.push(href.to_string());
            (raw_links.len() - 1) as u32
        };
        let mut image_fn = |src: &str| -> Option<String> {
            let fi = first_img?;
            let idx = if let Some(r) = src.strip_prefix("recindex:") {
                r.trim_start_matches('0').parse::<usize>().ok()?
            } else if let Some(r) = src.strip_prefix("kindle:embed:") {
                base32(r.split('?').next().unwrap_or(r))?
            } else {
                return None;
            };
            let rec = fi + idx.checked_sub(1)?;
            let key = format!("rec:{rec}");
            if !resources.contains_key(&key) {
                let b = pdb.record(rec);
                if !is_image(b) {
                    return None;
                }
                resources.insert(key.clone(), ResRef::Bytes(Arc::new(b.to_vec())));
            }
            Some(key)
        };
        let mut css_fn = |href: &str| -> Option<Sheet> {
            let n = href.strip_prefix("kindle:flow:")?.split('?').next()?;
            let k = base32(n)?;
            flows.get(k).map(|f| Sheet::parse(&String::from_utf8_lossy(f)))
        };
        let mut hooks = Hooks { link: &mut link_fn, image: &mut image_fn, stylesheet: &mut css_fn };
        let opts = Options {
            break_tags: if kf8 { &["html", "pagebreak"] } else { &["pagebreak"] },
            pos_anchors: pos_anchors.clone(),
            ..Default::default()
        };
        html::convert(&src, Sheet::default(), &mut hooks, &opts)
    };

    // Split into sections at breaks.
    let mut sections = Vec::new();
    let mut starts = vec![0];
    starts.extend(out.breaks.iter().copied());
    starts.push(out.blocks.len());
    starts.dedup();
    let mut blocks = out.blocks;
    for w in starts.windows(2).rev() {
        sections.push(blocks.split_off(w[0]));
    }
    sections.reverse();
    let mut secs: Vec<Section> = Vec::new();
    for bl in sections.into_iter() {
        if bl.is_empty() {
            continue;
        }
        let title = bl.iter().find(|b| matches!(b.kind, super::BlockKind::Heading(_))).map(|b| b.text()).unwrap_or_default();
        secs.push(Section { title, blocks: bl, chars: 0 });
    }
    // global block index → (section, block) before splitting
    let mut gmap: Vec<(usize, usize)> = Vec::new();
    for (si, s) in secs.iter().enumerate() {
        for bi in 0..s.blocks.len() {
            gmap.push((si, bi));
        }
    }
    let (sections, map) = split_long_sections(secs, 400);
    let anchors: HashMap<String, usize> = out.anchors.into_iter().collect();
    let links = raw_links
        .iter()
        .map(|h| {
            if h.contains("://") {
                return LinkTarget::External(h.clone());
            }
            let key = if let Some(fp) = h.strip_prefix("filepos:") { format!("pos:{}", fp.parse::<usize>().unwrap_or(0)) } else { h.split('#').nth(1).unwrap_or("").to_string() };
            match anchors.get(&key).and_then(|&g| gmap.get(g)) {
                Some(&(s, b)) => LinkTarget::Internal(remap(&map, s, b)),
                None => LinkTarget::Unresolved,
            }
        })
        .collect();
    let n = sections.len();
    Ok(Book {
        meta,
        toc: Vec::new(),
        content: Content::Flow(Flow { sections, links, resources, spine_sections: (0..n).collect() }),
    })
}
