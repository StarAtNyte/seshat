//! EPUB3 Media Overlays: the book's own narration, read along with the text.
//!
//! A content document's manifest item names a SMIL file (`media-overlay="…"`)
//! whose `<par>` elements pair a text fragment (`chapter.xhtml#id`) with an
//! audio clip (`<audio src clipBegin clipEnd>`). [`build`] turns those into
//! clips located in the book's flow; [`start`] plays them from a location,
//! reporting each clip as a `tts::Event::At`, so highlighting and page
//! following work exactly as for read-aloud.

use crate::i18n::tr;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::book::html::{decode_entities, tokenize, Tok};
use crate::book::util::{join_path, parent_of, percent_decode, read_zip_entry, zip_read};
use crate::book::{Loc, Section};
use crate::tts::{Event, Speaker};

/// One narrated fragment: the text at `loc` up to char `end` of that block,
/// spoken by `audio[file]` from `begin` to `stop` seconds (end of file if None).
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub loc: Loc,
    pub end: usize,
    pub file: u32,
    pub begin: f64,
    pub stop: Option<f64>,
}

#[derive(Debug, Default)]
pub struct Overlay {
    /// The EPUB the audio lives in.
    pub path: PathBuf,
    /// Audio zip entries, indexed by `Clip::file`.
    pub files: Vec<String>,
    /// In reading order.
    pub clips: Vec<Clip>,
}

// ---------- parsing ----------

/// A SMIL clock value in seconds: "12.5s", "500ms", "2min", "1h", "12.5",
/// "00:01:02.500" (full) or "1:02" / "01:02.5" (partial, minutes:seconds).
pub fn parse_clock(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let num = |t: &str| -> Option<f64> { t.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0) };
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        let (h, m, sec) = match parts.as_slice() {
            [m, sec] => (0.0, num(m)?, num(sec)?),
            [h, m, sec] => (num(h)?, num(m)?, num(sec)?),
            _ => return None,
        };
        return Some(h * 3600.0 + m * 60.0 + sec);
    }
    for (suffix, mul) in [("ms", 0.001), ("min", 60.0), ("h", 3600.0), ("s", 1.0)] {
        if let Some(v) = s.strip_suffix(suffix) {
            return num(v).map(|v| v * mul);
        }
    }
    num(s)
}

/// A `<par>` as written: text href and audio href (both resolved against the
/// SMIL file's directory, fragment kept on the text one), clip times.
#[derive(Clone, Debug, PartialEq)]
pub struct RawPar {
    pub text: String,
    pub audio: String,
    pub begin: f64,
    pub stop: Option<f64>,
}

/// Every `<par>` in a SMIL document, in document order (nested `<seq>`s are
/// flattened). `smil_path` is the SMIL file's zip path.
pub fn parse_smil(xml: &str, smil_path: &str) -> Vec<RawPar> {
    let dir = parent_of(smil_path);
    let resolve = |href: &str| -> String {
        let h = href.trim();
        match h.split_once('#') {
            Some((f, frag)) => {
                let file = if f.is_empty() { String::new() } else { join_path(&dir, f) };
                format!("{file}#{frag}")
            }
            None => join_path(&dir, h),
        }
    };
    let mut out = Vec::new();
    let mut cur: Option<(String, String, f64, Option<f64>)> = None;
    let mut depth = 0usize; // nested <par> (invalid, but don't get lost)
    tokenize(xml, |_, t| match t {
        Tok::Start { name, attrs, self_closing } => {
            let a = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| decode_entities(v).into_owned());
            match name.as_str() {
                "par" if !self_closing => {
                    depth += 1;
                    if depth == 1 {
                        cur = Some((String::new(), String::new(), 0.0, None));
                    }
                }
                "text" => {
                    if let (Some(c), Some(src)) = (cur.as_mut(), a("src")) {
                        c.0 = resolve(&src);
                    }
                }
                "audio" => {
                    if let (Some(c), Some(src)) = (cur.as_mut(), a("src")) {
                        c.1 = resolve(&src);
                        c.2 = a("clipbegin").or_else(|| a("clip-begin")).and_then(|v| parse_clock(&v)).unwrap_or(0.0);
                        c.3 = a("clipend").or_else(|| a("clip-end")).and_then(|v| parse_clock(&v));
                    }
                }
                _ => {}
            }
        }
        Tok::End { name } if name == "par" => {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                if let Some((text, audio, begin, stop)) = cur.take() {
                    if !text.is_empty() && !audio.is_empty() && stop.is_none_or(|e| e > begin) {
                        out.push(RawPar { text, audio, begin, stop });
                    }
                }
            }
        }
        _ => {}
    });
    out
}

/// The text inside each element whose id is in `ids`.
fn element_texts(src: &str, ids: &HashSet<String>) -> HashMap<String, String> {
    const VOID: &[&str] = &["br", "img", "hr", "meta", "link", "input", "col", "area", "base", "wbr", "source", "embed", "param", "track"];
    let mut out: HashMap<String, String> = HashMap::new();
    let mut open: Vec<(String, usize, String)> = Vec::new(); // (id, depth at open, text)
    let mut depth = 0usize;
    tokenize(src, |_, t| match t {
        Tok::Start { name, attrs, self_closing } => {
            let id = attrs.iter().find(|(k, _)| k == "id").map(|(_, v)| v.clone()).filter(|v| ids.contains(v));
            if self_closing || VOID.contains(&name.as_str()) {
                if let Some(id) = id {
                    out.entry(id).or_default();
                }
                return;
            }
            depth += 1;
            if let Some(id) = id {
                open.push((id, depth, String::new()));
            }
        }
        Tok::End { .. } => {
            while open.last().is_some_and(|o| o.1 >= depth) {
                let (id, _, text) = open.pop().unwrap();
                out.entry(id).or_insert(text);
            }
            depth = depth.saturating_sub(1);
        }
        Tok::Text(s) => {
            if !open.is_empty() {
                let s = decode_entities(s);
                for o in &mut open {
                    o.2.push_str(&s);
                }
            }
        }
        Tok::Raw(_) => {}
    });
    for (id, _, text) in open {
        out.entry(id).or_insert(text);
    }
    out
}

fn norm(c: char) -> Option<char> {
    c.is_alphanumeric().then(|| c.to_lowercase().next().unwrap_or(c))
}

/// Find fragment text `frag` in block text `block` at or after char `from`,
/// ignoring case, spaces and punctuation: (start char, end char).
fn locate(block: &str, frag: &str, from: usize) -> Option<(usize, usize)> {
    let idx: Vec<(usize, char)> = block.chars().enumerate().filter_map(|(i, c)| norm(c).map(|n| (i, n))).collect();
    let want: Vec<char> = frag.chars().filter_map(norm).collect();
    if want.is_empty() || want.len() > idx.len() {
        return None;
    }
    let first = idx.iter().position(|&(i, _)| i >= from)?;
    (first..=idx.len() - want.len()).find(|&k| idx[k..k + want.len()].iter().map(|p| p.1).eq(want.iter().copied())).map(|k| (idx[k].0, idx[k + want.len() - 1].0 + 1))
}

/// Locate every par of `docs` (content document → its SMIL file, in spine
/// order). `resolve(file, fragment)` is the book's anchor mapping (block level);
/// offsets within a block come from matching the fragment's text.
pub fn build<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, epub: &Path, docs: &[(String, String)], sections: &[Section], resolve: &dyn Fn(&str, &str) -> Option<Loc>) -> Option<Overlay> {
    let mut ov = Overlay { path: epub.to_path_buf(), ..Default::default() };
    let mut file_ix: HashMap<String, u32> = HashMap::new();
    let mut seen_smil = HashSet::new();
    let block_text = |l: Loc| sections.get(l.section).and_then(|s| s.blocks.get(l.block)).map(|b| b.text()).unwrap_or_default();
    for (doc, smil) in docs {
        if !seen_smil.insert(smil.clone()) {
            continue;
        }
        let Ok(bytes) = zip_read(zip, smil) else { continue };
        let pars = parse_smil(&String::from_utf8_lossy(&bytes), smil);
        if pars.is_empty() {
            continue;
        }
        // fragment texts, per content document the pars point into
        let mut want: HashMap<String, HashSet<String>> = HashMap::new();
        for p in &pars {
            let (f, frag) = p.text.split_once('#').unwrap_or((&p.text, ""));
            let f = if f.is_empty() { doc.as_str() } else { f };
            want.entry(f.to_string()).or_default().insert(percent_decode(frag));
        }
        let mut texts: HashMap<String, HashMap<String, String>> = HashMap::new();
        for (f, ids) in &want {
            if let Ok(b) = zip_read(zip, f) {
                texts.insert(f.clone(), element_texts(&crate::book::util::decode_text(&b), ids));
            }
        }
        let mut cursor: Option<(Loc, usize)> = None; // last match: block, end char
        let first = ov.clips.len();
        for p in &pars {
            let (f, frag) = p.text.split_once('#').unwrap_or((&p.text, ""));
            let f = if f.is_empty() { doc.as_str() } else { f };
            let frag = percent_decode(frag);
            let Some(block) = resolve(f, &frag) else { continue };
            let block = Loc::new(block.section, block.block, 0);
            let bt = block_text(block);
            let len = bt.chars().count();
            let from = cursor.filter(|c| c.0 == block).map(|c| c.1).unwrap_or(0);
            let ft = texts.get(f).and_then(|m| m.get(&frag)).map(|s| s.as_str()).unwrap_or("");
            let prefix: String = ft.chars().filter(|c| c.is_alphanumeric()).take(20).collect();
            let (s, e) = locate(&bt, ft, from)
                .or_else(|| locate(&bt, ft, 0))
                .or_else(|| locate(&bt, &prefix, from).map(|(s, _)| (s, len)))
                .unwrap_or((if from > 0 && from < len { from } else { 0 }, len));
            cursor = Some((block, e));
            let n = file_ix.len() as u32;
            let file = *file_ix.entry(p.audio.clone()).or_insert_with(|| {
                ov.files.push(p.audio.clone());
                n
            });
            ov.clips.push(Clip { loc: Loc::new(block.section, block.block, s), end: e, file, begin: p.begin, stop: p.stop });
        }
        // Each fragment runs up to where the next one starts in the same block;
        // otherwise past its trailing punctuation.
        for i in first..ov.clips.len() {
            let next = ov.clips.get(i + 1).map(|c| c.loc);
            let c = &mut ov.clips[i];
            let bt: Vec<char> = block_text(c.loc).chars().collect();
            match next {
                Some(n) if n.section == c.loc.section && n.block == c.loc.block && n.offset > c.loc.offset => {
                    let mut e = n.offset;
                    while e > c.loc.offset && bt.get(e - 1).is_some_and(|ch| ch.is_whitespace()) {
                        e -= 1;
                    }
                    c.end = e.max(c.loc.offset + 1);
                }
                _ => {
                    while c.end < bt.len() && !bt[c.end].is_whitespace() && !bt[c.end].is_alphanumeric() {
                        c.end += 1;
                    }
                }
            }
        }
    }
    (!ov.clips.is_empty()).then_some(ov)
}

// ---------- playback ----------

/// One audio file, decoded (to mono 16-bit) progressively in the background.
struct Track {
    rate: AtomicU32,
    samples: Mutex<Vec<i16>>,
    done: AtomicBool,
    error: Mutex<Option<String>>,
    cancel: Arc<AtomicBool>,
}

impl Track {
    fn new(cancel: Arc<AtomicBool>) -> Track {
        Track { rate: AtomicU32::new(0), samples: Mutex::new(Vec::new()), done: AtomicBool::new(false), error: Mutex::new(None), cancel }
    }

    /// Decode `bytes` (format guessed from `name`'s extension, then sniffed).
    fn decode(&self, bytes: Vec<u8>, name: &str) {
        let r = self.decode_inner(bytes, name);
        if let Err(e) = r {
            *self.error.lock().unwrap() = Some(e);
        }
        self.done.store(true, Ordering::SeqCst);
    }

    fn decode_inner(&self, bytes: Vec<u8>, name: &str) -> Result<(), String> {
        use rodio::Source;
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let len = bytes.len() as u64;
        let mut b = rodio::Decoder::builder().with_data(Cursor::new(bytes)).with_byte_len(len).with_seekable(true);
        if !ext.is_empty() && ext.len() <= 4 {
            b = b.with_hint(&ext);
        }
        let dec = b.build().map_err(|e| trf!("Could not play the narration ({name}): {err}", name = name, err = e))?;
        let ch = dec.channels().max(1) as usize;
        self.rate.store(dec.sample_rate(), Ordering::SeqCst);
        let mut buf: Vec<i16> = Vec::with_capacity(16384);
        let (mut acc, mut n) = (0.0f32, 0usize);
        for s in dec {
            acc += s;
            n += 1;
            if n == ch {
                buf.push(((acc / ch as f32).clamp(-1.0, 1.0) * 32767.0) as i16);
                acc = 0.0;
                n = 0;
                if buf.len() == buf.capacity() {
                    if self.cancel.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    self.samples.lock().unwrap().extend_from_slice(&buf);
                    buf.clear();
                }
            }
        }
        self.samples.lock().unwrap().extend_from_slice(&buf);
        Ok(())
    }

    /// The samples of `begin..stop` seconds, once decoded that far: Some(Ok(samples)),
    /// None while still decoding.
    fn clip(&self, begin: f64, stop: Option<f64>) -> Option<Result<(u32, Vec<f32>), String>> {
        let done = self.done.load(Ordering::SeqCst);
        if let Some(e) = self.error.lock().unwrap().clone() {
            return Some(Err(e));
        }
        let rate = self.rate.load(Ordering::SeqCst);
        if rate == 0 {
            return if done { Some(Ok((1, Vec::new()))) } else { None };
        }
        let s = self.samples.lock().unwrap();
        let a = (begin * rate as f64) as usize;
        let b = stop.map(|t| (t * rate as f64) as usize);
        let ready = done || b.is_some_and(|b| s.len() >= b);
        if !ready {
            return None;
        }
        let b = b.unwrap_or(s.len()).min(s.len());
        let a = a.min(b);
        Some(Ok((rate, s[a..b].iter().map(|&v| v as f32 / 32768.0).collect())))
    }
}

/// The clip to start from for a reading position: the first one still ahead
/// of (or containing) `from`.
pub fn first_clip(ov: &Overlay, from: Loc) -> Option<usize> {
    ov.clips.iter().position(|c| (c.loc.section, c.loc.block, c.end) > (from.section, from.block, from.offset))
}

/// Play the narration from `from` at `rate`× speed, reporting each clip as it
/// starts. Stop it through the returned speaker.
pub fn start(ov: Arc<Overlay>, from: Loc, rate: f32, on: impl Fn(Event) + Send + 'static) -> Speaker {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::spawn(move || {
        let Some(mut next) = first_clip(&ov, from) else {
            on(Event::Error(tr("The book's narration ends before this point.").into()));
            return;
        };
        let mut stream = match rodio::OutputStreamBuilder::open_default_stream() {
            Ok(s) => s,
            Err(e) => {
                on(Event::Error(trf!("No audio output: {err}", err = e)));
                return;
            }
        };
        stream.log_on_drop(false);
        let sink = rodio::Sink::connect_new(stream.mixer());
        sink.set_speed(rate.clamp(0.25, 4.0));
        // decoded files: at most the one playing and the one being queued
        let mut tracks: HashMap<u32, Arc<Track>> = HashMap::new();
        let mut queued: std::collections::VecDeque<usize> = Default::default();
        let finish = |tracks: &HashMap<u32, Arc<Track>>| {
            for t in tracks.values() {
                t.cancel.store(true, Ordering::SeqCst);
            }
        };
        loop {
            if stop2.load(Ordering::SeqCst) {
                sink.stop();
                finish(&tracks);
                return;
            }
            // clips that finished playing
            while sink.len() < queued.len() {
                queued.pop_front();
                if let Some(&i) = queued.front() {
                    on(Event::At(ov.clips[i].loc, ov.clips[i].end));
                }
            }
            // keep the next clip queued behind the current one (gapless)
            while queued.len() < 2 && next < ov.clips.len() {
                let c = &ov.clips[next];
                let playing = queued.front().map(|&i| ov.clips[i].file);
                if !tracks.contains_key(&c.file) {
                    tracks.retain(|f, t| {
                        let keep = Some(*f) == playing;
                        if !keep {
                            t.cancel.store(true, Ordering::SeqCst);
                        }
                        keep
                    });
                    let t = Arc::new(Track::new(Arc::new(AtomicBool::new(false))));
                    let (t2, path, name) = (t.clone(), ov.path.clone(), ov.files[c.file as usize].clone());
                    std::thread::spawn(move || match read_zip_entry(&path, &name) {
                        Ok(b) => t2.decode(b, &name),
                        Err(e) => {
                            *t2.error.lock().unwrap() = Some(trf!("Could not read the narration ({name}): {err}", name = name, err = e));
                            t2.done.store(true, Ordering::SeqCst);
                        }
                    });
                    tracks.insert(c.file, t);
                }
                match tracks[&c.file].clip(c.begin, c.stop) {
                    None => break,
                    Some(Err(e)) => {
                        sink.stop();
                        finish(&tracks);
                        on(Event::Error(e));
                        return;
                    }
                    Some(Ok((r, samples))) => {
                        if !samples.is_empty() {
                            sink.append(rodio::buffer::SamplesBuffer::new(1, r, samples));
                            queued.push_back(next);
                            if queued.len() == 1 {
                                on(Event::At(c.loc, c.end));
                            }
                        }
                        next += 1;
                    }
                }
            }
            if queued.is_empty() && next >= ov.clips.len() {
                finish(&tracks);
                on(Event::Done);
                return;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    });
    Speaker::external(stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn clock_values() {
        let eq = |s: &str, v: f64| assert!((parse_clock(s).unwrap() - v).abs() < 1e-9, "{s}");
        eq("12.5s", 12.5);
        eq("500ms", 0.5);
        eq("2min", 120.0);
        eq("1h", 3600.0);
        eq("7", 7.0);
        eq("00:01:02.500", 62.5);
        eq("1:02:03", 3723.0);
        eq("1:02", 62.0);
        eq("01:02.25", 62.25);
        assert_eq!(parse_clock(""), None);
        assert_eq!(parse_clock("abc"), None);
        assert_eq!(parse_clock("1:2:3:4"), None);
    }

    #[test]
    fn smil_nested_seq() {
        let xml = r#"<?xml version="1.0"?>
<smil xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops" version="3.0">
 <body>
  <seq epub:textref="../text/ch1.xhtml" epub:type="chapter">
   <par id="p1"><text src="../text/ch1.xhtml#s1"/><audio src="../audio/ch1.mp3" clipBegin="0s" clipEnd="1.5s"/></par>
   <seq epub:textref="../text/ch1.xhtml#aside">
     <par><text src="../text/ch1.xhtml#s2"/><audio src="../audio/ch1.mp3" clipBegin="00:00:01.500" clipEnd="0:03"/></par>
   </seq>
   <par><text src="../text/ch1.xhtml#s3"/><audio src="../audio/ch1.mp3" clipBegin="3s"/></par>
   <par><text src="../text/ch1.xhtml#bad"/><audio src="../audio/ch1.mp3" clipBegin="5s" clipEnd="4s"/></par>
  </seq>
 </body>
</smil>"#;
        let p = parse_smil(xml, "OEBPS/smil/ch1.smil");
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], RawPar { text: "OEBPS/text/ch1.xhtml#s1".into(), audio: "OEBPS/audio/ch1.mp3".into(), begin: 0.0, stop: Some(1.5) });
        assert_eq!(p[1].text, "OEBPS/text/ch1.xhtml#s2");
        assert_eq!((p[1].begin, p[1].stop), (1.5, Some(3.0)));
        assert_eq!((p[2].begin, p[2].stop), (3.0, None));
    }

    #[test]
    fn locate_ignores_case_and_punctuation() {
        let b = "“Hello,” she said. Then   she LEFT!";
        assert_eq!(locate(b, "hello she said", 0), Some((1, 17)));
        assert_eq!(locate(b, "Then she left", 0), Some((19, 34)));
        assert_eq!(locate(b, "hello", 5), None);
    }

    /// A mono 16-bit WAV of `secs` seconds at 8 kHz.
    fn wav(secs: f32) -> Vec<u8> {
        let rate = 8000u32;
        let n = (secs * rate as f32) as u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + n * 2).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 2).to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(n * 2).to_le_bytes());
        for i in 0..n {
            let s = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 8000.0) as i16;
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    fn tiny_epub(path: &Path) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default();
        let mut put = |name: &str, data: &[u8]| {
            z.start_file(name, o).unwrap();
            z.write_all(data).unwrap();
        };
        put("mimetype", b"application/epub+zip");
        put("META-INF/container.xml", br#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#);
        put(
            "OEBPS/content.opf",
            br#"<package version="3.0"><metadata><dc:title>Narrated</dc:title><dc:language>en</dc:language></metadata>
<manifest>
 <item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml" media-overlay="o1"/>
 <item id="o1" href="smil/ch1.smil" media-type="application/smil+xml"/>
 <item id="a1" href="audio/ch1.wav" media-type="audio/wav"/>
</manifest>
<spine><itemref idref="c1"/></spine></package>"#,
        );
        put(
            "OEBPS/text/ch1.xhtml",
            br#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1 id="h">Chapter One</h1>
<p id="p1"><span id="s1">It was a dark night.</span> <span id="s2">The wind &amp; rain came.</span></p>
<p id="p2">Morning came at last.</p>
</body></html>"#,
        );
        put(
            "OEBPS/smil/ch1.smil",
            br#"<smil xmlns="http://www.w3.org/ns/SMIL" version="3.0"><body><seq>
<par><text src="../text/ch1.xhtml#h"/><audio src="../audio/ch1.wav" clipBegin="0s" clipEnd="0.25s"/></par>
<seq><par><text src="../text/ch1.xhtml#s1"/><audio src="../audio/ch1.wav" clipBegin="0.25s" clipEnd="500ms"/></par>
<par><text src="../text/ch1.xhtml#s2"/><audio src="../audio/ch1.wav" clipBegin="0:00.5" clipEnd="00:00:00.750"/></par></seq>
<par><text src="../text/ch1.xhtml#p2"/><audio src="../audio/ch1.wav" clipBegin="0.75s"/></par>
</seq></body></smil>"#,
        );
        put("OEBPS/audio/ch1.wav", &wav(1.0));
        z.finish().unwrap();
    }

    #[test]
    fn pars_map_to_locations_and_clips_decode() {
        let dir = std::env::temp_dir().join(format!("seshat-overlay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("narrated.epub");
        tiny_epub(&path);
        let book = crate::book::open(&path).unwrap();
        let ov = book.meta.overlay.clone().expect("overlay parsed");
        let flow = book.flow().unwrap();
        let text = |c: &Clip| -> String { flow.sections[c.loc.section].blocks[c.loc.block].text().chars().skip(c.loc.offset).take(c.end - c.loc.offset).collect() };
        assert_eq!(ov.clips.len(), 4);
        assert_eq!(ov.files, vec!["OEBPS/audio/ch1.wav".to_string()]);
        let t: Vec<String> = ov.clips.iter().map(text).collect();
        assert_eq!(t, vec!["Chapter One", "It was a dark night.", "The wind & rain came.", "Morning came at last."]);
        assert_eq!(ov.clips[1].loc.block, ov.clips[2].loc.block);
        assert!(ov.clips[2].loc.offset > 0);
        assert_eq!((ov.clips[2].begin, ov.clips[2].stop), (0.5, Some(0.75)));
        assert_eq!(ov.clips[3].stop, None);

        // starting point: the clip containing the reading position
        let mid = Loc::new(ov.clips[2].loc.section, ov.clips[2].loc.block, ov.clips[2].loc.offset + 3);
        assert_eq!(first_clip(&ov, mid), Some(2));
        assert_eq!(first_clip(&ov, Loc::new(0, 0, 0)), Some(0));

        // decode once, cut clips out of the buffer
        let track = Track::new(Arc::new(AtomicBool::new(false)));
        track.decode(read_zip_entry(&path, &ov.files[0]).unwrap(), &ov.files[0]);
        let (rate, a) = track.clip(0.25, Some(0.5)).unwrap().unwrap();
        assert_eq!(rate, 8000);
        assert_eq!(a.len(), 2000);
        let (_, rest) = track.clip(0.75, None).unwrap().unwrap();
        assert_eq!(rest.len(), 2000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
