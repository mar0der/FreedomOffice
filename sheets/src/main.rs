//! FreedomOffice Sheets: a lightweight, open-source spreadsheet application.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod chart;
mod condfmt;
mod formula;
mod icon;
mod io;
mod model;
mod numfmt;
mod page;
mod print;
mod validation;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let icon = egui::IconData { rgba: icon::icon_rgba(64), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FreedomOffice Sheets")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native("FreedomOffice Sheets", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, path)))))
}
