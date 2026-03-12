mod app;
mod local_proxy;
mod models;
mod storage;
#[allow(dead_code)]
mod system_proxy;
mod tester;
mod ui;

use std::sync::Arc;

fn main() {
    let rt = Arc::new(
        tokio::runtime::Runtime::new().expect("Failed to create tokio runtime"),
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 520.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Proxy Manager",
        options,
        Box::new(|cc| Box::new(app::App::new(cc, rt))),
    )
    .expect("Failed to run eframe");
}
