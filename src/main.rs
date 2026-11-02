mod api;
mod app;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 680.0])
            .with_min_inner_size([700.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "zlib-lite",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
