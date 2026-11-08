//! Drift Doctor: gamepad tester, stick drift diagnosis and anti-drift correction for Windows.

#![windows_subsystem = "windows"]

mod app;
mod correction;
mod input;
mod stats;
mod store;
mod theme;
mod widgets;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Drift Doctor")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1100.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native("Drift Doctor", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
