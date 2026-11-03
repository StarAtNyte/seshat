//! Lenient HTML/XHTML tokenizer + converter into styled blocks.
//!
//! Tolerates the malformed markup common in MOBI files and sloppy EPUBs
//! (unquoted attributes, unclosed tags, stray `<`), so one pipeline serves
//! every HTML-based format.

use std::borrow::Cow;

use super::css::{self, Decl, Sheet};
use super::{Align, Block, BlockKind, Cell, Span, Style, Table};

// ---------- tokenizer ----------

pub enum Tok<'a> {
    Start { name: String, attrs: Vec<(String, String)>, self_closing: bool },
    End { name: String },
    /// Text with entities still encoded.
    Text(&'a str),
    /// Raw text that must not be entity-decoded (CDATA / <style> contents).
    Raw(&'a str),
}

fn find_ci(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > hay.len() || hay.len() < needle.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

fn local_name(raw: &str) -> String {
    let n = raw.rsplit(':').next().unwrap_or(raw);
    n.to_ascii_lowercase()
}

/// Tokenize `src`, calling `cb(byte_offset, token)`.
pub fn tokenize(src: &str, mut cb: impl FnMut(usize, Tok<'_>)) {
    let b = src.as_bytes();
    let len = b.len();
    let mut i = 0;
    let mut text_start = 0;
    while i < len {
        let Some(p) = b[i..].iter().position(|&c| c == b'<') else { break };
        i += p;
        if i + 1 >= len {
            break;
        }
        let next = b[i + 1];
        let is_tag = next.is_ascii_alphabetic() || next == b'/' || next == b'!' || next == b'?';
        if !is_tag {
            i += 1;
            continue;
        }
        if text_start < i {
            cb(text_start, Tok::Text(&src[text_start..i]));
        }
        let tag_pos = i;
        if b[i..].starts_with(b"<!--") {
            i = find(b, i + 4, b"-->").map(|e| e + 3).unwrap_or(len);
        } else if b[i..].starts_with(b"<![CDATA[") {
            let end = find(b, i + 9, b"]]>").unwrap_or(len);
            cb(tag_pos, Tok::Raw(&src[i + 9..end]));
            i = (end + 3).min(len);
        } else if next == b'!' || next == b'?' {
            i = find(b, i, b">").map(|e| e + 1).unwrap_or(len);
        } else if next == b'/' {
            let end = find(b, i, b">").unwrap_or(len);
            let name = src[i + 2..end].trim();
            cb(tag_pos, Tok::End { name: local_name(name) });
            i = (end + 1).min(len);
        } else {
            // start tag
            let mut j = i + 1;
            while j < len && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
                j += 1;
            }
            let raw_name = &src[i + 1..j];
            let name = local_name(raw_name);
            let mut attrs = Vec::new();
            let mut self_closing = false;
            loop {
                while j < len && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j >= len {
                    break;
                }
                if b[j] == b'>' {
                    j += 1;
                    break;
                }
                if b[j] == b'/' {
                    self_closing = true;
                    j += 1;
                    continue;
                }
                let ks = j;
                while j < len && !b[j].is_ascii_whitespace() && b[j] != b'=' && b[j] != b'>' && !(b[j] == b'/' && j + 1 < len && b[j + 1] == b'>') {
                    j += 1;
                }
                let key = src[ks..j].to_ascii_lowercase();
                while j < len && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                let mut val = String::new();
                if j < len && b[j] == b'=' {
                    j += 1;
                    while j < len && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < len && (b[j] == b'"' || b[j] == b'\'') {
                        let q = b[j];
                        let vs = j + 1;
                        let ve = b[vs..].iter().position(|&c| c == q).map(|p| p + vs).unwrap_or(len);
                        val = decode_entities(&src[vs..ve]).into_owned();
                        j = (ve + 1).min(len);
                    } else {
                        let vs = j;
                        while j < len && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                            j += 1;
                        }
                        val = decode_entities(&src[vs..j]).into_owned();
                    }
                }
                if !key.is_empty() {
                    attrs.push((key, val));
                } else {
                    j += 1;
                }
            }
            i = j;
            let raw_text = matches!(name.as_str(), "script" | "style") && !self_closing;
            cb(tag_pos, Tok::Start { name: name.clone(), attrs, self_closing });
            if raw_text {
                let close = format!("</{}", raw_name);
                let end = find_ci(b, i, close.as_bytes()).or_else(|| find_ci(b, i, format!("</{name}").as_bytes())).unwrap_or(len);
                if name == "style" {
                    cb(i, Tok::Raw(&src[i..end]));
                }
                cb(end, Tok::End { name: name.clone() });
                i = find(b, end, b">").map(|e| e + 1).unwrap_or(len);
            }
        }
        text_start = i;
    }
    if text_start < len {
        cb(text_start, Tok::Text(&src[text_start..]));
    }
}

const ENTITIES: &[(&str, char)] = &[
    ("amp", '&'), ("lt", '<'), ("gt", '>'), ("quot", '"'), ("apos", '\''), ("nbsp", '\u{a0}'),
    ("mdash", '—'), ("ndash", '–'), ("hellip", '…'), ("lsquo", '‘'), ("rsquo", '’'), ("ldquo", '“'),
    ("rdquo", '”'), ("sbquo", '‚'), ("bdquo", '„'), ("laquo", '«'), ("raquo", '»'), ("lsaquo", '‹'),
    ("rsaquo", '›'), ("copy", '©'), ("reg", '®'), ("trade", '™'), ("shy", '\u{ad}'), ("middot", '·'),
    ("bull", '•'), ("deg", '°'), ("plusmn", '±'), ("times", '×'), ("divide", '÷'), ("frac12", '½'),
    ("frac14", '¼'), ("frac34", '¾'), ("para", '¶'), ("sect", '§'), ("dagger", '†'), ("Dagger", '‡'),
    ("prime", '′'), ("Prime", '″'), ("euro", '€'), ("pound", '£'), ("yen", '¥'), ("cent", '¢'),
    ("iexcl", '¡'), ("iquest", '¿'), ("thinsp", '\u{2009}'), ("ensp", '\u{2002}'), ("emsp", '\u{2003}'),
    ("zwnj", '\u{200c}'), ("zwj", '\u{200d}'), ("larr", '←'), ("rarr", '→'), ("uarr", '↑'), ("darr", '↓'),
    ("hearts", '♥'), ("infin", '∞'), ("ne", '≠'), ("le", '≤'), ("ge", '≥'), ("minus", '−'), ("micro", 'µ'),
    ("ordf", 'ª'), ("ordm", 'º'), ("sup1", '¹'), ("sup2", '²'), ("sup3", '³'), ("acute", '´'), ("uml", '¨'),
    ("cedil", '¸'), ("macr", '¯'), ("szlig", 'ß'), ("aelig", 'æ'), ("AElig", 'Æ'), ("oelig", 'œ'), ("OElig", 'Œ'),
    ("oslash", 'ø'), ("Oslash", 'Ø'), ("aring", 'å'), ("Aring", 'Å'), ("ccedil", 'ç'), ("Ccedil", 'Ç'),
    ("ntilde", 'ñ'), ("Ntilde", 'Ñ'), ("eth", 'ð'), ("thorn", 'þ'), ("THORN", 'Þ'), ("alpha", 'α'), ("beta", 'β'),
    ("gamma", 'γ'), ("delta", 'δ'), ("pi", 'π'), ("sigma", 'σ'), ("omega", 'ω'), ("lambda", 'λ'), ("mu", 'μ'),
];

fn accent(name: &str) -> Option<char> {
    // &eacute; &Agrave; &ouml; &icirc; &atilde; ...
    let mut it = name.chars();
    let base = it.next()?;
    let acc = it.as_str();
    let table: &[(&str, &str, &str)] = &[
        ("acute", "aeiouyAEIOUY", "áéíóúýÁÉÍÓÚÝ"),
        ("grave", "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        ("circ", "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
        ("uml", "aeiouyAEIOUY", "äëïöüÿÄËÏÖÜŸ"),
        ("tilde", "aoAO", "ãõÃÕ"),
    ];
    for (suffix, from, to) in table {
        if acc == *suffix {
            let idx = from.chars().position(|c| c == base)?;
            return to.chars().nth(idx);
        }
    }
    None
}

pub fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| c == ';' || c == '&' || c.is_whitespace() || c == '<').map(|e| e + 1);
        let decoded = match end {
            Some(e) if rest.as_bytes()[e] == b';' && e < 12 => {
                let name = &rest[1..e];
                let c = if let Some(num) = name.strip_prefix('#') {
                    let v = if let Some(h) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        u32::from_str_radix(h, 16).ok()
                    } else {
                        num.parse::<u32>().ok()
                    };
                    v.map(|v| match v {
                        // Windows-1252 code points sneaking in as numeric refs
                        0x91 => '‘',
                        0x92 => '’',
                        0x93 => '“',
                        0x94 => '”',
                        0x96 => '–',
                        0x97 => '—',
                        0x85 => '…',
                        _ => char::from_u32(v).unwrap_or('\u{fffd}'),
                    })
                } else {
                    ENTITIES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c).or_else(|| accent(name))
                };
                c.map(|c| (c, e + 1))
            }
            _ => None,
        };
        match decoded {
            Some((c, n)) => {
                out.push(c);
                rest = &rest[n..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

// ---------- converter ----------

#[derive(Clone)]
struct Elem {
    tag: String,
    style: Style,
    size: f32,
    link: Option<u32>,
    block: bool,
    kind: Option<BlockKind>,
    align: Option<Align>,
    indent: f32,
    note: bool,
    /// How many levels of `hidden` this element added (undone on close).
    hid: u32,
    /// Base direction set on this element or an ancestor.
    rtl: Option<bool>,
    /// Table role of this element in the table being built (`TB_*`).
    tbl: u8,
}

const TB_TABLE: u8 = 1;
const TB_ROW: u8 = 2;
const TB_CELL: u8 = 3;
const TB_CAPTION: u8 = 4;
const TB_NESTED: u8 = 5;
const TB_HEAD: u8 = 6;

struct CellB {
    spans: Vec<Span>,
    colspan: u16,
    header: bool,
    align: Option<Align>,
}

/// The (outermost) table being converted. Nested tables are flattened into
/// the cell that contains them.
#[derive(Default)]
struct TableB {
    rows: Vec<Vec<CellB>>,
    row: Option<Vec<CellB>>,
    cell: Option<CellB>,
    in_head: bool,
    in_caption: bool,
    nested: usize,
    rtl: Option<bool>,
    /// Images / rules met inside the table, emitted after it.
    deferred: Vec<Block>,
}

/// A MathML element captured whole, rendered on `</math>`.
#[derive(Default)]
struct MNode {
    tag: String,
    attrs: Vec<(String, String)>,
    kids: Vec<MNode>,
    text: String,
}

pub struct Hooks<'h> {
    /// href → link-table index.
    pub link: &'h mut dyn FnMut(&str) -> u32,
    /// img src → resource key.
    pub image: &'h mut dyn FnMut(&str) -> Option<String>,
    /// stylesheet href → parsed sheet.
    pub stylesheet: &'h mut dyn FnMut(&str) -> Option<Sheet>,
}

pub struct Output {
    pub blocks: Vec<Block>,
    /// Element id → block index.
    pub anchors: Vec<(String, usize)>,
    /// Section breaks: block index where a new section starts.
    pub breaks: Vec<usize>,
    pub title: String,
}

pub struct Options {
    /// Tags whose start begins a new section (e.g. MOBI `pagebreak`).
    pub break_tags: &'static [&'static str],
    /// Break before `<section>` elements at this nesting depth (FB2 chapters).
    pub section_break_depth: Option<usize>,
    /// `(offset in the decoded source, original filepos)` pairs, sorted by the first
    /// value; each becomes an anchor named "pos:<filepos>" (MOBI filepos).
    pub pos_anchors: Vec<(usize, usize)>,
    /// FB2: treat <title> as a heading instead of document metadata.
    pub fb2: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { break_tags: &[], section_break_depth: None, pos_anchors: Vec::new(), fb2: false }
    }
}

struct Conv<'h, 'a> {
    hooks: &'a mut Hooks<'h>,
    opts: &'a Options,
    sheet: Sheet,
    out: Vec<Block>,
    cur: Vec<Span>,
    stack: Vec<Elem>,
    anchors: Vec<(String, usize)>,
    breaks: Vec<usize>,
    lists: Vec<(bool, u32)>,
    head: usize,
    hidden: usize,
    svg: usize,
    pre: usize,
    sections: usize,
    title: String,
    in_title: bool,
    pos_idx: usize,
    src: &'a str,
    table: Option<TableB>,
    /// Inline SVG being skipped: (start byte of `<svg`, it contained a raster `<image>`).
    svg_at: Option<(usize, bool)>,
    /// MathML being captured: open-element stack (root first).
    math: Vec<MNode>,
    doc_rtl: Option<bool>,
}

fn attr<'v>(attrs: &'v [(String, String)], key: &str) -> Option<&'v str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn attr_suffix<'v>(attrs: &'v [(String, String)], suffix: &str) -> Option<&'v str> {
    attrs.iter().find(|(k, _)| k == suffix || k.ends_with(&format!(":{suffix}"))).map(|(_, v)| v.as_str())
}

const VOID: &[&str] = &["br", "hr", "img", "image", "link", "meta", "input", "col", "area", "base", "wbr", "source", "pagebreak", "empty-line"];

impl<'h, 'a> Conv<'h, 'a> {
    fn top(&self) -> Elem {
        self.stack.last().cloned().unwrap_or(Elem {
            tag: String::new(),
            style: Style::default(),
            size: 1.0,
            link: None,
            block: true,
            kind: None,
            align: None,
            indent: 0.0,
            note: false,
            hid: 0,
            rtl: self.doc_rtl,
            tbl: 0,
        })
    }

    fn cur_has_text(&self) -> bool {
        self.cur.iter().any(|s| !s.text.trim().is_empty())
    }

    fn flush(&mut self) {
        if let Some(t) = &self.table {
            if t.cell.is_some() {
                // a block boundary inside a cell: a line break within the cell
                if self.cur_has_text() && !self.cur.last().is_some_and(|s| s.text.ends_with('\n')) {
                    let st = self.cur.last().map(|s| (s.style, s.size)).unwrap_or((Style::default(), 1.0));
                    self.cur.push(Span { text: "\n".into(), style: st.0, link: None, size: st.1 });
                }
                return;
            }
            if !t.in_caption {
                // stray text between rows
                self.cur.clear();
                return;
            }
        }
        if !self.cur_has_text() {
            self.cur.clear();
            return;
        }
        let spans = std::mem::take(&mut self.cur);
        let merged = trim_merge(spans, self.pre == 0);
        let mut kind = BlockKind::Para;
        let mut align = None;
        let mut indent = 0.0;
        let mut note = false;
        let mut rtl = self.doc_rtl;
        for e in self.stack.iter() {
            if e.rtl.is_some() {
                rtl = e.rtl;
            }
            if let Some(k) = &e.kind {
                kind = match (&kind, k) {
                    (BlockKind::Heading(_), _) => kind.clone(),
                    (_, k) => k.clone(),
                };
            }
            if e.align.is_some() {
                align = e.align;
            }
            indent += e.indent;
            note |= e.note;
        }
        let indent = indent.min(8.0);
        if self.pre > 0 {
            kind = BlockKind::Pre;
        }
        self.out.push(Block { kind, spans: merged, align, indent, note, rtl });
    }

    /// Close the innermost open MathML element; closing `<math>` renders it.
    fn math_close(&mut self) {
        let Some(n) = self.math.pop() else { return };
        if let Some(p) = self.math.last_mut() {
            p.kids.push(n);
            return;
        }
        let display = attr(&n.attrs, "display").is_some_and(|d| d.eq_ignore_ascii_case("block")) || attr(&n.attrs, "mode").is_some_and(|d| d.eq_ignore_ascii_case("display"));
        let e = self.top();
        let mut spans = Vec::new();
        math::render(&n, &mut spans, e.style, e.size, e.link);
        if !spans.iter().any(|s| !s.text.trim().is_empty()) {
            let alt = attr(&n.attrs, "alttext").map(|s| s.to_string()).or_else(|| math::annotation(&n)).unwrap_or_default();
            if !alt.trim().is_empty() {
                spans = vec![Span { text: alt.trim().to_string(), style: e.style, link: e.link, size: e.size }];
            }
        }
        let in_cell = self.table.as_ref().is_some_and(|t| t.cell.is_some());
        if display && !in_cell {
            self.flush();
            let n0 = self.out.len();
            self.cur = spans;
            self.flush();
            if self.out.len() > n0 {
                if let Some(b) = self.out.last_mut() {
                    b.align = Some(Align::Center);
                    b.kind = BlockKind::Para;
                }
            }
        } else {
            if display && self.cur_has_text() {
                self.cur.push(Span { text: "\n".into(), style: e.style, link: None, size: e.size });
            }
            self.cur.extend(spans);
        }
    }

    /// Push a non-text block (image, rule); inside a table it waits until the table ends.
    fn push_block(&mut self, b: Block) {
        match &mut self.table {
            Some(t) => t.deferred.push(b),
            None => self.out.push(b),
        }
    }

    fn close_cell(&mut self) {
        let Some(t) = &mut self.table else { return };
        let Some(mut cell) = t.cell.take() else { return };
        let spans = std::mem::take(&mut self.cur);
        cell.spans = trim_merge(spans, true);
        for sp in &mut cell.spans {
            if sp.text.contains('\t') {
                sp.text = sp.text.replace('\t', " ");
            }
        }
        t.row.get_or_insert_with(Vec::new).push(cell);
    }

    fn close_row(&mut self) {
        self.close_cell();
        let Some(t) = &mut self.table else { return };
        if let Some(r) = t.row.take() {
            if !r.is_empty() {
                t.rows.push(r);
            }
        }
    }

    fn finish_table(&mut self) {
        self.close_row();
        self.cur.clear();
        let Some(t) = self.table.take() else { return };
        let ncols = t.rows.iter().map(|r| r.iter().map(|c| c.colspan as usize).sum::<usize>()).max().unwrap_or(0);
        let has_text = t.rows.iter().flatten().any(|c| c.spans.iter().any(|s| !s.text.trim().is_empty()));
        if has_text && ncols <= 1 {
            // a one-column "layout" table: just its paragraphs
            for c in t.rows.into_iter().flatten() {
                let mut line: Vec<Span> = Vec::new();
                for sp in c.spans {
                    let mut parts = sp.text.split('\n').peekable();
                    while let Some(p) = parts.next() {
                        if !p.is_empty() {
                            line.push(Span { text: p.to_string(), ..sp.clone() });
                        }
                        if parts.peek().is_some() {
                            self.cur = std::mem::take(&mut line);
                            self.flush();
                        }
                    }
                }
                self.cur = line;
                self.flush();
            }
        } else if has_text {
            let mut spans: Vec<Span> = Vec::new();
            let mut rows = Vec::with_capacity(t.rows.len());
            let mut pos = 0usize;
            for r in t.rows {
                let n = r.len();
                let mut row = Vec::with_capacity(n);
                for (i, c) in r.into_iter().enumerate() {
                    let start = pos;
                    for sp in c.spans {
                        pos += sp.text.chars().count();
                        spans.push(sp);
                    }
                    row.push(Cell { start, end: pos, colspan: c.colspan, header: c.header, align: c.align });
                    spans.push(Span { text: if i + 1 == n { "\n".into() } else { "\t".into() }, style: Style::default(), link: None, size: 1.0 });
                    pos += 1;
                }
                rows.push(row);
            }
            let mut b = Block::new(BlockKind::Table(Box::new(Table { rows })));
            b.spans = spans;
            let mut indent = 0.0;
            for e in &self.stack {
                indent += e.indent;
            }
            b.indent = indent.min(8.0);
            b.rtl = t.rtl.or(self.doc_rtl);
            self.out.push(b);
        }
        self.out.extend(t.deferred);
    }

    /// Pop the element stack down to (and including) index `idx`.
    fn pop_to(&mut self, idx: usize, pos: usize) {
        while self.stack.len() > idx {
            let e = self.stack.pop().unwrap();
            let base = e.tag.split('#').next().unwrap_or("").to_string();
            if e.block {
                self.flush_with(&e);
            }
            self.hidden = self.hidden.saturating_sub(e.hid as usize);
            match e.tbl {
                TB_CELL => self.close_cell(),
                TB_ROW => self.close_row(),
                TB_TABLE => self.finish_table(),
                TB_CAPTION => {
                    if let Some(t) = &mut self.table {
                        t.in_caption = false;
                    }
                }
                TB_HEAD => {
                    if let Some(t) = &mut self.table {
                        t.in_head = false;
                    }
                }
                TB_NESTED => {
                    if let Some(t) = &mut self.table {
                        t.nested = t.nested.saturating_sub(1);
                    }
                }
                _ => {}
            }
            match base.as_str() {
                "pre" => self.pre = self.pre.saturating_sub(1),
                "ul" | "ol" => {
                    self.lists.pop();
                }
                "svg" => {
                    self.svg = self.svg.saturating_sub(1);
                    if self.svg == 0 {
                        self.end_svg(pos);
                    }
                }
                "section" => self.sections = self.sections.saturating_sub(1),
                _ => {}
            }
        }
    }

    /// The outermost `</svg>` at `pos`: rasterise it as an image unless it only
    /// wrapped a raster `<image>` (already emitted).
    fn end_svg(&mut self, pos: usize) {
        let Some((start, had_image)) = self.svg_at.take() else { return };
        if had_image || self.hidden > 0 {
            return;
        }
        let b = self.src.as_bytes();
        let end = find(b, pos, b">").map(|e| e + 1).unwrap_or(b.len());
        let Some(text) = self.src.get(start..end) else { return };
        if !text.contains("<path") && !text.contains("<rect") && !text.contains("<circle") && !text.contains("<ellipse") && !text.contains("<line") && !text.contains("<poly") && !text.contains("<use") && !text.contains(":path") {
            return; // nothing drawable (text-only SVG needs fonts we don't bundle)
        }
        let key = super::svg::register_inline(text);
        if self.table.as_ref().is_none_or(|t| t.cell.is_none()) {
            self.flush();
        }
        let mut blk = Block::new(BlockKind::Image(key));
        blk.align = Some(Align::Center);
        self.push_block(blk);
    }

    fn push_text(&mut self, text: &str) {
        if !self.math.is_empty() {
            if self.hidden == 0 {
                if let Some(n) = self.math.last_mut() {
                    n.kids.push(MNode { text: text.to_string(), ..Default::default() });
                }
            }
            return;
        }
        if self.hidden > 0 || self.svg > 0 || self.head > 0 && !self.in_title {
            return;
        }
        if self.in_title && !self.opts.fb2 {
            self.title.push_str(text);
            return;
        }
        let e = self.top();
        let mut t: String = if self.pre > 0 {
            text.replace("\r\n", "\n")
        } else {
            let mut s = String::with_capacity(text.len());
            let mut last_ws = self.cur.last().map(|s| s.text.ends_with(|c: char| c == ' ' || c == '\n')).unwrap_or(true);
            for c in text.chars() {
                if c.is_whitespace() && c != '\u{a0}' {
                    if !last_ws {
                        s.push(' ');
                        last_ws = true;
                    }
                } else if c == '\u{ad}' || c == '\u{200b}' || c == '\u{feff}' {
                    // drop soft hyphens / zero-width chars (no hyphenation engine)
                } else {
                    s.push(c);
                    last_ws = false;
                }
            }
            s
        };
        if t.is_empty() {
            return;
        }
        if e.style.has(Style::SMALLCAPS) {
            t = t.to_uppercase();
        }
        self.cur.push(Span { text: t, style: e.style, link: e.link, size: e.size });
    }

    fn start(&mut self, pos: usize, name: String, attrs: Vec<(String, String)>, self_closing: bool) {
        self.check_pos(pos);
        let void = self_closing || VOID.contains(&name.as_str());
        match name.as_str() {
            "head" => {
                if !void {
                    self.head += 1;
                }
                return;
            }
            "title" if !self.opts.fb2 => {
                if !void {
                    self.in_title = true;
                }
                return;
            }
            "link" => {
                let rel = attr(&attrs, "rel").unwrap_or("").to_lowercase();
                if rel.contains("stylesheet") || attr(&attrs, "type").is_some_and(|t| t.contains("css")) {
                    if let Some(href) = attr(&attrs, "href") {
                        if let Some(s) = (self.hooks.stylesheet)(href) {
                            self.sheet.extend(&s);
                        }
                    }
                }
                return;
            }
            "meta" | "script" | "style" | "body" | "html" => {
                if name == "html" && self.opts.break_tags.contains(&"html") && !self.out.is_empty() {
                    self.flush();
                    self.breaks.push(self.out.len());
                }
                if name == "body" {
                    self.head = 0;
                }
                if name == "html" || name == "body" {
                    match attr(&attrs, "dir").map(|d| d.to_ascii_lowercase()) {
                        Some(d) if d == "rtl" => self.doc_rtl = Some(true),
                        Some(d) if d == "ltr" => self.doc_rtl = Some(false),
                        _ => {}
                    }
                }
                return;
            }
            _ => {}
        }
        if self.opts.break_tags.contains(&name.as_str()) {
            self.flush();
            if !self.out.is_empty() {
                self.breaks.push(self.out.len());
            }
            if void {
                return;
            }
        }
        if name == "section" {
            self.sections += 1;
            if Some(self.sections) == self.opts.section_break_depth {
                self.flush();
                if !self.out.is_empty() {
                    self.breaks.push(self.out.len());
                }
            }
        }

        // anchors
        for key in ["id", "name"] {
            if let Some(id) = attr(&attrs, key) {
                if key == "name" && name != "a" {
                    continue;
                }
                self.anchors.push((id.to_string(), self.out.len()));
            }
        }

        if name == "br" || name == "empty-line" {
            if self.hidden == 0 && self.svg == 0 {
                if name == "empty-line" {
                    self.flush();
                } else {
                    let e = self.top();
                    self.cur.push(Span { text: "\n".into(), style: e.style, link: None, size: e.size });
                }
            }
            return;
        }
        if name == "hr" {
            if self.hidden == 0 {
                self.flush();
                self.push_block(Block::new(BlockKind::Rule));
            }
            return;
        }
        if name == "img" || name == "image" {
            if self.hidden == 0 {
                let src = attr(&attrs, "src").or_else(|| attr_suffix(&attrs, "href")).or_else(|| attr(&attrs, "recindex").map(|_| ""));
                let src = if let Some(r) = attr(&attrs, "recindex") { format!("recindex:{r}") } else { src.unwrap_or("").to_string() };
                if !src.is_empty() {
                    if let Some(key) = (self.hooks.image)(&src) {
                        if let Some(s) = &mut self.svg_at {
                            s.1 = true;
                        }
                        self.flush();
                        let mut b = Block::new(BlockKind::Image(key));
                        b.align = Some(Align::Center);
                        self.push_block(b);
                    }
                }
            }
            if !void {
                // <image> in FB2/SVG may have a closing tag; push a dummy elem
                let e = self.top();
                self.stack.push(Elem { tag: name, block: false, kind: None, align: None, indent: 0.0, hid: 0, tbl: 0, ..e });
            }
            return;
        }
        if void {
            return;
        }

        let parent = self.top();
        if name == "math" && self.hidden == 0 {
            self.math.push(MNode { tag: name, attrs, ..Default::default() });
            if void {
                self.math_close();
            }
            return;
        }
        // tables: implicit ends for sloppy markup, then the table roles
        let outer = self.table.as_ref().is_some_and(|t| t.nested == 0);
        if outer && matches!(name.as_str(), "td" | "th" | "tr") {
            let role = if name == "tr" { TB_ROW } else { TB_CELL };
            if let Some(i) = self.stack.iter().rposition(|e| e.tbl == role || e.tbl == TB_CELL) {
                let i = if role == TB_ROW { self.stack.iter().rposition(|e| e.tbl == TB_ROW).unwrap_or(i).min(i) } else { i };
                self.pop_to(i, pos);
            }
            if name == "tr" {
                self.close_row();
            }
        }
        let mut e = Elem { tag: name.clone(), kind: None, align: None, indent: 0.0, block: false, note: false, hid: 0, tbl: 0, ..parent.clone() };
        match attr(&attrs, "dir").map(|d| d.to_ascii_lowercase()) {
            Some(d) if d == "rtl" => e.rtl = Some(true),
            Some(d) if d == "ltr" => e.rtl = Some(false),
            _ => {}
        }
        let classes: Vec<&str> = attr(&attrs, "class").map(|c| c.split_whitespace().collect()).unwrap_or_default();
        let id = attr(&attrs, "id");
        let mut decl: Decl = self.sheet.compute(&name, &classes, id);
        if let Some(st) = attr(&attrs, "style") {
            decl.merge(&css::parse_decls(st));
        }
        let epub_type = attr_suffix(&attrs, "type").unwrap_or("");

        let is_block = matches!(
            name.as_str(),
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "li" | "dt" | "dd" | "pre" | "figure"
                | "figcaption" | "section" | "article" | "aside" | "header" | "footer" | "nav" | "main" | "table" | "tr"
                | "ul" | "ol" | "dl" | "address" | "center" | "details" | "summary" | "caption" | "hgroup"
                // FB2
                | "epigraph" | "cite" | "poem" | "stanza" | "v" | "subtitle" | "text-author" | "annotation" | "title"
        ) && !(name == "cite" && !self.opts.fb2)
            || decl.block == Some(true);
        let is_block = is_block && decl.block != Some(false);
        e.block = is_block;
        if is_block {
            self.flush();
        }
        if decl.rtl.is_some() {
            e.rtl = decl.rtl;
        }

        match name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let lvl = name.as_bytes()[1] - b'0';
                e.kind = Some(BlockKind::Heading(lvl));
                e.style = e.style.with(Style::BOLD);
            }
            "title" if self.opts.fb2 => {
                e.kind = Some(BlockKind::Heading((self.sections.max(1)).min(4) as u8));
                e.style = e.style.with(Style::BOLD);
                e.align = Some(Align::Center);
            }
            "subtitle" => {
                e.kind = Some(BlockKind::Heading(5));
                e.style = e.style.with(Style::BOLD);
                e.align = Some(Align::Center);
            }
            "blockquote" | "epigraph" | "cite" if is_block => {
                e.kind = Some(BlockKind::Quote);
                e.indent = 1.5;
                if name == "epigraph" {
                    e.style = e.style.with(Style::ITALIC);
                }
            }
            "poem" | "stanza" => e.indent = 1.0,
            "text-author" => {
                e.align = Some(Align::End);
                e.style = e.style.with(Style::ITALIC);
            }
            "dd" => e.indent = 1.5,
            "pre" => {
                self.pre += 1;
                e.style = e.style.with(Style::MONO);
            }
            "ul" | "ol" => {
                let start = attr(&attrs, "start").and_then(|s| s.parse().ok()).unwrap_or(1);
                self.lists.push((name == "ol", start));
            }
            "li" => {
                let depth = self.lists.len().max(1) as u8;
                e.kind = Some(BlockKind::ListItem(depth));
                let marker = match self.lists.last_mut() {
                    Some((true, n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => ["• ", "◦ ", "▪ "][(depth as usize - 1) % 3].to_string(),
                };
                self.stack.push(e);
                self.cur.push(Span { text: marker, style: Style::default(), link: None, size: 1.0 });
                return;
            }
            "aside" => {
                if epub_type.contains("note") {
                    e.note = true;
                    e.size *= 0.9;
                }
            }
            "center" => e.align = Some(Align::Center),
            "b" | "strong" => e.style = e.style.with(Style::BOLD),
            "i" | "em" | "cite" | "dfn" | "var" | "emphasis" => e.style = e.style.with(Style::ITALIC),
            "code" | "tt" | "kbd" | "samp" => e.style = e.style.with(Style::MONO),
            "u" | "ins" => e.style = e.style.with(Style::UNDERLINE),
            "s" | "strike" | "del" | "strikethrough" => e.style = e.style.with(Style::STRIKE),
            "sup" => {
                e.style = e.style.with(Style::SUP);
                e.size *= 0.75;
            }
            "sub" => {
                e.style = e.style.with(Style::SUB);
                e.size *= 0.75;
            }
            "small" => e.size *= 0.85,
            "big" => e.size *= 1.15,
            "table" => {
                if self.table.is_some() {
                    e.tbl = TB_NESTED;
                    if let Some(t) = &mut self.table {
                        t.nested += 1;
                    }
                } else if self.hidden == 0 {
                    e.tbl = TB_TABLE;
                    self.cur.clear();
                    self.table = Some(TableB { rtl: e.rtl, ..Default::default() });
                }
            }
            "thead" if outer => {
                e.tbl = TB_HEAD;
                if let Some(t) = &mut self.table {
                    t.in_head = true;
                }
            }
            "caption" if outer => {
                e.tbl = TB_CAPTION;
                e.align = Some(Align::Center);
                if let Some(t) = &mut self.table {
                    t.in_caption = true;
                }
            }
            "tr" if outer => {
                e.tbl = TB_ROW;
                if let Some(t) = &mut self.table {
                    t.row = Some(Vec::new());
                }
            }
            "td" | "th" if outer => {
                e.tbl = TB_CELL;
                self.cur.clear();
                let colspan = attr(&attrs, "colspan").and_then(|c| c.trim().parse::<u16>().ok()).unwrap_or(1).clamp(1, 64);
                let align = match attr(&attrs, "align").map(|a| a.to_ascii_lowercase()).as_deref() {
                    Some("center") => Some(Align::Center),
                    Some("right") => Some(Align::End),
                    Some("left") => Some(Align::Start),
                    _ => decl.align,
                };
                if let Some(t) = &mut self.table {
                    let header = name == "th" || t.in_head;
                    t.cell = Some(CellB { spans: Vec::new(), colspan, header, align });
                }
                if name == "th" {
                    e.style = e.style.with(Style::BOLD);
                }
            }
            "td" | "th" => {
                // nested table cell: flattened
                if self.cur_has_text() && !self.cur.last().is_some_and(|s| s.text.ends_with([' ', '\n'])) {
                    self.cur.push(Span { text: " ".into(), style: e.style, link: None, size: e.size });
                }
            }
            "a" => {
                if let Some(href) = attr(&attrs, "href").or_else(|| attr_suffix(&attrs, "href")).or_else(|| attr(&attrs, "filepos").map(|_| "")) {
                    let href = if let Some(fp) = attr(&attrs, "filepos") { format!("filepos:{}", fp.trim_start_matches('0')) } else { href.to_string() };
                    if !href.is_empty() && !href.starts_with("javascript:") {
                        e.link = Some((self.hooks.link)(&href));
                    }
                }
            }
            "svg" => {
                if self.svg == 0 {
                    self.svg_at = Some((pos, false));
                }
                self.svg += 1;
            }
            "rt" | "rp" => {
                self.hidden += 1;
                e.hid += 1;
            }
            _ => {}
        }

        // CSS on top of tag defaults
        if let Some(b) = decl.bold {
            e.style = if b { e.style.with(Style::BOLD) } else { e.style.without(Style::BOLD) };
        }
        if let Some(i) = decl.italic {
            e.style = if i { e.style.with(Style::ITALIC) } else { e.style.without(Style::ITALIC) };
        }
        if decl.underline == Some(true) {
            e.style = e.style.with(Style::UNDERLINE);
        }
        if decl.strike == Some(true) {
            e.style = e.style.with(Style::STRIKE);
        }
        if decl.smallcaps == Some(true) {
            e.style = e.style.with(Style::SMALLCAPS);
            e.size *= 0.85;
        }
        if decl.mono == Some(true) {
            e.style = e.style.with(Style::MONO);
        }
        if decl.sup == Some(true) {
            e.style = e.style.with(Style::SUP);
        }
        if let Some(s) = decl.size {
            if !matches!(e.kind, Some(BlockKind::Heading(_))) {
                e.size = (e.size * s).clamp(0.6, 2.5);
            }
        }
        if is_block {
            if decl.align.is_some() {
                e.align = decl.align;
            }
            if let Some(ind) = decl.indent {
                e.indent += ind.min(3.0);
            }
        }
        if decl.hidden == Some(true) {
            self.hidden += 1;
            e.hid += 1;
        }
        self.stack.push(e);
    }

    fn end(&mut self, pos: usize, name: String) {
        self.check_pos(pos);
        match name.as_str() {
            "head" => {
                self.head = self.head.saturating_sub(1);
                return;
            }
            "title" if !self.opts.fb2 => {
                self.in_title = false;
                return;
            }
            _ => {}
        }
        if VOID.contains(&name.as_str()) && name != "image" {
            return;
        }
        // pop up to and including the matching element (tolerate misnesting)
        let Some(idx) = self.stack.iter().rposition(|e| e.tag.split('#').next() == Some(name.as_str())) else { return };
        self.pop_to(idx, pos);
    }

    /// Flush while the closing element is still considered part of the context.
    fn flush_with(&mut self, e: &Elem) {
        self.stack.push(e.clone());
        self.flush();
        self.stack.pop();
    }

    fn check_pos(&mut self, pos: usize) {
        while self.pos_idx < self.opts.pos_anchors.len() && self.opts.pos_anchors[self.pos_idx].0 <= pos {
            let p = self.opts.pos_anchors[self.pos_idx].1;
            self.anchors.push((format!("pos:{p}"), self.out.len()));
            self.pos_idx += 1;
        }
    }
}

/// Trim a block's outer whitespace (unless preformatted) and merge adjacent
/// spans with identical formatting.
fn trim_merge(mut spans: Vec<Span>, trim: bool) -> Vec<Span> {
    if trim {
        while let Some(f) = spans.first_mut() {
            let t = f.text.trim_start().to_string();
            if t.is_empty() {
                spans.remove(0);
            } else {
                f.text = t;
                break;
            }
        }
        while let Some(l) = spans.last_mut() {
            let t = l.text.trim_end().to_string();
            if t.is_empty() {
                spans.pop();
            } else {
                l.text = t;
                break;
            }
        }
    }
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        if let Some(l) = merged.last_mut() {
            if l.style == s.style && l.link == s.link && l.size == s.size {
                l.text.push_str(&s.text);
                continue;
            }
        }
        merged.push(s);
    }
    merged
}

/// MathML presentation markup → linear styled text: identifiers italic,
/// scripts as superscript/subscript spans, fractions as a⁄b or (a)/(b),
/// roots as √(…).
mod math {
    use super::{attr, MNode};
    use crate::book::{Span, Style};

    fn kids(n: &MNode) -> Vec<&MNode> {
        n.kids.iter().filter(|k| !(k.tag.is_empty() && k.text.trim().is_empty())).collect()
    }

    fn text_of(n: &MNode) -> String {
        if n.tag.is_empty() {
            return n.text.clone();
        }
        n.kids.iter().map(text_of).collect()
    }

    /// A text annotation (`<annotation>`) usable as a fallback.
    pub fn annotation(n: &MNode) -> Option<String> {
        if n.tag == "annotation" {
            return Some(text_of(n));
        }
        n.kids.iter().find_map(annotation)
    }

    fn push(out: &mut Vec<Span>, text: &str, st: Style, size: f32, link: Option<u32>) {
        if text.is_empty() {
            return;
        }
        if let Some(l) = out.last_mut() {
            if l.style == st && l.size == size && l.link == link {
                l.text.push_str(text);
                return;
            }
        }
        out.push(Span { text: text.to_string(), style: st, link, size });
    }

    /// Rendered text length in chars (to decide whether a part needs brackets).
    fn simple(n: &MNode) -> bool {
        let mut v = Vec::new();
        render(n, &mut v, Style::default(), 1.0, None);
        let t: String = v.iter().map(|s| s.text.as_str()).collect();
        let t = t.trim();
        t.chars().count() <= 3 && !t.contains([' ', '+', '-', '−', '=', '/', '⁄'])
    }

    fn atom(n: &MNode) -> bool {
        let mut v = Vec::new();
        render(n, &mut v, Style::default(), 1.0, None);
        let t: String = v.iter().map(|s| s.text.as_str()).collect();
        let t = t.trim();
        t.chars().count() <= 1 || t.chars().all(|c| c.is_ascii_digit() || c == '.')
    }

    fn script(n: &MNode, out: &mut Vec<Span>, st: Style, size: f32, link: Option<u32>, flag: u16) {
        let st = st.without(Style::SUP).without(Style::SUB).with(flag);
        render(n, out, st, (size * 0.75).max(0.45), link);
    }

    pub fn render(n: &MNode, out: &mut Vec<Span>, st: Style, size: f32, link: Option<u32>) {
        if n.tag.is_empty() {
            let t = n.text.split_whitespace().collect::<Vec<_>>().join(" ");
            push(out, &t, st, size, link);
            return;
        }
        let k = kids(n);
        let variant = attr(&n.attrs, "mathvariant").unwrap_or("");
        let st = match variant {
            "bold" => st.with(Style::BOLD).without(Style::ITALIC),
            "italic" => st.with(Style::ITALIC),
            "bold-italic" => st.with(Style::BOLD).with(Style::ITALIC),
            "monospace" => st.with(Style::MONO),
            "normal" => st.without(Style::ITALIC),
            _ => st,
        };
        let in_script = st.has(Style::SUP) || st.has(Style::SUB);
        match n.tag.as_str() {
            "mi" => {
                let t = text_of(n).trim().to_string();
                let st = if t.chars().count() == 1 && variant.is_empty() { st.with(Style::ITALIC) } else { st };
                push(out, &t, st, size, link);
            }
            "mn" | "ms" => push(out, text_of(n).trim(), st, size, link),
            "mtext" => push(out, &text_of(n).split_whitespace().collect::<Vec<_>>().join(" "), st, size, link),
            "mo" => {
                let t = text_of(n).trim().to_string();
                let t = match t.as_str() {
                    "\u{2061}" | "\u{2062}" | "\u{2063}" | "\u{2064}" => return,
                    "-" => "−".to_string(),
                    _ => t,
                };
                let spaced = matches!(t.as_str(), "=" | "+" | "−" | "<" | ">" | "≤" | "≥" | "≠" | "≈" | "≡" | "→" | "←" | "⇒" | "⇔" | "±" | "×" | "÷" | "∈" | "∉" | "⊂" | "⊆" | "∪" | "∩" | "∼" | "≅" | "∝" | "·" | "∧" | "∨");
                let st = st.without(Style::ITALIC);
                if spaced && !in_script && !out.is_empty() {
                    push(out, &format!(" {t} "), st, size, link);
                } else if t == "," && !in_script {
                    push(out, ", ", st, size, link);
                } else {
                    push(out, &t, st, size, link);
                }
            }
            "mspace" => push(out, " ", st, size, link),
            "mphantom" | "annotation" | "annotation-xml" | "none" | "mprescripts" | "maligngroup" | "malignmark" => {}
            "semantics" => {
                if let Some(f) = k.first() {
                    render(f, out, st, size, link);
                }
            }
            "msup" | "mover" => {
                if let Some(b) = k.first() {
                    render(b, out, st, size, link);
                }
                if let Some(s) = k.get(1) {
                    script(s, out, st, size, link, Style::SUP);
                }
            }
            "msub" | "munder" => {
                if let Some(b) = k.first() {
                    render(b, out, st, size, link);
                }
                if let Some(s) = k.get(1) {
                    script(s, out, st, size, link, Style::SUB);
                }
            }
            "msubsup" | "munderover" => {
                if let Some(b) = k.first() {
                    render(b, out, st, size, link);
                }
                if let Some(s) = k.get(1) {
                    script(s, out, st, size, link, Style::SUB);
                }
                if let Some(s) = k.get(2) {
                    script(s, out, st, size, link, Style::SUP);
                }
            }
            "mmultiscripts" => {
                if let Some(b) = k.first() {
                    render(b, out, st, size, link);
                }
                let mut i = 1;
                while i + 1 < k.len() && k[i].tag != "mprescripts" {
                    script(k[i], out, st, size, link, Style::SUB);
                    script(k[i + 1], out, st, size, link, Style::SUP);
                    i += 2;
                }
            }
            "mfrac" => {
                let (Some(a), Some(b)) = (k.first(), k.get(1)) else { return };
                if attr(&n.attrs, "linethickness").is_some_and(|t| t.trim() == "0" || t.trim() == "0px") {
                    push(out, "(", st, size, link);
                    render(a, out, st, size, link);
                    push(out, " ", st, size, link);
                    render(b, out, st, size, link);
                    push(out, ")", st, size, link);
                } else if simple(a) && simple(b) && !in_script {
                    script(a, out, st, size, link, Style::SUP);
                    push(out, "⁄", st.without(Style::ITALIC), size, link);
                    script(b, out, st, size, link, Style::SUB);
                } else {
                    for (i, part) in [a, b].into_iter().enumerate() {
                        if i == 1 {
                            push(out, "/", st.without(Style::ITALIC), size, link);
                        }
                        // numerators bracket only compound terms; denominators anything longer than one symbol
                        let p = if i == 0 { !simple(part) } else { !atom(part) };
                        if p {
                            push(out, "(", st.without(Style::ITALIC), size, link);
                        }
                        render(part, out, st, size, link);
                        if p {
                            push(out, ")", st.without(Style::ITALIC), size, link);
                        }
                    }
                }
            }
            "msqrt" | "mroot" => {
                if n.tag == "mroot" {
                    if let Some(i) = k.get(1) {
                        script(i, out, st, size, link, Style::SUP);
                    }
                }
                let body: Vec<&MNode> = if n.tag == "mroot" { k.iter().take(1).copied().collect() } else { k.clone() };
                let one = body.len() == 1 && simple(body[0]);
                push(out, if one { "√" } else { "√(" }, st.without(Style::ITALIC), size, link);
                for b in body {
                    render(b, out, st, size, link);
                }
                if !one {
                    push(out, ")", st.without(Style::ITALIC), size, link);
                }
            }
            "mfenced" => {
                let open = attr(&n.attrs, "open").unwrap_or("(");
                let close = attr(&n.attrs, "close").unwrap_or(")");
                let seps: Vec<char> = attr(&n.attrs, "separators").unwrap_or(",").chars().filter(|c| !c.is_whitespace()).collect();
                push(out, open, st.without(Style::ITALIC), size, link);
                for (i, c) in k.iter().enumerate() {
                    if i > 0 {
                        let s = seps.get(i - 1).or(seps.last()).copied().unwrap_or(',');
                        push(out, &format!("{s} "), st.without(Style::ITALIC), size, link);
                    }
                    render(c, out, st, size, link);
                }
                push(out, close, st.without(Style::ITALIC), size, link);
            }
            "mtable" => {
                push(out, "[", st, size, link);
                for (ri, row) in k.iter().enumerate() {
                    if ri > 0 {
                        push(out, "; ", st, size, link);
                    }
                    for (ci, cell) in kids(row).iter().enumerate() {
                        if ci > 0 {
                            push(out, "  ", st, size, link);
                        }
                        render(cell, out, st, size, link);
                    }
                }
                push(out, "]", st, size, link);
            }
            _ => {
                for c in k {
                    render(c, out, st, size, link);
                }
            }
        }
    }
}

/// Convert an HTML document into blocks.
pub fn convert(src: &str, sheet: Sheet, hooks: &mut Hooks<'_>, opts: &Options) -> Output {
    let mut c = Conv {
        hooks,
        opts,
        sheet,
        out: Vec::new(),
        cur: Vec::new(),
        stack: Vec::new(),
        anchors: Vec::new(),
        breaks: Vec::new(),
        lists: Vec::new(),
        head: 0,
        hidden: 0,
        svg: 0,
        pre: 0,
        sections: 0,
        title: String::new(),
        in_title: false,
        pos_idx: 0,
        src,
        table: None,
        svg_at: None,
        math: Vec::new(),
        doc_rtl: None,
    };
    let mut pending_css = String::new();
    let mut in_style = false;
    tokenize(src, |pos, tok| match tok {
        Tok::Start { name, attrs, self_closing } if !c.math.is_empty() => {
            let void = self_closing || matches!(name.as_str(), "mspace" | "none" | "mprescripts" | "maligngroup" | "malignmark");
            c.math.push(MNode { tag: name, attrs, ..Default::default() });
            if void {
                c.math_close();
            }
        }
        Tok::End { name } if !c.math.is_empty() => {
            if let Some(i) = c.math.iter().rposition(|n| n.tag == name) {
                while c.math.len() > i {
                    c.math_close();
                }
            }
        }
        Tok::Start { name, attrs, self_closing } => {
            if name == "style" {
                in_style = true;
            }
            c.start(pos, name, attrs, self_closing)
        }
        Tok::End { name } => {
            if name == "style" {
                in_style = false;
                if !pending_css.is_empty() {
                    let s = Sheet::parse(&pending_css);
                    c.sheet.extend(&s);
                    pending_css.clear();
                }
            }
            c.end(pos, name)
        }
        Tok::Text(t) => {
            c.check_pos(pos);
            let d = decode_entities(t);
            c.push_text(&d);
        }
        Tok::Raw(t) => {
            if in_style {
                pending_css.push_str(t);
            } else {
                c.push_text(t);
            }
        }
    });
    while !c.math.is_empty() {
        c.math_close();
    }
    if c.table.is_some() {
        c.finish_table();
    }
    c.flush();
    let n = c.out.len();
    for a in &mut c.anchors {
        a.1 = a.1.min(n.saturating_sub(1));
    }
    Output { blocks: c.out, anchors: c.anchors, breaks: c.breaks, title: c.title.trim().to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(src: &str) -> Output {
        let mut link = |_: &str| 0u32;
        let mut image = |_: &str| None;
        let mut css = |_: &str| None;
        let mut hooks = Hooks { link: &mut link, image: &mut image, stylesheet: &mut css };
        convert(src, Sheet::default(), &mut hooks, &Options::default())
    }

    #[test]
    fn accent_no_panic() {
        assert_eq!(accent(""), None);
        assert_eq!(accent("éacute"), None);
        assert_eq!(accent("eacute"), Some('é'));
        let _ = decode_entities("AT&;T &éacute; &;");
    }

    #[test]
    fn self_closing_title_does_not_swallow_body() {
        let o = conv("<html><head><title/></head><body><p>Hello world</p></body></html>");
        assert!(o.blocks.iter().any(|b| b.text().contains("Hello world")));
    }

    #[test]
    fn ruby_hidden_by_css_balances() {
        let src = "<style>rt{display:none}</style><p>a<ruby>x<rt>y</rt></ruby>b</p><p>visible after</p>";
        let o = conv(src);
        let all: String = o.blocks.iter().map(|b| b.text()).collect::<Vec<_>>().join("|");
        assert!(all.contains("visible after"), "{all}");
        assert!(!all.contains('y'), "{all}");
    }

    #[test]
    fn table_cells_and_offsets() {
        let o = conv("<p>before</p><table><caption>Cap</caption><tr><th>A</th><th>B</th></tr><tr><td>one <b>x</b></td><td>two<br/>2<img src='q.png'/></td></tr></table><p>after</p>");
        let texts: Vec<String> = o.blocks.iter().map(|b| b.text()).collect();
        assert_eq!(texts, vec!["before", "Cap", "A\tB\none x\ttwo\n2\n", "after"]);
        let BlockKind::Table(t) = &o.blocks[2].kind else { panic!("not a table") };
        assert_eq!(t.rows.len(), 2);
        assert!(t.rows[0][0].header);
        let text: Vec<char> = o.blocks[2].text().chars().collect();
        let c = &t.rows[1][1];
        assert_eq!(text[c.start..c.end].iter().collect::<String>(), "two\n2");
    }

    #[test]
    fn sloppy_table_and_layout_table() {
        let o = conv("<table><tr><td>a<td>b<tr><td>c<td>d</table><table><tr><td><p>solo one</p><p>solo two</p></td></tr></table>");
        let texts: Vec<String> = o.blocks.iter().map(|b| b.text()).collect();
        assert_eq!(texts, vec!["a\tb\nc\td\n", "solo one", "solo two"]);
    }

    #[test]
    fn mathml_linear() {
        let o = conv("<p>Area <math><mi>x</mi><msup><mi>r</mi><mn>2</mn></msup><mo>=</mo><mfrac><mn>1</mn><mn>2</mn></mfrac><msqrt><mi>a</mi><mo>+</mo><mi>b</mi></msqrt></math> done</p><math display='block' alttext='E=mc^2'><semantics><annotation>E=mc^2</annotation></semantics></math>");
        assert_eq!(o.blocks[0].text(), "Area xr2 = 1⁄2√(a + b) done");
        let sup = o.blocks[0].spans.iter().find(|s| s.text == "2").unwrap();
        assert!(sup.style.has(Style::SUP));
        assert_eq!(o.blocks[1].text(), "E=mc^2");
        assert_eq!(o.blocks[1].align, Some(Align::Center));
    }

    #[test]
    fn inline_svg_becomes_image() {
        let o = conv("<p>x</p><svg width='10' height='10'><rect width='10' height='10'/></svg><svg><image xlink:href='a.jpg'/></svg>");
        assert!(matches!(&o.blocks[1].kind, BlockKind::Image(k) if k.starts_with(crate::book::svg::INLINE_PREFIX)));
        assert_eq!(o.blocks.len(), 2, "raster-wrapper svg adds nothing here (image hook returns None)");
    }

    #[test]
    fn dir_attribute() {
        let o = conv("<html dir='rtl'><body><p>שלום</p><p dir='ltr'>hi</p></body></html>");
        assert_eq!(o.blocks[0].rtl, Some(true));
        assert_eq!(o.blocks[1].rtl, Some(false));
    }
}
