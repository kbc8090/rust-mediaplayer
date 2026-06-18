// mediaplayer/src/main.rs
// Windows: hide console in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod decoder;
mod player;
mod ui;

use eframe::egui;

fn main() -> anyhow::Result<()> {
    // Initialize ffmpeg
    ffmpeg_next::init()?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Media Player")
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([480.0, 320.0])
            .with_drag_and_drop(true)
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Media Player",
        options,
        Box::new(|cc| Box::new(app::MediaPlayerApp::new(cc))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

fn load_icon() -> egui::IconData {
    // Minimal embedded icon (32x32 dark play button)
    egui::IconData {
        rgba: vec![0u8; 32 * 32 * 4],
        width: 32,
        height: 32,
    }
}
