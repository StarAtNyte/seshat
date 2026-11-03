//! Lenient HTML/XHTML tokenizer + converter into styled blocks.
//!
//! Tolerates the malformed markup common in MOBI files and sloppy EPUBs
//! (unquoted attributes, unclosed tags, stray `<`), so one pipeline serves
//! every HTML-based format.

use std::borrow::Cow;

use super::css::{self, Decl, Sheet};
use super::{Align, Block, BlockKind, Span, Style};

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
    let (base, acc) = name.split_at(1);
    let table: &[(&str, &str, &str)] = &[
        ("acute", "aeiouyAEIOUY", "áéíóúýÁÉÍÓÚÝ"),
        ("grave", "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        ("circ", "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
        ("uml", "aeiouyAEIOUY", "äëïöüÿÄËÏÖÜŸ"),
        ("tilde", "aoAO", "ãõÃÕ"),
    ];
    for (suffix, from, to) in table {
        if acc == *suffix {
            let idx = from.find(base)?;
            return to.chars().nth(from[..idx].chars().count());
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
    /// Byte offsets that should become anchors named "pos:<offset>" (MOBI filepos).
    pub pos_anchors: Vec<usize>,
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
        })
    }

    fn cur_has_text(&self) -> bool {
        self.cur.iter().any(|s| !s.text.trim().is_empty())
    }

    fn flush(&mut self) {
        if !self.cur_has_text() {
            self.cur.clear();
            return;
        }
        let mut spans = std::mem::take(&mut self.cur);
        if self.pre == 0 {
            // trim leading/trailing whitespace and newlines
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
        // merge adjacent spans with identical formatting
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
        let mut kind = BlockKind::Para;
        let mut align = None;
        let mut indent = 0.0;
        let mut note = false;
        for e in self.stack.iter() {
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
        if self.pre > 0 {
            kind = BlockKind::Pre;
        }
        self.out.push(Block { kind, spans: merged, align, indent, note });
    }

    fn push_text(&mut self, text: &str) {
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
                self.in_title = true;
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
                self.out.push(Block::new(BlockKind::Rule));
            }
            return;
        }
        if name == "img" || name == "image" {
            if self.hidden == 0 {
                let src = attr(&attrs, "src").or_else(|| attr_suffix(&attrs, "href")).or_else(|| attr(&attrs, "recindex").map(|_| ""));
                let src = if let Some(r) = attr(&attrs, "recindex") { format!("recindex:{r}") } else { src.unwrap_or("").to_string() };
                if !src.is_empty() {
                    if let Some(key) = (self.hooks.image)(&src) {
                        self.flush();
                        let mut b = Block::new(BlockKind::Image(key));
                        b.align = Some(Align::Center);
                        self.out.push(b);
                    }
                }
            }
            if !void {
                // <image> in FB2/SVG may have a closing tag; push a dummy elem
                let e = self.top();
                self.stack.push(Elem { tag: name, block: false, kind: None, align: None, indent: 0.0, ..e });
            }
            return;
        }
        if void {
            return;
        }

        let parent = self.top();
        let mut e = Elem { tag: name.clone(), kind: None, align: None, indent: 0.0, block: false, note: false, ..parent.clone() };
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
            "b" | "strong" | "th" => e.style = e.style.with(Style::BOLD),
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
            "td" => {
                if self.cur_has_text() {
                    self.cur.push(Span { text: "   ".into(), style: Style::default(), link: None, size: 1.0 });
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
            "svg" => self.svg += 1,
            "rt" | "rp" => {
                self.hidden += 1;
                e.tag = format!("{}#hidden", e.tag);
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
            e.tag = format!("{}#hidden", e.tag);
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
        while self.stack.len() > idx {
            let e = self.stack.pop().unwrap();
            let base = e.tag.split('#').next().unwrap_or("").to_string();
            if e.block {
                self.flush_with(&e);
            }
            if e.tag.ends_with("#hidden") {
                self.hidden = self.hidden.saturating_sub(1);
            }
            match base.as_str() {
                "pre" => self.pre = self.pre.saturating_sub(1),
                "ul" | "ol" => {
                    self.lists.pop();
                }
                "svg" => self.svg = self.svg.saturating_sub(1),
                "section" => self.sections = self.sections.saturating_sub(1),
                _ => {}
            }
        }
    }

    /// Flush while the closing element is still considered part of the context.
    fn flush_with(&mut self, e: &Elem) {
        self.stack.push(e.clone());
        self.flush();
        self.stack.pop();
    }

    fn check_pos(&mut self, pos: usize) {
        while self.pos_idx < self.opts.pos_anchors.len() && self.opts.pos_anchors[self.pos_idx] <= pos {
            let p = self.opts.pos_anchors[self.pos_idx];
            self.anchors.push((format!("pos:{p}"), self.out.len()));
            self.pos_idx += 1;
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
    };
    let mut pending_css = String::new();
    let mut in_style = false;
    tokenize(src, |pos, tok| match tok {
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
    c.flush();
    let n = c.out.len();
    for a in &mut c.anchors {
        a.1 = a.1.min(n.saturating_sub(1));
    }
    Output { blocks: c.out, anchors: c.anchors, breaks: c.breaks, title: c.title.trim().to_string() }
}
