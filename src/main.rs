#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

/// Display name (single place to rename the app).
pub const APP_NAME: &str = "Seshat";

#[macro_use]
mod i18n;
mod api;
mod app;
mod book;
mod calibre;
mod clippings;
mod dict;
mod fonts;
mod keys;
mod kosync;
mod library;
mod logo;
mod lookup;
mod online;
mod overlay;
mod platform;
mod reader;
mod store;
mod sync;
mod texload;
mod tts;
mod ui;
mod voice;
mod widgets;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a == "--make-icons").unwrap_or(false) {
        let dir = std::path::PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "icons".into()));
        logo::write_icons(&dir).expect("writing icons");
        println!("icons written to {}", dir.display());
        return Ok(());
    }
    // `seshat --info <files…>`: parse books and print a summary (debugging aid).
    if args.first().map(|a| a == "--info").unwrap_or(false) {
        for a in &args[1..] {
            let t = std::time::Instant::now();
            match book::open(std::path::Path::new(a)) {
                Ok(b) => {
                    let (units, chars) = match &b.content {
                        book::Content::Flow(f) => (f.sections.len(), f.total_chars()),
                        book::Content::Fixed(f) => (f.pages, 0),
                    };
                    let cov = book::read_meta(std::path::Path::new(a)).cover.map(|c| c.len()).unwrap_or(0);
                    println!("cover {cov} | {} | {} | {} | sections/pages {units} | chars {chars} | toc {} | {:?}", b.meta.title, b.meta.author, b.meta.language, b.toc.len(), t.elapsed());
                }
                Err(e) => println!("ERR {a}: {e}"),
            }
        }
        return Ok(());
    }
    // `seshat --say <book> [seconds]`: read a book aloud from the start with the
    // configured voice, printing each sentence as it is spoken (debugging aid).
    if args.first().map(|a| a == "--say").unwrap_or(false) {
        let Some(path) = args.get(1) else {
            eprintln!("usage: seshat --say <book> [seconds]");
            return Ok(());
        };
        let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
        let b = match book::open(std::path::Path::new(path)) {
            Ok(b) => std::sync::Arc::new(b),
            Err(e) => {
                eprintln!("{e}");
                return Ok(());
            }
        };
        let settings = store::AppSettings::load();
        let engine = tts::Engine::pick(&settings, &b.meta.language);
        println!("engine: {engine:?}");
        let (tx, rx) = std::sync::mpsc::channel();
        let b2 = b.clone();
        let Some(speaker) = tts::start(b.clone(), book::Loc::new(0, 0, 0), settings.tts_rate, engine, move |ev| {
            let _ = tx.send(ev);
        }) else {
            eprintln!("no speech engine available");
            return Ok(());
        };
        let t0 = std::time::Instant::now();
        while t0.elapsed().as_secs() < secs {
            match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(tts::Event::At(l, end)) => {
                    let text = b2.flow().and_then(|f| f.sections.get(l.section)).and_then(|s| s.blocks.get(l.block)).map(|bl| bl.text()).unwrap_or_default();
                    let s: String = text.chars().skip(l.offset).take(end.saturating_sub(l.offset)).collect();
                    println!("[{:6.2}s] {}", t0.elapsed().as_secs_f32(), s.trim());
                }
                Ok(tts::Event::Done) => {
                    println!("done");
                    break;
                }
                Ok(tts::Event::Error(e)) => {
                    eprintln!("error: {e}");
                    break;
                }
                Err(_) => {}
            }
        }
        speaker.stop();
        tts::shutdown_helper();
        return Ok(());
    }
    let open = args.into_iter().map(std::path::PathBuf::from).find(|p| p.is_file());
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_app_id("seshat")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([560.0, 420.0])
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(logo::icon_data(128)))
            .with_decorations(false)
            .with_transparent(false),
        // Multiple windows on Wayland: a vsync'd swap can block forever on a
        // window the compositor isn't drawing. We repaint on demand anyway.
        vsync: false,
        ..Default::default()
    };
    eframe::run_native("seshat", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))))
}
