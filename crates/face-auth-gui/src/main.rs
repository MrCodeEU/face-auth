mod app;
mod camera_texture;
mod inference_worker;
mod tabs;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ort=warn")),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("face-auth")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };

    eframe::run_native(
        "face-auth",
        options,
        Box::new(|_cc| Ok(Box::new(app::FaceAuthApp::new()))),
    )
    .expect("eframe failed to start");
}
