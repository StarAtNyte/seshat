#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

/// Display name (single place to rename the app).
pub const APP_NAME: &str = "Seshat";

mod api;
mod app;
mod book;
mod fonts;
mod kosync;
mod library;
mod logo;
mod lookup;
mod online;
mod platform;
mod reader;
mod store;
mod texload;
mod tts;
mod ui;

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
