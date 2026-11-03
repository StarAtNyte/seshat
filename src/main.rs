mod api;
mod app;
mod epub;
mod library;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([700.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "zlib-reader",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
