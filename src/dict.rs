//! Offline dictionaries: StarDict (.ifo + .idx[.gz] + .dict[.dz] + optional .syn)
//! and dictd (.index + .dict[.dz]). Loaded lazily on the first lookup and cached.
//! dictzip files are read with random access, so big dictionaries stay on disk.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use flate2::read::{DeflateDecoder, GzDecoder};

enum Format {
    /// sametypesequence from the .ifo (may be empty)
    StarDict(String),
    Dictd,
}

struct Entry {
    key: String,
    word: String,
    off: u64,
    size: u32,
}

pub struct Dict {
    pub name: String,
    format: Format,
    entries: Vec<Entry>,
    /// indices into `entries`, sorted by key
    sorted: Vec<u32>,
    /// synonym key -> entry index, sorted by key
    syn: Vec<(String, u32)>,
    data: PathBuf,
    dz: Option<DictZip>,
}

/// dictzip random-access table (the gzip "RA" extra field).
struct DictZip {
    chunk_len: usize,
    /// start offset of each compressed chunk in the file, plus the end
    starts: Vec<u64>,
}

fn key_of(w: &str) -> String {
    w.trim().to_lowercase()
}

fn gunzip(path: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut out = Vec::new();
    GzDecoder::new(f).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Read a plain or .gz file fully.
fn read_maybe_gz(path: &Path) -> Option<Vec<u8>> {
    if path.extension().is_some_and(|e| e == "gz") {
        gunzip(path)
    } else {
        std::fs::read(path).ok()
    }
}

fn parse_dictzip(path: &Path) -> Option<DictZip> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = [0u8; 10];
    f.read_exact(&mut h).ok()?;
    if h[0] != 0x1f || h[1] != 0x8b || h[3] & 4 == 0 {
        return None;
    }
    let mut xl = [0u8; 2];
    f.read_exact(&mut xl).ok()?;
    let xlen = u16::from_le_bytes(xl) as usize;
    let mut extra = vec![0u8; xlen];
    f.read_exact(&mut extra).ok()?;
    let mut i = 0;
    let mut ra = None;
    while i + 4 <= extra.len() {
        let len = u16::from_le_bytes([extra[i + 2], extra[i + 3]]) as usize;
        if &extra[i..i + 2] == b"RA" && i + 4 + len <= extra.len() {
            ra = Some(&extra[i + 4..i + 4 + len]);
            break;
        }
        i += 4 + len;
    }
    let ra = ra?;
    if ra.len() < 6 {
        return None;
    }
    let chunk_len = u16::from_le_bytes([ra[2], ra[3]]) as usize;
    let n = u16::from_le_bytes([ra[4], ra[5]]) as usize;
    if chunk_len == 0 || ra.len() < 6 + 2 * n {
        return None;
    }
    // after the extra field: optional name, comment, header crc
    let mut pos = 10 + 2 + xlen as u64;
    let mut b = [0u8; 1];
    for flag in [8u8, 16u8] {
        if h[3] & flag != 0 {
            loop {
                f.seek(SeekFrom::Start(pos)).ok()?;
                f.read_exact(&mut b).ok()?;
                pos += 1;
                if b[0] == 0 {
                    break;
                }
            }
        }
    }
    if h[3] & 2 != 0 {
        pos += 2;
    }
    let mut starts = Vec::with_capacity(n + 1);
    let mut at = pos;
    for k in 0..n {
        starts.push(at);
        at += u16::from_le_bytes([ra[6 + 2 * k], ra[7 + 2 * k]]) as u64;
    }
    starts.push(at);
    Some(DictZip { chunk_len, starts })
}

impl Dict {
    fn record(&self, e: &Entry) -> Option<Vec<u8>> {
        let (off, size) = (e.off, e.size as usize);
        if size > 8 * 1024 * 1024 {
            return None;
        }
        let mut f = std::fs::File::open(&self.data).ok()?;
        match &self.dz {
            None => {
                f.seek(SeekFrom::Start(off)).ok()?;
                let mut buf = vec![0u8; size];
                f.read_exact(&mut buf).ok()?;
                Some(buf)
            }
            Some(z) => {
                let first = off as usize / z.chunk_len;
                let last = ((off as usize + size).saturating_sub(1)) / z.chunk_len;
                let mut out = Vec::new();
                for c in first..=last {
                    let (a, b) = (*z.starts.get(c)?, *z.starts.get(c + 1)?);
                    f.seek(SeekFrom::Start(a)).ok()?;
                    let mut comp = vec![0u8; (b - a) as usize];
                    f.read_exact(&mut comp).ok()?;
                    DeflateDecoder::new(&comp[..]).read_to_end(&mut out).ok()?;
                }
                let skip = off as usize - first * z.chunk_len;
                out.get(skip..skip + size).map(|s| s.to_vec())
            }
        }
    }

    /// Render the definition(s) stored for an entry as plain text.
    fn definition(&self, e: &Entry) -> String {
        let Some(raw) = self.record(e) else { return String::new() };
        let text = match &self.format {
            Format::Dictd => strip_markup(&String::from_utf8_lossy(&raw)),
            Format::StarDict(seq) => decode_fields(&raw, seq),
        };
        text.trim().to_string()
    }

    fn find(&self, key: &str) -> Vec<&Entry> {
        let mut out: Vec<&Entry> = Vec::new();
        let lo = self.sorted.partition_point(|&i| self.entries[i as usize].key.as_str() < key);
        for &i in &self.sorted[lo..] {
            let e = &self.entries[i as usize];
            if e.key != key {
                break;
            }
            out.push(e);
        }
        let lo = self.syn.partition_point(|(k, _)| k.as_str() < key);
        for (k, i) in &self.syn[lo..] {
            if k != key {
                break;
            }
            if let Some(e) = self.entries.get(*i as usize) {
                if !out.iter().any(|x| std::ptr::eq(*x, e)) {
                    out.push(e);
                }
            }
        }
        out
    }
}

fn decode_fields(raw: &[u8], seq: &str) -> String {
    let mut out = String::new();
    let mut add = |kind: u8, bytes: &[u8]| match kind {
        b'm' | b'l' | b'g' | b'x' | b'h' | b'k' | b't' | b'y' | b'w' | b'r' => {
            let s = strip_markup(&String::from_utf8_lossy(bytes));
            if !s.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&s);
            }
        }
        _ => {}
    };
    let mut pos = 0;
    let seq_b = seq.as_bytes();
    if seq_b.is_empty() {
        // every field carries its own type byte
        while pos < raw.len() {
            let kind = raw[pos];
            pos += 1;
            if kind.is_ascii_uppercase() {
                let Some(l) = raw.get(pos..pos + 4) else { break };
                let n = u32::from_be_bytes([l[0], l[1], l[2], l[3]]) as usize;
                pos += 4;
                let Some(b) = raw.get(pos..pos + n) else { break };
                add(kind, b);
                pos += n;
            } else {
                let end = raw[pos..].iter().position(|&b| b == 0).map(|p| pos + p).unwrap_or(raw.len());
                add(kind, &raw[pos..end]);
                pos = end + 1;
            }
        }
        return out;
    }
    for (i, &kind) in seq_b.iter().enumerate() {
        if pos > raw.len() {
            break;
        }
        let last = i + 1 == seq_b.len();
        if last {
            add(kind, &raw[pos..]);
        } else if kind.is_ascii_uppercase() {
            let Some(l) = raw.get(pos..pos + 4) else { break };
            let n = u32::from_be_bytes([l[0], l[1], l[2], l[3]]) as usize;
            pos += 4;
            let Some(b) = raw.get(pos..pos + n) else { break };
            add(kind, b);
            pos += n;
        } else {
            let end = raw[pos..].iter().position(|&b| b == 0).map(|p| pos + p).unwrap_or(raw.len());
            add(kind, &raw[pos..end]);
            pos = end + 1;
        }
    }
    out
}

/// Strip simple HTML / pango / XDXF markup to readable plain text.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        let mut tag = String::new();
        let mut closed = false;
        for t in chars.by_ref() {
            if t == '>' {
                closed = true;
                break;
            }
            tag.push(t);
        }
        if !closed {
            out.push('<');
            out.push_str(&tag);
            break;
        }
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
        match name.as_str() {
            "br" | "p" | "div" | "tr" | "ex" | "dtrn" | "def" | "co" => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            "li" if !tag.starts_with('/') => {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("• ");
            }
            "li" => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    let decoded = crate::book::html::decode_entities(&out).into_owned();
    let mut res = String::new();
    let mut blank = 0;
    for line in decoded.lines() {
        let l = line.trim();
        if l.is_empty() {
            blank += 1;
            if blank > 1 || res.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        res.push_str(l);
        res.push('\n');
    }
    res.trim().to_string()
}

fn parse_ifo(text: &str) -> HashMap<String, String> {
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect()
}

fn find_sibling(dir: &Path, stem: &str, exts: &[&str]) -> Option<PathBuf> {
    exts.iter().map(|e| dir.join(format!("{stem}.{e}"))).find(|p| p.is_file())
}

fn finish(name: String, format: Format, entries: Vec<Entry>, syn: Vec<(String, u32)>, data: PathBuf) -> Dict {
    let mut sorted: Vec<u32> = (0..entries.len() as u32).collect();
    sorted.sort_by(|&a, &b| entries[a as usize].key.cmp(&entries[b as usize].key));
    let mut syn = syn;
    syn.sort();
    let dz = if data.extension().is_some_and(|e| e == "dz") { parse_dictzip(&data) } else { None };
    Dict { name, format, entries, sorted, syn, data, dz }
}

pub fn load_stardict(ifo: &Path) -> Option<Dict> {
    let meta = parse_ifo(&std::fs::read_to_string(ifo).ok()?);
    let dir = ifo.parent()?;
    let stem = ifo.file_stem()?.to_str()?;
    let idx = read_maybe_gz(&find_sibling(dir, stem, &["idx", "idx.gz"])?)?;
    let data = find_sibling(dir, stem, &["dict", "dict.dz"])?;
    let off64 = meta.get("idxoffsetbits").is_some_and(|v| v == "64");
    let w = if off64 { 8 } else { 4 };
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos < idx.len() {
        let Some(nul) = idx[pos..].iter().position(|&b| b == 0) else { break };
        let word = String::from_utf8_lossy(&idx[pos..pos + nul]).into_owned();
        pos += nul + 1;
        let Some(b) = idx.get(pos..pos + w + 4) else { break };
        let off = if off64 { u64::from_be_bytes(b[..8].try_into().ok()?) } else { u32::from_be_bytes(b[..4].try_into().ok()?) as u64 };
        let size = u32::from_be_bytes(b[w..w + 4].try_into().ok()?);
        pos += w + 4;
        entries.push(Entry { key: key_of(&word), word, off, size });
    }
    let mut syn = Vec::new();
    if let Some(sp) = find_sibling(dir, stem, &["syn", "syn.gz"]) {
        if let Some(sb) = read_maybe_gz(&sp) {
            let mut p = 0;
            while p < sb.len() {
                let Some(nul) = sb[p..].iter().position(|&b| b == 0) else { break };
                let w = String::from_utf8_lossy(&sb[p..p + nul]).into_owned();
                p += nul + 1;
                let Some(b) = sb.get(p..p + 4) else { break };
                syn.push((key_of(&w), u32::from_be_bytes(b.try_into().ok()?)));
                p += 4;
            }
        }
    }
    let name = meta.get("bookname").cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| stem.to_string());
    let seq = meta.get("sametypesequence").cloned().unwrap_or_default();
    Some(finish(name, Format::StarDict(seq), entries, syn, data))
}

fn b64(s: &str) -> Option<u64> {
    let mut v = 0u64;
    for c in s.bytes() {
        let d = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        v = v.checked_mul(64)?.checked_add(d as u64)?;
    }
    Some(v)
}

pub fn load_dictd(index: &Path) -> Option<Dict> {
    let dir = index.parent()?;
    let stem = index.file_stem()?.to_str()?;
    let data = find_sibling(dir, stem, &["dict", "dict.dz"])?;
    let text = String::from_utf8_lossy(&std::fs::read(index).ok()?).into_owned();
    let mut entries = Vec::new();
    let mut name = stem.to_string();
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(w), Some(o), Some(s)) = (it.next(), it.next(), it.next()) else { continue };
        let (Some(off), Some(size)) = (b64(o), b64(s)) else { continue };
        if w.starts_with("00-database-") {
            // metadata entries aren't headwords
            if w == "00-database-short" {
                name = format!("{stem}*{off}:{size}");
            }
            continue;
        }
        entries.push(Entry { key: key_of(w), word: w.to_string(), off, size: size as u32 });
    }
    let mut d = finish(stem.to_string(), Format::Dictd, entries, Vec::new(), data);
    if let Some(rest) = name.strip_prefix(&format!("{stem}*")) {
        if let Some((o, s)) = rest.split_once(':') {
            let e = Entry { key: String::new(), word: String::new(), off: o.parse().unwrap_or(0), size: s.parse().unwrap_or(0) };
            let t = d.definition(&e);
            if let Some(first) = t.lines().map(|l| l.trim()).find(|l| !l.is_empty() && !l.starts_with("00-database")) {
                d.name = first.chars().take(60).collect();
            }
        }
    }
    Some(d)
}

fn scan(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 3 {
                scan(&p, depth + 1, out);
            }
        } else if p.extension().is_some_and(|x| x == "ifo" || x == "index") {
            out.push(p);
        }
    }
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Option<Arc<Dict>>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<Dict>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_cached(p: &Path) -> Option<Arc<Dict>> {
    if let Some(d) = cache().lock().unwrap().get(p) {
        return d.clone();
    }
    let d = if p.extension().is_some_and(|e| e == "ifo") { load_stardict(p) } else { load_dictd(p) }.map(Arc::new);
    cache().lock().unwrap().insert(p.to_path_buf(), d.clone());
    d
}

/// All dictionaries found under the given folders.
pub fn load_all(dirs: &[String]) -> Vec<Arc<Dict>> {
    let mut files = Vec::new();
    for d in dirs {
        scan(Path::new(d), 0, &mut files);
    }
    files.sort();
    files.iter().filter_map(|p| load_cached(p)).collect()
}

/// How many dictionaries a folder contains (for the settings panel).
pub fn count_in(dir: &str) -> usize {
    let mut files = Vec::new();
    scan(Path::new(dir), 0, &mut files);
    files.len()
}

fn candidates(word: &str) -> Vec<String> {
    let w = key_of(word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '\''));
    let mut v = vec![w.clone()];
    let w2 = w.replace(['’', '‘'], "'");
    if w2 != w {
        v.push(w2.clone());
    }
    if let Some(b) = w2.strip_suffix("'s") {
        v.push(b.to_string());
    }
    for (suf, rep) in [("ies", "y"), ("es", ""), ("s", ""), ("ied", "y"), ("ed", ""), ("d", ""), ("ing", ""), ("ing", "e"), ("ly", ""), ("er", ""), ("est", "")] {
        if let Some(b) = w.strip_suffix(suf) {
            if b.chars().count() >= 3 {
                v.push(format!("{b}{rep}"));
                // doubled consonant: running -> run
                let mut cs: Vec<char> = b.chars().collect();
                if cs.len() >= 3 && cs[cs.len() - 1] == cs[cs.len() - 2] {
                    cs.pop();
                    v.push(cs.into_iter().collect::<String>() + rep);
                }
            }
        }
    }
    v.dedup();
    v
}

/// Look `word` up in every dictionary: (dictionary name, headword, definition).
pub fn lookup(dicts: &[Arc<Dict>], word: &str) -> Vec<(String, String, String)> {
    for cand in candidates(word) {
        if cand.is_empty() {
            continue;
        }
        let mut out = Vec::new();
        for d in dicts {
            for e in d.find(&cand).into_iter().take(4) {
                let def = d.definition(e);
                if !def.is_empty() {
                    out.push((d.name.clone(), e.word.clone(), def));
                }
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("seshat-dict-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn be32(n: u32) -> [u8; 4] {
        n.to_be_bytes()
    }

    #[test]
    fn markup() {
        assert_eq!(strip_markup("<b>run</b> &amp; walk<br>second <i>line</i>"), "run & walk\nsecond line");
        assert_eq!(strip_markup("<ul><li>a</li><li>b</li></ul>"), "• a\n• b");
    }

    #[test]
    fn stardict_roundtrip() {
        let dir = tmp("sd");
        let defs: [(&str, &str); 3] = [("Apple", "a <b>fruit</b>"), ("banana", "yellow &lt;thing&gt;"), ("cherry", "red")];
        let mut dict = Vec::new();
        let mut idx = Vec::new();
        for (w, d) in defs {
            idx.extend_from_slice(w.as_bytes());
            idx.push(0);
            idx.extend_from_slice(&be32(dict.len() as u32));
            idx.extend_from_slice(&be32(d.len() as u32));
            dict.extend_from_slice(d.as_bytes());
        }
        std::fs::write(dir.join("t.ifo"), "StarDict's dict ifo file\nversion=2.4.2\nbookname=Test\nwordcount=3\nsametypesequence=h\n").unwrap();
        // idx gzipped, dict plain; synonym "pomme" -> entry 0
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&idx).unwrap();
        std::fs::write(dir.join("t.idx.gz"), gz.finish().unwrap()).unwrap();
        std::fs::write(dir.join("t.dict"), &dict).unwrap();
        let mut syn = b"pomme\0".to_vec();
        syn.extend_from_slice(&be32(0));
        std::fs::write(dir.join("t.syn"), syn).unwrap();

        let d = Arc::new(load_stardict(&dir.join("t.ifo")).unwrap());
        assert_eq!(d.name, "Test");
        let r = lookup(&[d.clone()], "apple");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].2, "a fruit");
        assert_eq!(lookup(&[d.clone()], "Bananas.")[0].2, "yellow <thing>");
        assert_eq!(lookup(&[d.clone()], "pomme")[0].1, "Apple");
        assert!(lookup(&[d], "zzz").is_empty());
        let all = load_all(&[dir.to_string_lossy().into_owned()]);
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn dictzip_and_dictd() {
        let dir = tmp("dz");
        // two records; build a dictzip with 16-byte chunks
        let text = b"hello: a greeting. world: the earth, all of it.";
        let chunk = 16usize;
        let mut chunks = Vec::new();
        for c in text.chunks(chunk) {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(c).unwrap();
            chunks.push(e.finish().unwrap());
        }
        let mut ra = vec![1u8, 0];
        ra.extend_from_slice(&(chunk as u16).to_le_bytes());
        ra.extend_from_slice(&(chunks.len() as u16).to_le_bytes());
        for c in &chunks {
            ra.extend_from_slice(&(c.len() as u16).to_le_bytes());
        }
        let mut f = vec![0x1f, 0x8b, 8, 4, 0, 0, 0, 0, 0, 3];
        f.extend_from_slice(&((ra.len() + 4) as u16).to_le_bytes());
        f.extend_from_slice(b"RA");
        f.extend_from_slice(&(ra.len() as u16).to_le_bytes());
        f.extend_from_slice(&ra);
        for c in &chunks {
            f.extend_from_slice(c);
        }
        std::fs::write(dir.join("d.dict.dz"), f).unwrap();
        // dictd base64 offsets/sizes
        let off = |n: u64| -> String {
            let al = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = Vec::new();
            let mut n = n;
            loop {
                s.push(al[(n % 64) as usize]);
                n /= 64;
                if n == 0 {
                    break;
                }
            }
            s.reverse();
            String::from_utf8(s).unwrap()
        };
        let idx = format!("hello\t{}\t{}\nworld\t{}\t{}\n", off(0), off(18), off(19), off(28));
        std::fs::write(dir.join("d.index"), idx).unwrap();
        let d = Arc::new(load_dictd(&dir.join("d.index")).unwrap());
        assert!(d.dz.is_some());
        let r = lookup(&[d.clone()], "world");
        assert_eq!(r[0].2, "world: the earth, all of it.");
        assert_eq!(lookup(&[d], "hello")[0].2, "hello: a greeting.");
    }
}
