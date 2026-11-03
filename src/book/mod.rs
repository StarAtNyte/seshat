//! Format-independent document model.
//!
//! Every reflowable format (EPUB, MOBI/AZW3, FB2, TXT, HTML) is converted into
//! `Flow` content: a list of sections, each a list of styled blocks. Fixed-layout
//! formats (PDF, CBZ) become `Fixed` content rendered page by page.

pub mod cbz;
pub mod css;
pub mod epub;
pub mod fb2;
pub mod html;
pub mod mobi;
pub mod pdf;
pub mod txt;
pub mod util;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

// ---------- inline styling ----------

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct Style(pub u16);

impl Style {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 2;
    pub const MONO: u16 = 4;
    pub const UNDERLINE: u16 = 8;
    pub const STRIKE: u16 = 16;
    pub const SUP: u16 = 32;
    pub const SUB: u16 = 64;
    pub const SMALL: u16 = 128;
    pub const SMALLCAPS: u16 = 256;

    pub fn has(self, f: u16) -> bool {
        self.0 & f != 0
    }
    pub fn with(self, f: u16) -> Style {
        Style(self.0 | f)
    }
    pub fn without(self, f: u16) -> Style {
        Style(self.0 & !f)
    }
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Index into `Flow::links`.
    pub link: Option<u32>,
    /// Relative font size (1.0 = body).
    pub size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Justify,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BlockKind {
    Para,
    Heading(u8),
    Quote,
    /// List item nesting depth (1-based).
    ListItem(u8),
    Pre,
    /// Resource key into `Flow::resources`.
    Image(String),
    Rule,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
    pub align: Option<Align>,
    /// Extra left indent in em (blockquote nesting etc).
    pub indent: f32,
    /// Block is a footnote/endnote body (aside epub:type="footnote" ...).
    pub note: bool,
}

impl Block {
    pub fn new(kind: BlockKind) -> Self {
        Block { kind, spans: Vec::new(), align: None, indent: 0.0, note: false }
    }
    pub fn text(&self) -> String {
        let mut s = String::new();
        for sp in &self.spans {
            s.push_str(&sp.text);
        }
        s
    }
    pub fn char_len(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum()
    }
    /// Link index at a char offset, if that char is inside a link span.
    pub fn link_at(&self, offset: usize) -> Option<u32> {
        let mut pos = 0;
        for sp in &self.spans {
            let n = sp.text.chars().count();
            if offset < pos + n {
                return sp.link;
            }
            pos += n;
        }
        None
    }
}

#[derive(Clone, Debug, Default)]
pub struct Section {
    pub title: String,
    pub blocks: Vec<Block>,
    /// Total characters (for progress estimates).
    pub chars: usize,
}

impl Section {
    pub fn finish(mut self) -> Self {
        self.chars = self.blocks.iter().map(|b| b.char_len().max(1)).sum();
        self
    }
}

/// A position in reflowable content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Loc {
    pub section: usize,
    pub block: usize,
    /// Character offset within the block.
    pub offset: usize,
}

impl Loc {
    pub fn new(section: usize, block: usize, offset: usize) -> Self {
        Loc { section, block, offset }
    }
}

#[derive(Clone, Debug)]
pub enum LinkTarget {
    Internal(Loc),
    External(String),
    Unresolved,
}

#[derive(Clone, Debug)]
pub struct TocEntry {
    pub title: String,
    pub depth: usize,
    /// For flow books: a location. For fixed books: `Loc { section: page, .. }`.
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub enum ResRef {
    Zip { path: PathBuf, entry: String },
    Bytes(Arc<Vec<u8>>),
}

impl ResRef {
    pub fn load(&self) -> Option<Vec<u8>> {
        match self {
            ResRef::Bytes(b) => Some(b.as_ref().clone()),
            ResRef::Zip { path, entry } => util::read_zip_entry(path, entry).ok(),
        }
    }
}

#[derive(Default, Debug)]
pub struct Flow {
    pub sections: Vec<Section>,
    pub links: Vec<LinkTarget>,
    pub resources: HashMap<String, ResRef>,
    /// For each spine item (KOReader DocFragment index - 1), its first section.
    pub spine_sections: Vec<usize>,
}

impl Flow {
    pub fn total_chars(&self) -> usize {
        self.sections.iter().map(|s| s.chars).sum()
    }

    /// Fraction 0..1 through the book for a location.
    pub fn fraction(&self, loc: Loc) -> f32 {
        let total = self.total_chars().max(1);
        let mut before = 0usize;
        for s in self.sections.iter().take(loc.section) {
            before += s.chars;
        }
        if let Some(s) = self.sections.get(loc.section) {
            for b in s.blocks.iter().take(loc.block) {
                before += b.char_len().max(1);
            }
            before += loc.offset;
        }
        (before as f32 / total as f32).clamp(0.0, 1.0)
    }

    /// Location at a fraction 0..1 through the book.
    pub fn loc_at_fraction(&self, f: f32) -> Loc {
        let total = self.total_chars();
        let mut target = (f.clamp(0.0, 1.0) * total as f32) as usize;
        for (si, s) in self.sections.iter().enumerate() {
            if target < s.chars || si + 1 == self.sections.len() {
                for (bi, b) in s.blocks.iter().enumerate() {
                    let n = b.char_len().max(1);
                    if target < n {
                        return Loc::new(si, bi, target.min(b.char_len()));
                    }
                    target -= n;
                }
                return Loc::new(si, s.blocks.len().saturating_sub(1), 0);
            }
            target -= s.chars;
        }
        Loc::default()
    }

    /// Characters remaining in the section after `loc`.
    pub fn section_chars_after(&self, loc: Loc) -> usize {
        let Some(s) = self.sections.get(loc.section) else { return 0 };
        let mut n = 0;
        for (bi, b) in s.blocks.iter().enumerate() {
            if bi > loc.block {
                n += b.char_len();
            } else if bi == loc.block {
                n += b.char_len().saturating_sub(loc.offset);
            }
        }
        n
    }
}

#[derive(Clone, Debug)]
pub enum FixedKind {
    Pdf,
    Cbz { entries: Vec<String> },
}

#[derive(Debug)]
pub struct Fixed {
    pub kind: FixedKind,
    pub path: PathBuf,
    pub pages: usize,
}

#[derive(Debug)]
pub enum Content {
    Flow(Flow),
    Fixed(Fixed),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    pub title: String,
    pub author: String,
    pub language: String,
    pub publisher: String,
    pub description: String,
    pub series: String,
    pub subjects: Vec<String>,
    #[serde(skip)]
    pub cover: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct Book {
    pub meta: Meta,
    pub toc: Vec<TocEntry>,
    pub content: Content,
}

impl Book {
    pub fn flow(&self) -> Option<&Flow> {
        match &self.content {
            Content::Flow(f) => Some(f),
            _ => None,
        }
    }
    pub fn fixed(&self) -> Option<&Fixed> {
        match &self.content {
            Content::Fixed(f) => Some(f),
            _ => None,
        }
    }

    /// Index of the TOC entry that contains `loc` (last entry at or before it).
    pub fn toc_index_for(&self, loc: Loc) -> Option<usize> {
        let mut best = None;
        for (i, e) in self.toc.iter().enumerate() {
            if e.loc <= loc {
                best = Some(i);
            }
        }
        best
    }
}

pub const SUPPORTED: &[&str] = &["epub", "mobi", "azw3", "azw", "prc", "fb2", "txt", "pdf", "cbz", "html", "htm", "xhtml", "md"];

pub fn ext_of(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if name.ends_with(".fb2.zip") {
        return "fb2.zip".into();
    }
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).unwrap_or_default()
}

pub fn is_supported(path: &Path) -> bool {
    let e = ext_of(path);
    e == "fb2.zip" || SUPPORTED.contains(&e.as_str())
}

/// Fully open a book for reading.
pub fn open(path: &Path) -> Result<Book, String> {
    let mut book = match ext_of(path).as_str() {
        "epub" => epub::open(path)?,
        "mobi" | "azw3" | "azw" | "prc" => mobi::open(path)?,
        "fb2" | "fb2.zip" => fb2::open(path)?,
        "txt" | "md" => txt::open(path)?,
        "html" | "htm" | "xhtml" => txt::open_html(path)?,
        "pdf" => pdf::open(path)?,
        "cbz" => cbz::open(path)?,
        e => return Err(format!("unsupported format: .{e}")),
    };
    if book.meta.title.trim().is_empty() {
        book.meta.title = util::title_from_filename(path);
    }
    if let Content::Flow(f) = &book.content {
        if f.sections.is_empty() || f.sections.iter().all(|s| s.blocks.is_empty()) {
            return Err("book has no readable text (it may be DRM-protected or image-only)".into());
        }
    }
    if book.toc.is_empty() {
        if let Content::Flow(f) = &book.content {
            book.toc = util::toc_from_headings(f);
        }
    }
    Ok(book)
}

/// Read metadata (and cover bytes) cheaply for the library index.
pub fn read_meta(path: &Path) -> Meta {
    let mut m = match ext_of(path).as_str() {
        "epub" => epub::read_meta(path).unwrap_or_default(),
        "mobi" | "azw3" | "azw" | "prc" => mobi::read_meta(path).unwrap_or_default(),
        "fb2" | "fb2.zip" => fb2::read_meta(path).unwrap_or_default(),
        "pdf" => pdf::read_meta(path).unwrap_or_default(),
        "cbz" => cbz::read_meta(path).unwrap_or_default(),
        _ => Meta::default(),
    };
    if m.title.trim().is_empty() {
        m.title = util::title_from_filename(path);
    }
    m
}
