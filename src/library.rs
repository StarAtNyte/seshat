//! Local library: scan a folder for EPUB/PDF books, index metadata, and search.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub path: String,
    pub title: String,
    pub author: String,
    pub format: String,
    pub size: u64,
    /// 0.0..=1.0 reading progress, if known.
    pub progress: Option<f32>,
}

/// Scan `dir` for supported ebook files and extract metadata (EPUB only for now).
pub fn scan(dir: &Path) -> Vec<LibraryEntry> {
    let mut entries = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return entries;
    };
    for f in rd.flatten() {
        let path = f.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "epub" | "pdf" | "mobi" | "azw3" | "fb2" | "djvu") {
            continue;
        }
        let (title, author) = if ext == "epub" {
            match crate::epub::open(&path) {
                Ok(b) => (b.title, b.author),
                Err(_) => (String::new(), String::new()),
            }
        } else {
            (String::new(), String::new())
        };
        let fallback = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        entries.push(LibraryEntry {
            path: path.to_string_lossy().into_owned(),
            title: if title.is_empty() { fallback } else { title },
            author,
            format: ext.to_uppercase(),
            size,
            progress: None,
        });
    }
    entries.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    entries
}

/// Filter entries by a query across title and author (case-insensitive, all terms must match).
pub fn search(entries: &[LibraryEntry], query: &str) -> Vec<LibraryEntry> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    if terms.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|e| {
            let hay = format!("{} {}", e.title, e.author).to_lowercase();
            terms.iter().all(|t| hay.contains(t))
        })
        .cloned()
        .collect()
}

// ---------- reading progress persistence ----------

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct ProgressStore {
    /// book path -> (chapter index, scroll offset within chapter in px)
    pub books: std::collections::HashMap<String, Progress>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Progress {
    pub chapter: usize,
    pub scroll: f32,
}

pub fn load_progress() -> ProgressStore {
    let p = crate::app::config_dir_path().join("progress.json");
    if let Ok(text) = std::fs::read_to_string(p) {
        if let Ok(store) = serde_json::from_str(&text) {
            return store;
        }
    }
    ProgressStore::default()
}

pub fn save_progress(store: &ProgressStore) {
    let dir = crate::app::config_dir_path();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(text) = serde_json::to_string_pretty(store) {
        let _ = std::fs::write(dir.join("progress.json"), text);
    }
}

pub fn set_progress(store: &mut ProgressStore, path: &str, progress: Progress) {
    store.books.insert(path.to_string(), progress);
}

pub fn get_progress(store: &ProgressStore, path: &str) -> Option<Progress> {
    store.books.get(path).copied()
}

/// Percentage of chapters read.
pub fn book_percent(entry: &LibraryEntry, store: &ProgressStore, chapters: usize) -> f32 {
    if chapters == 0 {
        return 0.0;
    }
    match get_progress(store, &entry.path) {
        Some(p) => p.chapter as f32 / chapters as f32,
        None => 0.0,
    }
}

pub fn fmt_size(size: u64) -> String {
    if size >= 1_048_576 {
        format!("{:.1} MB", size as f64 / 1_048_576.0)
    } else {
        format!("{} KB", size / 1024)
    }
}
