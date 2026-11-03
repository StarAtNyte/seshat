//! Interface translations. Strings are keyed by their English source text:
//! `tr("Settings")` returns the translation for the active language, or the
//! English text itself when there is none. Catalogs are flat JSON maps
//! (`assets/i18n/<code>.json`) embedded in the binary and parsed on first use.
//!
//! Strings with values use named placeholders so word order can change:
//! `trf!("{n} books", n = count)`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// (code, name in its own language, embedded catalog). Index 0 is English.
pub const LANGS: &[(&str, &str, &str)] = &[
    ("en", "English", ""),
    ("de", "Deutsch", include_str!("../assets/i18n/de.json")),
    ("es", "Español", include_str!("../assets/i18n/es.json")),
    ("fr", "Français", include_str!("../assets/i18n/fr.json")),
    ("pt-BR", "Português (Brasil)", include_str!("../assets/i18n/pt-BR.json")),
    ("ru", "Русский", include_str!("../assets/i18n/ru.json")),
    ("zh-CN", "简体中文", include_str!("../assets/i18n/zh-CN.json")),
    ("ja", "日本語", include_str!("../assets/i18n/ja.json")),
    ("hi", "हिन्दी", include_str!("../assets/i18n/hi.json")),
    ("ne", "नेपाली", include_str!("../assets/i18n/ne.json")),
];

type Catalog = HashMap<&'static str, &'static str>;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static CATALOGS: [OnceLock<Catalog>; 10] =
    [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];

/// Parse a flat JSON object into a catalog (empty on malformed input).
pub fn parse_catalog(json: &str) -> HashMap<String, String> {
    serde_json::from_str(json).unwrap_or_default()
}

fn catalog(idx: usize) -> &'static Catalog {
    CATALOGS[idx].get_or_init(|| {
        parse_catalog(LANGS[idx].2)
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (&*Box::leak(k.into_boxed_str()), &*Box::leak(v.into_boxed_str())))
            .collect()
    })
}

/// Translate an English UI string. Unknown strings (and English) pass through.
pub fn tr<'a>(s: &'a str) -> &'a str {
    let idx = CURRENT.load(Ordering::Relaxed);
    if idx == 0 {
        return s;
    }
    catalog(idx).get(s).copied().unwrap_or(s)
}

/// Replace `{name}` placeholders in `template` (already translated).
pub fn fill(template: &str, args: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Translate and fill named placeholders: `trf!("Page {n}", n = 3)`.
#[macro_export]
macro_rules! trf {
    ($s:expr $(, $k:ident = $v:expr)* $(,)?) => {
        $crate::i18n::fill($crate::i18n::tr($s), &[$((stringify!($k), ($v).to_string())),*])
    };
}

/// Language code for the OS locale, if we ship a catalog for it.
pub fn detect() -> &'static str {
    let loc = sys_locale::get_locale().unwrap_or_default();
    resolve_locale(&loc)
}

pub fn resolve_locale(loc: &str) -> &'static str {
    let l = loc.replace('_', "-").to_lowercase();
    let lang = l.split(['-', '.', '@']).next().unwrap_or("");
    match lang {
        "pt" => "pt-BR",
        "zh" => "zh-CN",
        _ => LANGS.iter().find(|(c, ..)| c.eq_ignore_ascii_case(lang)).map(|(c, ..)| *c).unwrap_or("en"),
    }
}

/// Make `setting` ("" = follow the OS, else a language code) the active
/// language. Returns the resolved code and whether it changed.
pub fn apply(setting: &str) -> (&'static str, bool) {
    let code = if setting.is_empty() { detect() } else { LANGS.iter().find(|(c, ..)| *c == setting).map(|(c, ..)| *c).unwrap_or("en") };
    let idx = LANGS.iter().position(|(c, ..)| *c == code).unwrap_or(0);
    let prev = CURRENT.swap(idx, Ordering::Relaxed);
    (code, prev != idx)
}

/// How much wider UI text runs than English in the active language; used to
/// switch to compact toolbars earlier.
pub fn width_factor() -> f32 {
    match current() {
        "de" | "ru" | "fr" | "es" | "pt-BR" => 1.2,
        "hi" | "ne" => 1.1,
        _ => 1.0,
    }
}

/// Active language code.
pub fn current() -> &'static str {
    LANGS[CURRENT.load(Ordering::Relaxed)].0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every string that goes through `tr(...)` / `trf!(...)` / `tr_key!(...)`
    /// in the sources (the keys translators work from).
    fn source_strings() -> BTreeSet<String> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut set = BTreeSet::new();
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            // Skip this module's own docs/tests.
            if f.ends_with("i18n.rs") {
                continue;
            }
            for marker in ["tr(\"", "trf!(\"", "tr_key!(\""] {
                let mut rest = src.as_str();
                while let Some(i) = rest.find(marker) {
                    // Not part of a longer identifier (e.g. `str(`, `entr(`).
                    let before = rest[..i].chars().next_back();
                    let start = i + marker.len();
                    let b = rest.as_bytes();
                    let mut j = start;
                    let mut lit = String::new();
                    while j < b.len() && b[j] != b'"' {
                        if b[j] == b'\\' && j + 1 < b.len() {
                            match b[j + 1] {
                                b'n' => lit.push('\n'),
                                b'"' => lit.push('"'),
                                b'\\' => lit.push('\\'),
                                b'\'' => lit.push('\''),
                                b'u' => {
                                    let close = rest[j..].find('}').unwrap() + j;
                                    let hex = &rest[j + 3..close];
                                    lit.push(char::from_u32(u32::from_str_radix(hex, 16).unwrap()).unwrap());
                                    j = close + 1;
                                    continue;
                                }
                                _ => {}
                            }
                            j += 2;
                        } else {
                            let ch = rest[j..].chars().next().unwrap();
                            lit.push(ch);
                            j += ch.len_utf8();
                        }
                    }
                    if !before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        set.insert(lit);
                    }
                    rest = &rest[start..];
                }
            }
        }
        set
    }

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            if let Some(j) = rest[i..].find('}') {
                out.insert(rest[i + 1..i + j].to_string());
                rest = &rest[i + j + 1..];
            } else {
                break;
            }
        }
        out
    }

    #[test]
    fn catalogs_match_sources() {
        let src = source_strings();
        assert!(src.len() > 100, "found only {} source strings", src.len());
        let mut problems = Vec::new();
        for (code, _, json) in LANGS.iter().skip(1) {
            let cat = parse_catalog(json);
            let missing: Vec<&String> = src.iter().filter(|s| cat.get(*s).map_or(true, |v| v.is_empty())).collect();
            println!("{code}: {} keys, {} of {} source strings untranslated", cat.len(), missing.len(), src.len());
            for m in missing.iter().take(15) {
                println!("    untranslated: {m:?}");
            }
            for (k, v) in &cat {
                if !src.contains(k) {
                    problems.push(format!("{code}: stale key (not in the sources): {k:?}"));
                } else if placeholders(k) != placeholders(v) && !v.is_empty() {
                    problems.push(format!("{code}: placeholders differ for {k:?} -> {v:?}"));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn placeholders_fill() {
        assert_eq!(fill("{n} of {m}", &[("n", "2".into()), ("m", "5".into())]), "2 of 5");
        assert_eq!(resolve_locale("pt_PT.UTF-8"), "pt-BR");
        assert_eq!(resolve_locale("zh-Hans-CN"), "zh-CN");
        assert_eq!(resolve_locale("xx"), "en");
    }
}
