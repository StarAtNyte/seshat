//! A deliberately small CSS subset: enough to honour the typographic intent
//! of most ebooks (bold/italic classes, centring, hidden elements, small caps)
//! without a full cascade. Only single compound selectors (`p`, `.c`, `p.c`,
//! `#id`) are applied; descendant/combinator rules are skipped so we never
//! over-apply styles.

use std::collections::HashMap;

use super::Align;

#[derive(Clone, Copy, Default, Debug)]
pub struct Decl {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub smallcaps: Option<bool>,
    pub mono: Option<bool>,
    pub sup: Option<bool>,
    pub sub: Option<bool>,
    pub align: Option<Align>,
    pub hidden: Option<bool>,
    pub size: Option<f32>,
    pub indent: Option<f32>,
    pub block: Option<bool>,
    /// `direction: rtl|ltr`
    pub rtl: Option<bool>,
}

impl Decl {
    pub fn merge(&mut self, o: &Decl) {
        macro_rules! m {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f = o.$f; } )* };
        }
        m!(bold, italic, underline, strike, smallcaps, mono, sup, sub, align, hidden, size, indent, block, rtl);
    }
    pub fn is_empty(&self) -> bool {
        self.bold.is_none()
            && self.italic.is_none()
            && self.underline.is_none()
            && self.strike.is_none()
            && self.smallcaps.is_none()
            && self.mono.is_none()
            && self.sup.is_none()
            && self.sub.is_none()
            && self.align.is_none()
            && self.hidden.is_none()
            && self.size.is_none()
            && self.indent.is_none()
            && self.block.is_none()
            && self.rtl.is_none()
    }
}

#[derive(Clone, Debug)]
struct Rule {
    tag: Option<String>,
    classes: Vec<String>,
    id: Option<String>,
    specificity: u32,
    order: u32,
    decl: Decl,
}

#[derive(Clone, Debug, Default)]
pub struct Sheet {
    rules: Vec<Rule>,
    /// key ("t:p", ".c", "#id") → rule indices
    index: HashMap<String, Vec<usize>>,
}

impl Sheet {
    pub fn parse(css: &str) -> Sheet {
        let mut sheet = Sheet::default();
        let css = strip_comments(css);
        sheet.parse_block(&css);
        for (i, r) in sheet.rules.iter().enumerate() {
            let key = if let Some(id) = &r.id {
                format!("#{id}")
            } else if let Some(c) = r.classes.first() {
                format!(".{c}")
            } else if let Some(t) = &r.tag {
                format!("t:{t}")
            } else {
                continue;
            };
            sheet.index.entry(key).or_default().push(i);
        }
        sheet
    }

    pub fn extend(&mut self, other: &Sheet) {
        let base = self.rules.len();
        let order_base = self.rules.iter().map(|r| r.order + 1).max().unwrap_or(0);
        for r in &other.rules {
            let mut r = r.clone();
            r.order += order_base;
            self.rules.push(r);
        }
        for (k, v) in &other.index {
            self.index.entry(k.clone()).or_default().extend(v.iter().map(|i| i + base));
        }
    }

    fn parse_block(&mut self, css: &str) {
        let mut rest = css;
        while let Some(open) = rest.find('{') {
            let selector = rest[..open].trim();
            let Some(close) = matching_brace(&rest[open..]) else { break };
            let body = &rest[open + 1..open + close];
            rest = &rest[open + close + 1..];
            // at-rule without block before this one (e.g. @import ...;)
            let selector = selector.rsplit(';').next().unwrap_or(selector).trim();
            if selector.starts_with("@media") {
                let s = selector.to_lowercase();
                if !s.contains("print") && !s.contains("amzn-mobi") {
                    self.parse_block(body);
                }
                continue;
            }
            if selector.starts_with('@') {
                continue;
            }
            let decl = parse_decls(body);
            if decl.is_empty() {
                continue;
            }
            for sel in selector.split(',') {
                if let Some(mut r) = parse_selector(sel.trim()) {
                    r.decl = decl;
                    r.order = self.rules.len() as u32;
                    self.rules.push(r);
                }
            }
        }
    }

    /// Computed declarations for an element.
    pub fn compute(&self, tag: &str, classes: &[&str], id: Option<&str>) -> Decl {
        let mut cands: Vec<usize> = Vec::new();
        if let Some(v) = self.index.get(&format!("t:{tag}")) {
            cands.extend(v);
        }
        for c in classes {
            if let Some(v) = self.index.get(&format!(".{c}")) {
                cands.extend(v);
            }
        }
        if let Some(id) = id {
            if let Some(v) = self.index.get(&format!("#{id}")) {
                cands.extend(v);
            }
        }
        let mut matched: Vec<&Rule> = cands
            .into_iter()
            .map(|i| &self.rules[i])
            .filter(|r| {
                r.tag.as_deref().map_or(true, |t| t == tag)
                    && r.classes.iter().all(|c| classes.contains(&c.as_str()))
                    && r.id.as_deref().map_or(true, |i| Some(i) == id)
            })
            .collect();
        matched.sort_by_key(|r| (r.specificity, r.order));
        matched.dedup_by_key(|r| r.order);
        let mut d = Decl::default();
        for r in matched {
            d.merge(&r.decl);
        }
        d
    }
}

/// Whether a stylesheet sets `writing-mode: vertical-rl` (any vendor prefix)
/// on any selector. Used to switch a CJK book to vertical layout.
pub fn declares_vertical(css: &str) -> bool {
    let css = strip_comments(css).to_ascii_lowercase();
    let mut rest = css.as_str();
    while let Some(i) = rest.find("writing-mode") {
        rest = &rest[i + 12..];
        let decl = rest.trim_start();
        if let Some(v) = decl.strip_prefix(':') {
            let v = v.split([';', '}']).next().unwrap_or("");
            if v.contains("vertical-rl") || v.contains("tb-rl") {
                return true;
            }
        }
    }
    false
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i + 2..].find("*/") {
            Some(j) => rest = &rest[i + 2 + j + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_selector(sel: &str) -> Option<Rule> {
    if sel.is_empty() || sel.contains(|c: char| c.is_whitespace() || matches!(c, '>' | '+' | '~' | ':' | '[' | '*')) {
        return None;
    }
    let mut tag = None;
    let mut classes = Vec::new();
    let mut id = None;
    let mut cur = String::new();
    let mut kind = 't';
    let flush = |kind: char, cur: &mut String, tag: &mut Option<String>, classes: &mut Vec<String>, id: &mut Option<String>| {
        if !cur.is_empty() {
            match kind {
                't' => *tag = Some(cur.to_lowercase()),
                '.' => classes.push(cur.clone()),
                '#' => *id = Some(cur.clone()),
                _ => {}
            }
        }
        cur.clear();
    };
    for c in sel.chars() {
        match c {
            '.' | '#' => {
                flush(kind, &mut cur, &mut tag, &mut classes, &mut id);
                kind = c;
            }
            _ => cur.push(c),
        }
    }
    flush(kind, &mut cur, &mut tag, &mut classes, &mut id);
    let tag = tag.map(|t| t.rsplit('|').next().unwrap_or(&t).to_string());
    if tag.as_deref().is_some_and(|t| matches!(t, "body" | "html")) {
        return None;
    }
    let specificity = id.is_some() as u32 * 100 + classes.len() as u32 * 10 + tag.is_some() as u32;
    Some(Rule { tag, classes, id, specificity, order: 0, decl: Decl::default() })
}

pub fn parse_decls(body: &str) -> Decl {
    let mut d = Decl::default();
    for decl in body.split(';') {
        let Some((k, v)) = decl.split_once(':') else { continue };
        let k = k.trim().to_lowercase();
        let v = v.trim().to_lowercase().replace("!important", "");
        let v = v.trim();
        match k.as_str() {
            "font-weight" => {
                d.bold = match v {
                    "bold" | "bolder" => Some(true),
                    "normal" | "lighter" => Some(false),
                    n => n.parse::<u32>().ok().map(|w| w >= 600),
                }
            }
            "font-style" => d.italic = Some(v == "italic" || v == "oblique"),
            "font-variant" | "font-variant-caps" => d.smallcaps = Some(v.contains("small-caps")),
            "text-decoration" | "text-decoration-line" => {
                d.underline = Some(v.contains("underline"));
                d.strike = Some(v.contains("line-through"));
            }
            "font-family" => {
                if v.contains("monospace") || v.contains("courier") {
                    d.mono = Some(true);
                }
            }
            "vertical-align" => {
                if v == "super" {
                    d.sup = Some(true)
                } else if v == "sub" {
                    d.sub = Some(true)
                } else if v == "baseline" {
                    d.sup = Some(false);
                    d.sub = Some(false);
                }
            }
            "text-align" => {
                d.align = match v {
                    "center" => Some(Align::Center),
                    "right" | "end" => Some(Align::End),
                    "left" | "start" => Some(Align::Start),
                    "justify" => Some(Align::Justify),
                    _ => None,
                }
            }
            "display" => {
                if v == "none" {
                    d.hidden = Some(true);
                } else {
                    d.hidden = Some(false);
                    d.block = Some(v != "inline" && v != "inline-block");
                }
            }
            "visibility" => {
                if v == "hidden" {
                    d.hidden = Some(true);
                }
            }
            "font-size" => d.size = parse_size(v),
            "direction" => {
                d.rtl = match v {
                    "rtl" => Some(true),
                    "ltr" => Some(false),
                    _ => None,
                }
            }
            "margin-left" | "padding-left" => {
                if let Some(em) = parse_em(v) {
                    if em >= 1.0 {
                        d.indent = Some(em.min(4.0));
                    }
                }
            }
            "margin" => {
                // margin: top right bottom left
                let parts: Vec<&str> = v.split_whitespace().collect();
                let left = match parts.len() {
                    4 => parts[3],
                    2 | 3 => parts[1],
                    _ => "",
                };
                if let Some(em) = parse_em(left) {
                    if em >= 1.0 {
                        d.indent = Some(em.min(4.0));
                    }
                }
            }
            _ => {}
        }
    }
    d
}

fn parse_em(v: &str) -> Option<f32> {
    if let Some(n) = v.strip_suffix("rem").or_else(|| v.strip_suffix("em")) {
        return n.trim().parse().ok();
    }
    if let Some(n) = v.strip_suffix("px") {
        return n.trim().parse::<f32>().ok().map(|p| p / 16.0);
    }
    if let Some(n) = v.strip_suffix('%') {
        // percentage of the column width; ~30em column
        return n.trim().parse::<f32>().ok().map(|p| p * 0.3);
    }
    None
}

fn parse_size(v: &str) -> Option<f32> {
    let s = match v {
        "xx-small" => 0.6,
        "x-small" => 0.75,
        "small" | "smaller" => 0.85,
        "medium" => 1.0,
        "large" | "larger" => 1.15,
        "x-large" => 1.4,
        "xx-large" => 1.7,
        _ => {
            if let Some(n) = v.strip_suffix('%') {
                n.trim().parse::<f32>().ok()? / 100.0
            } else if let Some(n) = v.strip_suffix("rem").or_else(|| v.strip_suffix("em")) {
                n.trim().parse::<f32>().ok()?
            } else if let Some(n) = v.strip_suffix("px") {
                n.trim().parse::<f32>().ok()? / 16.0
            } else if let Some(n) = v.strip_suffix("pt") {
                n.trim().parse::<f32>().ok()? / 12.0
            } else {
                return None;
            }
        }
    };
    Some(s.clamp(0.6, 2.5))
}

#[cfg(test)]
mod tests {
    use super::parse_em;

    #[test]
    fn rem_and_em() {
        assert_eq!(parse_em("2rem"), Some(2.0));
        assert_eq!(parse_em("1.5em"), Some(1.5));
        assert_eq!(parse_em("16px"), Some(1.0));
    }
}
