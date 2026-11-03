//! Online lookups for selected text: dictionary, Wikipedia, translation.
//! Only runs when the user explicitly asks for a lookup.

use crate::i18n::tr;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Dictionary,
    Wikipedia,
    Translate,
}

impl Kind {
    pub fn title(self) -> &'static str {
        match self {
            Kind::Dictionary => tr("Dictionary"),
            Kind::Wikipedia => "Wikipedia",
            Kind::Translate => tr("Translation"),
        }
    }
}

pub struct Answer {
    pub heading: String,
    pub body: String,
    pub url: Option<String>,
}

const UA: &str = concat!("Seshat/", env!("CARGO_PKG_VERSION"));

fn client(proxy: &Option<String>) -> reqwest::blocking::Client {
    let mut b = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).user_agent(UA);
    if let Some(p) = proxy {
        if let Ok(px) = reqwest::Proxy::all(p) {
            b = b.proxy(px);
        }
    }
    b.build().unwrap_or_else(|_| reqwest::blocking::Client::new())
}

pub fn run(kind: Kind, text: &str, book_lang: &str, target_lang: &str, proxy: &Option<String>, dict_dirs: &[String]) -> Result<Answer, String> {
    let text = text.trim();
    if kind == Kind::Dictionary && !dict_dirs.is_empty() {
        if let Some(a) = offline(text, dict_dirs) {
            return Ok(a);
        }
    }
    let c = client(proxy);
    match kind {
        Kind::Dictionary => dictionary(&c, text, book_lang),
        Kind::Wikipedia => wikipedia(&c, text, book_lang),
        Kind::Translate => translate(&c, text, target_lang),
    }
}

/// Offline dictionaries first (StarDict / dictd); None when nothing matches.
fn offline(text: &str, dirs: &[String]) -> Option<Answer> {
    let dicts = crate::dict::load_all(dirs);
    let hits = crate::dict::lookup(&dicts, text);
    let (_, head, _) = hits.first()?;
    let heading = head.clone();
    let mut body = String::new();
    for (name, _, def) in &hits {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&format!("[{name}]\n{def}"));
    }
    Some(Answer { heading, body, url: None })
}

fn lang2(l: &str) -> String {
    let l = l.split(['-', '_']).next().unwrap_or("").to_lowercase();
    // used as a Wikipedia/Wiktionary subdomain: only accept a plain two-letter code
    if l.len() == 2 && l.bytes().all(|b| b.is_ascii_lowercase()) {
        l
    } else {
        "en".into()
    }
}

fn dictionary(c: &reqwest::blocking::Client, word: &str, lang: &str) -> Result<Answer, String> {
    let word = word.trim_matches(|ch: char| !ch.is_alphanumeric()).to_lowercase();
    if word.is_empty() {
        return Err("nothing to look up".into());
    }
    let l = lang2(lang);
    // dictionaryapi.dev covers English best; Wiktionary REST for other languages.
    if l == "en" {
        let url = format!("https://api.dictionaryapi.dev/api/v2/entries/en/{}", urlenc(&word));
        let r = c.get(&url).send().map_err(|e| e.to_string())?;
        if r.status().as_u16() == 404 {
            return wiktionary(c, &word, &l);
        }
        let v: serde_json::Value = r.json().map_err(|e| e.to_string())?;
        let mut body = String::new();
        let mut heading = word.clone();
        if let Some(arr) = v.as_array() {
            for entry in arr.iter().take(2) {
                if let Some(p) = entry.get("phonetic").and_then(|p| p.as_str()) {
                    if !heading.contains('/') {
                        heading = format!("{word}  {p}");
                    }
                }
                for m in entry.get("meanings").and_then(|m| m.as_array()).into_iter().flatten() {
                    let pos = m.get("partOfSpeech").and_then(|p| p.as_str()).unwrap_or("");
                    body.push_str(&format!("{pos}\n"));
                    for (i, d) in m.get("definitions").and_then(|d| d.as_array()).into_iter().flatten().take(4).enumerate() {
                        let def = d.get("definition").and_then(|x| x.as_str()).unwrap_or("");
                        body.push_str(&format!("  {}. {def}\n", i + 1));
                        if let Some(ex) = d.get("example").and_then(|x| x.as_str()) {
                            body.push_str(&format!("     “{ex}”\n"));
                        }
                    }
                    if let Some(syn) = m.get("synonyms").and_then(|s| s.as_array()).filter(|s| !s.is_empty()) {
                        let s: Vec<&str> = syn.iter().filter_map(|x| x.as_str()).take(6).collect();
                        body.push_str(&format!("  synonyms: {}\n", s.join(", ")));
                    }
                    body.push('\n');
                }
            }
        }
        if body.is_empty() {
            return wiktionary(c, &word, &l);
        }
        return Ok(Answer { heading, body: body.trim_end().to_string(), url: Some(format!("https://en.wiktionary.org/wiki/{}", urlenc(&word))) });
    }
    wiktionary(c, &word, &l)
}

fn wiktionary(c: &reqwest::blocking::Client, word: &str, lang: &str) -> Result<Answer, String> {
    let url = format!("https://en.wiktionary.org/api/rest_v1/page/definition/{}", urlenc(word));
    let r = c.get(&url).send().map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("no definition found for “{word}”"));
    }
    let v: serde_json::Value = r.json().map_err(|e| e.to_string())?;
    let mut body = String::new();
    let entries = v.get(lang).or_else(|| v.as_object().and_then(|o| o.values().next())).and_then(|e| e.as_array()).cloned().unwrap_or_default();
    for e in entries.iter().take(4) {
        let pos = e.get("partOfSpeech").and_then(|p| p.as_str()).unwrap_or("");
        body.push_str(&format!("{pos}\n"));
        for (i, d) in e.get("definitions").and_then(|d| d.as_array()).into_iter().flatten().take(4).enumerate() {
            let def = strip_html(d.get("definition").and_then(|x| x.as_str()).unwrap_or(""));
            if !def.is_empty() {
                body.push_str(&format!("  {}. {def}\n", i + 1));
            }
        }
        body.push('\n');
    }
    if body.trim().is_empty() {
        return Err(format!("no definition found for “{word}”"));
    }
    Ok(Answer { heading: word.to_string(), body: body.trim_end().to_string(), url: Some(format!("https://en.wiktionary.org/wiki/{}", urlenc(word))) })
}

fn wikipedia(c: &reqwest::blocking::Client, q: &str, lang: &str) -> Result<Answer, String> {
    let l = lang2(lang);
    let q: String = q.chars().take(120).collect();
    // Resolve the best title via search first (handles case/plurals).
    let search = format!("https://{l}.wikipedia.org/w/api.php?action=query&list=search&format=json&srlimit=1&srsearch={}", urlenc(&q));
    let title = c
        .get(&search)
        .send()
        .ok()
        .and_then(|r| r.json::<serde_json::Value>().ok())
        .and_then(|v| v.pointer("/query/search/0/title").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| q.clone());
    let url = format!("https://{l}.wikipedia.org/api/rest_v1/page/summary/{}", urlenc(&title.replace(' ', "_")));
    let r = c.get(&url).send().map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("no Wikipedia article for “{q}”"));
    }
    let v: serde_json::Value = r.json().map_err(|e| e.to_string())?;
    let heading = v.get("title").and_then(|t| t.as_str()).unwrap_or(&title).to_string();
    let mut body = String::new();
    if let Some(d) = v.get("description").and_then(|d| d.as_str()) {
        body.push_str(d);
        body.push_str("\n\n");
    }
    body.push_str(v.get("extract").and_then(|e| e.as_str()).unwrap_or(""));
    let link = v.pointer("/content_urls/desktop/page").and_then(|u| u.as_str()).map(|s| s.to_string());
    Ok(Answer { heading, body, url: link })
}

fn translate(c: &reqwest::blocking::Client, text: &str, target: &str) -> Result<Answer, String> {
    let t = if target.is_empty() { "en" } else { target };
    let text: String = text.chars().take(4500).collect();
    let r = c
        .get("https://translate.googleapis.com/translate_a/single")
        .query(&[("client", "gtx"), ("sl", "auto"), ("tl", t), ("dt", "t"), ("q", text.as_str())])
        .send()
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("translation failed (HTTP {})", r.status()));
    }
    let v: serde_json::Value = r.json().map_err(|e| e.to_string())?;
    let mut out = String::new();
    for seg in v.get(0).and_then(|s| s.as_array()).into_iter().flatten() {
        if let Some(s) = seg.get(0).and_then(|s| s.as_str()) {
            out.push_str(s);
        }
    }
    let src = v.get(2).and_then(|s| s.as_str()).unwrap_or("auto");
    if out.is_empty() {
        return Err("empty translation".into());
    }
    Ok(Answer { heading: format!("{src} → {t}"), body: out, url: None })
}

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    crate::book::html::decode_entities(&out).trim().to_string()
}

pub fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::lang2;

    #[test]
    fn lang2_only_accepts_two_letters() {
        assert_eq!(lang2("fr-CA"), "fr");
        assert_eq!(lang2("DE"), "de");
        assert_eq!(lang2("a/"), "en");
        assert_eq!(lang2("1x"), "en");
        assert_eq!(lang2("é"), "en");
        assert_eq!(lang2(""), "en");
        assert_eq!(lang2("eng"), "en");
    }
}
