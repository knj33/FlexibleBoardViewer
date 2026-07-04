//! FlexibleBoardViewer — boardviewer + searchable donor-board library.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod settings;
mod tabs;
mod view;

fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("FlexibleBoardViewer"),
        ..Default::default()
    };
    eframe::run_native(
        "FlexibleBoardViewer",
        options,
        Box::new(|cc| {
            let app = app::App::new(cc).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                format!("startup failed: {e}").into()
            })?;
            Ok(Box::new(app))
        }),
    )
}
