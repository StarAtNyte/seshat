//! Cross-platform system font discovery (fontdb) and egui font installation.
//!
//! Reading fonts get four faces (regular/bold/italic/bold-italic) registered
//! as egui families `book`, `book-b`, `book-i`, `book-bi`, plus `mono`.
//! A CJK fallback is attached only when a book actually needs it.

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

static SG_R: &[u8] = include_bytes!("../assets/fonts/SpaceGrotesk-Regular.ttf");
static SG_M: &[u8] = include_bytes!("../assets/fonts/SpaceGrotesk-Medium.ttf");
static SG_B: &[u8] = include_bytes!("../assets/fonts/SpaceGrotesk-Bold.ttf");
static PLEX_R: &[u8] = include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf");
static PLEX_M: &[u8] = include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf");

pub const SERIF_PREFS: &[&str] = &[
    "Literata", "Bookerly", "Charis SIL", "Source Serif 4", "Source Serif Pro", "Noto Serif", "Georgia", "Iowan Old Style", "Palatino Linotype",
    "Palatino", "Cambria", "Constantia", "DejaVu Serif", "Liberation Serif", "Times New Roman", "FreeSerif",
];
const MONO_PREFS: &[&str] = &["JetBrains Mono", "Cascadia Mono", "Consolas", "SF Mono", "Menlo", "Fira Mono", "DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono", "Courier New"];
const CJK_PREFS: &[&str] = &[
    "Noto Sans CJK SC", "Noto Serif CJK SC", "Source Han Sans SC", "Microsoft YaHei", "PingFang SC", "Hiragino Sans GB", "Hiragino Sans", "Yu Gothic",
    "MS Gothic", "WenQuanYi Micro Hei", "Malgun Gothic",
];

pub struct FontDb {
    db: fontdb::Database,
    pub families: Vec<String>,
    cache: HashMap<(String, bool, bool), Option<Arc<(Vec<u8>, u32)>>>,
}

impl FontDb {
    /// Scans system font directories. Takes 50–300 ms; run off the UI thread.
    pub fn load() -> FontDb {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut families: Vec<String> = db
            .faces()
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .filter(|n| !n.starts_with('.') && !n.to_lowercase().contains("emoji"))
            .collect();
        families.sort_by_key(|a| a.to_lowercase());
        families.dedup();
        FontDb { db, families, cache: HashMap::new() }
    }

    pub fn has(&self, family: &str) -> bool {
        self.families.iter().any(|f| f.eq_ignore_ascii_case(family))
    }

    pub fn first_available(&self, prefs: &[&str]) -> Option<String> {
        prefs.iter().find(|p| self.has(p)).map(|s| s.to_string())
    }

    pub fn default_serif(&self) -> Option<String> {
        self.first_available(SERIF_PREFS)
    }

    fn face(&mut self, family: &str, bold: bool, italic: bool) -> Option<Arc<(Vec<u8>, u32)>> {
        let key = (family.to_string(), bold, italic);
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        let q = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: if bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL },
            style: if italic { fontdb::Style::Italic } else { fontdb::Style::Normal },
            stretch: fontdb::Stretch::Normal,
        };
        let res = self.db.query(&q).and_then(|id| {
            // fontdb returns the closest match; reject if it ignored the style we asked for.
            let info = self.db.face(id)?;
            if bold && info.weight.0 < 600 {
                return None;
            }
            if italic && info.style == fontdb::Style::Normal {
                return None;
            }
            self.db.with_face_data(id, |d, i| Arc::new((d.to_vec(), i)))
        });
        self.cache.insert(key, res.clone());
        res
    }
}

fn data(face: &(Vec<u8>, u32)) -> Arc<FontData> {
    let mut fd = FontData::from_owned(face.0.clone());
    fd.index = face.1;
    Arc::new(fd)
}

#[derive(Clone, PartialEq, Default)]
pub struct Installed {
    pub book_family: String,
    pub cjk: bool,
    pub has_bold: bool,
    pub has_italic: bool,
}

/// Build and install the font set. Returns what was actually installed.
pub fn install(ctx: &egui::Context, db: Option<&mut FontDb>, book_family: &str, cjk: bool) -> Installed {
    let mut defs = FontDefinitions::default();
    let base_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let base_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let mut inst = Installed { book_family: String::new(), cjk: false, has_bold: false, has_italic: false };

    let mut names: HashMap<&str, Vec<String>> = HashMap::new();
    let mut cjk_name: Option<String> = None;
    // Embedded identity fonts (OFL): identical look on every OS.
    for (k, b) in [("sg-r", SG_R), ("sg-m", SG_M), ("sg-b", SG_B), ("plex-r", PLEX_R), ("plex-m", PLEX_M)] {
        defs.font_data.insert(k.into(), Arc::new(FontData::from_static(b)));
    }
    defs.families.entry(FontFamily::Proportional).or_default().insert(0, "sg-r".into());
    for (fam, face, mono) in [("ui-m", "sg-m", false), ("ui-b", "sg-b", false), ("label", "plex-r", true), ("label-m", "plex-m", true)] {
        let mut chain = vec![face.to_string()];
        chain.extend(if mono { base_mono.clone() } else { base_prop.clone() });
        defs.families.insert(FontFamily::Name(fam.into()), chain);
    }
    if let Some(db) = db {
        let fam = if !book_family.is_empty() && db.has(book_family) { Some(book_family.to_string()) } else { db.default_serif() };
        if let Some(fam) = fam {
            inst.book_family = fam.clone();
            let reg = db.face(&fam, false, false);
            if let Some(r) = &reg {
                defs.font_data.insert("book-r".into(), data(r));
            }
            for (key, b, i) in [("book-b", true, false), ("book-i", false, true), ("book-bi", true, true)] {
                if let Some(f) = db.face(&fam, b, i) {
                    defs.font_data.insert(key.into(), data(&f));
                    names.insert(key, vec![key.to_string()]);
                    if b && !i {
                        inst.has_bold = true;
                    }
                    if i && !b {
                        inst.has_italic = true;
                    }
                }
            }
            if reg.is_some() {
                names.insert("book", vec!["book-r".into()]);
            }
        }
        if let Some(m) = db.first_available(MONO_PREFS) {
            if let Some(f) = db.face(&m, false, false) {
                defs.font_data.insert("mono-r".into(), data(&f));
                names.insert("mono", vec!["mono-r".into()]);
            }
        }
        if cjk {
            if let Some(c) = db.first_available(CJK_PREFS) {
                if let Some(f) = db.face(&c, false, false) {
                    defs.font_data.insert("cjk".into(), data(&f));
                    cjk_name = Some("cjk".into());
                    inst.cjk = true;
                }
            }
        }
    }
    // Fallback chains: specific face → regular → egui defaults (+ CJK).
    for fam in ["book", "book-b", "book-i", "book-bi", "mono"] {
        let mut chain = names.get(fam).cloned().unwrap_or_default();
        if fam.starts_with("book") && fam != "book" {
            chain.extend(names.get("book").cloned().unwrap_or_default());
        }
        if fam == "mono" {
            chain.push("plex-r".into());
            chain.extend(base_mono.clone());
        } else {
            chain.extend(base_prop.clone());
        }
        if let Some(c) = &cjk_name {
            chain.push(c.clone());
        }
        defs.families.insert(FontFamily::Name(fam.into()), chain);
    }
    if let Some(c) = &cjk_name {
        defs.families.entry(FontFamily::Proportional).or_default().push(c.clone());
    }
    ctx.set_fonts(defs);
    inst
}

/// Does the text sample need a CJK font?
pub fn needs_cjk(sample: &str) -> bool {
    sample.chars().filter(|c| matches!(*c as u32, 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF)).take(20).count() >= 20
}
