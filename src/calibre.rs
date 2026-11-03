//! Calibre sidecar import: a Calibre library keeps one `metadata.opf` and a
//! `cover.jpg` next to every book (`Author/Title (id)/`). We read those two
//! files only; `metadata.db` is never touched and nothing is written back.

use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

#[derive(Default, Debug)]
pub struct CalMeta {
    pub title: String,
    pub author: String,
    pub series: String,
    pub series_index: f32,
    pub tags: Vec<String>,
    /// 0..=5 stars
    pub rating: u8,
    pub description: String,
    pub publisher: String,
    pub language: String,
    pub cover: Option<Vec<u8>>,
}

/// Sidecar data for a book file, if its folder holds a Calibre `metadata.opf`.
pub fn read(book: &Path) -> Option<CalMeta> {
    let dir = book.parent()?;
    let text = std::fs::read_to_string(dir.join("metadata.opf")).ok()?;
    if !text.contains("calibre") && !text.contains("<dc:title") {
        return None;
    }
    let mut m = parse(&text);
    m.cover = read_cover(dir);
    Some(m)
}

pub fn read_cover(dir: &Path) -> Option<Vec<u8>> {
    ["cover.jpg", "cover.jpeg", "cover.png"].iter().find_map(|n| std::fs::read(dir.join(n)).ok())
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    s.rsplit(':').next().unwrap_or("").to_lowercase()
}

fn attrs(e: &BytesStart) -> Vec<(String, String)> {
    e.attributes()
        .flatten()
        .map(|a| (String::from_utf8_lossy(a.key.as_ref()).to_string(), a.unescape_value().map(|v| v.to_string()).unwrap_or_default()))
        .collect()
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = false;
    for c in s.chars() {
        match c {
            '<' => depth = true,
            '>' => {
                depth = false;
                out.push(' ');
            }
            _ if !depth => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn parse(xml: &str) -> CalMeta {
    let mut m = CalMeta::default();
    let mut r = Reader::from_str(xml);
    let mut cur: Option<String> = None;
    let mut buf = String::new();
    let mut authors: Vec<String> = Vec::new();
    let mut meta = |e: &BytesStart, m: &mut CalMeta| {
        let a = attrs(e);
        let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
        let content = get("content");
        match get("name").as_str() {
            "calibre:series" => m.series = content,
            "calibre:series_index" => m.series_index = content.trim().parse().unwrap_or(0.0),
            // Calibre stores 0..10
            "calibre:rating" => m.rating = (content.trim().parse::<f32>().unwrap_or(0.0) / 2.0).round().clamp(0.0, 5.0) as u8,
            _ => {}
        }
    };
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let n = local(e.name().as_ref());
                if n == "meta" {
                    meta(&e, &mut m);
                } else if matches!(n.as_str(), "title" | "creator" | "subject" | "description" | "publisher" | "language") {
                    cur = Some(n);
                    buf.clear();
                }
            }
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == "meta" {
                    meta(&e, &mut m);
                }
            }
            Ok(Event::Text(t)) => {
                if cur.is_some() {
                    buf.push_str(&t.unescape().map(|c| c.to_string()).unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => {
                if let Some(c) = cur.clone() {
                    if local(e.name().as_ref()) == c {
                        let v = buf.split_whitespace().collect::<Vec<_>>().join(" ");
                        match c.as_str() {
                            "title" if m.title.is_empty() => m.title = v,
                            "creator" if !v.is_empty() && !authors.contains(&v) => authors.push(v),
                            "subject" if !v.is_empty() => m.tags.push(v),
                            "description" => m.description = strip_tags(&buf),
                            "publisher" => m.publisher = v,
                            "language" => m.language = v,
                            _ => {}
                        }
                        cur = None;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    m.author = authors.join(", ");
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_calibre_opf() {
        let x = r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
<dc:title>Dune</dc:title><dc:creator opf:role="aut">Frank Herbert</dc:creator><dc:subject>Science Fiction</dc:subject>
<dc:description>&lt;p&gt;Spice.&lt;/p&gt;</dc:description>
<meta name="calibre:series" content="Dune Chronicles"/><meta name="calibre:series_index" content="1.0"/><meta name="calibre:rating" content="8.0"/></metadata></package>"#;
        let m = parse(x);
        assert_eq!(m.title, "Dune");
        assert_eq!(m.author, "Frank Herbert");
        assert_eq!(m.series, "Dune Chronicles");
        assert_eq!(m.series_index, 1.0);
        assert_eq!(m.rating, 4);
        assert_eq!(m.tags, vec!["Science Fiction"]);
        assert_eq!(m.description, "Spice.");
    }
}
