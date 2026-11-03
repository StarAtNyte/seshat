//! Plain text / Markdown / standalone HTML.

use std::collections::HashMap;
use std::path::Path;

use super::css::Sheet;
use super::html::{self, Hooks, Options};
use super::util::{decode_text, split_long_sections};
use super::{Align, Block, BlockKind, Book, Content, Flow, LinkTarget, Meta, Section, Span, Style};

fn is_chapter_heading(line: &str) -> bool {
    let l = line.trim();
    let n = l.chars().count();
    if n == 0 || n > 70 {
        return false;
    }
    let low = l.to_lowercase();
    for kw in ["chapter ", "part ", "book ", "prologue", "epilogue", "preface", "introduction", "contents", "глава ", "capítulo ", "chapitre ", "kapitel "] {
        if low.starts_with(kw) {
            return true;
        }
    }
    // Roman numerals alone ("IV", "XII.")
    let core = l.trim_end_matches('.');
    if !core.is_empty() && core.len() <= 7 && core.chars().all(|c| "IVXLC".contains(c)) {
        return true;
    }
    // CJK: 第…章 / 第…回
    if l.starts_with('第') && (l.contains('章') || l.contains('回') || l.contains('节')) && n < 30 {
        return true;
    }
    false
}

fn para(text: String) -> Block {
    let mut b = Block::new(BlockKind::Para);
    b.spans.push(Span { text, style: Style::default(), link: None, size: 1.0 });
    b
}

pub fn open(path: &Path) -> Result<Book, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = decode_text(&bytes).replace("\r\n", "\n").replace('\r', "\n");
    let md = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"));
    let lines: Vec<&str> = text.lines().collect();
    let blank = lines.iter().filter(|l| l.trim().is_empty()).count();
    // If the file uses blank lines between paragraphs, join wrapped lines.
    let blank_separated = blank * 5 > lines.len().max(1);

    let mut sections: Vec<Section> = Vec::new();
    let mut cur = Section::default();
    let mut buf = String::new();
    let mut in_code = false;
    let flush = |buf: &mut String, cur: &mut Section| {
        let t = buf.trim();
        if !t.is_empty() {
            cur.blocks.push(para(t.to_string()));
        }
        buf.clear();
    };
    for line in lines {
        let t = line.trim();
        if md && t.starts_with("```") {
            flush(&mut buf, &mut cur);
            in_code = !in_code;
            continue;
        }
        if in_code {
            let mut b = Block::new(BlockKind::Pre);
            b.spans.push(Span { text: line.to_string(), style: Style(Style::MONO), link: None, size: 1.0 });
            cur.blocks.push(b);
            continue;
        }
        let md_heading = md && t.starts_with('#');
        if md_heading || (!md && is_chapter_heading(t) && (buf.trim().is_empty())) {
            flush(&mut buf, &mut cur);
            let level = if md_heading { t.chars().take_while(|&c| c == '#').count().min(6) as u8 } else { 2 };
            let title = t.trim_start_matches('#').trim().to_string();
            if level <= 2 && !cur.blocks.is_empty() {
                sections.push(std::mem::take(&mut cur));
            }
            if cur.title.is_empty() {
                cur.title = title.clone();
            }
            let mut b = Block::new(BlockKind::Heading(level));
            b.spans.push(Span { text: title, style: Style(Style::BOLD), link: None, size: 1.0 });
            b.align = Some(Align::Center);
            cur.blocks.push(b);
            continue;
        }
        if t.is_empty() {
            flush(&mut buf, &mut cur);
        } else if blank_separated {
            if !buf.is_empty() {
                buf.push(' ');
            }
            buf.push_str(t);
        } else {
            flush(&mut buf, &mut cur);
            buf.push_str(t);
        }
    }
    flush(&mut buf, &mut cur);
    if !cur.blocks.is_empty() {
        sections.push(cur);
    }
    if sections.is_empty() {
        sections.push(Section::default());
    }
    for s in &mut sections {
        if s.title.is_empty() {
            s.title = s.blocks.first().map(|b| b.text().chars().take(50).collect()).unwrap_or_default();
        }
    }
    let (sections, _) = split_long_sections(sections, 300);
    let n = sections.len();
    Ok(Book {
        meta: Meta { title: super::util::title_from_filename(path), author: super::util::author_from_filename(path), ..Default::default() },
        toc: Vec::new(),
        content: Content::Flow(Flow { sections, links: Vec::new(), resources: HashMap::new(), spine_sections: (0..n).collect() }),
    })
}

pub fn open_html(path: &Path) -> Result<Book, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let src = decode_text(&bytes);
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut resources = HashMap::new();
    let mut raw_links: Vec<String> = Vec::new();
    let out = {
        let mut link_fn = |h: &str| {
            raw_links.push(h.to_string());
            (raw_links.len() - 1) as u32
        };
        let mut image_fn = |s: &str| {
            let p = dir.join(super::util::percent_decode(s));
            let b = std::fs::read(&p).ok()?;
            let key = s.to_string();
            resources.insert(key.clone(), super::ResRef::Bytes(std::sync::Arc::new(b)));
            Some(key)
        };
        let dir2 = dir.clone();
        let mut css_fn = |h: &str| std::fs::read_to_string(dir2.join(h)).ok().map(|s| Sheet::parse(&s));
        let mut hooks = Hooks { link: &mut link_fn, image: &mut image_fn, stylesheet: &mut css_fn };
        html::convert(&src, Sheet::default(), &mut hooks, &Options::default())
    };
    let anchors: HashMap<String, usize> = out.anchors.iter().cloned().collect();
    let title = out.title.clone();
    let (sections, map) = split_long_sections(vec![Section { title: title.clone(), blocks: out.blocks, chars: 0 }], 300);
    let links = raw_links
        .iter()
        .map(|h| {
            if h.contains("://") {
                return LinkTarget::External(h.clone());
            }
            match h.split_once('#').and_then(|(_, f)| anchors.get(f)) {
                Some(&b) => LinkTarget::Internal(super::util::remap(&map, 0, b)),
                None => LinkTarget::Unresolved,
            }
        })
        .collect();
    let n = sections.len();
    Ok(Book {
        meta: Meta { title, ..Default::default() },
        toc: Vec::new(),
        content: Content::Flow(Flow { sections, links, resources, spine_sections: (0..n).collect() }),
    })
}
