mod app;
mod ui;

fn main() -> eframe::Result<()> {
    rustyseo_core::db::init_db();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([960.0, 600.0])
            .with_title("RustySEO"),
        ..Default::default()
    };

    eframe::run_native(
        "RustySEO",
        options,
        Box::new(|cc| Ok(Box::new(app::RustySeoApp::new(cc)))),
    )
}
