#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    eframe::run_native(
        "Xonata",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 900.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            let transport = xonata_app::native::NativeTransport::new(cc.egui_ctx.clone());
            Ok(Box::new(xonata_app::ui::Viewer::new(
                cc,
                Box::new(transport),
            )))
        }),
    )
}
#[cfg(target_arch = "wasm32")]
fn main() {}
