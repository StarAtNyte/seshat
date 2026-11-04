//! Background image decoding / PDF page rendering into egui textures.
//!
//! Jobs are served LIFO so whatever was requested most recently (the page on
//! screen) is decoded first; stale prefetches can be cancelled.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::book::ResRef;

#[derive(Clone)]
pub enum Src {
    Res(ResRef),
    File(PathBuf),
    Pdf { path: PathBuf, page: usize, width: u32 },
    Url { url: String, proxy: Option<String> },
}

pub struct Loaded {
    pub tex: TextureHandle,
    /// Natural pixel size of the source image.
    pub size: [usize; 2],
    /// Mean colour of the image (a book's binding takes its cover's colour).
    pub avg: egui::Color32,
}

/// Mean colour, sampled on a coarse grid.
fn mean(img: &ColorImage) -> egui::Color32 {
    let step = (img.pixels.len() / 4096).max(1);
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for p in img.pixels.iter().step_by(step) {
        r += p.r() as u64;
        g += p.g() as u64;
        b += p.b() as u64;
        n += 1;
    }
    let n = n.max(1);
    egui::Color32::from_rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
}

pub enum State {
    Pending,
    Ready(Loaded),
    Failed,
}

type Job = (String, Src, u32);
type Done = (String, Option<(ColorImage, [usize; 2])>);

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    done: Mutex<Vec<Done>>,
    alive: AtomicBool,
}

pub struct TexLoader {
    ctx: egui::Context,
    map: HashMap<String, State>,
    shared: Arc<Shared>,
}

impl Drop for TexLoader {
    fn drop(&mut self) {
        self.shared.alive.store(false, Ordering::Relaxed);
        self.shared.cv.notify_all();
    }
}

fn decode(src: &Src, max_dim: u32, pdf_cache: &mut Option<(PathBuf, hayro::hayro_interpret::hayro_syntax::Pdf)>) -> Option<(ColorImage, [usize; 2])> {
    let bytes = match src {
        Src::Res(r) => r.load()?,
        Src::File(p) => std::fs::read(p).ok()?,
        Src::Url { url, proxy } => {
            // never falls back to a direct connection when a proxy is set but broken
            let client = crate::api::client(proxy, Some(std::time::Duration::from_secs(20))).ok()?;
            let r = client.get(url).send().ok()?;
            if !r.status().is_success() {
                return None;
            }
            // cover images: refuse anything implausibly large
            const CAP: u64 = 32 * 1024 * 1024;
            if r.content_length().is_some_and(|n| n > CAP) {
                return None;
            }
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::Read::take(r, CAP + 1), &mut buf).ok()?;
            if buf.len() as u64 > CAP {
                return None;
            }
            buf
        }
        Src::Pdf { path, page, width } => {
            if pdf_cache.as_ref().map(|(p, _)| p != path).unwrap_or(true) {
                *pdf_cache = Some((path.clone(), crate::book::pdf::load(path).ok()?));
            }
            let pdf = &pdf_cache.as_ref()?.1;
            let (w, h, rgba) = crate::book::pdf::render(pdf, *page, *width)?;
            let img = ColorImage::from_rgba_premultiplied([w as usize, h as usize], &rgba);
            return Some((img, [w as usize, h as usize]));
        }
    };
    let img = image::load_from_memory(&bytes).ok()?;
    let natural = [img.width() as usize, img.height() as usize];
    let img = if img.width().max(img.height()) > max_dim {
        img.resize(max_dim, max_dim, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some((ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), natural))
}

impl TexLoader {
    pub fn new(ctx: &egui::Context, workers: usize) -> Self {
        let shared = Arc::new(Shared { queue: Mutex::new(VecDeque::new()), cv: Condvar::new(), done: Mutex::new(Vec::new()), alive: AtomicBool::new(true) });
        for _ in 0..workers.max(1) {
            let s = shared.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let mut pdf_cache = None;
                loop {
                    let job = {
                        let mut q = s.queue.lock().unwrap();
                        while q.is_empty() && s.alive.load(Ordering::Relaxed) {
                            q = s.cv.wait(q).unwrap();
                        }
                        if !s.alive.load(Ordering::Relaxed) {
                            return;
                        }
                        q.pop_back()
                    };
                    let Some((key, src, max_dim)) = job else { continue };
                    let res = decode(&src, max_dim, &mut pdf_cache);
                    s.done.lock().unwrap().push((key, res));
                    ctx.request_repaint();
                }
            });
        }
        TexLoader { ctx: ctx.clone(), map: HashMap::new(), shared }
    }

    /// Upload finished jobs. Returns true if anything new became available.
    pub fn poll(&mut self) -> bool {
        let done: Vec<Done> = std::mem::take(&mut *self.shared.done.lock().unwrap());
        let changed = !done.is_empty();
        for (key, res) in done {
            let st = match res {
                Some((img, natural)) => {
                    let avg = mean(&img);
                    State::Ready(Loaded { tex: self.ctx.load_texture(&key, img, TextureOptions::LINEAR), size: natural, avg })
                }
                None => State::Failed,
            };
            self.map.insert(key, st);
        }
        changed
    }

    pub fn request(&mut self, key: &str, src: impl FnOnce() -> Option<Src>, max_dim: u32) {
        if self.map.contains_key(key) {
            return;
        }
        let Some(src) = src() else {
            self.map.insert(key.to_string(), State::Failed);
            return;
        };
        self.map.insert(key.to_string(), State::Pending);
        self.shared.queue.lock().unwrap().push_back((key.to_string(), src, max_dim));
        self.shared.cv.notify_one();
    }

    pub fn get(&self, key: &str) -> Option<&Loaded> {
        match self.map.get(key) {
            Some(State::Ready(l)) => Some(l),
            _ => None,
        }
    }

    pub fn state(&self, key: &str) -> Option<&State> {
        self.map.get(key)
    }

    pub fn is_pending(&self, key: &str) -> bool {
        matches!(self.map.get(key), Some(State::Pending))
    }

    /// Drop queued (not yet started) jobs that fail `keep`.
    pub fn cancel_queued(&mut self, keep: impl Fn(&str) -> bool) {
        let mut q = self.shared.queue.lock().unwrap();
        let mut dropped = Vec::new();
        q.retain(|(k, _, _)| {
            let k2 = keep(k);
            if !k2 {
                dropped.push(k.clone());
            }
            k2
        });
        drop(q);
        for k in dropped {
            self.map.remove(&k);
        }
    }

    /// Free textures that fail `keep` (bounded GPU memory).
    pub fn evict(&mut self, keep: impl Fn(&str) -> bool) {
        self.map.retain(|k, s| matches!(s, State::Pending) || keep(k));
    }

    pub fn forget(&mut self, key: &str) {
        self.map.remove(key);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}
