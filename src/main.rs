mod app;
mod tun_proxy;
mod models;
mod storage;
mod tester;
mod ui;

use std::sync::Arc;

fn main() {
    // Initialize logging: set RUST_LOG=debug for verbose output
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    log::info!("Proxy Manager starting");
    log::info!("exe: {:?}", std::env::current_exe().unwrap_or_default());
    log::info!("cwd: {:?}", std::env::current_dir().unwrap_or_default());

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
