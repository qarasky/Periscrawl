mod app;
mod ui;

/// Append core logs (robots decisions, crawl stats, warnings) to
/// `.../rustyseo/gui.log` so GUI-driven crawls are inspectable afterwards.
fn init_logging() {
    let log_path = rustyseo_core::db::get_db_path().with_file_name("gui.log");
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = tracing_subscriber::fmt()
            .with_writer(file)
            .with_ansi(false)
            .try_init();
    }
}

fn main() -> eframe::Result<()> {
    rustyseo_core::db::init_db();
    init_logging();

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
