//! Shared helpers for format parsers.

use std::io::Read;
use std::path::Path;

use super::{BlockKind, Flow, Loc, Section, TocEntry};

pub fn read_zip_entry(path: &Path, entry: &str) -> Result<Vec<u8>, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())?;
    zip_read(&mut zip, entry)
}

/// Largest zip entry we will inflate (guards against zip bombs).
pub const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

/// Read a zip entry fully, refusing entries larger than [`MAX_ENTRY_BYTES`].
pub fn read_entry_capped<R: Read>(f: &mut zip::read::ZipFile<R>) -> Result<Vec<u8>, String> {
    if f.size() > MAX_ENTRY_BYTES {
        return Err(format!("{} is too large ({} bytes)", f.name(), f.size()));
    }
    let mut buf = Vec::with_capacity((f.size() as usize).min(64 << 20));
    f.by_ref().take(MAX_ENTRY_BYTES + 1).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    if buf.len() as u64 > MAX_ENTRY_BYTES {
        return Err(format!("{} is too large", f.name()));
    }
    Ok(buf)
}

pub fn zip_read<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>, String> {
    if let Ok(mut f) = zip.by_name(name) {
        return read_entry_capped(&mut f);
    }
    // Case-insensitive / percent-decoded fallback (sloppy EPUBs).
    let want = percent_decode(name).to_lowercase();
    let found = zip.file_names().find(|n| percent_decode(n).to_lowercase() == want).map(|s| s.to_string());
    if let Some(n) = found {
        let mut f = zip.by_name(&n).map_err(|e| e.to_string())?;
        return read_entry_capped(&mut f);
    }
    Err(format!("missing {name}"))
}

pub fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// Resolve `href` (without fragment) relative to directory `base`.
pub fn join_path(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let href = percent_decode(href);
    if href.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = if href.starts_with('/') || base.is_empty() {
        Vec::new()
    } else {
        base.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()
    };
    for seg in href.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s.to_string()),
        }
    }
    parts.join("/")
}

pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = h {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const CP1252: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘', '’', '“',
    '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

pub fn decode_cp1252(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x80..=0x9F => CP1252[(c - 0x80) as usize],
            _ => c as char,
        })
        .collect()
}

/// Charset declared in the first bytes: `<?xml encoding=…?>`, `<meta charset=…>` or
/// `<meta http-equiv content="…charset=…">`.
pub fn declared_charset(b: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let head: String = b[..b.len().min(4096)].iter().map(|&c| c.to_ascii_lowercase() as char).collect();
    let grab = |key: &str, from: usize| -> Option<String> {
        let i = head[from..].find(key)? + from + key.len();
        let rest = head[i..].trim_start_matches(|c: char| c == '=' || c == '"' || c == '\'' || c.is_whitespace());
        let v: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')).collect();
        (!v.is_empty()).then_some(v)
    };
    let mut cands = Vec::new();
    if let Some(x) = head.find("<?xml") {
        let end = head[x..].find("?>").map(|e| e + x).unwrap_or(head.len());
        if head[x..end].contains("encoding") {
            cands.extend(grab("encoding", x));
        }
    }
    let mut from = 0;
    while let Some(m) = head[from..].find("<meta") {
        let m = m + from;
        let end = head[m..].find('>').map(|e| e + m).unwrap_or(head.len());
        if head[m..end].contains("charset") {
            cands.extend(grab("charset", m));
        }
        from = end.max(m + 5);
    }
    cands.into_iter().find_map(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
}

/// Decode text of unknown encoding: BOM-sniffed UTF-16/UTF-8, else UTF-8, else a
/// declared charset, else statistical detection (Cyrillic, CJK, Windows-125x …).
pub fn decode_text(b: &[u8]) -> String {
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&b[3..]).into_owned();
    }
    if b.len() >= 2 && (b[0] == 0xFF && b[1] == 0xFE || b[0] == 0xFE && b[1] == 0xFF) {
        let le = b[0] == 0xFF;
        let units: Vec<u16> = b[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Ok(s) = std::str::from_utf8(b) {
        return s.to_string();
    }
    // Mostly-valid UTF-8 with a few stray bytes → lossy. Judged by the fraction of
    // invalid sequences, not by where the first one happens to be.
    let invalid = b.utf8_chunks().filter(|c| !c.invalid().is_empty()).count();
    if invalid * 1000 < b.len() {
        return String::from_utf8_lossy(b).into_owned();
    }
    let enc = match declared_charset(b) {
        Some(e) if e != encoding_rs::UTF_8 => e,
        _ => {
            let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
            det.feed(b, true);
            det.guess(None, chardetng::Utf8Detection::Allow)
        }
    };
    enc.decode_without_bom_handling(b).0.into_owned()
}

pub fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}

/// "Title (Author) (z-library.sk, 1lib.sk).epub" → "Title".
pub fn title_from_filename(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = stem.trim_end_matches(".fb2");
    let groups = paren_groups(stem);
    let mut end = stem.len();
    for (s, _e, inner) in groups.iter().rev() {
        let l = inner.to_lowercase();
        if l.contains("z-lib") || l.contains("1lib") || l.contains("z-library") || l.contains("libgen") {
            end = *s;
            continue;
        }
        end = *s;
        break;
    }
    let t = stem[..end].trim().replace('_', " ");
    if t.is_empty() {
        stem.to_string()
    } else {
        t
    }
}

pub fn author_from_filename(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    for (_s, _e, inner) in paren_groups(&stem).iter().rev() {
        let l = inner.to_lowercase();
        if l.contains("z-lib") || l.contains("1lib") || l.contains("z-library") || l.contains("libgen") {
            continue;
        }
        return inner.trim().to_string();
    }
    String::new()
}

/// Top-level parenthesized groups: (start, end, inner).
fn paren_groups(s: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push((start, i + 1, s[start + 1..i].to_string()));
                }
            }
            _ => {}
        }
    }
    out
}

/// Build a TOC from headings when the book has none.
pub fn toc_from_headings(flow: &Flow) -> Vec<TocEntry> {
    let mut toc = Vec::new();
    for (si, s) in flow.sections.iter().enumerate() {
        let mut found = false;
        for (bi, b) in s.blocks.iter().enumerate() {
            if let BlockKind::Heading(l) = b.kind {
                if l <= 3 {
                    let t = b.text();
                    let t = t.trim();
                    if !t.is_empty() && t.chars().count() < 120 {
                        toc.push(TocEntry { title: t.to_string(), depth: (l.saturating_sub(1)) as usize, loc: Loc::new(si, bi, 0) });
                        found = true;
                    }
                }
            }
        }
        if !found && !s.title.is_empty() {
            toc.push(TocEntry { title: s.title.clone(), depth: 0, loc: Loc::new(si, 0, 0) });
        }
    }
    // Normalise depth so the shallowest level is 0.
    if let Some(min) = toc.iter().map(|e| e.depth).min() {
        for e in &mut toc {
            e.depth -= min;
        }
    }
    toc
}

/// Split very long sections so layout stays fast. Returns the new sections and,
/// for each input section, `(first output section, block start of each part)`.
pub fn split_long_sections(sections: Vec<Section>, max_blocks: usize) -> (Vec<Section>, Vec<(usize, Vec<usize>)>) {
    let mut out = Vec::new();
    let mut map = Vec::new();
    for s in sections {
        let first = out.len();
        if s.blocks.len() <= max_blocks {
            out.push(s.finish());
            map.push((first, vec![0]));
            continue;
        }
        let title = s.title.clone();
        let mut blocks = s.blocks;
        let mut starts = Vec::new();
        let mut consumed = 0;
        let mut part = 0;
        while !blocks.is_empty() {
            // Prefer to cut right before a heading near the limit.
            let mut cut = blocks.len().min(max_blocks);
            if cut < blocks.len() {
                if let Some(h) = (max_blocks / 2..cut).rev().find(|&i| matches!(blocks[i].kind, BlockKind::Heading(_))) {
                    cut = h.max(1);
                }
            }
            let rest = blocks.split_off(cut);
            let t = if part == 0 { title.clone() } else { format!("{title} ({})", part + 1) };
            starts.push(consumed);
            consumed += blocks.len();
            out.push(Section { title: t, blocks, chars: 0 }.finish());
            blocks = rest;
            part += 1;
        }
        map.push((first, starts));
    }
    (out, map)
}

/// Map an (original section, block) pair through a split map.
pub fn remap(map: &[(usize, Vec<usize>)], section: usize, block: usize) -> Loc {
    let Some((first, starts)) = map.get(section) else { return Loc::default() };
    let k = starts.iter().rposition(|&s| s <= block).unwrap_or(0);
    Loc::new(first + k, block - starts[k], 0)
}

/// Natural ordering ("page2" < "page10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek(), bi.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().filter(|c| c.is_ascii_digit()) {
                    na.push(*c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().filter(|c| c.is_ascii_digit()) {
                    nb.push(*c);
                    bi.next();
                }
                let o = na.trim_start_matches('0').len().cmp(&nb.trim_start_matches('0').len()).then(na.trim_start_matches('0').cmp(nb.trim_start_matches('0')));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

pub fn is_image_name(name: &str) -> bool {
    let l = name.to_lowercase();
    [".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp"].iter().any(|e| l.ends_with(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossy_utf8_decided_by_fraction() {
        // One bad byte very early in a long, otherwise valid UTF-8 text.
        let mut v = vec![b'a', 0xFF];
        v.extend("é".repeat(5000).as_bytes());
        let s = decode_text(&v);
        assert!(s.starts_with("a\u{FFFD}é"));
        assert_eq!(s.chars().filter(|&c| c == 'é').count(), 5000);
    }

    #[test]
    fn legacy_encodings() {
        let (b, _, _) = encoding_rs::WINDOWS_1251.encode("Привет, мир! Это длинный русский текст для проверки.");
        assert!(decode_text(&b).starts_with("Привет, мир!"));
        let html = b"<html><head><meta charset=\"windows-1251\"></head>\xcf\xf0\xe8\xe2\xe5\xf2";
        assert!(decode_text(html).ends_with("Привет"));
        let (b, _, _) = encoding_rs::SHIFT_JIS.encode("<?xml version=\"1.0\" encoding=\"Shift_JIS\"?>こんにちは世界");
        assert!(decode_text(&b).ends_with("こんにちは世界"));
    }
}
