//! Kindle "My Clippings.txt" import: highlights and notes are matched to
//! library books by title/author, then located by searching the highlight's
//! text in the book's own text, and stored as ordinary annotations.

use crate::book::{Content, Flow, Loc};
use crate::store::{Annotation, HlColor, HlStyle};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Highlight,
    Note,
    Other,
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub title: String,
    pub author: String,
    pub kind: Kind,
    pub loc: (u32, u32),
    pub text: String,
}

/// A library book a clipping can be matched to.
#[derive(Clone)]
pub struct Target {
    pub id: String,
    pub path: String,
    pub title: String,
    pub author: String,
}

#[derive(Default)]
pub struct Outcome {
    pub per_book: Vec<(String, Vec<Annotation>)>,
    /// Highlights whose book is not in the library.
    pub unmatched: usize,
    /// Highlights whose book matched but whose text could not be found.
    pub unlocated: usize,
    pub books: usize,
}

fn split_title(line: &str) -> (String, String) {
    let l = line.trim().trim_start_matches('\u{feff}').trim();
    if l.ends_with(')') {
        if let Some(i) = l.rfind(" (") {
            return (l[..i].trim().to_string(), l[i + 2..l.len() - 1].trim().to_string());
        }
    }
    (l.to_string(), String::new())
}

fn numbers(s: &str) -> (u32, u32) {
    let mut it = s.split(|c: char| !c.is_ascii_digit()).filter(|x| !x.is_empty()).filter_map(|x| x.parse::<u32>().ok());
    let a = it.next().unwrap_or(0);
    (a, it.next().unwrap_or(a))
}

pub fn parse(text: &str) -> Vec<Clip> {
    let mut out = Vec::new();
    for entry in text.split("==========") {
        let mut lines = entry.lines().map(|l| l.trim_end_matches('\r'));
        let Some(first) = lines.by_ref().find(|l| !l.trim().is_empty()) else { continue };
        let (title, author) = split_title(first);
        let Some(meta) = lines.next() else { continue };
        let low = meta.to_lowercase();
        let kind = if low.contains("highlight") {
            Kind::Highlight
        } else if low.contains("note") {
            Kind::Note
        } else {
            Kind::Other
        };
        let loc = low.find("location").map(|i| numbers(low[i..].split('|').next().unwrap_or(""))).unwrap_or((0, 0));
        let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        if title.is_empty() {
            continue;
        }
        out.push(Clip { title, author, kind, loc, text: body });
    }
    out
}

fn norm_key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2).map(|w| w.to_lowercase()).collect()
}

fn author_overlap(a: &str, b: &str) -> bool {
    let (wa, wb) = (words(a), words(b));
    wa.iter().any(|x| wb.contains(x))
}

pub fn find_book(books: &[Target], title: &str, author: &str) -> Option<usize> {
    let nt = norm_key(title);
    if nt.is_empty() {
        return None;
    }
    let mut best: Option<(usize, i32)> = None;
    for (i, b) in books.iter().enumerate() {
        let nb = norm_key(&b.title);
        if nb.is_empty() {
            continue;
        }
        let exact = nb == nt;
        let contains = nt.len() >= 6 && nb.len() >= 6 && (nb.contains(&nt) || nt.contains(&nb));
        let auth = !author.is_empty() && author_overlap(author, &b.author);
        let ok = exact || (contains && (auth || author.is_empty()));
        if !ok {
            continue;
        }
        let score = if exact { 4 } else { 2 } + if auth { 2 } else { 0 };
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((i, score));
        }
    }
    best.map(|b| b.0)
}

/// Lower-cased, whitespace-collapsed, quote-straightened characters, each with
/// the offset it came from in the original block.
fn norm_chars(s: &str) -> (Vec<char>, Vec<usize>) {
    let (mut v, mut map) = (Vec::new(), Vec::new());
    let mut space = true;
    for (i, c) in s.chars().enumerate() {
        if c.is_whitespace() {
            if !space {
                v.push(' ');
                map.push(i);
                space = true;
            }
            continue;
        }
        space = false;
        let c = match c {
            '\u{2018}' | '\u{2019}' | '\u{2032}' => '\'',
            '\u{201c}' | '\u{201d}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            c => c.to_lowercase().next().unwrap_or(c),
        };
        v.push(c);
        map.push(i);
    }
    while v.last() == Some(&' ') {
        v.pop();
        map.pop();
    }
    (v, map)
}

struct Hay {
    section: usize,
    block: usize,
    chars: Vec<char>,
    map: Vec<usize>,
}

fn haystack(flow: &Flow) -> Vec<Hay> {
    let mut v = Vec::new();
    for (si, s) in flow.sections.iter().enumerate() {
        for (bi, b) in s.blocks.iter().enumerate() {
            if b.char_len() == 0 {
                continue;
            }
            let (chars, map) = norm_chars(&b.text());
            v.push(Hay { section: si, block: bi, chars, map });
        }
    }
    v
}

fn pos(h: &[char], n: &[char]) -> Option<usize> {
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    let first = n[0];
    (0..=h.len() - n.len()).find(|&i| h[i] == first && h[i..i + n.len()] == *n)
}

/// Locate `needle` in the text: within one block, or starting in one block and
/// ending a few blocks later in the same section.
fn locate(hay: &[Hay], needle: &str) -> Option<(Loc, Loc)> {
    let (n, _) = norm_chars(needle);
    if n.len() < 3 {
        return None;
    }
    for h in hay {
        if let Some(p) = pos(&h.chars, &n) {
            let end = h.map[p + n.len() - 1] + 1;
            return Some((Loc::new(h.section, h.block, h.map[p]), Loc::new(h.section, h.block, end)));
        }
    }
    // spans blocks: match the opening and the closing words separately
    let k = n.len().min(40);
    if n.len() < 60 {
        return None;
    }
    let (head, tail) = (&n[..k], &n[n.len() - k..]);
    for (i, h) in hay.iter().enumerate() {
        let Some(p) = pos(&h.chars, head) else { continue };
        for t in hay[i + 1..].iter().take(8) {
            if t.section != h.section {
                break;
            }
            if let Some(q) = pos(&t.chars, tail) {
                return Some((Loc::new(h.section, h.block, h.map[p]), Loc::new(t.section, t.block, t.map[q + k - 1] + 1)));
            }
        }
    }
    None
}

pub fn import(text: &str, books: &[Target], ctx: &eframe::egui::Context) -> Outcome {
    let clips = parse(text);
    let mut out = Outcome::default();
    // group clips by matched book
    let mut by_book: Vec<(usize, Vec<&Clip>)> = Vec::new();
    for c in &clips {
        if c.kind == Kind::Other {
            continue;
        }
        let Some(bi) = find_book(books, &c.title, &c.author) else {
            if c.kind == Kind::Highlight {
                out.unmatched += 1;
            }
            continue;
        };
        match by_book.iter_mut().find(|g| g.0 == bi) {
            Some(g) => g.1.push(c),
            None => by_book.push((bi, vec![c])),
        }
    }
    let base = crate::store::now() as u64 * 1000;
    let mut next_id = 0u64;
    for (bi, cs) in by_book {
        let t = &books[bi];
        let highlights = cs.iter().filter(|c| c.kind == Kind::Highlight).count();
        let Ok(book) = crate::book::open(std::path::Path::new(&t.path)) else {
            out.unlocated += highlights;
            continue;
        };
        let Content::Flow(flow) = &book.content else {
            out.unlocated += highlights;
            continue;
        };
        let hay = haystack(flow);
        let mut anns: Vec<Annotation> = Vec::new();
        let mut spans: Vec<(u32, u32, usize)> = Vec::new(); // kindle location range -> annotation index
        for c in cs.iter().filter(|c| c.kind == Kind::Highlight) {
            match locate(&hay, &c.text) {
                Some((start, end)) => {
                    next_id += 1;
                    let chapter = book.toc_index_for(start).and_then(|i| book.toc.get(i)).map(|e| e.title.clone()).unwrap_or_default();
                    spans.push((c.loc.0, c.loc.1, anns.len()));
                    anns.push(Annotation { id: base + next_id, start, end, color: HlColor::Yellow, style: HlStyle::Highlight, note: String::new(), text: c.text.clone(), chapter, created: crate::store::now() });
                }
                None => out.unlocated += 1,
            }
        }
        // a Kindle note sits at (or inside) the location of the highlight it belongs to
        for c in cs.iter().filter(|c| c.kind == Kind::Note) {
            if let Some(&(_, _, ai)) = spans.iter().find(|s| c.loc.0 >= s.0 && c.loc.0 <= s.1.max(s.0) + 1) {
                anns[ai].note = c.text.clone();
            }
        }
        if !anns.is_empty() {
            anns.sort_by_key(|a| a.start);
            out.books += 1;
            out.per_book.push((t.id.clone(), anns));
        }
        ctx.request_repaint();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries() {
        let t = "\u{feff}Dune (Frank Herbert)\n- Your Highlight on page 12 | location 100-101 | Added on Monday, 1 January 2020 10:00:00\n\nFear is the mind-killer.\n==========\nDune (Frank Herbert)\n- Your Note on location 101 | Added on Monday\n\nso true\n==========\n";
        let c = parse(t);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].title, "Dune");
        assert_eq!(c[0].author, "Frank Herbert");
        assert_eq!(c[0].kind, Kind::Highlight);
        assert_eq!(c[0].loc, (100, 101));
        assert_eq!(c[1].kind, Kind::Note);
        assert_eq!(c[1].text, "so true");
    }

    #[test]
    fn matches_books() {
        let b = vec![Target { id: "a".into(), path: String::new(), title: "Dune".into(), author: "Frank Herbert".into() }, Target { id: "b".into(), path: String::new(), title: "Emma".into(), author: "Jane Austen".into() }];
        assert_eq!(find_book(&b, "Dune", "Herbert, Frank"), Some(0));
        assert_eq!(find_book(&b, "Nothing", ""), None);
    }
}
