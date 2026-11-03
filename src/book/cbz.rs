//! Comic book archives (.cbz): a zip of page images.

use std::path::Path;

use super::util::{is_image_name, natural_cmp, read_zip_entry};
use super::{Book, Content, Fixed, FixedKind, Meta};

fn entries(path: &Path) -> Result<Vec<String>, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let zip = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())?;
    let mut v: Vec<String> = zip.file_names().filter(|n| is_image_name(n) && !n.starts_with("__MACOSX")).map(|s| s.to_string()).collect();
    v.sort_by(|a, b| natural_cmp(a, b));
    Ok(v)
}

pub fn read_meta(path: &Path) -> Result<Meta, String> {
    let e = entries(path)?;
    let cover = e.first().and_then(|n| read_zip_entry(path, n).ok());
    Ok(Meta { cover, ..Default::default() })
}

pub fn open(path: &Path) -> Result<Book, String> {
    let e = entries(path)?;
    if e.is_empty() {
        return Err("no images in comic archive".into());
    }
    let pages = e.len();
    Ok(Book {
        meta: Meta::default(),
        toc: Vec::new(),
        content: Content::Fixed(Fixed { kind: FixedKind::Cbz { entries: e }, path: path.to_path_buf(), pages }),
    })
}

pub fn page_bytes(path: &Path, entries: &[String], page: usize) -> Option<Vec<u8>> {
    read_zip_entry(path, entries.get(page)?).ok()
}
