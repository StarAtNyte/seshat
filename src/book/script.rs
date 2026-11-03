//! Script helpers: right-to-left detection, Arabic contextual shaping, bidi
//! mirroring, vertical-CJK detection and the hyphenation language of a book.
//!
//! egui draws one glyph per char with no shaping, so Arabic letters are mapped
//! to their presentation forms (isolated/final/initial/medial, lam-alef
//! ligatures) before layout. All of this is display-only: the document model
//! keeps the logical text.

use std::path::Path;

use super::{Book, Content};

/// Strong right-to-left character (Hebrew, Arabic, Syriac, Thaana, NKo, ... and
/// their presentation forms).
pub fn is_rtl_char(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
        && !is_transparent(c)
        && !matches!(c as u32, 0x0660..=0x0669 | 0x06F0..=0x06F9)
}

pub fn has_rtl(s: &str) -> bool {
    s.chars().any(is_rtl_char)
}

pub fn is_arabic_joining(c: char) -> bool {
    matches!(c as u32, 0x0600..=0x06FF | 0x0750..=0x077F)
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x11FF | 0x2E80..=0x2FDF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x31FF | 0x3200..=0x9FFF | 0xA960..=0xA97F | 0xAC00..=0xD7FF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// Arabic combining marks (harakat etc.) that do not break joining.
fn is_transparent(c: char) -> bool {
    matches!(c as u32, 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7 | 0x06E8 | 0x06EA..=0x06ED)
}

/// (letter, isolated, final, initial, medial); 0 = form doesn't exist (right-joining).
const FORMS: &[(u16, u16, u16, u16, u16)] = &[
    (0x0621, 0xFE80, 0, 0, 0),
    (0x0622, 0xFE81, 0xFE82, 0, 0),
    (0x0623, 0xFE83, 0xFE84, 0, 0),
    (0x0624, 0xFE85, 0xFE86, 0, 0),
    (0x0625, 0xFE87, 0xFE88, 0, 0),
    (0x0626, 0xFE89, 0xFE8A, 0xFE8B, 0xFE8C),
    (0x0627, 0xFE8D, 0xFE8E, 0, 0),
    (0x0628, 0xFE8F, 0xFE90, 0xFE91, 0xFE92),
    (0x0629, 0xFE93, 0xFE94, 0, 0),
    (0x062A, 0xFE95, 0xFE96, 0xFE97, 0xFE98),
    (0x062B, 0xFE99, 0xFE9A, 0xFE9B, 0xFE9C),
    (0x062C, 0xFE9D, 0xFE9E, 0xFE9F, 0xFEA0),
    (0x062D, 0xFEA1, 0xFEA2, 0xFEA3, 0xFEA4),
    (0x062E, 0xFEA5, 0xFEA6, 0xFEA7, 0xFEA8),
    (0x062F, 0xFEA9, 0xFEAA, 0, 0),
    (0x0630, 0xFEAB, 0xFEAC, 0, 0),
    (0x0631, 0xFEAD, 0xFEAE, 0, 0),
    (0x0632, 0xFEAF, 0xFEB0, 0, 0),
    (0x0633, 0xFEB1, 0xFEB2, 0xFEB3, 0xFEB4),
    (0x0634, 0xFEB5, 0xFEB6, 0xFEB7, 0xFEB8),
    (0x0635, 0xFEB9, 0xFEBA, 0xFEBB, 0xFEBC),
    (0x0636, 0xFEBD, 0xFEBE, 0xFEBF, 0xFEC0),
    (0x0637, 0xFEC1, 0xFEC2, 0xFEC3, 0xFEC4),
    (0x0638, 0xFEC5, 0xFEC6, 0xFEC7, 0xFEC8),
    (0x0639, 0xFEC9, 0xFECA, 0xFECB, 0xFECC),
    (0x063A, 0xFECD, 0xFECE, 0xFECF, 0xFED0),
    (0x0641, 0xFED1, 0xFED2, 0xFED3, 0xFED4),
    (0x0642, 0xFED5, 0xFED6, 0xFED7, 0xFED8),
    (0x0643, 0xFED9, 0xFEDA, 0xFEDB, 0xFEDC),
    (0x0644, 0xFEDD, 0xFEDE, 0xFEDF, 0xFEE0),
    (0x0645, 0xFEE1, 0xFEE2, 0xFEE3, 0xFEE4),
    (0x0646, 0xFEE5, 0xFEE6, 0xFEE7, 0xFEE8),
    (0x0647, 0xFEE9, 0xFEEA, 0xFEEB, 0xFEEC),
    (0x0648, 0xFEED, 0xFEEE, 0, 0),
    (0x0649, 0xFEEF, 0xFEF0, 0xFBE8, 0xFBE9),
    (0x064A, 0xFEF1, 0xFEF2, 0xFEF3, 0xFEF4),
    (0x0671, 0xFB50, 0xFB51, 0, 0),
    (0x0679, 0xFB66, 0xFB67, 0xFB68, 0xFB69),
    (0x067E, 0xFB56, 0xFB57, 0xFB58, 0xFB59),
    (0x0686, 0xFB7A, 0xFB7B, 0xFB7C, 0xFB7D),
    (0x0688, 0xFB88, 0xFB89, 0, 0),
    (0x0691, 0xFB8C, 0xFB8D, 0, 0),
    (0x0698, 0xFB8A, 0xFB8B, 0, 0),
    (0x06A4, 0xFB6A, 0xFB6B, 0xFB6C, 0xFB6D),
    (0x06A9, 0xFB8E, 0xFB8F, 0xFB90, 0xFB91),
    (0x06AD, 0xFBD3, 0xFBD4, 0xFBD5, 0xFBD6),
    (0x06AF, 0xFB92, 0xFB93, 0xFB94, 0xFB95),
    (0x06BA, 0xFB9E, 0xFB9F, 0, 0),
    (0x06BE, 0xFBAA, 0xFBAB, 0xFBAC, 0xFBAD),
    (0x06C0, 0xFBA4, 0xFBA5, 0, 0),
    (0x06C1, 0xFBA6, 0xFBA7, 0xFBA8, 0xFBA9),
    (0x06CC, 0xFBFC, 0xFBFD, 0xFBFE, 0xFBFF),
    (0x06D2, 0xFBAE, 0xFBAF, 0, 0),
];

fn forms(c: char) -> Option<&'static (u16, u16, u16, u16, u16)> {
    let u = c as u32;
    if !(0x0621..=0x06D2).contains(&u) {
        return None;
    }
    FORMS.binary_search_by_key(&(u as u16), |f| f.0).ok().map(|i| &FORMS[i])
}

/// Joins to the following letter (dual-joining, or tatweel / ZWJ).
fn joins_next(c: char) -> bool {
    c == '\u{640}' || c == '\u{200d}' || forms(c).is_some_and(|f| f.3 != 0)
}

/// Joins to the preceding letter.
fn joins_prev(c: char) -> bool {
    c == '\u{640}' || c == '\u{200d}' || forms(c).is_some_and(|f| f.2 != 0)
}

/// Contextual Arabic shaping in logical order. Returns the display chars, each
/// paired with the index of the logical char it came from (lam-alef
/// ligatures consume two logical chars: the pair gets the lam's index).
pub fn shape_arabic(chars: &[char]) -> Vec<(char, usize)> {
    let n = chars.len();
    let prev_letter = |i: usize| (0..i).rev().find(|&j| !is_transparent(chars[j])).map(|j| chars[j]);
    let next_letter = |i: usize| (i + 1..n).find(|&j| !is_transparent(chars[j])).map(|j| (j, chars[j]));
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let Some(f) = forms(c) else {
            out.push((c, i));
            i += 1;
            continue;
        };
        let jp = prev_letter(i).is_some_and(joins_next) && f.2 != 0;
        // lam + alef → ligature
        if c == '\u{644}' {
            if let Some((j, a)) = next_letter(i) {
                let lig = match a {
                    '\u{622}' => Some((0xFEF5, 0xFEF6)),
                    '\u{623}' => Some((0xFEF7, 0xFEF8)),
                    '\u{625}' => Some((0xFEF9, 0xFEFA)),
                    '\u{627}' => Some((0xFEFB, 0xFEFC)),
                    _ => None,
                };
                if let Some((iso, fin)) = lig {
                    let cp = if jp { fin } else { iso };
                    out.push((char::from_u32(cp).unwrap_or(c), i));
                    // keep any marks between lam and alef
                    for (k, &m) in chars.iter().enumerate().take(j).skip(i + 1) {
                        out.push((m, k));
                    }
                    i = j + 1;
                    continue;
                }
            }
        }
        let jn = f.3 != 0 && next_letter(i).is_some_and(|(_, d)| joins_prev(d));
        let cp = match (jp, jn) {
            (true, true) => f.4,
            (false, true) => f.3,
            (true, false) => f.2,
            (false, false) => f.1,
        };
        out.push((char::from_u32(cp as u32).filter(|_| cp != 0).unwrap_or(c), i));
        i += 1;
    }
    out
}

/// Mirrored glyph for a char shown inside a right-to-left run.
pub fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        '≤' => '≥',
        '≥' => '≤',
        _ => c,
    }
}

/// Primary subtag of a BCP-47 language tag, lowercased.
pub fn primary_lang(lang: &str) -> String {
    lang.trim().split(['-', '_']).next().unwrap_or("").to_ascii_lowercase()
}

pub fn is_rtl_lang(lang: &str) -> bool {
    matches!(primary_lang(lang).as_str(), "ar" | "ara" | "he" | "heb" | "iw" | "fa" | "per" | "fas" | "ur" | "urd" | "yi" | "ps" | "sd" | "ug" | "dv" | "ckb" | "syr" | "arc")
}

/// Hyphenation patterns for a book language, when bundled.
pub fn hyph_lang(lang: &str) -> Option<hypher::Lang> {
    let p = primary_lang(lang);
    let p = match p.as_str() {
        "eng" => "en",
        "ger" | "deu" => "de",
        "fre" | "fra" => "fr",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "dut" | "nld" => "nl",
        "rus" => "ru",
        "nb" | "nn" => "no",
        x => x,
    };
    let b = p.as_bytes();
    if b.len() != 2 {
        return None;
    }
    hypher::Lang::from_iso([b[0], b[1]])
}

fn is_cjk_lang(lang: &str) -> bool {
    matches!(primary_lang(lang).as_str(), "ja" | "jpn" | "zh" | "chi" | "zho" | "ko" | "kor")
}

/// Decide the book's page direction and writing mode: language, a sample of
/// the text, and (EPUB) the spine's page-progression-direction and the CSS
/// `writing-mode`.
pub fn detect_direction(path: &Path, book: &mut Book) {
    if book.meta.language.is_empty() && matches!(super::ext_of(path).as_str(), "html" | "htm" | "xhtml") {
        book.meta.language = html_lang(path).unwrap_or_default();
    }
    let Content::Flow(f) = &book.content else { return };
    let (mut rtl_n, mut ltr_n, mut cjk_n, mut seen) = (0usize, 0usize, 0usize, 0usize);
    'outer: for s in &f.sections {
        for b in &s.blocks {
            for sp in &b.spans {
                for c in sp.text.chars() {
                    if is_rtl_char(c) {
                        rtl_n += 1;
                    } else if is_cjk(c) {
                        cjk_n += 1;
                    } else if c.is_alphabetic() {
                        ltr_n += 1;
                    }
                    seen += 1;
                    if seen > 40_000 {
                        break 'outer;
                    }
                }
            }
        }
    }
    let letters = (rtl_n + ltr_n + cjk_n).max(1);
    let rtl_text = is_rtl_lang(&book.meta.language) || rtl_n * 2 > letters;
    let cjk_text = is_cjk_lang(&book.meta.language) || cjk_n * 2 > letters;
    let (ppd_rtl, vertical_css) = if super::ext_of(path) == "epub" { epub_hints(path) } else { (false, false) };
    book.meta.vertical = vertical_css && cjk_text;
    // Japanese/Chinese with page-progression rtl but laid out horizontally reads
    // left-to-right, so rtl page order only follows rtl text or vertical layout.
    book.meta.rtl = rtl_text || book.meta.vertical;
    let _ = ppd_rtl;
}

/// `lang` / `xml:lang` of a standalone HTML file's root element.
fn html_lang(path: &Path) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = std::io::Read::read(&mut std::fs::File::open(path).ok()?, &mut buf).ok()?;
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    let i = head.to_ascii_lowercase().find("<html")?;
    let tag = &head[i..head[i..].find('>').map(|e| i + e).unwrap_or(head.len())];
    let at = tag.find(" lang=").map(|p| p + 6).or_else(|| tag.find(":lang=").map(|p| p + 6))?;
    let q = tag[at..].chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    tag[at + 1..].split(q).next().map(|s| s.to_string())
}

/// (spine page-progression-direction is rtl, some stylesheet sets writing-mode: vertical-rl)
fn epub_hints(path: &Path) -> (bool, bool) {
    let Ok(file) = std::fs::File::open(path) else { return (false, false) };
    let Ok(mut zip) = zip::ZipArchive::new(std::io::BufReader::new(file)) else { return (false, false) };
    let read = |zip: &mut zip::ZipArchive<std::io::BufReader<std::fs::File>>, name: &str| -> Option<String> {
        let mut e = zip.by_name(name).ok()?;
        if e.size() > 4 << 20 {
            return None;
        }
        let mut s = String::new();
        std::io::Read::read_to_string(&mut e, &mut s).ok()?;
        Some(s)
    };
    let mut ppd = false;
    if let Some(c) = read(&mut zip, "META-INF/container.xml") {
        if let Some(i) = c.find("full-path=") {
            let rest = &c[i + 10..];
            let q = rest.chars().next().unwrap_or('"');
            let opf = rest[1..].split(q).next().unwrap_or("");
            if let Some(o) = read(&mut zip, opf) {
                if let Some(sp) = o.find("<spine").or_else(|| o.find(":spine")) {
                    let tag = &o[sp..o[sp..].find('>').map(|e| sp + e).unwrap_or(o.len())];
                    ppd = tag.contains("page-progression-direction=\"rtl\"") || tag.contains("page-progression-direction='rtl'");
                }
            }
        }
    }
    let names: Vec<String> = zip.file_names().filter(|n| n.to_ascii_lowercase().ends_with(".css")).map(|s| s.to_string()).collect();
    let mut vertical = false;
    for n in names.iter().take(40) {
        if let Some(css) = read(&mut zip, n) {
            if super::css::declares_vertical(&css) {
                vertical = true;
                break;
            }
        }
    }
    (ppd, vertical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_salam() {
        // سلام: seen initial, lam-alef final ligature, meem isolated
        let s: Vec<char> = "سلام".chars().collect();
        let out: String = shape_arabic(&s).into_iter().map(|c| c.0).collect();
        assert_eq!(out, "\u{FEB3}\u{FEFC}\u{FEE1}");
    }

    #[test]
    fn langs() {
        assert!(is_rtl_lang("ar-EG"));
        assert!(!is_rtl_lang("en"));
        assert!(hyph_lang("en-US").is_some());
        assert!(hyph_lang("xx").is_none());
    }
}

/// A short single-line string (a title, an author) in display order for the
/// interface: Arabic letters shaped, right-to-left runs reordered. Plain
/// left-to-right text comes back untouched. Display only; never store it.
pub fn visual(s: &str) -> std::borrow::Cow<'_, str> {
    if !has_rtl(s) {
        return std::borrow::Cow::Borrowed(s);
    }
    let chars: Vec<char> = s.chars().collect();
    let shaped: String = shape_arabic(&chars).into_iter().map(|(c, _)| c).collect();
    let info = unicode_bidi::BidiInfo::new(&shaped, None);
    let mut out = String::with_capacity(shaped.len());
    for para in &info.paragraphs {
        let line = para.range.clone();
        let levels = info.reordered_levels(para, line.clone());
        let runs = info.visual_runs(para, line).1;
        for run in runs {
            let rtl = levels[run.start].is_rtl();
            let seg = &shaped[run];
            if rtl {
                out.extend(seg.chars().rev().map(mirror));
            } else {
                out.push_str(seg);
            }
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod visual_tests {
    use super::visual;

    #[test]
    fn latin_untouched_and_hebrew_reversed() {
        assert_eq!(visual("Ulysses"), "Ulysses");
        assert_eq!(visual("שלום"), "םולש");
        // mixed: the Latin word keeps its order
        assert!(visual("ספר Seshat").contains("Seshat"));
    }

    #[test]
    fn arabic_is_shaped() {
        let v = visual("كتاب");
        assert!(v.chars().all(|c| ('\u{FE70}'..='\u{FEFF}').contains(&c)), "{v:?}");
    }
}
