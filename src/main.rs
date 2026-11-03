#![allow(dead_code)]
mod book;
fn main() {
    for a in std::env::args().skip(1) {
        let t = std::time::Instant::now();
        match book::open(std::path::Path::new(&a)) {
            Ok(b) => { if let Ok(s) = std::env::var("DUMP") { dump(&b, s.parse().unwrap()); }
                let (secs, chars) = b.flow().map(|f| (f.sections.len(), f.total_chars())).unwrap_or((b.fixed().map(|f| f.pages).unwrap_or(0), 0));
                println!("{:?} | {} | {} | sections/pages {} chars {} toc {} cover {} | {:?}", b.meta.title, b.meta.author, b.meta.language, secs, chars, b.toc.len(), b.meta.cover.as_ref().map(|c| c.len()).unwrap_or(0), t.elapsed());
            }
            Err(e) => println!("ERR {a}: {e}"),
        }
    }
}
#[allow(unused)]
fn dump(b: &book::Book, s: usize) {
    let f = b.flow().unwrap();
    for bl in f.sections[s].blocks.iter().take(14) {
        let t: String = bl.spans.iter().map(|sp| if sp.style.0 != 0 || sp.link.is_some() { format!("[{}{}:{}]", sp.style.0, if sp.link.is_some() {"L"} else {""}, sp.text) } else { sp.text.clone() }).collect();
        println!("{:?} {:?} | {}", bl.kind, bl.align, t.chars().take(160).collect::<String>());
    }
}
